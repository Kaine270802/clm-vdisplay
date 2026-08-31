//! X11 Subsystem: Sub-supervisor, MIT-SHM & XDamage Capture, and XTest Input Injection.

pub mod capture;
pub mod clipboard;
pub mod detector;
pub mod input;
pub mod shm;
pub mod supervisor;

pub use capture::{DirtyBounds, DirtyTracker, X11CaptureEngine};
pub use clipboard::X11ClipboardBridge;
pub use detector::{X11Detector, X11DisplayState};
pub use input::X11InputInjector;
pub use shm::ShmSegment;
pub use supervisor::{X11ProcessGuard, X11Supervisor};
