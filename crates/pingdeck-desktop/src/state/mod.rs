//! Headless, UI-framework-agnostic application ViewModels, routing, command palette engine,
//! and stream reducers.

pub mod chat_reducer;
pub mod palette;
pub mod shell;

pub use chat_reducer::{
    ChatThreadState, LiveApproval, LiveCostPause, LiveFileChange, LiveSubagent, LiveToolCall,
};
pub use palette::{PaletteAction, PaletteCategory, PaletteEngine, PaletteItem};
pub use shell::{
    NavGroup, NavModule, SettingsGroup, SettingsSection, ShellState, ToastKind, ToastMessage,
    UtilityDialog,
};
