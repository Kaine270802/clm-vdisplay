use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Represents the state of a given X11 display number :N
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X11DisplayState {
    /// Actively running X11 server with alive PID and responsive Unix socket
    Active {
        pid: u32,
        lock_path: PathBuf,
        socket_path: PathBuf,
    },
    /// Stale lock or unresponsive/orphaned socket from dead process
    Stale {
        stale_pid: Option<u32>,
        lock_path: PathBuf,
        socket_path: PathBuf,
    },
    /// Display is free to allocate (no locks, no sockets)
    Free {
        lock_path: PathBuf,
        socket_path: PathBuf,
    },
}

pub struct X11Detector;

impl X11Detector {
    /// Get paths for lock file and Unix domain socket for display N
    pub fn get_display_paths(display_num: u32) -> (PathBuf, PathBuf) {
        let lock_path = PathBuf::from(format!("/tmp/.X{}-lock", display_num));
        let socket_path = PathBuf::from(format!("/tmp/.X11-unix/X{}", display_num));
        (lock_path, socket_path)
    }

    /// Probe the status of an X11 display :N
    pub fn probe_display(display_num: u32) -> X11DisplayState {
        let (lock_path, socket_path) = Self::get_display_paths(display_num);

        let lock_pid = Self::read_lock_pid(&lock_path);
        let pid_alive = lock_pid.map(Self::is_pid_alive).unwrap_or(false);

        if pid_alive {
            // Verify socket is actually connectable
            if socket_path.exists() && Self::can_connect_socket(&socket_path) {
                return X11DisplayState::Active {
                    pid: lock_pid.unwrap(),
                    lock_path,
                    socket_path,
                };
            }
        }

        // If lock exists but PID is dead, or socket exists without live PID or socket is dead
        if lock_path.exists() || socket_path.exists() {
            X11DisplayState::Stale {
                stale_pid: lock_pid,
                lock_path,
                socket_path,
            }
        } else {
            X11DisplayState::Free {
                lock_path,
                socket_path,
            }
        }
    }

    /// Safely purge stale lock and socket files
    pub fn purge_stale(state: &X11DisplayState) -> anyhow::Result<()> {
        if let X11DisplayState::Stale {
            lock_path,
            socket_path,
            stale_pid,
        } = state
        {
            warn!(
                "Purging stale X11 artifacts (stale_pid={:?}, lock={:?}, socket={:?})",
                stale_pid, lock_path, socket_path
            );
            if lock_path.exists() {
                let _ = fs::remove_file(lock_path);
            }
            if socket_path.exists() {
                let _ = fs::remove_file(socket_path);
            }
        }
        Ok(())
    }

    /// Connect to X11 server via Unix socket or fallback to x11rb::connect
    pub fn connect_to_display(display_num: u32) -> anyhow::Result<(x11rb::rust_connection::RustConnection, usize)> {
        let socket_path = format!("/tmp/.X11-unix/X{}", display_num);
        if let Ok(unix_stream) = UnixStream::connect(&socket_path) {
            let (stream, _peer_addr) = x11rb::rust_connection::DefaultStream::from_unix_stream(unix_stream)?;
            let conn = x11rb::rust_connection::RustConnection::connect_to_stream(stream, 0)?;
            return Ok((conn, 0));
        }

        // Fallback to standard x11rb::connect if Unix socket not directly accessible
        let display_str = format!(":{}", display_num);
        let (conn, screen) = x11rb::connect(Some(&display_str))?;
        Ok((conn, screen))
    }

    /// Read PID from X11 lock file (format: formatted integer string)
    pub fn read_lock_pid(lock_path: &Path) -> Option<u32> {
        if !lock_path.is_file() {
            return None;
        }
        let content = fs::read_to_string(lock_path).ok()?;
        content.trim().parse::<u32>().ok()
    }

    /// Check if process is alive without delivering signal using kill(pid, 0)
    pub fn is_pid_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Check if Unix domain socket accepts connections
    pub fn can_connect_socket(socket_path: &Path) -> bool {
        UnixStream::connect(socket_path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_read_lock_pid_valid() {
        let dir = tempdir().unwrap();
        let lock_file = dir.path().join(".X999-lock");
        let mut file = fs::File::create(&lock_file).unwrap();
        writeln!(file, "   12345").unwrap();

        let pid = X11Detector::read_lock_pid(&lock_file);
        assert_eq!(pid, Some(12345));
    }

    #[test]
    fn test_read_lock_pid_missing() {
        let path = PathBuf::from("/tmp/non_existent_lock_file_test_9999");
        let pid = X11Detector::read_lock_pid(&path);
        assert_eq!(pid, None);
    }

    #[test]
    fn test_is_pid_alive_self() {
        let my_pid = std::process::id();
        assert!(X11Detector::is_pid_alive(my_pid));
    }

    #[test]
    fn test_is_pid_alive_invalid() {
        assert!(!X11Detector::is_pid_alive(0));
        assert!(!X11Detector::is_pid_alive(99999999));
    }
}
