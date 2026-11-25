//! Active settings section page dispatcher for [`SettingsView`].

use gpui_kit::{AnyElement, AppContext as _, Context, IntoElement as _, Window};
use pingdeck_desktop::state::SettingsSection;

use super::{
    SettingsView, about, agent, appearance, catalog, page_advanced, page_data, page_general,
    page_mock, page_network, page_requests, page_routing, page_shortcuts, page_usage,
};

pub(super) fn render_page_body(
    view: &mut SettingsView,
    zh: bool,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    match view.active {
        SettingsSection::About => view
            .about_view
            .get_or_insert_with(|| cx.new(about::AboutSettingsView::new))
            .clone()
            .into_any_element(),
        SettingsSection::Agent
        | SettingsSection::Personalization
        | SettingsSection::Memory
        | SettingsSection::Subagents
        | SettingsSection::Tools
        | SettingsSection::Permissions
        | SettingsSection::Mcp
        | SettingsSection::Skills => {
            let sec = view.active;
            let ent = if let Some(existing) = &view.agent_view {
                existing.clone()
            } else {
                let created = cx.new(|cx| agent::AgentSettingsView::new(sec, window, cx));
                let sub = cx.observe(&created, |this, ent, cx| {
                    this.settings = ent.read(cx).settings.clone();
                    cx.notify();
                });
                view._subscriptions.push(sub);
                view.agent_view = Some(created.clone());
                created
            };
            if ent.read(cx).active != sec {
                ent.update(cx, |v, cx| v.set_section(sec, cx));
            }
            ent.into_any_element()
        }
        SettingsSection::Catalog => view
            .catalog_view
            .get_or_insert_with(|| cx.new(|cx| catalog::CatalogSettingsView::new(window, cx)))
            .clone()
            .into_any_element(),
        SettingsSection::General => page_general::render_general(view, zh, cx).into_any_element(),
        SettingsSection::Appearance => {
            appearance::render_appearance(view, zh, cx).into_any_element()
        }
        SettingsSection::Shortcuts => {
            page_shortcuts::render_shortcuts(view, zh, cx).into_any_element()
        }
        SettingsSection::Requests => {
            page_requests::render_requests(view, zh, cx).into_any_element()
        }
        SettingsSection::Network => page_network::render_network(view, zh, cx).into_any_element(),
        SettingsSection::Mock => page_mock::render_mock(view, zh, cx).into_any_element(),
        SettingsSection::Data => page_data::render_data(view, zh, cx).into_any_element(),
        SettingsSection::Routing => page_routing::render_routing(view, zh, cx).into_any_element(),
        SettingsSection::Usage => page_usage::render_usage(view, zh, cx).into_any_element(),
        SettingsSection::Advanced => {
            page_advanced::render_advanced(view, zh, cx).into_any_element()
        }
    }
}
