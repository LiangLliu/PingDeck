//! Sole `gpui-kit` component isolation boundary and unified PingDeck Design System.
//!
//! Feature views in `src/views/` import ONLY from `crate::ui::*` (enforced by
//! `cargo xtask check-arch`). Upgrading or replacing `gpui-component` touches only this module.

pub mod assets;
pub mod data;
pub mod dialog;
pub mod feedback;
pub mod icons;
pub mod layout;
pub mod menus;
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
    dialog::{AlertDialog, Dialog, DialogButtonProps},
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
    table::{Column, ColumnSort, TableDelegate, TableEvent, TableState},
};

pub use assets::{APP_ICON_PNG_BYTES, PingDeckAssets};
pub use data::{choice_chip, data_row, kpi_card, kv_line, table_header};
pub use dialog::modal_dialog;
pub use feedback::{
    alert_banner, dismissible_banner, empty_inline, empty_state, empty_state_with_actions,
};
pub use icons::{UiIcon, app_icon, icon, icon_sized, icon_xs, pingmark};
pub use layout::{
    doc_tab, master_pane, meta_pill, number_field, pane_footer, pane_header, section_header,
    setting_row, subtab_strip, toolbar_strip,
};
pub use menus::{menu_divider, menu_item, method_color, method_pill, popover_header};
pub use primitives::{
    Tone, app_title_bar, badge, btn_danger, btn_ghost, btn_icon, btn_primary, btn_secondary, card,
    code_editor, field_row, h_row, kbd_chip, list_row, markdown_view, method_badge,
    multiline_input, pane_box, section_heading, status_dot, streaming_markdown_view, table_column,
    text_input, title_bar_options, toggle_switch, v_col, virtual_table,
};
pub use theme::{
    PingDeckPalette, apply_palette, is_compact, palette, resolve_is_dark, sync_window_scale,
    to_hsla,
};
pub use widgets::{
    StatusShape, borderless_multiline, checkbox, collapsible_box, diff_lines_preview, filter_chip,
    kv_chip, pill_btn, popover, popover_trigger_btn, progress_bar, round_icon_btn, search_input,
    select_pill, slider, spinner_xs, status_indicator, tab_bar_segmented, tab_bar_underline,
    tab_item, top_accent_card, wrap_root,
};
