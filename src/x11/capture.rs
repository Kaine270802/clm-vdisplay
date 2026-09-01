use crate::display::framebuffer::SharedFramebuffer;
use crate::metrics::GLOBAL_METRICS;
use crate::x11::shm::ShmSegment;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::{self, ConnectionExt as DamageConnectionExt};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;
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

/// High-performance X11 Zero-Copy Framebuffer Capture Engine.
///
/// Damage-event-driven: XDamage extension reports which regions changed, so
/// full-frame copies only happen when the screen actually changed (near-zero
/// CPU when idle). Falls back to tick-based polling if XDamage is unavailable.
pub struct X11CaptureEngine {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
}

/// Maximum interval between event-queue polls when the screen is idle.
const IDLE_POLL_MAX: Duration = Duration::from_millis(25);
/// Minimum interval between event-queue polls (keeps CPU near zero when idle).
const IDLE_POLL_MIN: Duration = Duration::from_millis(2);

/// Recompute capture pacing from a live FPS cap (min 1). Called each wait.
pub fn frame_pacing_from_fps(fps: u32) -> Duration {
    Duration::from_nanos((1_000_000_000u64 / (fps.max(1) as u64)).max(1_000_000))
}

/// Time left until the next grab is allowed. Zero if `elapsed` already meets `pacing`.
pub fn remaining_pacing(elapsed: Duration, pacing: Duration) -> Duration {
    pacing.saturating_sub(elapsed)
}

fn load_fps(fps: &AtomicU32) -> u32 {
    fps.load(Ordering::Relaxed).max(1)
}

/// Full-frame SHM grab is only useful while an RFB client is attached.
/// Drain/XDamage still run when this returns false; `needs_capture` is left set.
#[inline]
pub fn grab_while_clients_attached(active_connections: u64) -> bool {
    active_connections > 0
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
        fps: Arc<AtomicU32>,
        cancel_token: CancellationToken,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        // Run the whole capture pipeline on the blocking thread-pool so the
        // heavy shm copy + FNV hashing never starve the async RFB workers.
        tokio::task::spawn_blocking(move || {
            // Dedicated single-thread runtime for the blocking capture loop.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build capture runtime");
            rt.block_on(self.run_capture_loop(framebuffer, fps, cancel_token))
        })
    }

    /// Connect to X11 server and run a damage-event-driven MIT-SHM capture
    /// pipeline with xproto fallback, adaptive idle polling, and auto-reconnect.
    pub async fn run_capture_loop(
        self,
        framebuffer: SharedFramebuffer,
        fps: Arc<AtomicU32>,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let display_str = format!(":{}", self.display_num);
        let initial_fps = load_fps(&fps);

        info!(
            "X11CaptureEngine (damage-event-driven) started for display {} (fps cap {}, live AtomicU32)",
            display_str, initial_fps
        );

        while !cancel_token.is_cancelled() {
            // 1. Establish connection to X server with retry
            let (conn, screen_num) = match self
                .connect_with_retry(&display_str, 50, Duration::from_millis(50))
                .await
            {
                Ok(res) => res,
                Err(e) => {
                    warn!(
                        "Failed to connect to X11 display {}: {}. Retrying in 1s...",
                        display_str, e
                    );
                    tokio::select! {
                        _ = cancel_token.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
                    }
                }
            };

            let screen = &conn.setup().roots[screen_num];
            let root = screen.root;

            // 2. Query and verify MIT-SHM extension
            let shm_segment = match conn.shm_query_version() {
                Ok(cookie) => match cookie.reply() {
                    Ok(shm_ver) => {
                        info!(
                            "MIT-SHM extension available: v{}.{} on {}",
                            shm_ver.major_version, shm_ver.minor_version, display_str
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

            // 3. Register XDamage on the root window (NON_EMPTY => single
            //    bounding-box event per change burst; cheapest event level).
            let damage_id: Option<u32> = match conn.damage_query_version(0, 0) {
                Ok(cookie) => match cookie.reply() {
                    Ok(ver) => {
                        let id = conn.generate_id()?;
                        match conn.damage_create(id, root, damage::ReportLevel::NON_EMPTY) {
                            Ok(_) => {
                                info!(
                                    "XDamage registered on {} (v{}.{}), switching to event-driven capture",
                                    display_str, ver.major_version, ver.minor_version
                                );
                                Some(id)
                            }
                            Err(e) => {
                                warn!(
                                    "damage_create failed on {} ({}), falling back to tick polling",
                                    display_str, e
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "damage_query_version reply failed on {}: {}, falling back to tick polling",
                            display_str, e
                        );
                        None
                    }
                },
                Err(e) => {
                    warn!(
                        "damage_query_version request failed on {}: {}, falling back to tick polling",
                        display_str, e
                    );
                    None
                }
            };

            let mut consecutive_errors: u32 = 0;
            let mut last_warn_time = Instant::now();
            // First frame is always captured immediately so clients see content
            // even while the screen stays static.
            let mut needs_capture = true;
            let mut last_capture = Instant::now() - frame_pacing_from_fps(load_fps(&fps));
            let mut idle_poll = IDLE_POLL_MIN;

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        info!("Shutting down X11CaptureEngine for display {}", display_str);
                        if let Some(mut shm) = shm_segment {
                            shm.detach(&conn);
                        }
                        return Ok(());
                    }
                    _ = tokio::time::sleep(idle_poll) => {
                        // A. Drain pending damage events (cheap: non-blocking
                        //    read of the socket event queue).
                        let dirty = DirtyTracker::new();
                        let mut events_drained = 0usize;
                        let mut drain_broken = false;
                        while events_drained < 256 {
                            match conn.poll_for_event() {
                                Ok(Some(ev)) => {
                                    events_drained += 1;
                                    if let x11rb::protocol::Event::DamageNotify(dn) = ev {
                                        let area = dn.area;
                                        if area.width > 0 && area.height > 0 {
                                            let x = if area.x >= 0 { area.x as u16 } else { 0 };
                                            let y = if area.y >= 0 { area.y as u16 } else { 0 };
                                            dirty.mark_dirty_rect(x, y, area.width, area.height);
                                        }
                                    }
                                }
                                Ok(None) => break,
                                Err(_) => {
                                    drain_broken = true;
                                    break;
                                }
                            }
                        }

                        if drain_broken {
                            consecutive_errors = consecutive_errors.saturating_add(1);
                        }

                        // B. Reset the accumulated damage region on the server.
                        // Region 0 = None (reset toàn bộ, không repair).
                        if let Some(d) = damage_id {
                            if events_drained > 0 {
                                let _ = conn.damage_subtract(d, 0u32, 0u32);
                            }
                        }

                        // C. Decide whether a capture is due.
                        if framebuffer.take_full_capture_request() {
                            needs_capture = true;
                        }
                        if damage_id.is_none() {
                            // Fallback: tick-based polling at the requested fps.
                            needs_capture = true;
                        }
                        if events_drained > 0 {
                            needs_capture = true;
                        }

                        if !needs_capture {
                            // Screen static: back the poll interval off toward
                            // IDLE_POLL_MAX so idle CPU cost stays near zero.
                            idle_poll = (idle_poll.saturating_mul(2)).min(IDLE_POLL_MAX);
                            continue;
                        }

                        // No viewer: skip shm_get_image. Keep needs_capture so a
                        // newly attached client grabs on the next tick. Back off
                        // toward IDLE_POLL_MAX (not IDLE_POLL_MIN).
                        if !grab_while_clients_attached(
                            GLOBAL_METRICS.active_connections.load(Ordering::Relaxed),
                        ) {
                            idle_poll = (idle_poll.saturating_mul(2)).min(IDLE_POLL_MAX);
                            continue;
                        }

                        // Enforce the fps cap between *successful* captures.
                        // Failed grabs must not consume a frame slot.
                        // Recompute from the live atomic each wait (SetFps / --fps seed).
                        let fps_now = load_fps(&fps);
                        let frame_pacing = frame_pacing_from_fps(fps_now);
                        let remaining = remaining_pacing(last_capture.elapsed(), frame_pacing);
                        if remaining > Duration::ZERO {
                            // Sleep the pacing remainder (not IDLE_POLL_MIN).
                            // Do not clamp to IDLE_POLL_MAX — that would still wake ~40 Hz.
                            // Do not stamp last_capture; after this select sleep we
                            // drain XDamage, re-load the FPS atomic, and re-check
                            // against the new pacing (SetFps 15↔30 during the wait).
                            idle_poll = remaining;
                            continue;
                        }
                        // Slot open: grab, then restore fast poll for the next
                        // damage burst. last_capture is stamped only on success.
                        idle_poll = IDLE_POLL_MIN;

                        // D. Capture (MIT-SHM primary path, xproto fallback).
                        let mut capture_succeeded = false;
                        if let Some(ref shm) = shm_segment {
                            let cookie = conn.shm_get_image(
                                root,
                                0,
                                0,
                                self.width as u16,
                                self.height as u16,
                                !0,
                                x11rb::protocol::xproto::ImageFormat::Z_PIXMAP.into(),
                                shm.shmseg,
                                0,
                            );
                            match cookie {
                                Ok(reply_cookie) => match reply_cookie.reply() {
                                    Ok(_) => {
                                        let raw_slice = shm.as_slice();
                                        {
                                            let mut fb = framebuffer.inner.write();
                                            fb.update_rect_from_full_frame(
                                                0,
                                                0,
                                                self.width,
                                                self.height,
                                                raw_slice,
                                            );
                                        }
                                        capture_succeeded = true;
                                        consecutive_errors = 0;
                                    }
                                    Err(e) => {
                                        consecutive_errors = consecutive_errors.saturating_add(1);
                                        if last_warn_time.elapsed() >= Duration::from_secs(3) {
                                            warn!(
                                                "XShmGetImage reply error on display {}: {}, falling back to xproto",
                                                display_str, e
                                            );
                                            last_warn_time = Instant::now();
                                        }
                                    }
                                },
                                Err(e) => {
                                    consecutive_errors = consecutive_errors.saturating_add(1);
                                    if last_warn_time.elapsed() >= Duration::from_secs(3) {
                                        warn!(
                                            "XShmGetImage request error on display {}: {}, falling back to xproto",
                                            display_str, e
                                        );
                                        last_warn_time = Instant::now();
                                    }
                                }
                            }
                        }

                        if !capture_succeeded {
                            match conn.get_image(
                                x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
                                root,
                                0,
                                0,
                                self.width as u16,
                                self.height as u16,
                                !0,
                            ) {
                                Ok(cookie) => match cookie.reply() {
                                    Ok(reply) => {
                                        {
                                            let mut fb = framebuffer.inner.write();
                                            fb.update_rect_from_full_frame(
                                                0,
                                                0,
                                                self.width,
                                                self.height,
                                                &reply.data,
                                            );
                                        }
                                        capture_succeeded = true;
                                        consecutive_errors = 0;
                                    }
                                    Err(e) => {
                                        consecutive_errors = consecutive_errors.saturating_add(1);
                                        if last_warn_time.elapsed() >= Duration::from_secs(3) {
                                            warn!(
                                                "xproto get_image reply error on display {}: {}",
                                                display_str, e
                                            );
                                            last_warn_time = Instant::now();
                                        }
                                    }
                                },
                                Err(e) => {
                                    consecutive_errors = consecutive_errors.saturating_add(1);
                                    if last_warn_time.elapsed() >= Duration::from_secs(3) {
                                        warn!(
                                            "xproto get_image request error on display {}: {}",
                                            display_str, e
                                        );
                                        last_warn_time = Instant::now();
                                    }
                                }
                            }
                        }

                        // Only drop needs_capture after a successful grab. A
                        // failed/too-early first capture must retry without
                        // waiting for XDamage (static screen => empty FB).
                        if capture_succeeded {
                            needs_capture = false;
                            last_capture = Instant::now();
                            framebuffer.note_capture_complete();
                        } else {
                            needs_capture = true;
                        }

                        // E. X11 connection broken -> reconnect.
                        if consecutive_errors >= fps_now.saturating_mul(2).max(8) {
                            warn!(
                                "X11 connection broken on {} ({} consecutive errors), reconnecting...",
                                display_str, consecutive_errors
                            );
                            if let Some(mut shm) = shm_segment {
                                shm.detach(&conn);
                            }
                            break;
                        }
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
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

    #[test]
    fn test_frame_pacing_recomputes_from_atomic() {
        let fps = AtomicU32::new(15);
        let p15 = frame_pacing_from_fps(load_fps(&fps));
        fps.store(30, Ordering::Relaxed);
        let p30 = frame_pacing_from_fps(load_fps(&fps));
        assert!(p30 < p15);
        assert_eq!(p15, Duration::from_nanos(1_000_000_000 / 15));
        fps.store(0, Ordering::Relaxed);
        let pmin = frame_pacing_from_fps(load_fps(&fps));
        assert_eq!(pmin, Duration::from_nanos(1_000_000_000));
    }

    #[test]
    fn test_frame_pacing_from_fps_15() {
        assert_eq!(
            frame_pacing_from_fps(15),
            Duration::from_nanos(1_000_000_000 / 15)
        );
    }

    #[test]
    fn test_remaining_pacing() {
        let pacing_15 = frame_pacing_from_fps(15);
        let r0 = remaining_pacing(Duration::ZERO, pacing_15);
        assert_eq!(r0, pacing_15);
        assert_eq!(r0, Duration::from_nanos(1_000_000_000 / 15));

        assert_eq!(remaining_pacing(pacing_15, pacing_15), Duration::ZERO);
        assert_eq!(
            remaining_pacing(pacing_15 + Duration::from_millis(1), pacing_15),
            Duration::ZERO
        );

        let elapsed = Duration::from_millis(10);
        let pacing = Duration::from_millis(33);
        let rem = remaining_pacing(elapsed, pacing);
        assert_eq!(rem, Duration::from_millis(23));

        // identity: remaining + elapsed == pacing when elapsed < pacing
        assert_eq!(rem + elapsed, pacing);
        assert_eq!(r0 + Duration::ZERO, pacing_15);
    }

    #[test]
    fn test_grab_while_clients_attached() {
        assert!(!grab_while_clients_attached(0));
        assert!(grab_while_clients_attached(1));
        assert!(grab_while_clients_attached(2));
    }
}
