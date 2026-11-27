//! Default pure-Rust [`HostDialog`] implementation using `rfd`.

use std::path::PathBuf;

use rfd::{FileDialog, MessageButtons, MessageDialog, MessageLevel};

use super::ports::{DialogKind, FilePickerSpec, HostDialog, SaveFileSpec};

/// Native OS dialog adapter backed by `rfd` (AppKit on macOS, xdg-desktop-portal on Linux).
#[derive(Debug, Clone, Copy, Default)]
pub struct RfdDialog;

impl HostDialog for RfdDialog {
    fn show_message(&self, kind: DialogKind, title: &str, body: &str) {
        let level = match kind {
            DialogKind::Info => MessageLevel::Info,
            DialogKind::Warning => MessageLevel::Warning,
            DialogKind::Error => MessageLevel::Error,
        };
        let _ = MessageDialog::new()
            .set_level(level)
            .set_title(title)
            .set_description(body)
            .set_buttons(MessageButtons::Ok)
            .show();
    }

    fn pick_file(&self, spec: &FilePickerSpec) -> Option<PathBuf> {
        build_open_dialog(spec).pick_file()
    }

    fn pick_files(&self, spec: &FilePickerSpec) -> Vec<PathBuf> {
        build_open_dialog(spec).pick_files().unwrap_or_default()
    }

    fn pick_folder(&self, title: Option<&str>) -> Option<PathBuf> {
        let mut dialog = FileDialog::new();
        if let Some(title) = title {
            dialog = dialog.set_title(title);
        }
        dialog.pick_folder()
    }

    fn save_file(&self, spec: &SaveFileSpec) -> Option<PathBuf> {
        let mut dialog = FileDialog::new();
        if let Some(title) = &spec.title {
            dialog = dialog.set_title(title);
        }
        if let Some(name) = &spec.file_name {
            dialog = dialog.set_file_name(name);
        }
        for filter in &spec.filters {
            let exts: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter(&filter.name, &exts);
        }
        dialog.save_file()
    }
}

fn build_open_dialog(spec: &FilePickerSpec) -> FileDialog {
    let mut dialog = FileDialog::new();
    if let Some(title) = &spec.title {
        dialog = dialog.set_title(title);
    }
    if let Some(dir) = &spec.starting_dir {
        dialog = dialog.set_directory(dir);
    }
    for filter in &spec.filters {
        let exts: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &exts);
    }
    dialog
}
