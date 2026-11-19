//! Implements [`pingdeck_desktop::services::HostWindow`] for a GPUI main window.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pingdeck_desktop::core::model::events::EventSink;
use pingdeck_desktop::services::HostWindow;
use pingdeck_desktop::streams::{ChannelSink, StreamReceiver};

/// Commands sent from background OS services (notifications, tray, dock) to the GPUI main loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommand {
    ShowAndFocus,
    Hide,
    OpenChat(String),
    Quit,
}

/// Thread-safe window host handle shared with `pingdeck-desktop` background services.
pub struct GpuiHostWindow {
    focused: AtomicBool,
    commands: ChannelSink<HostCommand>,
}

impl GpuiHostWindow {
    #[must_use]
    pub fn pair() -> (Arc<Self>, StreamReceiver<HostCommand>) {
        let (commands, rx) = ChannelSink::pair();
        let host = Arc::new(Self {
            focused: AtomicBool::new(true),
            commands,
        });
        (host, rx)
    }

    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }

    pub fn send(&self, cmd: HostCommand) {
        let _ = self.commands.emit(cmd);
    }
}

impl HostWindow for GpuiHostWindow {
    fn is_focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn show_and_focus(&self) {
        self.send(HostCommand::ShowAndFocus);
    }

    fn hide(&self) {
        self.send(HostCommand::Hide);
    }

    fn request_open_chat(&self, session_id: &str) {
        self.send(HostCommand::OpenChat(session_id.to_owned()));
    }

    fn request_quit(&self) {
        self.send(HostCommand::Quit);
    }
}
