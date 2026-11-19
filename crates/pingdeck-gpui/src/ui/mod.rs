//! Sole `gpui-kit` component isolation boundary for PingDeck.
//!
//! Feature views in `src/views/` import ONLY from `crate::ui::*` (enforced by
//! `cargo xtask check-arch`). Upgrading or replacing `gpui-component` touches only this module.

pub mod assets;
pub mod icons;
pub mod primitives;
pub mod theme;
pub mod widgets;

// Controlled re-exports of stateful headless/controller types from `gpui-component` so views
// never reference `gpui_kit::component` directly.
pub use gpui_kit::Anchor;
pub use gpui_kit::component::{
    Disableable, Root, Sizable, Size, StyledExt, WindowExt,
    button::{ButtonRounded, ButtonVariants},
    chart::{AreaChart, BarChart, LineChart},
    checkbox::Checkbox,
    collapsible::Collapsible,
    diff::{Diff, DiffFile, DiffState},
    input::{EditorState, InputEvent, InputState, TextareaState},
    popover::{Popover, PopoverState},
    progress::Progress,
    resizable::{ResizablePanelGroup, h_resizable, resizable_panel, v_resizable},
    scroll::ScrollableElement,
    sheet::Sheet,
    slider::{Slider, SliderEvent, SliderState, SliderValue},
    spinner::Spinner,
    tab::{Tab, TabBar},
    table::{Column, ColumnSort, TableDelegate, TableState},
};

pub use assets::{APP_ICON_PNG_BYTES, PingDeckAssets};
pub use icons::{UiIcon, app_icon, icon, icon_sized, icon_xs, pingmark};
pub use primitives::{
    Tone, app_title_bar, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary, card,
    code_editor, empty_state, field_row, h_row, kbd_chip, list_row, markdown_view, method_badge,
    multiline_input, pane_box, section_heading, status_dot, streaming_markdown_view, table_column,
    text_input, title_bar_options, toggle_switch, v_col, virtual_table,
};
pub use theme::{
    PingDeckPalette, apply_palette, is_compact, palette, resolve_is_dark, sync_window_scale,
    to_hsla,
};
pub use widgets::{
    StatusShape, borderless_multiline, checkbox, collapsible_box, diff_lines_preview, filter_chip,
    kv_chip, pill_btn, popover, progress_bar, round_icon_btn, search_input, select_pill, slider,
    spinner_xs, status_indicator, tab_bar_segmented, tab_bar_underline, tab_item, top_accent_card,
    wrap_root,
};
