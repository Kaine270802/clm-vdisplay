//! Multi-display async supervisor and session manager.

pub mod session;
pub mod supervisor;

pub use session::{DisplaySession, SessionConfig, SessionEvent};
pub use supervisor::{DisplayInfo, DisplaySupervisor, SupervisorCommand, SupervisorResponse};

pub struct DisplayServer {
    pub supervisor: DisplaySupervisor,
}

impl DisplayServer {
    pub fn new(base_vnc_port: u16, control_socket: String) -> Self {
        Self {
            supervisor: DisplaySupervisor::new(base_vnc_port, control_socket),
        }
    }
}
