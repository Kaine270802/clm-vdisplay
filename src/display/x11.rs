use crate::display::framebuffer::SharedFramebuffer;
use crate::x11::capture::X11CaptureEngine;
use crate::x11::input::X11InputInjector;
use crate::x11::supervisor::{X11ProcessGuard, X11Supervisor};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// High-level Headless X11 Server integration managing supervisor, capture, and input
pub struct HeadlessX11Server {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub framebuffer: SharedFramebuffer,
    pub lock_file: PathBuf,
    pub socket_path: PathBuf,
    pub process_guard: Arc<Mutex<Option<X11ProcessGuard>>>,
    pub capture_handle: Arc<Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>>,
    pub input_injector: Arc<Mutex<Option<Arc<X11InputInjector>>>>,
}

impl HeadlessX11Server {
    pub fn new(display_num: u32, width: u32, height: u32, framebuffer: SharedFramebuffer) -> Self {
        let lock_file = PathBuf::from(format!("/tmp/.X{}-lock", display_num));
        let socket_path = PathBuf::from(format!("/tmp/.X11-unix/X{}", display_num));

        Self {
            display_num,
            width,
            height,
            depth: 24,
            framebuffer,
            lock_file,
            socket_path,
            process_guard: Arc::new(Mutex::new(None)),
            capture_handle: Arc::new(Mutex::new(None)),
            input_injector: Arc::new(Mutex::new(None)),
        }
    }

    /// Initialize the X11 server instance, MIT-SHM capture loop, and XTest input injection
    pub async fn initialize(
        &self,
        manage_x11: bool,
        xvfb_path: Option<String>,
        xvfb_args: Option<Vec<String>>,
        fps: u32,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<()> {
        info!("Initializing Headless X11 Server on :{}", self.display_num);

        // 1. Supervise / Ensure X11 server socket readiness
        let mut supervisor = X11Supervisor::with_custom_args(
            self.display_num,
            self.width,
            self.height,
            self.depth,
            xvfb_path,
            xvfb_args,
        );

        let guard = supervisor.ensure_ready(manage_x11).await?;
        *self.process_guard.lock() = Some(guard);

        // 2. Start MIT-SHM & XDamage 60 FPS capture engine
        let capture_engine = X11CaptureEngine::new(self.display_num, self.width, self.height);
        let capture_task = capture_engine.start_capture_loop(
            self.framebuffer.clone(),
            fps,
            cancel_token.child_token(),
        );
        *self.capture_handle.lock() = Some(capture_task);

        // 3. Connect XTest input injector in background
        let display_num = self.display_num;
        let input_inj_slot = self.input_injector.clone();
        tokio::spawn(async move {
            match X11InputInjector::new(display_num) {
                Ok(injector) => {
                    *input_inj_slot.lock() = Some(Arc::new(injector));
                    info!(
                        "XTest input injector initialized on display :{}",
                        display_num
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to initialize XTest input injector on display :{}: {}",
                        display_num, e
                    );
                }
            }
        });

        Ok(())
    }

    /// Get reference to active X11InputInjector if available
    pub fn get_input_injector(&self) -> Option<Arc<X11InputInjector>> {
        self.input_injector.lock().clone()
    }

    /// Auto-maximize client window and update frame
    pub fn update_window_buffer(
        &self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        data: &[u8],
        stride: usize,
    ) {
        {
            let mut fb = self.framebuffer.inner.write();
            fb.update_rect(x, y, w, h, data, stride);
        }
        self.framebuffer.notify_damage();
    }

    /// Clean up X11 lock files, socket, and terminate child Xvfb on exit
    pub fn cleanup(&self) {
        if let Some(mut guard) = self.process_guard.lock().take() {
            guard.cleanup();
        }
    }
}

impl Drop for HeadlessX11Server {
    fn drop(&mut self) {
        self.cleanup();
    }
}
