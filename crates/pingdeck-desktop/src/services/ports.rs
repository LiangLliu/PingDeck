//! Trait seams implemented by any concrete GUI shell (GPUI, headless test doubles, etc.).

use std::path::PathBuf;

/// Kind of native message dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Info,
    Warning,
    Error,
}

/// File extension filter for open/save file pickers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// Specification for picking a file to open.
#[derive(Debug, Clone, Default)]
pub struct FilePickerSpec {
    pub title: Option<String>,
    pub filters: Vec<FileFilter>,
    pub starting_dir: Option<PathBuf>,
}

/// Specification for choosing a file destination to save.
#[derive(Debug, Clone, Default)]
pub struct SaveFileSpec {
    pub title: Option<String>,
    pub file_name: Option<String>,
    pub filters: Vec<FileFilter>,
    pub starting_dir: Option<PathBuf>,
}

/// Main window control surface implemented by the concrete UI shell.
pub trait HostWindow: Send + Sync + 'static {
    /// Whether the main window currently has focus.
    fn is_focused(&self) -> bool;
    /// Brings the main window back from hidden/minimised state and focuses it.
    fn show_and_focus(&self);
    /// Hides the main window (macOS red-button behavior while services keep running).
    fn hide(&self);
    /// Requests the UI shell to focus and navigate to a specific chat session.
    fn request_open_chat(&self, session_id: &str);
    /// Requests the UI shell to begin the graceful quit flow.
    fn request_quit(&self);
}

/// OS dock badge and attention bounce seam.
pub trait HostDock: Send + Sync + 'static {
    /// Sets the numeric badge count (`None` clears it).
    fn set_badge_count(&self, count: Option<i64>);
    /// Requests informational user attention (e.g., macOS Dock bounce).
    fn request_informational_attention(&self);
}

/// Native dialog seam (message alerts + open/save file/folder pickers).
pub trait HostDialog: Send + Sync + 'static {
    fn show_message(&self, kind: DialogKind, title: &str, body: &str);
    fn pick_file(&self, spec: &FilePickerSpec) -> Option<PathBuf>;
    fn pick_files(&self, spec: &FilePickerSpec) -> Vec<PathBuf>;
    fn pick_folder(&self, title: Option<&str>) -> Option<PathBuf>;
    fn save_file(&self, spec: &SaveFileSpec) -> Option<PathBuf>;
}

/// No-op [`HostDock`] for platforms without a dock or headless tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDock;

impl HostDock for NoopDock {
    fn set_badge_count(&self, _count: Option<i64>) {}
    fn request_informational_attention(&self) {}
}
