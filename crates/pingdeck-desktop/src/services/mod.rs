//! UI-framework-agnostic desktop OS service ports and pure-Rust engines.

pub mod clipboard;
pub mod dialog;
pub mod dock;
pub mod grants;
pub mod notify;
pub mod ports;
pub mod tray;

pub use clipboard::{set_secret_text, set_text};
pub use dialog::RfdDialog;
pub use dock::{DockPlan, start_dock};
pub use grants::ImportGrants;
pub use notify::start_notifications;
pub use ports::{
    DialogKind, FileFilter, FilePickerSpec, HostDialog, HostDock, HostWindow, NoopDock,
    SaveFileSpec,
};
pub use tray::{TraySnapshot, labels as tray_labels, snapshot as tray_snapshot, status_line};
