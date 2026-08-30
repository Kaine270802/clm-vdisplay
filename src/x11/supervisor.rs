use crate::x11::detector::{X11Detector, X11DisplayState};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{error, info, warn};

/// RAII Process Guard that manages child Xvfb lifecycle and cleanup of X11 filesystem artifacts
pub struct X11ProcessGuard {
    pub display_num: u32,
    pub child_pid: Option<u32>,
    pub lock_path: PathBuf,
    pub socket_path: PathBuf,
    pub is_managed: bool,
    pub is_cleaned_up: bool,
}

impl X11ProcessGuard {
    pub fn new(display_num: u32, child_pid: Option<u32>, is_managed: bool) -> Self {
        let (lock_path, socket_path) = X11Detector::get_display_paths(display_num);
        Self {
            display_num,
            child_pid,
            lock_path,
            socket_path,
            is_managed,
            is_cleaned_up: false,
        }
    }

    /// Disarm the guard so it does not terminate the process or delete artifacts upon drop
    pub fn disarm(&mut self) {
        self.is_managed = false;
        self.is_cleaned_up = true;
    }

    /// Explicit cleanup of child process and filesystem artifacts
    pub fn cleanup(&mut self) {
        if self.is_cleaned_up {
            return;
        }
        self.is_cleaned_up = true;

        if self.is_managed {
            if let Some(pid) = self.child_pid {
                if pid > 0 {
                    let raw_pid = Pid::from_raw(pid as i32);
                    info!("Stopping managed Xvfb (pid={}) with SIGTERM", pid);
                    let _ = kill(raw_pid, Signal::SIGTERM);

                    // Wait up to 500ms for graceful child exit
                    for _ in 0..50 {
                        std::thread::sleep(Duration::from_millis(10));
                        if !X11Detector::is_pid_alive(pid) {
                            break;
                        }
                    }

                    // Escalate to SIGKILL if still running
                    if X11Detector::is_pid_alive(pid) {
                        warn!("Escalating to SIGKILL for Xvfb (pid={})", pid);
                        let _ = kill(raw_pid, Signal::SIGKILL);
                        unsafe {
                            let mut status = 0;
                            libc::waitpid(pid as i32, &mut status, libc::WNOHANG);
                        }
                    }
                }
            }

            if self.lock_path.exists() {
                let _ = fs::remove_file(&self.lock_path);
            }
            if self.socket_path.exists() {
                let _ = fs::remove_file(&self.socket_path);
            }
            info!("Cleaned up X11 display artifacts for :{}", self.display_num);
        }
    }
}

impl Drop for X11ProcessGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Supervisor responsible for probing, spawning, and managing X11 server instances
pub struct X11Supervisor {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub xvfb_path: String,
    pub xvfb_args: Option<Vec<String>>,
}

impl X11Supervisor {
    pub fn new(
        display_num: u32,
        width: u32,
        height: u32,
        depth: u32,
        xvfb_path: Option<String>,
    ) -> Self {
        Self {
            display_num,
            width,
            height,
            depth,
            xvfb_path: xvfb_path.unwrap_or_else(|| "Xvfb".to_string()),
            xvfb_args: None,
        }
    }

    pub fn with_custom_args(
        display_num: u32,
        width: u32,
        height: u32,
        depth: u32,
        xvfb_path: Option<String>,
        xvfb_args: Option<Vec<String>>,
    ) -> Self {
        Self {
            display_num,
            width,
            height,
            depth,
            xvfb_path: xvfb_path.unwrap_or_else(|| "Xvfb".to_string()),
            xvfb_args,
        }
    }

    /// Ensure the X11 display is active and ready for capture / rendering
    pub async fn ensure_ready(&mut self, manage_x11: bool) -> anyhow::Result<X11ProcessGuard> {
        let (lock_path, socket_path) = X11Detector::get_display_paths(self.display_num);
        info!(
            "Probing display :{} (lock={:?}, socket={:?})",
            self.display_num, lock_path, socket_path
        );

        let state = X11Detector::probe_display(self.display_num);

        match state {
            X11DisplayState::Active { pid, .. } => {
                info!(
                    "Detected existing active X11 display :{} (pid={}), attaching...",
                    self.display_num, pid
                );
                Ok(X11ProcessGuard::new(self.display_num, Some(pid), false))
            }
            X11DisplayState::Stale { stale_pid, .. } => {
                if !manage_x11 {
                    anyhow::bail!(
                        "Display :{} has stale artifacts (stale_pid={:?}) and manage_x11 is disabled",
                        self.display_num,
                        stale_pid
                    );
                }
                info!(
                    "Purging stale X11 display :{} artifacts before spawn",
                    self.display_num
                );
                X11Detector::purge_stale(&state)?;
                self.spawn_xvfb().await
            }
            X11DisplayState::Free { .. } => {
                if !manage_x11 {
                    anyhow::bail!(
                        "Display :{} is not active and manage_x11 is disabled (attach mode)",
                        self.display_num
                    );
                }
                self.spawn_xvfb().await
            }
        }
    }

    /// Spawn a supervised child Xvfb process and poll for socket readiness
    async fn spawn_xvfb(&self) -> anyhow::Result<X11ProcessGuard> {
        // Ensure /tmp/.X11-unix exists with sticky bit permissions (01777)
        let socket_dir = PathBuf::from("/tmp/.X11-unix");
        if !socket_dir.exists() {
            let _ = fs::create_dir_all(&socket_dir);
            let _ = fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o1777));
        }

        let display_arg = format!(":{}", self.display_num);
        let screen_arg = format!("{}x{}x{}", self.width, self.height, self.depth);

        let args = if let Some(ref custom_args) = self.xvfb_args {
            custom_args.clone()
        } else {
            vec![
                display_arg.clone(),
                "-screen".to_string(),
                "0".to_string(),
                screen_arg.clone(),
                "-ac".to_string(),
                "-nolisten".to_string(),
                "tcp".to_string(),
                "-noreset".to_string(),
                "+extension".to_string(),
                "GLX".to_string(),
                "+extension".to_string(),
                "RANDR".to_string(),
                "+extension".to_string(),
                "RENDER".to_string(),
                "+extension".to_string(),
                "DAMAGE".to_string(),
                "+extension".to_string(),
                "Composite".to_string(),
                "+extension".to_string(),
                "MIT-SHM".to_string(),
            ]
        };

        info!(
            "Spawning managed Xvfb (cmd: {} {})",
            self.xvfb_path,
            args.join(" ")
        );

        let mut child = tokio::process::Command::new(&self.xvfb_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to execute Xvfb at {:?}: {}. Make sure xvfb is installed.",
                    self.xvfb_path,
                    e
                )
            })?;

        let child_pid = child.id().ok_or_else(|| {
            anyhow::anyhow!("Failed to retrieve process ID of spawned Xvfb instance")
        })?;

        info!("Managed Xvfb spawned (pid={})", child_pid);

        // High-frequency socket readiness polling (< 50ms typical)
        let socket_path = PathBuf::from(format!("/tmp/.X11-unix/X{}", self.display_num));
        let start = Instant::now();
        let timeout = Duration::from_secs(3);
        let poll_interval = Duration::from_millis(1);

        while start.elapsed() < timeout {
            // Check if child exited prematurely
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut stderr_msg = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        use tokio::io::AsyncReadExt;
                        let _ = stderr.read_to_string(&mut stderr_msg).await;
                    }
                    error!(
                        "Managed Xvfb exited prematurely (status={:?}): {}",
                        status, stderr_msg
                    );
                    anyhow::bail!(
                        "Xvfb exited prematurely with status {:?}: {}",
                        status,
                        stderr_msg.trim()
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Error polling Xvfb process status: {}", e);
                }
            }

            // Check if Unix socket is active and connectable
            if socket_path.exists() && UnixStream::connect(&socket_path).is_ok() {
                let elapsed = start.elapsed();
                info!("X11 socket {:?} ready in {:?}", socket_path, elapsed);
                return Ok(X11ProcessGuard::new(
                    self.display_num,
                    Some(child_pid),
                    true,
                ));
            }

            sleep(poll_interval).await;
        }

        // Timed out: kill child and clean up
        let _ = child.kill().await;
        anyhow::bail!(
            "Timed out waiting for X11 socket {:?} after {:?}",
            socket_path,
            timeout
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_guard_disarm() {
        let mut guard = X11ProcessGuard::new(999, Some(12345), true);
        assert!(guard.is_managed);
        assert!(!guard.is_cleaned_up);

        guard.disarm();
        assert!(!guard.is_managed);
        assert!(guard.is_cleaned_up);
    }

    #[test]
    fn test_supervisor_constructor() {
        let supervisor = X11Supervisor::new(100, 1920, 1080, 24, None);
        assert_eq!(supervisor.display_num, 100);
        assert_eq!(supervisor.width, 1920);
        assert_eq!(supervisor.height, 1080);
        assert_eq!(supervisor.depth, 24);
        assert_eq!(supervisor.xvfb_path, "Xvfb");
    }
}
