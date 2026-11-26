//! Active settings section page dispatcher for [`SettingsView`].

use gpui_kit::{AnyElement, AppContext as _, Context, IntoElement as _, Window};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::SettingsSection;

use super::{
    SettingsView, about, agent, appearance, catalog, page_advanced, page_data, page_general,
    page_mock, page_network, page_requests, page_routing, page_shortcuts, page_usage,
};

pub(super) fn page_subtitle(section: SettingsSection, lang: Language) -> &'static str {
    match section {
        SettingsSection::General => crate::ui::Tr(lang)
            .settings()
            .dispatch_manage_interface_language_startup_session(),
        SettingsSection::Appearance => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_light_dark_mode_8(),
        SettingsSection::Shortcuts => crate::ui::Tr(lang)
            .settings()
            .dispatch_search_and_rebind_desktop_keyboard(),
        SettingsSection::Requests => crate::ui::Tr(lang)
            .settings()
            .dispatch_set_default_http_transport_limits(),
        SettingsSection::Network => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_global_outbound_proxy_bypass(),
        SettingsSection::Mock => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_default_port_allocation_latency(),
        SettingsSection::Data => crate::ui::Tr(lang)
            .settings()
            .dispatch_inspect_local_sqlite_storage_clear(),
        SettingsSection::Agent => crate::ui::Tr(lang)
            .settings()
            .dispatch_set_default_chat_model_reasoning(),
        SettingsSection::Personalization => crate::ui::Tr(lang)
            .settings()
            .dispatch_provide_global_personal_preferences_append(),
        SettingsSection::Memory => crate::ui::Tr(lang)
            .settings()
            .dispatch_manage_folders_trusted_to_load(),
        SettingsSection::Subagents => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_sub_agent_concurrency_limits(),
        SettingsSection::Tools => crate::ui::Tr(lang)
            .settings()
            .dispatch_adjust_default_approval_policies_for(),
        SettingsSection::Permissions => crate::ui::Tr(lang)
            .settings()
            .dispatch_review_the_6_layer_permission(),
        SettingsSection::Mcp => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_external_mcp_tool_servers(),
        SettingsSection::Skills => crate::ui::Tr(lang)
            .settings()
            .dispatch_scan_import_and_toggle_local(),
        SettingsSection::Routing => crate::ui::Tr(lang)
            .settings()
            .dispatch_configure_shared_retry_counts_retry(),
        SettingsSection::Catalog => crate::ui::Tr(lang)
            .settings()
            .dispatch_sync_model_specs_and_pricing(),
        SettingsSection::Usage => crate::ui::Tr(lang)
            .settings()
            .dispatch_control_per_call_usage_retention(),
        SettingsSection::Advanced => crate::ui::Tr(lang)
            .settings()
            .dispatch_tune_script_sandbox_budgets_console(),
        SettingsSection::About => crate::ui::Tr(lang)
            .settings()
            .dispatch_view_application_version_runtime_environme(),
    }
}

pub(super) fn render_page_body(
    view: &mut SettingsView,
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
                let sub = cx.observe(&created, |this, _ent, cx| {
                    let latest = pingdeck_desktop::core::app::settings_get_impl(
                        crate::bridge::runtime(cx).state(),
                    );
                    if latest != this.settings {
                        this.apply_updated_settings(latest, cx);
                    }
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
        SettingsSection::General => page_general::render_general(view, cx).into_any_element(),
        SettingsSection::Appearance => appearance::render_appearance(view, cx).into_any_element(),
        SettingsSection::Shortcuts => page_shortcuts::render_shortcuts(view, cx).into_any_element(),
        SettingsSection::Requests => page_requests::render_requests(view, cx).into_any_element(),
        SettingsSection::Network => page_network::render_network(view, cx).into_any_element(),
        SettingsSection::Mock => page_mock::render_mock(view, cx).into_any_element(),
        SettingsSection::Data => page_data::render_data(view, cx).into_any_element(),
        SettingsSection::Routing => page_routing::render_routing(view, cx).into_any_element(),
        SettingsSection::Usage => page_usage::render_usage(view, cx).into_any_element(),
        SettingsSection::Advanced => page_advanced::render_advanced(view, cx).into_any_element(),
    }
}
