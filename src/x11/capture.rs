use crate::display::framebuffer::SharedFramebuffer;
use crate::x11::shm::ShmSegment;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

/// Bounding rectangle for accumulated damaged screen regions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyBounds {
    pub min_x: u16,
    pub min_y: u16,
    pub max_x: u16,
    pub max_y: u16,
}

/// Lockless atomic flag + mutex bounding box dirty area tracker
pub struct DirtyTracker {
    is_dirty: AtomicBool,
    bounds: Mutex<Option<DirtyBounds>>,
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self {
            is_dirty: AtomicBool::new(false),
            bounds: Mutex::new(None),
        }
    }

    /// Mark a rectangular region as dirty
    pub fn mark_dirty_rect(&self, x: u16, y: u16, w: u16, h: u16) {
        if w == 0 || h == 0 {
            return;
        }
        let mut guard = self.bounds.lock();
        match *guard {
            Some(ref mut b) => {
                b.min_x = b.min_x.min(x);
                b.min_y = b.min_y.min(y);
                b.max_x = b.max_x.max(x.saturating_add(w));
                b.max_y = b.max_y.max(y.saturating_add(h));
            }
            None => {
                *guard = Some(DirtyBounds {
                    min_x: x,
                    min_y: y,
                    max_x: x.saturating_add(w),
                    max_y: y.saturating_add(h),
                });
            }
        }
        self.is_dirty.store(true, Ordering::Release);
    }

    /// Take the current accumulated dirty region and reset dirty flag
    pub fn take_dirty(&self) -> Option<DirtyBounds> {
        if !self.is_dirty.swap(false, Ordering::AcqRel) {
            return None;
        }
        let mut guard = self.bounds.lock();
        guard.take()
    }
}

/// High-performance X11 Zero-Copy Framebuffer Capture Engine (MIT-SHM + xproto fallback)
pub struct X11CaptureEngine {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
}

impl X11CaptureEngine {
    pub fn new(display_num: u32, width: u32, height: u32) -> Self {
        Self {
            display_num,
            width,
            height,
        }
    }

    /// Start the capture loop in a background tokio task
    pub fn start_capture_loop(
        self,
        framebuffer: SharedFramebuffer,
        fps: u32,
        cancel_token: CancellationToken,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::spawn(async move {
            self.run_capture_loop(framebuffer, fps, cancel_token).await
        })
    }

    /// Connect to X11 server and run MIT-SHM 60 FPS capture pipeline with xproto fallback
    pub async fn run_capture_loop(
        self,
        framebuffer: SharedFramebuffer,
        fps: u32,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let display_str = format!(":{}", self.display_num);
        info!("Connecting X11CaptureEngine to display {}", display_str);

        // 1. Establish connection to X server with retry
        let (conn, screen_num) = self
            .connect_with_retry(&display_str, 50, Duration::from_millis(50))
            .await?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        // 2. Query and verify MIT-SHM extension
        let shm_segment = match conn.shm_query_version() {
            Ok(cookie) => match cookie.reply() {
                Ok(shm_ver) => {
                    info!(
                        "MIT-SHM extension available: v{}.{}",
                        shm_ver.major_version, shm_ver.minor_version
                    );
                    let stride = (self.width * 4) as usize;
                    let total_size = stride * (self.height as usize);
                    match ShmSegment::create(&conn, total_size) {
                        Ok(seg) => {
                            info!(
                                "Allocated SysV SHM segment: {} bytes for {}x{}",
                                total_size, self.width, self.height
                            );
                            Some(seg)
                        }
                        Err(e) => {
                            warn!(
                                "Failed to allocate MIT-SHM segment (falling back to xproto::get_image): {}",
                                e
                            );
                            None
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "MIT-SHM query version reply failed (falling back to xproto::get_image): {}",
                        e
                    );
                    None
                }
            },
            Err(e) => {
                warn!(
                    "MIT-SHM query version request failed (falling back to xproto::get_image): {}",
                    e
                );
                None
            }
        };

        // 3. Frame Timing & Pacing Loop (target 60 FPS = 16.66ms interval)
        let frame_pacing_nanos = (1_000_000_000u64 / (fps.max(1) as u64)).max(1_000_000);
        let mut tick_timer = interval(Duration::from_nanos(frame_pacing_nanos));
        tick_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!(
            "X11CaptureEngine started at {} FPS (interval: {:?}) without background polling contention",
            fps,
            Duration::from_nanos(frame_pacing_nanos)
        );

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down X11CaptureEngine for display :{}", self.display_num);
                    break;
                }
                _ = tick_timer.tick() => {
                    let mut capture_succeeded = false;

                    // Primary capture path: MIT-SHM DMA from X11 root window
                    if let Some(ref shm) = shm_segment {
                        let cookie = conn.shm_get_image(
                            root,
                            0,
                            0,
                            self.width as u16,
                            self.height as u16,
                            !0, // Plane mask: all bitplanes
                            ImageFormat::Z_PIXMAP.into(),
                            shm.shmseg,
                            0,
                        );

                        match cookie {
                            Ok(reply_cookie) => match reply_cookie.reply() {
                                Ok(_) => {
                                    let raw_slice = shm.as_slice();
                                    let has_changes = {
                                        let mut fb = framebuffer.inner.write();
                                        fb.update_rect_from_full_frame(
                                            0,
                                            0,
                                            self.width,
                                            self.height,
                                            raw_slice,
                                        );
                                        fb.has_dirty_tiles()
                                    };
                                    if has_changes {
                                        framebuffer.notify_damage();
                                    }
                                    capture_succeeded = true;
                                }
                                Err(e) => {
                                    warn!(
                                        "XShmGetImage reply error on display :{}: {}, falling back to xproto",
                                        self.display_num, e
                                    );
                                }
                            },
                            Err(e) => {
                                warn!(
                                    "XShmGetImage request error on display :{}: {}, falling back to xproto",
                                    self.display_num, e
                                );
                            }
                        }
                    }

                    // Fallback capture path: standard xproto get_image
                    if !capture_succeeded {
                        match conn.get_image(
                            ImageFormat::Z_PIXMAP,
                            root,
                            0,
                            0,
                            self.width as u16,
                            self.height as u16,
                            !0,
                        ) {
                            Ok(cookie) => match cookie.reply() {
                                Ok(reply) => {
                                    let has_changes = {
                                        let mut fb = framebuffer.inner.write();
                                        fb.update_rect_from_full_frame(
                                            0,
                                            0,
                                            self.width,
                                            self.height,
                                            &reply.data,
                                        );
                                        fb.has_dirty_tiles()
                                    };
                                    if has_changes {
                                        framebuffer.notify_damage();
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "xproto get_image reply error on display :{}: {}",
                                        self.display_num, e
                                    );
                                }
                            },
                            Err(e) => {
                                warn!(
                                    "xproto get_image request error on display :{}: {}",
                                    self.display_num, e
                                );
                            }
                        }
                    }
                }
            }
        }

        // Cleanup
        if let Some(mut shm) = shm_segment {
            shm.detach(&conn);
        }

        Ok(())
    }

    /// Helper to connect to X11 with exponential retry
    async fn connect_with_retry(
        &self,
        display_str: &str,
        max_retries: usize,
        delay: Duration,
    ) -> anyhow::Result<(RustConnection, usize)> {
        for attempt in 1..=max_retries {
            match crate::x11::detector::X11Detector::connect_to_display(self.display_num) {
                Ok(res) => return Ok(res),
                Err(e) => {
                    if attempt == max_retries {
                        return Err(anyhow::anyhow!(
                            "Failed to connect to X11 display {} after {} attempts: {}",
                            display_str,
                            max_retries,
                            e
                        ));
                    }
                    tokio::time::sleep(delay).await;
                }
            }
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_tracker_accumulation() {
        let tracker = DirtyTracker::new();
        assert_eq!(tracker.take_dirty(), None);

        tracker.mark_dirty_rect(10, 20, 50, 40);
        tracker.mark_dirty_rect(30, 10, 100, 200);

        let dirty = tracker.take_dirty().unwrap();
        assert_eq!(dirty.min_x, 10);
        assert_eq!(dirty.min_y, 10);
        assert_eq!(dirty.max_x, 130);
        assert_eq!(dirty.max_y, 210);

        assert_eq!(tracker.take_dirty(), None);
    }
}
