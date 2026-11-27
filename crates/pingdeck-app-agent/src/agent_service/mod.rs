//! Agent harness use-cases: chat sessions, the tool-calling run lifecycle, approvals, the queue,
//! MCP servers, skills, tools.
//!
//! Single responsibility: connect the pure pieces in `crate::agent` (runner, registry, MCP client,
//! search, skills) to the store, the LLM client and the settings. A shell calls only the `*_impl`
//! functions re-exported from `app`.

mod approval;
mod attachments;
mod board;
mod branch_state;
mod catalog;
mod change_diff;
mod changes;
mod config;
mod context_breakdown;
mod context_payload;
mod host;
mod model_info;
mod plan;
mod policies;
mod processes;
mod queue;
pub(crate) mod request_changes;
mod resolve;
mod roles;
mod run;
mod sessions;
mod skill_delivery;
mod subagent;
mod task_recovery;
#[cfg(test)]
mod tests;
mod title;
mod tool_output;
mod tools;
mod topology;
mod trust;
mod untracked;
mod work;
mod workspace_info;

pub use approval::{agent_approval_answer_impl, agent_question_answer_impl};
pub(crate) use attachments::store_for;
pub use attachments::{
    PickedFile, PickedKind, agent_attachment_describe_path_impl, agent_attachment_gc_impl,
    agent_attachment_get_bytes_impl, agent_attachment_get_impl, agent_attachment_put_bytes_impl,
    agent_attachment_put_path_impl, agent_attachment_read_image_impl,
    agent_attachment_set_thumb_impl, agent_blob_put_impl, agent_blob_read_impl,
    ensure_extracted_text,
};
pub use board::{agent_board_impl, agent_session_seen_impl};
pub use branch_state::{agent_changes_off_branch_impl, on_head_moved};
pub(crate) use catalog::tool_entries;
pub use catalog::{
    AgentSkillEntry, AgentToolEntry, PermissionRuleView, RuleInactive, SandboxTestDto,
    agent_permission_rule_views_impl, agent_sandbox_test_impl, agent_skill_entries_impl,
    agent_tool_entries_impl,
};
pub use change_diff::{ChangeDiff, agent_change_diff_impl, agent_change_net_diff_impl};
pub use changes::{agent_change_keep_impl, agent_change_undo_impl, agent_changes_list_impl};
pub use config::{
    agent_mcp_last_seq, agent_mcp_server_delete_impl, agent_mcp_server_list_impl,
    agent_mcp_server_log_impl, agent_mcp_server_restart_impl, agent_mcp_server_save_impl,
    agent_mcp_server_test_impl, agent_mcp_status_list_impl, agent_mcp_subscribe_impl,
    agent_mcp_tool_review_impl, agent_mcp_unsubscribe_impl, agent_skill_import_impl,
    agent_skill_list_impl, agent_skill_remove_impl, agent_skill_set_enabled_impl,
    agent_tool_list_impl,
};
pub use context_breakdown::{ContextBreakdown, agent_context_breakdown_impl};
pub use context_payload::agent_context_preview_impl;
pub use plan::{
    PLAN_APPROVED_TEXT, agent_plan_approve_impl, agent_plan_get_impl, agent_plan_set_impl,
};
pub use policies::{
    BlockedRule, SandboxStatusDto, agent_blocked_rules_impl, agent_permission_rule_delete_impl,
    agent_permission_rule_list_impl, agent_recent_blocks_impl, agent_sandbox_acknowledge_impl,
    agent_sandbox_status_impl, agent_tool_policy_list_impl, agent_tool_policy_set_impl,
};
pub use processes::{agent_process_kill_impl, agent_process_list_impl};
pub use queue::{
    agent_queue_add_impl, agent_queue_clear_impl, agent_queue_list_impl, agent_queue_remove_impl,
    agent_queue_steer_impl, agent_queue_update_impl,
};
pub use roles::{AgentRoleStatus, RoleWarning, agent_roles_get_impl};
pub use run::{
    agent_attention_subscribe_impl, agent_attention_unsubscribe_impl, agent_context_compact_impl,
    agent_message_edit_impl, agent_message_retry_impl, agent_queue_resume_impl,
    agent_run_continue_impl, agent_run_list_impl, agent_run_start_impl, agent_run_steer_impl,
    agent_run_stop_all_impl, agent_run_stop_impl, agent_run_subscribe_impl,
    agent_run_unsubscribe_impl, agent_turn_changes_impl, queue_changed,
};
pub use sessions::{
    AgentPageRequest, agent_message_prepare_edit_impl, agent_message_prepare_retry_impl,
    agent_session_bind_pingdeck_impl, agent_session_branch_impl, agent_session_create_impl,
    agent_session_delete_impl, agent_session_delete_message_impl,
    agent_session_delete_message_preview_impl, agent_session_dismiss_notice_impl,
    agent_session_export_impl, agent_session_export_preview_impl,
    agent_session_export_redacted_impl, agent_session_find_impl, agent_session_get_impl,
    agent_session_list_impl, agent_session_restore_impl, agent_session_search_impl,
    agent_session_set_head_impl, agent_session_update_impl,
};
pub(crate) use subagent::run_subagent;
pub use task_recovery::agent_task_node_recover_impl;
pub use title::agent_session_generate_title_impl;
pub(crate) use tool_output::agent_tool_output_read_all_impl;
pub use tool_output::{agent_tool_output_delete_impl, agent_tool_output_read_impl};
pub use topology::{
    agent_all_runs_history_impl, agent_run_topology_impl, agent_session_latest_topology_impl,
    agent_session_run_history_impl, agent_session_runs_impl,
};
pub use trust::{
    agent_folder_trust_get_impl, agent_folder_trust_list_impl, agent_folder_trust_revoke_impl,
    agent_folder_trust_set_impl,
};
pub use work::{
    RunningWork, agent_reap_gone_sinks_impl, agent_runtime_start_impl, agent_work_stop_impl,
    app_running_work_impl,
};
pub use workspace_info::{
    WorkspaceInfo, WorkspaceSkillDir, agent_workspace_info_impl, workspace_search_files_impl,
};
