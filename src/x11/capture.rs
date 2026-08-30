use crate::display::framebuffer::SharedFramebuffer;
use crate::x11::shm::ShmSegment;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::{ConnectionExt as _, ReportLevel};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::ImageFormat;
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

/// High-performance X11 Zero-Copy Framebuffer Capture Engine (MIT-SHM + XDamage)
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

    /// Connect to X11 server and run MIT-SHM / XDamage 60 FPS capture pipeline
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
        let shm_ver = conn.shm_query_version()?.reply()?;
        info!(
            "MIT-SHM extension available: v{}.{}",
            shm_ver.major_version, shm_ver.minor_version
        );

        // 3. Query and setup XDamage extension
        let damage_ver = conn.damage_query_version(1, 1)?.reply()?;
        info!(
            "XDamage extension available: v{}.{}",
            damage_ver.major_version, damage_ver.minor_version
        );

        let damage_id = conn.generate_id()?;
        conn.damage_create(damage_id, root, ReportLevel::BOUNDING_BOX)?
            .check()?;

        // 4. Allocate SysV Shared Memory Segment
        let stride = (self.width * 4) as usize;
        let total_size = stride * (self.height as usize);
        let mut shm_segment = ShmSegment::create(&conn, total_size)?;
        info!(
            "Allocated SysV SHM segment: {} bytes for {}x{}",
            total_size, self.width, self.height
        );

        let dirty_tracker = Arc::new(DirtyTracker::new());
        // Initial full-frame capture
        dirty_tracker.mark_dirty_rect(0, 0, self.width as u16, self.height as u16);

        // 5. Spawn X11 Event Polling Thread
        let conn_arc = Arc::new(conn);
        let dirty_tracker_clone = dirty_tracker.clone();
        let cancel_event = cancel_token.child_token();
        let conn_event_ref = conn_arc.clone();

        let event_handle = std::thread::spawn(move || {
            while !cancel_event.is_cancelled() {
                match conn_event_ref.poll_for_event() {
                    Ok(Some(event)) => {
                        if let x11rb::protocol::Event::DamageNotify(ev) = event {
                            dirty_tracker_clone.mark_dirty_rect(
                                ev.area.x as u16,
                                ev.area.y as u16,
                                ev.area.width,
                                ev.area.height,
                            );
                            let _ = conn_event_ref.damage_subtract(damage_id, 0u32, 0u32);
                        }
                    }
                    Ok(None) => {
                        std::thread::sleep(Duration::from_micros(500));
                    }
                    Err(e) => {
                        warn!("X11 event polling loop terminated: {}", e);
                        break;
                    }
                }
            }
        });

        // 6. Frame Timing & Pacing Loop (target 60 FPS = 16.66ms interval)
        let frame_pacing_nanos = (1_000_000_000u64 / (fps.max(1) as u64)).max(1_000_000);
        let mut tick_timer = interval(Duration::from_nanos(frame_pacing_nanos));
        tick_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!(
            "X11CaptureEngine started at {} FPS (interval: {:?})",
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
                    if let Some(dirty) = dirty_tracker.take_dirty() {
                        let dw = (dirty.max_x.min(self.width as u16).saturating_sub(dirty.min_x)) as u32;
                        let dh = (dirty.max_y.min(self.height as u16).saturating_sub(dirty.min_y)) as u32;

                        if dw > 0 && dh > 0 {
                            // Fetch updated pixels via MIT-SHM DMA into ShmSegment
                            let cookie = conn_arc.shm_get_image(
                                root,
                                0,
                                0,
                                self.width as u16,
                                self.height as u16,
                                !0, // Plane mask: all bitplanes
                                ImageFormat::Z_PIXMAP.into(),
                                shm_segment.shmseg,
                                0,
                            );

                            match cookie {
                                Ok(reply_cookie) => {
                                    if reply_cookie.reply().is_ok() {
                                        let raw_slice = shm_segment.as_slice();
                                        {
                                            let mut fb = framebuffer.inner.write();
                                            fb.update_rect_from_full_frame(
                                                dirty.min_x as u32,
                                                dirty.min_y as u32,
                                                dw,
                                                dh,
                                                raw_slice,
                                            );
                                        }
                                        framebuffer.notify_damage();
                                    }
                                }
                                Err(e) => {
                                    warn!("XShmGetImage error on display :{}: {}", self.display_num, e);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Cleanup
        shm_segment.detach(&conn_arc);
        let _ = event_handle.join();

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
