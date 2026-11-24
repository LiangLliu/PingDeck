//! Main PingDeck desktop application shell: orchestrates navigation, core subscriptions,
//! workspace switching, overlays, and persistent local UI state.

pub mod actions;
pub mod chrome;
pub mod module_host;
pub mod palette;
pub mod sidebar;

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Task, Window, div, px,
};
use pingdeck_desktop::core::app::{
    agent_attention_subscribe_impl, app_running_work_impl, set_window_focused, settings_get_impl,
    settings_subscribe_impl, workspace_list_impl, workspace_touch_impl,
};
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::persistence::UiLocalState;
use pingdeck_desktop::state::{
    NavModule, PaletteAction, SettingsSection, ShellState, ToastKind, UtilityDialog,
};
use pingdeck_desktop::streams::{StreamReceiver, channel_sink};
use pingdeck_desktop::theme::PaletteId;

use crate::app::actions::{
    CloseOverlay, CloseTab, HideWindow, Nav1, Nav2, Nav3, Nav4, Nav5, Nav6, Nav7, Nav8, Nav9,
    NewTab, OpenSettings, QuitApp, SaveActive, SendRequest, ToggleConsole, TogglePalette,
    ToggleSidebar,
};
use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::platform::{GpuiHostWindow, HostCommand};
use crate::ui::{
    InputState, apply_palette, palette as active_palette, resolve_is_dark, sync_window_scale,
    to_hsla, v_col,
};
use crate::views::requests::RequestsHandoff;
use crate::views::utility::UtilityPanel;

use chrome::{
    render_palette_overlay, render_startup_banner, render_toast_stack, render_top_drag_edge,
    render_utility_overlay,
};
use module_host::ModuleHost;
use palette::CommandPaletteView;
use sidebar::render_sidebar;

/// Top-level PingDeck window view managing navigation, overlays, and live core subscriptions.
pub struct AppShell {
    pub state: ShellState,
    local_ui: UiLocalState,
    focus_handle: FocusHandle,
    modules: ModuleHost,
    pub(crate) palette_view: Entity<CommandPaletteView>,
    pub(crate) utility_view: Entity<UtilityPanel>,
    pub(crate) workspace_menu_open: bool,
    pub(crate) new_workspace_input: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl AppShell {
    pub fn new(
        host_commands: StreamReceiver<HostCommand>,
        host_window: Arc<GpuiHostWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rt = runtime(cx);
        let settings = settings_get_impl(rt.state());
        let workspaces = workspace_list_impl(rt.state()).unwrap_or_default();
        let local_ui = rt.load_ui_state();
        let mut state = ShellState::new(&local_ui, settings, workspaces);
        state.running_work = app_running_work_impl(rt.state());
        state.gateway_port = state
            .running_work
            .gateway_running
            .then(|| rt.state().llm().gateway.status(rt.state().llm()).port);

        let palette_view = cx.new(|cx| CommandPaletteView::new(window, cx));
        let utility_view = cx.new(|cx| UtilityPanel::new(window, cx));

        let mut tasks = Vec::new();

        // 1. Background post-paint startup (bootstrap_ai + chat retention).
        let (notice_sink, notice_rx) = channel_sink::<Vec<String>>();
        rt.start_background_services(move |notices| {
            let _ = notice_sink.emit(notices);
        });
        tasks.push(subscribe_stream_into(
            cx,
            notice_rx,
            4,
            |this, batches, cx| {
                for notices in batches {
                    if !notices.is_empty() {
                        this.state.startup_notices = notices;
                    }
                }
                this.refresh_running_work(cx);
                cx.notify();
            },
        ));

        // 2. Host OS commands (tray, OS notification clicks, Dock reopen).
        tasks.push(subscribe_stream_into(
            cx,
            host_commands,
            16,
            |this, cmds, cx| {
                for cmd in cmds {
                    match cmd {
                        HostCommand::ShowAndFocus => cx.activate(true),
                        HostCommand::Hide => cx.hide(),
                        HostCommand::OpenChat(session_id) => {
                            cx.activate(true);
                            this.navigate(NavModule::Chat, cx);
                            this.modules.open_chat_session(session_id, cx);
                        }
                        HostCommand::Quit => cx.quit(),
                    }
                }
                cx.notify();
            },
        ));

        // 3. Core AgentAttention stream.
        let (attn_sink, attn_rx) =
            channel_sink::<Sequenced<pingdeck_desktop::core::agent::attention::AgentAttention>>();
        let _ = agent_attention_subscribe_impl(rt.state(), None, attn_sink);
        tasks.push(subscribe_stream_into(
            cx,
            attn_rx,
            16,
            |this, batches, cx| {
                if let Some(last) = batches.into_iter().last() {
                    this.state.attention = last.event;
                    this.refresh_running_work(cx);
                    cx.notify();
                }
            },
        ));

        // 4. Core SettingsEvent stream.
        let (set_sink, set_rx) =
            channel_sink::<pingdeck_desktop::core::model::settings::SettingsEvent>();
        let _ = settings_subscribe_impl(rt.state(), set_sink);
        tasks.push(subscribe_stream_into(cx, set_rx, 8, |this, _, cx| {
            this.state.settings = settings_get_impl(runtime(cx).state());
            this.reapply_theme(cx);
            cx.notify();
        }));

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let act_sub = cx.observe_window_activation(window, move |_this, window, cx| {
            let active = window.is_window_active();
            host_window.set_focused(active);
            set_window_focused(runtime(cx).state(), active);
        });

        Self {
            state,
            local_ui,
            focus_handle,
            modules: ModuleHost::default(),
            palette_view,
            utility_view,
            workspace_menu_open: false,
            new_workspace_input: None,
            _subscriptions: vec![act_sub],
            _tasks: tasks,
        }
    }

    pub fn language(&self) -> Language {
        self.state.settings.general.language
    }

    pub(super) fn persist_local(&mut self, cx: &App) {
        self.state.sync_to_local(&mut self.local_ui);
        runtime(cx).save_ui_state(&self.local_ui);
    }

    fn refresh_running_work(&mut self, cx: &App) {
        let rt = runtime(cx);
        self.state.running_work = app_running_work_impl(rt.state());
        self.state.gateway_port = self
            .state
            .running_work
            .gateway_running
            .then(|| rt.state().llm().gateway.status(rt.state().llm()).port);
    }

    pub(super) fn reapply_theme(&self, cx: &mut Context<Self>) {
        let current_dark = active_palette(cx).is_dark;
        let is_dark = resolve_is_dark(self.state.settings.appearance.theme, current_dark);
        apply_palette(
            self.state.palette,
            is_dark,
            false,
            &self.state.settings.appearance,
            cx,
        );
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        self.persist_local(cx);
        cx.notify();
    }

    pub fn navigate(&mut self, module: NavModule, cx: &mut Context<Self>) {
        self.state.navigate(module);
        self.modules.on_enter_module(module, cx);
        self.workspace_menu_open = false;
        self.persist_local(cx);
        cx.notify();
    }

    pub fn open_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.state.active_settings_section = section;
        self.modules.set_settings_section(section, cx);
        self.state.navigate(NavModule::Settings);
        self.persist_local(cx);
        cx.notify();
    }

    pub fn open_utility(
        &mut self,
        dialog: UtilityDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.active_utility = Some(dialog);
        let ws_id = self.state.active_workspace_id.clone();
        self.utility_view
            .update(cx, |panel, cx| panel.open(dialog, ws_id, window, cx));
        cx.notify();
    }

    pub fn close_overlays(&mut self, cx: &mut Context<Self>) {
        self.state.palette_open = false;
        self.state.active_utility = None;
        self.workspace_menu_open = false;
        cx.notify();
    }

    pub fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.palette_open = !self.state.palette_open;
        if self.state.palette_open {
            let lang = self.language();
            let workspaces = self
                .state
                .workspaces
                .iter()
                .map(|w| (w.id.as_str().to_owned(), w.name.clone()))
                .collect();
            let attention = self
                .state
                .attention
                .approvals
                .iter()
                .map(|a| (a.session_id.clone(), a.summary.clone()))
                .collect();
            let frecency = self.local_ui.palette_frecency.clone();
            self.palette_view.update(cx, |view, cx| {
                view.open(lang, workspaces, attention, frecency, window, cx);
            });
        }
        cx.notify();
    }

    pub fn execute_palette_action(
        &mut self,
        item_id: String,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .local_ui
            .palette_frecency
            .entry(item_id)
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = pingdeck_desktop::core::app::current_time_ms();
        self.state.palette_open = false;

        match action {
            PaletteAction::Navigate(m) => self.navigate(m, cx),
            PaletteAction::OpenSettings(s) => self.open_settings_section(s, cx),
            PaletteAction::OpenUtility(u) => self.open_utility(u, window, cx),
            PaletteAction::SwitchWorkspace(id) => self.switch_workspace(id, cx),
            PaletteAction::OpenChatSession(_) => self.navigate(NavModule::Chat, cx),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(cx),
            PaletteAction::CycleThemeMode => self.toggle_theme_mode(cx),
            PaletteAction::ToggleLanguage => self.toggle_language(cx),
        }
    }

    pub fn switch_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        self.state.active_workspace_id = Some(id.clone());
        self.modules.set_workspace(Some(id.clone()), cx);
        self.workspace_menu_open = false;
        self.persist_local(cx);
        let task = spawn_blocking_into(
            cx,
            move |state| workspace_touch_impl(state, id),
            |_, _, _| {},
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn set_palette(&mut self, palette_id: PaletteId, cx: &mut Context<Self>) {
        self.state.palette = palette_id;
        self.persist_local(cx);
        self.reapply_theme(cx);
        cx.notify();
    }

    fn render_main_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.modules.render_active(
            self.state.active_module,
            self.state.active_workspace_id.clone(),
            self.state.active_settings_section,
            self.state.palette,
            window,
            cx,
        )
    }
}

impl Focusable for AppShell {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self
            .palette_view
            .update(cx, |view, _| view.take_pending_action());
        if let Some((id, action)) = pending {
            self.execute_palette_action(id, action, window, cx);
        }
        if self.palette_view.read(cx).take_dismissed_flag() {
            self.state.palette_open = false;
        }
        if self.utility_view.read(cx).is_dismissed() {
            self.state.active_utility = None;
        }
        let (pending_pal, pending_back) = self.modules.poll_settings_signals(cx);
        if let Some(pid) = pending_pal {
            self.set_palette(pid, cx);
        }
        if pending_back {
            self.state.leave_settings();
            self.persist_local(cx);
        }
        if let Some(handoff) = self.modules.poll_requests_handoff(cx) {
            match handoff {
                RequestsHandoff::OpenSettings(sec) => self.open_settings_section(sec, cx),
                RequestsHandoff::OpenEnvironments => self.navigate(NavModule::Environments, cx),
                RequestsHandoff::OpenHistory => self.navigate(NavModule::History, cx),
                RequestsHandoff::OpenUsageCall(_) => self.navigate(NavModule::Usage, cx),
                RequestsHandoff::AskAgent {
                    title,
                    context_markdown,
                } => {
                    self.navigate(NavModule::Chat, cx);
                    self.modules
                        .handoff_to_chat(title, context_markdown, window, cx);
                }
            }
        }

        let p = active_palette(cx);
        sync_window_scale(&self.state.settings.appearance, window);
        let main_view = self.render_main_content(window, cx);

        let mut root = div()
            .id("pingdeck-app-root")
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(to_hsla(p.canvas))
            .text_color(to_hsla(p.ink))
            .track_focus(&self.focus_handle)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                let list: Vec<std::path::PathBuf> = paths.paths().to_vec();
                if !list.is_empty() {
                    runtime(cx).grants().grant("main", &list);
                    this.state.push_toast(
                        ToastKind::Info,
                        if this.language() == Language::ZhCn {
                            format!("已授权 {}个拖入文件（60秒内有效）", list.len())
                        } else {
                            format!("Granted {} dropped file(s) for 60s", list.len())
                        },
                        None,
                    );
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| {
                this.toggle_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.navigate(NavModule::Settings, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseOverlay, _, cx| {
                if this.state.active_module != NavModule::Requests
                    || !this.modules.requests_dismiss_modal(cx)
                {
                    this.close_overlays(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SaveActive, w, cx| {
                if this.state.active_module == NavModule::Requests {
                    this.modules.requests_save(w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SendRequest, w, cx| {
                if this.state.active_module == NavModule::Requests {
                    this.modules.requests_send(w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleConsole, _, cx| {
                if this.state.active_module == NavModule::Requests {
                    this.modules.requests_toggle_console(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NewTab, w, cx| {
                if this.state.active_module == NavModule::Requests {
                    this.modules.requests_new_tab(w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, w, cx| {
                if this.state.active_module == NavModule::Requests {
                    this.modules.requests_close_tab(w, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &HideWindow, _, cx| cx.hide()))
            .on_action(cx.listener(|_, _: &QuitApp, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &Nav1, _, cx| this.navigate(NavModule::Requests, cx)))
            .on_action(cx.listener(|this, _: &Nav2, _, cx| this.navigate(NavModule::Mock, cx)))
            .on_action(cx.listener(|this, _: &Nav3, _, cx| this.navigate(NavModule::Flows, cx)))
            .on_action(cx.listener(|this, _: &Nav4, _, cx| this.navigate(NavModule::History, cx)))
            .on_action(
                cx.listener(|this, _: &Nav5, _, cx| this.navigate(NavModule::Environments, cx)),
            )
            .on_action(cx.listener(|this, _: &Nav6, _, cx| this.navigate(NavModule::Chat, cx)))
            .on_action(cx.listener(|this, _: &Nav7, _, cx| this.navigate(NavModule::Providers, cx)))
            .on_action(cx.listener(|this, _: &Nav8, _, cx| this.navigate(NavModule::Usage, cx)))
            .on_action(cx.listener(|this, _: &Nav9, _, cx| this.navigate(NavModule::Settings, cx)))
            .child(render_sidebar(self, cx));

        let mut right_col = v_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .pr(px(8.0))
            .pb(px(8.0))
            .child(render_top_drag_edge());

        if let Some(banner) = render_startup_banner(self, cx) {
            right_col = right_col.child(banner);
        }

        right_col = right_col.child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .rounded(px(14.0))
                .overflow_hidden()
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.page_edge.composite_over(p.canvas)))
                .shadow_sm()
                .child(main_view),
        );

        root = root.child(right_col);

        if let Some(overlay) = render_palette_overlay(self, cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = render_utility_overlay(self, cx) {
            root = root.child(overlay);
        }
        if let Some(toasts) = render_toast_stack(self, cx) {
            root = root.child(toasts);
        }
        root
    }
}
