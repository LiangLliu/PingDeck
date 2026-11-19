//! Lazy module host mounting and keeping all 9 PingDeck feature views alive across navigation.
//!
//! Switching modules never drops mounted module entities or cancels active streams, matching the
//! run/view separation contract.

use gpui_kit::{AnyElement, AppContext as _, Context, Entity, IntoElement as _, Window};
use pingdeck_desktop::state::{NavModule, SettingsSection};
use pingdeck_desktop::theme::PaletteId;

use crate::views::{
    chat::ChatView, environments::EnvironmentsView, flows::FlowsView, history::HistoryView,
    mock::MockView, providers::ProvidersView, requests::RequestsView, settings::SettingsView,
    usage::UsageView,
};

/// Owns lazily-mounted GPUI entities for all 9 navigation destinations.
#[derive(Default)]
pub struct ModuleHost {
    requests: Option<Entity<RequestsView>>,
    mock: Option<Entity<MockView>>,
    flows: Option<Entity<FlowsView>>,
    history: Option<Entity<HistoryView>>,
    environments: Option<Entity<EnvironmentsView>>,
    chat: Option<Entity<ChatView>>,
    providers: Option<Entity<ProvidersView>>,
    usage: Option<Entity<UsageView>>,
    settings: Option<Entity<SettingsView>>,
}

impl ModuleHost {
    /// Synchronizes workspace-scoped modules whenever the active workspace changes.
    pub fn set_workspace<V: 'static>(&mut self, ws_id: Option<String>, cx: &mut Context<V>) {
        if let Some(v) = &self.requests {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.mock {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.flows {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.history {
            v.update(cx, |m, cx| m.set_workspace(ws_id.clone(), cx));
        }
        if let Some(v) = &self.environments {
            v.update(cx, |m, cx| m.set_workspace(ws_id, cx));
        }
    }

    /// Selects a specific section in the Settings view if mounted.
    pub fn set_settings_section<V: 'static>(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<V>,
    ) {
        if let Some(v) = &self.settings {
            v.update(cx, |s, cx| s.set_section(section, cx));
        }
    }

    /// Opens a specific chat session when invoked from notifications / command palette.
    pub fn open_chat_session<V: 'static>(&mut self, session_id: String, cx: &mut Context<V>) {
        if let Some(v) = &self.chat {
            v.update(cx, |c, cx| c.open_session(session_id, cx));
        }
    }

    /// Refreshes lightweight catalogs when navigating back into a mounted module (e.g., picking up
    /// newly added providers/models when returning from Providers to Chat).
    pub fn on_enter_module<V: 'static>(&mut self, module: NavModule, cx: &mut Context<V>) {
        if module == NavModule::Chat
            && let Some(v) = &self.chat
        {
            v.update(cx, |c, cx| c.refresh_catalog(cx));
        }
    }

    /// Polls settings view for palette-change or back-button signals.
    pub fn poll_settings_signals<V: 'static>(&self, cx: &Context<V>) -> (Option<PaletteId>, bool) {
        let Some(v) = &self.settings else {
            return (None, false);
        };
        let s = v.read(cx);
        (s.take_pending_palette(), s.take_pending_back())
    }

    /// Lazily mounts (if first visit) and returns the active module element.
    pub fn render_active<V: 'static>(
        &mut self,
        module: NavModule,
        workspace_id: Option<String>,
        settings_section: SettingsSection,
        palette: PaletteId,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        match module {
            NavModule::Requests => self
                .requests
                .get_or_insert_with(|| cx.new(|cx| RequestsView::new(workspace_id, window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Mock => self
                .mock
                .get_or_insert_with(|| cx.new(|cx| MockView::new(workspace_id, window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Flows => self
                .flows
                .get_or_insert_with(|| cx.new(|cx| FlowsView::new(workspace_id, window, cx)))
                .clone()
                .into_any_element(),
            NavModule::History => self
                .history
                .get_or_insert_with(|| cx.new(|cx| HistoryView::new(workspace_id, window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Environments => self
                .environments
                .get_or_insert_with(|| cx.new(|cx| EnvironmentsView::new(workspace_id, window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Chat => self
                .chat
                .get_or_insert_with(|| cx.new(|cx| ChatView::new(window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Providers => self
                .providers
                .get_or_insert_with(|| cx.new(|cx| ProvidersView::new(window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Usage => self
                .usage
                .get_or_insert_with(|| cx.new(|cx| UsageView::new(window, cx)))
                .clone()
                .into_any_element(),
            NavModule::Settings => self
                .settings
                .get_or_insert_with(|| {
                    cx.new(|cx| SettingsView::new(settings_section, palette, window, cx))
                })
                .clone()
                .into_any_element(),
        }
    }
}
