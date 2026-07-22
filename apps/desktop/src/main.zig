//! Native SDK product shell for Hyper Term.
//!
//! Zig owns presentation state only. PTYs, process lifecycle, transcripts,
//! permissions, files, and agent runtimes remain behind the Rust `hyperd`
//! boundary. A child system WebView may render terminal cells, but its bytes
//! travel directly over hyperd's authenticated terminal plane; this Zig host
//! has no JavaScript bridge and never spawns a shell.

const std = @import("std");
const builtin = @import("builtin");
const runner = @import("runner");
const native_sdk = @import("native_sdk");
const agent_capabilities = @import("agent_capabilities.zig");
const agent_block_view = @import("agent_block_view.zig");
const agent_provider = @import("agent_provider.zig");
const agent_start_policy = @import("agent_start_policy.zig");
const agent_wire = @import("agent_wire.zig");
const desktop_model = @import("desktop_model.zig");
const desktop_view = @import("desktop_view.zig");
const AgentAttentionResponseWire = agent_wire.AttentionResponse;
const AgentBlockWire = agent_wire.Block;
const AgentCapabilitiesResponseWire = agent_wire.CapabilitiesResponse;
const AgentCapabilitiesWire = agent_wire.Capabilities;
const AgentExecutionContextEventWire = agent_wire.ExecutionContextEvent;
const AgentGoalWire = agent_wire.Goal;
const AgentPatchWire = agent_wire.Patch;
const AgentPlanEntryWire = agent_wire.PlanEntry;
const AgentProviderStatusWire = agent_wire.ProviderStatus;
const AgentSnapshotWire = agent_wire.Snapshot;
const AgentStreamFrameWire = agent_wire.StreamFrame;
const AgentTier2PreviewWire = agent_wire.Tier2Preview;
const AgentTier2ResultWire = agent_wire.Tier2Result;
const AgentTier2ResultsWire = agent_wire.Tier2Results;
const AgentToolCallWire = agent_wire.ToolCall;

pub const panic = std.debug.FullPanic(native_sdk.debug.capturePanic);

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;

pub const canvas_label = "hyper-term-canvas";
pub const terminal_view_label = "hyper-term-terminal-view";
pub const terminal_view_anchor = "Terminal viewport";
pub const genui_view_label = "hyper-term-genui-view";
pub const genui_view_anchor = "Agent artifact editor viewport";
pub const terminal_gateway_origin = "http://127.0.0.1:47437";
pub const max_sessions = desktop_model.max_sessions;
const max_inline_session_tabs = desktop_model.max_inline_session_tabs;
const max_session_tab_title_bytes = desktop_model.max_session_tab_title_bytes;
const terminal_url_capacity = desktop_model.terminal_url_capacity;
const agent_url_capacity = desktop_model.agent_url_capacity;
const genui_url_capacity = desktop_model.genui_url_capacity;
const max_gateway_token_bytes: usize = 128;
const max_agent_provider_status_bytes: usize = 4 * 1024;
const max_agent_attention_status_bytes: usize = 4 * 1024;
const terminal_close_url_capacity: usize = terminal_url_capacity + 64;
const agent_effect_url_capacity: usize = agent_url_capacity + 64;
pub const max_agent_blocks = desktop_model.max_agent_blocks;
const max_agent_search_bytes = desktop_model.max_agent_search_bytes;
const max_agent_block_bytes = agent_block_view.max_block_bytes;
const max_agent_operation_id_bytes = desktop_model.max_agent_operation_id_bytes;
const max_agent_operation_kind_bytes = agent_block_view.max_operation_kind_bytes;
const max_agent_activity_title_bytes = agent_block_view.max_activity_title_bytes;
const max_agent_activity_meta_bytes = agent_block_view.max_activity_meta_bytes;
const max_agent_diff_files = agent_block_view.max_diff_files;
const max_agent_diff_path_bytes = agent_block_view.max_diff_path_bytes;
const max_agent_goal_step_columns = desktop_model.max_agent_goal_step_columns;
const max_agent_goal_objective_bytes = desktop_model.max_agent_goal_objective_bytes;
const max_agent_goal_meta_bytes = desktop_model.max_agent_goal_meta_bytes;
const max_agent_error_bytes = desktop_model.max_agent_error_bytes;
const max_agent_prompt_bytes = desktop_model.max_agent_prompt_bytes;
const max_agent_config_options = desktop_model.max_agent_config_options;
const max_agent_config_choices = desktop_model.max_agent_config_choices;
const max_agent_commands = desktop_model.max_agent_commands;
const max_agent_tier2_results = desktop_model.max_agent_tier2_results;
const max_agent_tier2_files = desktop_model.max_agent_tier2_files;
const max_agent_tier2_path_bytes = desktop_model.max_agent_tier2_path_bytes;
const max_agent_tier2_diff_bytes = desktop_model.max_agent_tier2_diff_bytes;
const max_agent_capability_id_bytes = desktop_model.max_agent_capability_id_bytes;
const max_agent_capability_label_bytes = desktop_model.max_agent_capability_label_bytes;
const max_agent_execution_contexts = desktop_model.max_agent_execution_contexts;
const max_agent_context_id_bytes = desktop_model.max_agent_context_id_bytes;
const agent_context_digest_bytes = desktop_model.agent_context_digest_bytes;
const max_agent_context_summary_bytes = desktop_model.max_agent_context_summary_bytes;
const max_desktop_workspace_bytes: usize = 4 * 1024;
const max_terminal_metadata_bytes: usize = 8 * 1024;
const max_terminal_title_bytes = desktop_model.max_terminal_title_bytes;
const max_terminal_cwd_bytes = desktop_model.max_terminal_cwd_bytes;
const ui_font_id: canvas.FontId = canvas.min_registered_font_id;
const max_ui_font_bytes: usize = 24 * 1024 * 1024;
const default_macos_ui_font_path = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf";
const terminal_close_effect_key_base: u64 = 0x4854_4300;
pub const agent_start_effect_key_base: u64 = 0x4854_4100;
const agent_close_effect_key_base: u64 = 0x4854_4200;
pub const agent_turn_effect_key_base: u64 = 0x4854_4400;
pub const agent_cancel_effect_key_base: u64 = 0x4854_4f00;
pub const agent_snapshot_effect_key_base: u64 = 0x4854_4500;
pub const agent_poll_timer_key_base: u64 = 0x4854_4600;
pub const agent_permission_effect_key_base: u64 = 0x4854_4700;
pub const agent_config_effect_key_base: u64 = 0x4854_4800;
pub const agent_stream_effect_key_base: u64 = 0x4854_4900;
pub const agent_tier2_results_effect_key_base: u64 = 0x4854_4a00;
pub const agent_tier2_preview_effect_key_base: u64 = 0x4854_4b00;
pub const agent_tier2_review_effect_key_base: u64 = 0x4854_4c00;
pub const agent_tier2_discard_effect_key_base: u64 = 0x4854_4d00;
pub const agent_provider_refresh_effect_key: u64 = 0x4854_4e00;
pub const agent_goal_effect_key_base: u64 = 0x4854_5000;
pub const deferred_webview_timer_id: u64 = 0x4854_5100;
pub const agent_attention_effect_key: u64 = 0x4854_5200;
pub const agent_attention_poll_timer_key: u64 = 0x4854_5300;
pub const desktop_workspace_effect_key: u64 = 0x4854_5400;
pub const terminal_metadata_effect_key: u64 = 0x4854_5500;
pub const terminal_metadata_poll_timer_key: u64 = 0x4854_5600;
pub const provider_login_clipboard_effect_key: u64 = 0x4854_5700;
const deferred_webview_delay_ns: u64 = 1_000_000;
pub const window_width: f32 = 1180;
pub const window_height: f32 = 760;
pub const window_min_width: f32 = 840;
pub const window_min_height: f32 = 520;
pub const titlebar_natural_height = desktop_model.titlebar_natural_height;

const app_permissions = [_][]const u8{
    native_sdk.security.permission_command,
    native_sdk.security.permission_clipboard,
    native_sdk.security.permission_network,
    native_sdk.security.permission_notifications,
    native_sdk.security.permission_view,
};
const shell_views = [_]native_sdk.ShellView{
    .{
        .label = canvas_label,
        .kind = .gpu_surface,
        .fill = true,
        .role = "Hyper Term canvas",
        .accessibility_label = "Hyper Term",
        .gpu_backend = .metal,
        .gpu_pixel_format = .bgra8_unorm,
        .gpu_present_mode = .timer,
        .gpu_alpha_mode = .@"opaque",
        .gpu_color_space = .srgb,
        .gpu_vsync = true,
    },
};
const shell_windows = [_]native_sdk.ShellWindow{.{
    .label = "main",
    .title = "Hyper Term",
    .width = window_width,
    .height = window_height,
    .min_width = window_min_width,
    .min_height = window_min_height,
    .restore_state = true,
    .titlebar = .hidden_inset,
    .views = &shell_views,
}};
pub const shell_scene: native_sdk.ShellConfig = .{ .windows = &shell_windows };

pub const SessionMode = desktop_model.SessionMode;
pub const AgentMessageRole = desktop_model.AgentMessageRole;
pub const AgentBlockKind = desktop_model.AgentBlockKind;
pub const AgentToolStatus = desktop_model.AgentToolStatus;
pub const AgentRisk = desktop_model.AgentRisk;
pub const AgentOperationState = desktop_model.AgentOperationState;
pub const AgentDecision = desktop_model.AgentDecision;
pub const AgentDiffFileView = desktop_model.AgentDiffFileView;
pub const AgentBlockView = desktop_model.AgentBlockView;
pub const AgentProvider = desktop_model.AgentProvider;
pub const AgentProviderReadiness = desktop_model.AgentProviderReadiness;
pub const AgentConnection = desktop_model.AgentConnection;
pub const AgentTurnStatus = desktop_model.AgentTurnStatus;
pub const AgentAttention = desktop_model.AgentAttention;
pub const AgentGoalStatus = desktop_model.AgentGoalStatus;
pub const AgentGoalAction = desktop_model.AgentGoalAction;
pub const AgentGoalView = desktop_model.AgentGoalView;
pub const AgentTier2FileView = desktop_model.AgentTier2FileView;
pub const AgentTier2ResultView = desktop_model.AgentTier2ResultView;
pub const AgentExecutionMode = desktop_model.AgentExecutionMode;
pub const AgentExecutionContextView = desktop_model.AgentExecutionContextView;
pub const AgentConfigChoiceView = desktop_model.AgentConfigChoiceView;
pub const AgentConfigOptionView = desktop_model.AgentConfigOptionView;
pub const AgentCommandView = desktop_model.AgentCommandView;
pub const Session = desktop_model.Session;
pub const Model = desktop_model.Model;
pub const agentAttention = desktop_model.agentAttention;

const TerminalSessionMetadataWire = desktop_model.TerminalSessionMetadataWire;
const TerminalMetadataResponseWire = desktop_model.TerminalMetadataResponseWire;
const DesktopWorkspaceWire = desktop_model.DesktopWorkspaceWire;
const DesktopSessionWire = desktop_model.DesktopSessionWire;
const PendingAgentPrompt = desktop_model.PendingAgentPrompt;

pub const Msg = union(enum) {
    choose_terminal,
    choose_agent,
    toggle_session_picker,
    dismiss_session_picker,
    toggle_agent_provider_picker,
    dismiss_agent_provider_picker,
    refresh_agent_providers,
    agent_providers_refreshed: native_sdk.EffectResponse,
    open_codex_login_terminal,
    open_claude_login_terminal,
    copy_provider_login_command,
    dismiss_provider_login_hint,
    provider_login_command_copied: native_sdk.EffectClipboardResult,
    agent_attention_received: native_sdk.EffectResponse,
    agent_attention_poll: native_sdk.EffectTimer,
    terminal_metadata_received: native_sdk.EffectResponse,
    terminal_metadata_poll: native_sdk.EffectTimer,
    choose_codex_agent,
    choose_codex_acp_agent,
    choose_claude_acp_agent,
    choose_copilot_acp_agent,
    select_session: u8,
    close_session: u8,
    close_active_session,
    terminal_session_closed: native_sdk.EffectResponse,
    desktop_workspace_persisted: native_sdk.EffectResponse,
    agent_session_started: native_sdk.EffectResponse,
    agent_session_closed: native_sdk.EffectResponse,
    agent_composer_changed: canvas.TextInputEvent,
    open_agent_search,
    close_agent_search,
    agent_search_changed: canvas.TextInputEvent,
    send_agent_prompt,
    agent_turn_started: native_sdk.EffectResponse,
    cancel_agent_turn,
    agent_turn_cancelled: native_sdk.EffectResponse,
    agent_snapshot_received: native_sdk.EffectResponse,
    agent_stream_line: native_sdk.EffectLine,
    agent_stream_closed: native_sdk.EffectResponse,
    toggle_agent_config_picker: u8,
    dismiss_agent_config_picker,
    toggle_agent_command_picker,
    dismiss_agent_command_picker,
    choose_agent_config: u16,
    agent_config_updated: native_sdk.EffectResponse,
    insert_agent_command: u8,
    toggle_agent_block: u64,
    toggle_agent_goal,
    toggle_agent_goal_menu,
    dismiss_agent_goal_menu,
    edit_agent_goal,
    apply_agent_goal_action: AgentGoalAction,
    agent_goal_updated: native_sdk.EffectResponse,
    toggle_agent_execution_context,
    reject_agent_effect: []const u8,
    allow_agent_effect: []const u8,
    cancel_agent_effect: []const u8,
    agent_permission_decided: native_sdk.EffectResponse,
    agent_poll: native_sdk.EffectTimer,
    agent_tier2_results_received: native_sdk.EffectResponse,
    preview_agent_tier2_result: []const u8,
    agent_tier2_preview_received: native_sdk.EffectResponse,
    request_agent_tier2_review: []const u8,
    agent_tier2_review_requested: native_sdk.EffectResponse,
    discard_agent_tier2_result: []const u8,
    agent_tier2_result_discarded: native_sdk.EffectResponse,
    open_agent_editor,
    close_agent_editor,
    agent_split_resized: f32,
    system_appearance: struct {
        scheme: canvas.ColorScheme,
        high_contrast: bool,
        reduce_motion: bool,
    },
    chrome_changed: native_sdk.WindowChrome,

    /// Platform callbacks dispatch these messages; markup never does.
    pub const view_unbound = .{ "toggle_session_picker", "dismiss_session_picker", "select_session", "close_session", "close_active_session", "open_agent_search", "terminal_session_closed", "terminal_metadata_received", "terminal_metadata_poll", "desktop_workspace_persisted", "agent_providers_refreshed", "provider_login_command_copied", "agent_attention_received", "agent_attention_poll", "agent_session_started", "agent_session_closed", "agent_turn_started", "agent_turn_cancelled", "agent_snapshot_received", "agent_stream_line", "agent_stream_closed", "agent_config_updated", "agent_permission_decided", "agent_poll", "agent_tier2_results_received", "preview_agent_tier2_result", "agent_tier2_preview_received", "request_agent_tier2_review", "agent_tier2_review_requested", "discard_agent_tier2_result", "agent_tier2_result_discarded", "toggle_agent_goal", "toggle_agent_goal_menu", "dismiss_agent_goal_menu", "edit_agent_goal", "apply_agent_goal_action", "agent_goal_updated", "system_appearance", "chrome_changed" };
};

// Debug watches compiled markup as a fragment instead of installing it as the
// runtime root. That keeps `rootView` authoritative for Zig-owned Agent Block
// composition while preserving edit-save-refresh authoring for app.native.
const dev_markup_reload = builtin.mode == .Debug;
pub const HyperTermApp = native_sdk.UiAppWithFeatures(Model, Msg, .{ .runtime_markup = dev_markup_reload });
pub const Effects = HyperTermApp.Effects;

pub fn update(model: *Model, msg: Msg, fx: *Effects) void {
    switch (msg) {
        .choose_terminal => {
            if (appendSession(model, .terminal) != null) markDesktopWorkspaceChanged(model, fx);
        },
        .choose_agent => createAgentSession(model, model.selected_agent_provider, fx),
        .toggle_session_picker => {
            model.session_picker_open = !model.session_picker_open;
            if (model.session_picker_open) model.agent_provider_picker_open = false;
        },
        .dismiss_session_picker => model.session_picker_open = false,
        .toggle_agent_provider_picker => toggleAgentProviderPicker(model, fx),
        .dismiss_agent_provider_picker => model.agent_provider_picker_open = false,
        .refresh_agent_providers => requestAgentProviderRefresh(model, fx),
        .agent_providers_refreshed => |response| applyAgentProviderRefresh(model, response),
        .open_codex_login_terminal => openProviderLoginTerminal(model, .codex, fx),
        .open_claude_login_terminal => openProviderLoginTerminal(model, .claude_acp, fx),
        .copy_provider_login_command => copyProviderLoginCommand(model, fx),
        .dismiss_provider_login_hint => model.provider_login.clear(),
        .provider_login_command_copied => |result| applyProviderLoginClipboardResult(model, result),
        .agent_attention_received => |response| applyAgentAttentionResponse(model, response, fx),
        .agent_attention_poll => |timer| {
            if (timer.outcome == .fired) requestAgentAttention(model, fx);
        },
        .terminal_metadata_received => |response| applyTerminalMetadataResponse(model, response, fx),
        .terminal_metadata_poll => |timer| {
            if (timer.outcome == .fired) requestTerminalMetadata(model, fx);
        },
        .choose_codex_agent => createAgentSession(model, .codex, fx),
        .choose_codex_acp_agent => createAgentSession(model, .codex_acp, fx),
        .choose_claude_acp_agent => createAgentSession(model, .claude_acp, fx),
        .choose_copilot_acp_agent => createAgentSession(model, .copilot_acp, fx),
        .select_session => |session_id| {
            model.session_picker_open = false;
            const previous = model.active_session_id;
            saveActiveAgentComposer(model);
            selectSession(model, session_id);
            if (previous != model.active_session_id) {
                loadActiveAgentComposer(model);
                markDesktopWorkspaceChanged(model, fx);
                clearAgentSearch(model);
                acknowledgeSessionAttention(model, model.active_session_id);
                cancelAgentStream(model, previous, fx);
                resetAgentProjection(model, model.active_session_id);
                requestActiveAgentStream(model, fx);
                if (model.activeSession().mode != .agent and hasAgentSessions(model)) {
                    requestAgentAttention(model, fx);
                }
                requestAgentComposerFocus(model);
            }
        },
        .close_session => |session_id| closeSession(model, session_id, fx),
        .close_active_session => closeSession(model, model.active_session_id, fx),
        .terminal_session_closed => {},
        .desktop_workspace_persisted => |response| applyDesktopWorkspacePersisted(model, response, fx),
        .agent_session_started => |response| applyAgentStartResponse(model, response, fx),
        .agent_session_closed => {},
        .agent_composer_changed => |edit| {
            model.agent_goal_editing = false;
            model.agent_composer_focus_requested = false;
            model.agent_composer_buffer.apply(edit);
        },
        .open_agent_search => {
            if (model.activeSession().mode == .agent) {
                model.agent_search_open = true;
                model.agent_composer_focus_requested = false;
            }
        },
        .close_agent_search => {
            clearAgentSearch(model);
            requestAgentComposerFocus(model);
        },
        .agent_search_changed => |edit| {
            if (model.activeSession().mode == .agent and model.agent_search_open) {
                model.agent_search_buffer.apply(edit);
            }
        },
        .send_agent_prompt => requestAgentTurn(model, fx),
        .agent_turn_started => |response| applyAgentTurnResponse(model, response, fx),
        .cancel_agent_turn => requestAgentCancel(model, fx),
        .agent_turn_cancelled => |response| applyAgentCancelResponse(model, response, fx),
        .agent_snapshot_received => |response| applyAgentSnapshotResponse(model, response, fx),
        .agent_stream_line => |line| applyAgentStreamLine(model, line, fx),
        .agent_stream_closed => |response| applyAgentStreamClosed(model, response, fx),
        .toggle_agent_config_picker => |index| toggleAgentConfigPicker(model, index),
        .dismiss_agent_config_picker => closeAgentConfigPickers(model),
        .toggle_agent_command_picker => model.agent_command_picker_open = !model.agent_command_picker_open,
        .dismiss_agent_command_picker => model.agent_command_picker_open = false,
        .choose_agent_config => |action_id| requestAgentConfig(model, action_id, fx),
        .agent_config_updated => |response| applyAgentConfigResponse(model, response, fx),
        .insert_agent_command => |index| insertAgentCommand(model, index),
        .toggle_agent_block => |block_id| {
            if (model.agent_plan_visible and model.agent_plan.id == block_id) {
                model.agent_plan.expanded = !model.agent_plan.expanded;
            }
            for (model.agent_blocks[0..model.agent_block_count]) |*block| {
                if (block.id == block_id and
                    (block.isActivity() or
                        block.isThoughtMessage() or
                        block.isSystemMessage() or
                        (block.isApproval() and
                            (!block.isApprovalPending() or !model.isLiveAgentApproval(block)))))
                {
                    block.expanded = !block.expanded;
                    break;
                }
            }
        },
        .toggle_agent_goal => model.agent_goal.expanded = !model.agent_goal.expanded,
        .toggle_agent_goal_menu => {
            if (!model.agentGoalActionDisabled()) {
                // Dropping the source focus request here creates a fresh
                // false -> true edge when Edit is selected again.
                model.agent_goal_editing = false;
                model.agent_composer_focus_requested = false;
                model.agent_goal_menu_open = !model.agent_goal_menu_open;
            }
        },
        .dismiss_agent_goal_menu => model.agent_goal_menu_open = false,
        .edit_agent_goal => editAgentGoal(model),
        .apply_agent_goal_action => |action| requestAgentGoalAction(model, action, fx),
        .agent_goal_updated => |response| applyAgentGoalResponse(model, response, fx),
        .toggle_agent_execution_context => {
            if (model.hasAgentExecutionContext()) {
                model.agent_execution_context_expanded = !model.agent_execution_context_expanded;
            }
        },
        .reject_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "reject_once", fx),
        .allow_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "allow_once", fx),
        .cancel_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "cancelled", fx),
        .agent_permission_decided => |response| applyAgentPermissionResponse(model, response, fx),
        .agent_poll => |timer| {
            if (timer.outcome == .fired) requestActiveAgentStream(model, fx);
        },
        .agent_tier2_results_received => |response| applyAgentTier2ResultsResponse(model, response),
        .preview_agent_tier2_result => |operation_id| requestAgentTier2Preview(model, operation_id, fx),
        .agent_tier2_preview_received => |response| applyAgentTier2PreviewResponse(model, response),
        .request_agent_tier2_review => |operation_id| requestAgentTier2Review(model, operation_id, fx),
        .agent_tier2_review_requested => |response| applyAgentTier2ReviewResponse(model, response, fx),
        .discard_agent_tier2_result => |operation_id| requestAgentTier2Discard(model, operation_id, fx),
        .agent_tier2_result_discarded => |response| applyAgentTier2DiscardResponse(model, response, fx),
        .open_agent_editor => {
            if (model.hasEditableAgentArtifact()) {
                model.agent_editor_open_session_id = model.active_session_id;
            }
        },
        .close_agent_editor => {
            if (model.agent_editor_open_session_id == model.active_session_id) {
                model.agent_editor_open_session_id = 0;
                requestAgentComposerFocus(model);
            }
        },
        .agent_split_resized => |fraction| model.agent_split = std.math.clamp(fraction, 0.48, 0.76),
        .system_appearance => |appearance| {
            model.system_scheme = appearance.scheme;
            model.high_contrast = appearance.high_contrast;
            model.reduce_motion = appearance.reduce_motion;
        },
        .chrome_changed => |chrome| {
            model.chrome_leading = chrome.insets.left;
            model.chrome_trailing = chrome.insets.right;
            model.titlebar_height = @max(titlebar_natural_height, chrome.insets.top);
        },
    }
}

fn appendSession(model: *Model, mode: SessionMode) ?u8 {
    if (model.session_count >= max_sessions) return null;
    saveActiveAgentComposer(model);
    const session_id = model.next_session_id;
    model.agent_composer_drafts[model.session_count] = .{};
    model.session_slots[model.session_count] = .{
        .id = session_id,
        .mode = mode,
        .title = if (mode == .terminal) "zsh" else model.selected_agent_provider.label(),
        .icon = if (mode == .terminal) "terminal" else "circle-dot",
        .agent_provider = model.selected_agent_provider,
    };
    model.agent_pending_prompts[model.session_count] = .{};
    model.session_count += 1;
    model.active_session_id = session_id;
    loadActiveAgentComposer(model);
    clearAgentSearch(model);
    model.next_session_id +%= 1;
    if (model.next_session_id == 0) model.next_session_id = 1;
    refreshTerminalUrl(model);
    return session_id;
}

fn createAgentSession(model: *Model, provider: AgentProvider, fx: *Effects) void {
    model.agent_provider_picker_open = false;
    if (model.available_agent_providers != 0 and !model.agentProviderAvailable(provider)) return;
    model.selected_agent_provider = provider;
    if (appendSession(model, .agent)) |session_id| {
        if (model.agent_base_url_len > 0 and model.agentProviderReady(provider)) {
            requestAgentStart(model, session_id, fx);
        }
        markDesktopWorkspaceChanged(model, fx);
    }
}

fn openProviderLoginTerminal(model: *Model, provider: AgentProvider, fx: *Effects) void {
    model.agent_provider_picker_open = false;
    if (provider.loginCommand() == null) return;
    const session_id = appendSession(model, .terminal) orelse return;
    model.provider_login.open(provider, session_id);
    copyProviderLoginCommand(model, fx);
    markDesktopWorkspaceChanged(model, fx);
}

fn copyProviderLoginCommand(model: *Model, fx: *Effects) void {
    const login_command = model.provider_login.command();
    if (login_command.len == 0) return;
    model.provider_login.copy_state = .copying;
    fx.writeClipboard(.{
        .key = provider_login_clipboard_effect_key,
        .text = login_command,
        .on_result = Effects.clipboardMsg(.provider_login_command_copied),
    });
}

fn applyProviderLoginClipboardResult(model: *Model, result: native_sdk.EffectClipboardResult) void {
    if (result.key != provider_login_clipboard_effect_key or result.op != .write) return;
    model.provider_login.copy_state = if (result.outcome == .ok) .copied else .failed;
}

fn closeSession(model: *Model, session_id: u8, fx: *Effects) void {
    model.session_picker_open = false;
    saveActiveAgentComposer(model);
    var closing_index: ?usize = null;
    for (model.openSessions(), 0..) |session, index| {
        if (session.id == session_id) {
            closing_index = index;
            break;
        }
    }
    const index = closing_index orelse return;
    const session = model.session_slots[index];
    if (model.provider_login.session_id == session_id) model.provider_login.clear();
    const was_active = model.active_session_id == session_id;
    if (model.agent_editor_open_session_id == session_id) {
        model.agent_editor_open_session_id = 0;
    }
    requestTerminalClose(model, session_id, fx);
    if (session.mode == .agent) {
        requestAgentClose(model, session_id, fx);
        fx.cancelTimer(agent_poll_timer_key_base + session_id);
        cancelAgentStream(model, session_id, fx);
        fx.cancel(agent_permission_effect_key_base + session_id);
        fx.cancel(agent_cancel_effect_key_base + session_id);
        fx.cancel(agent_config_effect_key_base + session_id);
        fx.cancel(agent_tier2_results_effect_key_base + session_id);
        fx.cancel(agent_tier2_preview_effect_key_base + session_id);
        fx.cancel(agent_tier2_review_effect_key_base + session_id);
        fx.cancel(agent_tier2_discard_effect_key_base + session_id);
        fx.cancel(agent_goal_effect_key_base + session_id);
        if (model.agent_permission_in_flight_session_id == session_id) {
            model.agent_permission_in_flight_session_id = 0;
        }
        if (model.agent_config_in_flight_session_id == session_id) {
            model.agent_config_in_flight_session_id = 0;
        }
        if (model.agent_tier2_results_in_flight_session_id == session_id) {
            model.agent_tier2_results_in_flight_session_id = 0;
        }
        if (model.agent_tier2_action_in_flight_session_id == session_id) {
            model.agent_tier2_action_in_flight_session_id = 0;
        }
        if (model.agent_goal_in_flight_session_id == session_id) {
            model.agent_goal_in_flight_session_id = 0;
        }
    }

    if (model.session_count == 1) {
        fx.closeWindow("main");
        return;
    }

    if (model.active_session_id == session_id) {
        model.active_session_id = if (index + 1 < model.session_count)
            model.session_slots[index + 1].id
        else
            model.session_slots[index - 1].id;
    }

    var cursor = index;
    while (cursor + 1 < model.session_count) : (cursor += 1) {
        model.session_slots[cursor] = model.session_slots[cursor + 1];
        model.agent_composer_drafts[cursor] = model.agent_composer_drafts[cursor + 1];
        model.agent_pending_prompts[cursor] = model.agent_pending_prompts[cursor + 1];
    }
    model.session_count -= 1;
    model.session_slots[model.session_count] = .{};
    model.agent_composer_drafts[model.session_count] = .{};
    model.agent_pending_prompts[model.session_count] = .{};
    loadActiveAgentComposer(model);
    if (!hasAgentSessions(model)) {
        fx.cancel(agent_attention_effect_key);
        fx.cancelTimer(agent_attention_poll_timer_key);
        model.agent_attention_in_flight = false;
    }
    refreshTerminalUrl(model);
    markDesktopWorkspaceChanged(model, fx);
    if (was_active) {
        clearAgentSearch(model);
        resetAgentProjection(model, model.active_session_id);
        requestActiveAgentStream(model, fx);
        requestAgentComposerFocus(model);
    }
}

fn toggleAgentProviderPicker(model: *Model, fx: *Effects) void {
    model.agent_provider_picker_open = !model.agent_provider_picker_open;
    if (model.agent_provider_picker_open) model.session_picker_open = false;
    if (model.agent_provider_picker_open) requestAgentProviderRefresh(model, fx);
}

fn requestAgentProviderRefresh(model: *Model, fx: *Effects) void {
    if (model.agent_base_url_len == 0 or model.agent_provider_refresh_in_flight) return;
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentProviderUrl(model, storage[0..]) orelse return;
    model.agent_provider_refresh_in_flight = true;
    fx.fetch(.{
        .key = agent_provider_refresh_effect_key,
        .method = .POST,
        .url = request_url,
        .body = "{}",
        .timeout_ms = 10_000,
        .on_response = Effects.responseMsg(.agent_providers_refreshed),
    });
}

fn writeAgentProviderUrl(model: *const Model, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/providers?token={s}",
        .{ origin, token },
    ) catch null;
}

fn applyAgentProviderRefresh(model: *Model, response: native_sdk.EffectResponse) void {
    if (response.key != agent_provider_refresh_effect_key) return;
    model.agent_provider_refresh_in_flight = false;
    if (response.outcome != .ok or response.status != 200 or response.truncated) return;
    if (!applyAgentProviderStatus(model, response.body)) return;
    if (model.provider_login.provider) |provider| {
        const ready = if (provider == .codex or provider == .codex_acp)
            model.agentProviderReady(.codex) or model.agentProviderReady(.codex_acp)
        else
            model.agentProviderReady(provider);
        if (ready) model.provider_login.clear();
    }
    const ready = model.authenticated_agent_providers | model.session_auth_agent_providers;
    if (!model.agentProviderReady(model.selected_agent_provider)) {
        model.selected_agent_provider = firstAvailableAgentProvider(ready) orelse
            firstAvailableAgentProvider(model.available_agent_providers) orelse .codex;
    }
}

fn hasAgentSessions(model: *const Model) bool {
    for (model.openSessions()) |session| {
        if (session.mode == .agent) return true;
    }
    return false;
}

fn agentSessionCount(model: *const Model) usize {
    var count: usize = 0;
    for (model.openSessions()) |session| {
        if (session.mode == .agent) count += 1;
    }
    return count;
}

fn requestAgentAttention(model: *Model, fx: *Effects) void {
    if (!hasAgentSessions(model) or model.agent_attention_in_flight) return;
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentAttentionUrl(model, &storage) orelse return;
    model.agent_attention_in_flight = true;
    fx.fetch(.{
        .key = agent_attention_effect_key,
        .url = request_url,
        .timeout_ms = 4_000,
        .on_response = Effects.responseMsg(.agent_attention_received),
    });
}

fn writeAgentAttentionUrl(model: *const Model, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(storage, "{s}/agent/attention?token={s}", .{ origin, token }) catch null;
}

fn applyAgentAttentionResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    if (response.key != agent_attention_effect_key) return;
    model.agent_attention_in_flight = false;
    defer scheduleAgentAttentionRefresh(model, fx);
    if (response.outcome != .ok or response.status != 200 or response.truncated or
        response.body.len == 0 or response.body.len > max_agent_attention_status_bytes) return;
    const parsed = std.json.parseFromSlice(
        AgentAttentionResponseWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = false },
    ) catch return;
    defer parsed.deinit();
    if (parsed.value.sessions.len > max_sessions) return;

    var seen: [256]bool = @splat(false);
    for (parsed.value.sessions) |incoming| {
        if (incoming.session_id == 0 or incoming.session_id > std.math.maxInt(u8)) return;
        const session_id: u8 = @intCast(incoming.session_id);
        if (seen[session_id]) return;
        seen[session_id] = true;
        const provider = parseAgentProvider(incoming.provider) orelse return;
        _ = parseAgentTurnStatusStrict(incoming.status) orelse return;
        const session = findSession(model, session_id) orelse continue;
        if (session.mode != .agent or session.agent_provider != provider) return;
    }
    for (parsed.value.sessions) |incoming| {
        const session_id: u8 = @intCast(incoming.session_id);
        const session = findSession(model, session_id) orelse continue;
        const status = parseAgentTurnStatusStrict(incoming.status).?;
        session.agent_attention_status = status;
        session.agent_attention_revision = incoming.document_revision;
    }
}

fn scheduleAgentAttentionRefresh(model: *const Model, fx: *Effects) void {
    if (!hasAgentSessions(model)) return;
    fx.startTimer(.{
        .key = agent_attention_poll_timer_key,
        .interval_ms = 1_000,
        .on_fire = Effects.timerMsg(.agent_attention_poll),
    });
}

fn findSession(model: *Model, session_id: u8) ?*Session {
    for (model.session_slots[0..model.session_count]) |*session| {
        if (session.id == session_id) return session;
    }
    return null;
}

fn acknowledgeSessionAttention(model: *Model, session_id: u8) void {
    const session = findSession(model, session_id) orelse return;
    session.acknowledged_attention_status = session.agent_attention_status;
    session.acknowledged_attention_revision = session.agent_attention_revision;
}

fn requestAgentStart(model: *Model, session_id: u8, fx: *Effects) void {
    const session = findSession(model, session_id) orelse return;
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentStartUrl(model, session_id, storage[0..]) orelse return;
    setAgentConnection(model, session_id, .connecting);
    fx.fetch(.{
        .key = agent_start_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        // Zig 0.16's HTTP client requires POST to take the body-aware send
        // path even when this endpoint has no request fields.
        .body = "{}",
        .timeout_ms = agent_start_policy.timeoutMs(session.agent_provider.id()),
        .on_response = Effects.responseMsg(.agent_session_started),
    });
}

fn writeAgentStartUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const session = for (model.openSessions()) |candidate| {
        if (candidate.id == session_id and candidate.mode == .agent) break candidate;
    } else return null;
    var base_storage: [agent_effect_url_capacity]u8 = undefined;
    const base = writeAgentSessionUrl(model, session_id, base_storage[0..]) orelse return null;
    return std.fmt.bufPrint(storage, "{s}&provider={s}", .{ base, session.agent_provider.id() }) catch null;
}

fn requestAgentClose(model: *const Model, session_id: u8, fx: *Effects) void {
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentSessionUrl(model, session_id, storage[0..]) orelse return;
    fx.fetch(.{
        .key = agent_close_effect_key_base + session_id,
        .method = .DELETE,
        .url = request_url,
        .timeout_ms = 2_000,
        .on_response = Effects.responseMsg(.agent_session_closed),
    });
}

fn writeAgentSessionUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn writeAgentTurnUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/turn?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn writeAgentCancelUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/cancel?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn writeAgentPermissionUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/permission?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn writeAgentConfigUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/config?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn writeAgentTier2Url(
    model: *const Model,
    session_id: u8,
    suffix: []const u8,
    storage: []u8,
) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/tier2{s}?token={s}&session_id={d}",
        .{ origin, suffix, token, session_id },
    ) catch null;
}

fn requestAgentTier2Results(model: *Model, session_id: u8, fx: *Effects) void {
    if (session_id != model.active_session_id or
        model.activeSession().mode != .agent or
        model.agent_tier2_results_in_flight_session_id != 0) return;
    var url_storage: [agent_effect_url_capacity + 32]u8 = undefined;
    const request_url = writeAgentTier2Url(model, session_id, "", &url_storage) orelse return;
    fx.fetch(.{
        .key = agent_tier2_results_effect_key_base + session_id,
        .url = request_url,
        .timeout_ms = 4_000,
        .on_response = Effects.responseMsg(.agent_tier2_results_received),
    });
    model.agent_tier2_results_in_flight_session_id = session_id;
}

fn requestAgentTier2Preview(model: *Model, operation_id: []const u8, fx: *Effects) void {
    if (!validOperationId(operation_id)) return;
    if (model.agent_tier2_preview_ready and
        std.mem.eql(u8, model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len], operation_id))
    {
        clearAgentTier2Preview(model);
        return;
    }
    if (model.agent_tier2_action_in_flight_session_id != 0 or
        findAgentTier2Result(model, operation_id) == null) return;
    const session_id = model.active_session_id;
    var url_storage: [agent_effect_url_capacity + 32]u8 = undefined;
    const request_url = writeAgentTier2Url(model, session_id, "/preview", &url_storage) orelse return;
    var body_storage: [96]u8 = undefined;
    const body = std.fmt.bufPrint(&body_storage, "{{\"source_operation_id\":\"{s}\"}}", .{operation_id}) catch return;
    copyAgentTier2PreviewSource(model, operation_id);
    model.agent_tier2_preview_ready = false;
    model.agent_tier2_diff_len = 0;
    model.agent_tier2_diff_truncated = false;
    fx.fetch(.{
        .key = agent_tier2_preview_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = body,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_tier2_preview_received),
    });
    model.agent_tier2_action_in_flight_session_id = session_id;
}

fn requestAgentTier2Review(model: *Model, operation_id: []const u8, fx: *Effects) void {
    const result = findAgentTier2Result(model, operation_id) orelse return;
    if (!model.agent_tier2_preview_ready or result.has_acceptance or
        model.agent_tier2_action_in_flight_session_id != 0 or
        !std.mem.eql(u8, model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len], operation_id)) return;
    const session_id = model.active_session_id;
    var url_storage: [agent_effect_url_capacity + 32]u8 = undefined;
    const request_url = writeAgentTier2Url(model, session_id, "/review", &url_storage) orelse return;
    var body_storage: [96]u8 = undefined;
    const body = std.fmt.bufPrint(&body_storage, "{{\"source_operation_id\":\"{s}\"}}", .{operation_id}) catch return;
    fx.fetch(.{
        .key = agent_tier2_review_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = body,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_tier2_review_requested),
    });
    model.agent_tier2_action_in_flight_session_id = session_id;
}

fn requestAgentTier2Discard(model: *Model, operation_id: []const u8, fx: *Effects) void {
    const result = findAgentTier2Result(model, operation_id) orelse return;
    if (result.has_acceptance or model.agent_tier2_action_in_flight_session_id != 0) return;
    const session_id = model.active_session_id;
    var url_storage: [agent_effect_url_capacity + 32]u8 = undefined;
    const request_url = writeAgentTier2Url(model, session_id, "/discard", &url_storage) orelse return;
    var body_storage: [96]u8 = undefined;
    const body = std.fmt.bufPrint(&body_storage, "{{\"source_operation_id\":\"{s}\"}}", .{operation_id}) catch return;
    fx.fetch(.{
        .key = agent_tier2_discard_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = body,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_tier2_result_discarded),
    });
    model.agent_tier2_action_in_flight_session_id = session_id;
}

fn applyAgentStartResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    if (response.key <= agent_start_effect_key_base) return;
    const raw_session_id = response.key - agent_start_effect_key_base;
    if (raw_session_id > std.math.maxInt(u8)) return;
    const session_id: u8 = @intCast(raw_session_id);
    const ready = response.outcome == .ok and
        response.status == 200 and
        !response.truncated and
        std.mem.indexOf(u8, response.body, "\"status\":\"ready\"") != null;
    setAgentConnection(model, session_id, if (ready) .ready else .failed);
    if (session_id == model.active_session_id) {
        resetAgentProjection(model, session_id);
        model.agent_turn_status = if (ready) .ready else .failed;
        if (ready) {
            requestAgentComposerFocus(model);
            requestAgentSnapshot(model, session_id, fx);
            requestAgentStream(model, session_id, fx);
        } else {
            setAgentError(model, agentStartFailureMessage(response));
        }
    }
    if (agentSessionCount(model) > 1 or model.activeSession().mode != .agent) {
        requestAgentAttention(model, fx);
    }
}

fn agentStartFailureMessage(response: native_sdk.EffectResponse) []const u8 {
    if (response.outcome != .ok or response.truncated) {
        return "Agent gateway unavailable · no command executed";
    }
    return switch (response.status) {
        409 => "Agent tab already uses another provider · no command executed",
        429 => "Agent session limit reached · close a tab and retry",
        502 => "Agent provider failed to initialize · no command executed",
        503 => "Agent provider unavailable · no command executed",
        else => "Agent start failed · no command executed",
    };
}

fn requestAgentTurn(model: *Model, fx: *Effects) void {
    if (model.agentSubmitDisabled()) return;
    const prompt = std.mem.trim(u8, model.agent_composer_buffer.text(), " \t\r\n");
    if (prompt.len == 0) return;
    const session_id = model.active_session_id;
    var storage: [agent_effect_url_capacity + 8]u8 = undefined;
    const request_url = writeAgentTurnUrl(model, session_id, storage[0..]) orelse return;
    const pending = pendingAgentPrompt(model, session_id) orelse return;
    pending.set(prompt);
    fx.fetch(.{
        .key = agent_turn_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = prompt,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_turn_started),
    });
    model.agent_composer_buffer.clear();
    model.agent_goal_editing = false;
    requestAgentComposerFocus(model);
    model.agent_turn_status = .running;
    model.agent_error_len = 0;
}

fn editAgentGoal(model: *Model) void {
    if (model.agentGoalActionDisabled()) return;
    const current = std.mem.trim(u8, model.agent_composer_buffer.text(), " \t\r\n");
    if (current.len > 0 and !std.mem.startsWith(u8, current, "/goal ")) return;
    var storage: [max_agent_prompt_bytes]u8 = undefined;
    const goal_command = std.fmt.bufPrint(&storage, "/goal {s}", .{model.agent_goal.objective()}) catch return;
    model.agent_composer_buffer.set(goal_command);
    model.agent_goal_editing = true;
    model.agent_goal_menu_open = false;
}

fn requestAgentGoalAction(model: *Model, action: AgentGoalAction, fx: *Effects) void {
    if (model.agentGoalActionDisabled()) return;
    const session_id = model.active_session_id;
    var storage: [agent_effect_url_capacity + 8]u8 = undefined;
    const request_url = writeAgentTurnUrl(model, session_id, storage[0..]) orelse return;
    fx.fetch(.{
        .key = agent_goal_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = action.command(),
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_goal_updated),
    });
    model.agent_goal_in_flight_session_id = session_id;
    model.agent_goal_menu_open = false;
    model.agent_error_len = 0;
}

fn applyAgentGoalResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_goal_effect_key_base) orelse return;
    if (model.agent_goal_in_flight_session_id == session_id) {
        model.agent_goal_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        setAgentError(model, "Persistent Goal could not be updated");
        model.agent_turn_status = .failed;
        return;
    }
    scheduleAgentRefresh(session_id, fx);
}

fn applyAgentTurnResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_turn_effect_key_base) orelse return;
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent turn could not be started");
        restorePendingAgentPrompt(model, session_id);
        return;
    }
    scheduleAgentRefresh(session_id, fx);
}

fn requestAgentCancel(model: *Model, fx: *Effects) void {
    if (!model.hasAgentStopControl() or model.agentCancelDisabled()) return;
    const session_id = model.active_session_id;
    var storage: [agent_effect_url_capacity + 8]u8 = undefined;
    const request_url = writeAgentCancelUrl(model, session_id, storage[0..]) orelse return;
    fx.fetch(.{
        .key = agent_cancel_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = "{}",
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_turn_cancelled),
    });
    model.agent_turn_status = .cancelling;
    model.agent_error_len = 0;
    closeAgentConfigPickers(model);
    model.agent_command_picker_open = false;
}

fn applyAgentCancelResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_cancel_effect_key_base) orelse return;
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent turn could not be stopped safely");
        return;
    }
    scheduleAgentRefresh(session_id, fx);
}

fn requestAgentPermission(model: *Model, operation_id: []const u8, decision: []const u8, fx: *Effects) void {
    if (model.agent_permission_in_flight_session_id != 0 or
        !validOperationId(operation_id)) return;
    const block = for (model.agentBlocks()) |*candidate| {
        if (candidate.isApprovalPending() and
            std.mem.eql(u8, candidate.operationId(), operation_id)) break candidate;
    } else return;
    if (block.operation_revision == 0) return;
    const session = model.activeSession();
    if (session.mode != .agent or session.agent_connection != .ready) return;
    var url_storage: [agent_effect_url_capacity + 16]u8 = undefined;
    const request_url = writeAgentPermissionUrl(model, session.id, url_storage[0..]) orelse return;
    var body_storage: [256]u8 = undefined;
    const body = std.fmt.bufPrint(
        body_storage[0..],
        "{{\"operation_id\":\"{s}\",\"expected_revision\":{d},\"decision\":\"{s}\"}}",
        .{ block.operationId(), block.operation_revision, decision },
    ) catch return;
    fx.fetch(.{
        .key = agent_permission_effect_key_base + session.id,
        .method = .POST,
        .url = request_url,
        .body = body,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_permission_decided),
    });
    model.agent_permission_in_flight_session_id = session.id;
    model.agent_error_len = 0;
}

fn applyAgentPermissionResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_permission_effect_key_base) orelse return;
    if (model.agent_permission_in_flight_session_id == session_id) {
        model.agent_permission_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .waiting_approval;
        setAgentError(model, "Permission decision was not accepted; refresh before retrying");
        return;
    }
    model.agent_turn_status = .running;
    requestAgentTier2Results(model, session_id, fx);
    scheduleAgentRefresh(session_id, fx);
}

fn toggleAgentConfigPicker(model: *Model, index: u8) void {
    model.agent_command_picker_open = false;
    for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
        option.picker_open = option.index == index and !option.picker_open;
    }
}

fn closeAgentConfigPickers(model: *Model) void {
    for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
        option.picker_open = false;
    }
}

fn requestAgentConfig(model: *Model, action_id: u16, fx: *Effects) void {
    if (model.agent_config_in_flight_session_id != 0 or model.agentSubmitDisabled()) return;
    var selected_option: ?*const AgentConfigOptionView = null;
    var selected_choice: ?*const AgentConfigChoiceView = null;
    for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
        for (option.choices[0..option.choice_count]) |*choice| {
            if (choice.action_id == action_id) {
                selected_option = option;
                selected_choice = choice;
                break;
            }
        }
        if (selected_choice != null) break;
    }
    const option = selected_option orelse return;
    const choice = selected_choice orelse return;
    if (choice.selected) {
        closeAgentConfigPickers(model);
        return;
    }
    const session_id = model.active_session_id;
    var url_storage: [agent_effect_url_capacity + 16]u8 = undefined;
    const request_url = writeAgentConfigUrl(model, session_id, url_storage[0..]) orelse return;
    if (option.is_boolean) {
        const request = .{
            .config_id = option.id(),
            .value = .{
                .type = "boolean",
                .value = std.mem.eql(u8, choice.value(), "true"),
            },
        };
        const body = std.json.Stringify.valueAlloc(std.heap.page_allocator, request, .{}) catch return;
        defer std.heap.page_allocator.free(body);
        fetchAgentConfig(model, session_id, request_url, body, fx);
    } else {
        const request = .{
            .config_id = option.id(),
            .value = .{ .type = "id", .value = choice.value() },
        };
        const body = std.json.Stringify.valueAlloc(std.heap.page_allocator, request, .{}) catch return;
        defer std.heap.page_allocator.free(body);
        fetchAgentConfig(model, session_id, request_url, body, fx);
    }
}

fn fetchAgentConfig(
    model: *Model,
    session_id: u8,
    request_url: []const u8,
    body: []const u8,
    fx: *Effects,
) void {
    fx.fetch(.{
        .key = agent_config_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .body = body,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_config_updated),
    });
    model.agent_config_in_flight_session_id = session_id;
    closeAgentConfigPickers(model);
}

fn applyAgentConfigResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    _ = fx;
    const session_id = effectSessionId(response.key, agent_config_effect_key_base) orelse return;
    if (model.agent_config_in_flight_session_id == session_id) {
        model.agent_config_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    if (response.outcome != .ok or response.status != 200 or response.truncated) {
        setAgentError(model, "Agent configuration could not be updated");
        return;
    }
    const parsed = std.json.parseFromSlice(
        AgentCapabilitiesResponseWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = true },
    ) catch {
        setAgentError(model, "Agent configuration response was invalid");
        return;
    };
    defer parsed.deinit();
    if (parsed.value.session_id != session_id) return;
    projectAgentCapabilities(model, parsed.value.capabilities);
    model.agent_error_len = 0;
}

fn insertAgentCommand(model: *Model, index: u8) void {
    if (index >= model.agent_command_count) return;
    model.agent_command_picker_open = false;
    const entry = &model.agent_commands[index];
    var storage: [max_agent_prompt_bytes]u8 = undefined;
    const current = model.agent_composer_buffer.text();
    const command_name = entry.name();
    const prefix: []const u8 = if (std.mem.startsWith(u8, command_name, "$")) "" else "/";
    const next = if (current.len == 0)
        std.fmt.bufPrint(&storage, "{s}{s} ", .{ prefix, command_name }) catch return
    else
        std.fmt.bufPrint(&storage, "{s}\n{s}{s} ", .{ current, prefix, command_name }) catch return;
    model.agent_composer_buffer = canvas.TextBuffer(max_agent_prompt_bytes).init(next);
}

fn requestActiveAgentStream(model: *Model, fx: *Effects) void {
    const session = model.activeSession();
    if (session.mode == .agent and session.agent_connection == .ready) {
        requestAgentSnapshot(model, session.id, fx);
        requestAgentStream(model, session.id, fx);
    }
}

fn requestAgentStream(model: *Model, session_id: u8, fx: *Effects) void {
    if (model.agent_stream_session_id != 0) return;
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentStreamUrl(model, session_id, storage[0..]) orelse return;
    fx.cancelTimer(agent_poll_timer_key_base + session_id);
    model.agent_stream_session_id = session_id;
    fx.fetch(.{
        .key = agent_stream_effect_key_base + session_id,
        .url = request_url,
        .timeout_ms = 30 * 60 * 1_000,
        .response = .stream,
        .max_line_bytes = native_sdk.max_effect_line_bytes_ceiling,
        .on_line = Effects.lineMsg(.agent_stream_line),
        .on_response = Effects.responseMsg(.agent_stream_closed),
    });
}

fn cancelAgentStream(model: *Model, session_id: u8, fx: *Effects) void {
    if (model.agent_stream_session_id != session_id) return;
    model.agent_stream_session_id = 0;
    fx.cancel(agent_stream_effect_key_base + session_id);
}

fn writeAgentStreamUrl(model: *const Model, session_id: u8, storage: []u8) ?[]const u8 {
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse return null;
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    return std.fmt.bufPrint(
        storage,
        "{s}/agent/session/stream?token={s}&session_id={d}",
        .{ origin, token, session_id },
    ) catch null;
}

fn requestAgentSnapshot(model: *Model, session_id: u8, fx: *Effects) void {
    if (model.agent_snapshot_in_flight_session_id != 0) return;
    var storage: [agent_effect_url_capacity]u8 = undefined;
    const request_url = writeAgentSessionUrl(model, session_id, storage[0..]) orelse return;
    model.agent_snapshot_in_flight_session_id = session_id;
    fx.fetch(.{
        .key = agent_snapshot_effect_key_base + session_id,
        .url = request_url,
        .timeout_ms = 4_000,
        .on_response = Effects.responseMsg(.agent_snapshot_received),
    });
}

fn applyAgentTier2ResultsResponse(model: *Model, response: native_sdk.EffectResponse) void {
    const session_id = effectSessionId(response.key, agent_tier2_results_effect_key_base) orelse return;
    if (model.agent_tier2_results_in_flight_session_id == session_id) {
        model.agent_tier2_results_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    if (response.outcome != .ok or response.status != 200 or response.truncated) {
        setAgentError(model, "Tier 2 review results could not be refreshed");
        return;
    }
    const parsed = std.json.parseFromSlice(
        AgentTier2ResultsWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = true },
    ) catch {
        setAgentError(model, "Tier 2 review results were invalid");
        return;
    };
    defer parsed.deinit();
    projectAgentTier2Results(model, session_id, parsed.value.results);
}

fn projectAgentTier2Results(model: *Model, session_id: u8, results: []const AgentTier2ResultWire) void {
    for (&model.agent_tier2_results) |*result| result.* = .{};
    model.agent_tier2_result_count = 0;
    var cursor = results.len;
    while (cursor > 0 and model.agent_tier2_result_count < max_agent_tier2_results) {
        cursor -= 1;
        const wire = results[cursor];
        if (!validOperationId(wire.source_operation_id)) continue;
        const result = &model.agent_tier2_results[model.agent_tier2_result_count];
        copyTier2Id(
            &result.source_operation_id_storage,
            &result.source_operation_id_len,
            wire.source_operation_id,
        );
        result.changed_bytes = wire.changed_bytes;
        for (wire.changed_files) |file| {
            if (result.file_count == max_agent_tier2_files) {
                result.files_truncated = true;
                break;
            }
            if (file.path.len == 0 or file.path.len > max_agent_tier2_path_bytes or
                !std.unicode.utf8ValidateSlice(file.path)) continue;
            const target = &result.files[result.file_count];
            copyTier2Text(&target.path_storage, &target.path_len, file.path);
            copyTier2Text(&target.kind_storage, &target.kind_len, file.kind);
            target.bytes = file.bytes;
            result.file_count += 1;
        }
        if (wire.acceptance) |acceptance| {
            if (validOperationId(acceptance.operation_id) and acceptance.operation_revision > 0) {
                result.has_acceptance = true;
                copyTier2Id(
                    &result.acceptance_operation_id_storage,
                    &result.acceptance_operation_id_len,
                    acceptance.operation_id,
                );
                result.acceptance_revision = acceptance.operation_revision;
                result.acceptance_state = parseAgentOperationState(acceptance.state);
            }
        }
        model.agent_tier2_result_count += 1;
    }
    model.agent_tier2_projection_session_id = session_id;
    if (model.agent_tier2_preview_source_len > 0 and
        findAgentTier2Result(
            model,
            model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len],
        ) == null)
    {
        clearAgentTier2Preview(model);
    }
}

fn applyAgentTier2PreviewResponse(model: *Model, response: native_sdk.EffectResponse) void {
    const session_id = effectSessionId(response.key, agent_tier2_preview_effect_key_base) orelse return;
    if (model.agent_tier2_action_in_flight_session_id == session_id) {
        model.agent_tier2_action_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    if (response.outcome != .ok or response.status != 200 or response.truncated) {
        clearAgentTier2Preview(model);
        setAgentError(model, "Tier 2 Diff could not be prepared safely");
        return;
    }
    const parsed = std.json.parseFromSlice(
        AgentTier2PreviewWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = true },
    ) catch {
        clearAgentTier2Preview(model);
        setAgentError(model, "Tier 2 Diff response was invalid");
        return;
    };
    defer parsed.deinit();
    if (!validOperationId(parsed.value.source_operation_id) or
        !std.mem.eql(
            u8,
            parsed.value.source_operation_id,
            model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len],
        )) return;
    model.agent_tier2_diff_len = 0;
    model.agent_tier2_diff_truncated = parsed.value.truncated;
    for (parsed.value.changes, 0..) |change, change_index| {
        if (change_index > 0) appendAgentTier2Diff(model, "\n");
        appendAgentTier2Diff(model, change.target_path);
        appendAgentTier2Diff(model, "\n");
        if (change.binary) {
            var metadata: [192]u8 = undefined;
            const digest = if (change.proposed_digest.len >= 12) change.proposed_digest[0..12] else change.proposed_digest;
            const label = if (change.deleted)
                std.fmt.bufPrint(&metadata, "Binary file · {d} bytes → delete · no textual Diff\n", .{change.base_bytes}) catch "Binary file · delete · no textual Diff\n"
            else if (digest.len > 0)
                std.fmt.bufPrint(&metadata, "Binary file · {d} → {d} bytes · SHA-256 {s}… · no textual Diff\n", .{ change.base_bytes, change.proposed_bytes, digest }) catch "Binary file · no textual Diff\n"
            else
                std.fmt.bufPrint(&metadata, "Binary file · {d} → {d} bytes · no textual Diff\n", .{ change.base_bytes, change.proposed_bytes }) catch "Binary file · no textual Diff\n";
            appendAgentTier2Diff(model, label);
        }
        for (change.hunks) |hunk| {
            appendAgentTier2Diff(model, hunk.patch);
            if (hunk.patch.len > 0 and hunk.patch[hunk.patch.len - 1] != '\n') {
                appendAgentTier2Diff(model, "\n");
            }
            model.agent_tier2_diff_truncated = model.agent_tier2_diff_truncated or hunk.truncated;
        }
        model.agent_tier2_diff_truncated = model.agent_tier2_diff_truncated or change.truncated;
    }
    model.agent_tier2_preview_ready = true;
    model.agent_error_len = 0;
}

fn applyAgentTier2ReviewResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_tier2_review_effect_key_base) orelse return;
    if (model.agent_tier2_action_in_flight_session_id == session_id) {
        model.agent_tier2_action_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    if (response.outcome != .ok or response.status != 202 or response.truncated) {
        setAgentError(model, "Tier 2 workspace review could not enter the permission broker");
        return;
    }
    requestAgentTier2Results(model, session_id, fx);
    requestAgentSnapshot(model, session_id, fx);
}

fn applyAgentTier2DiscardResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_tier2_discard_effect_key_base) orelse return;
    if (model.agent_tier2_action_in_flight_session_id == session_id) {
        model.agent_tier2_action_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) return;
    if (response.outcome != .ok or response.status != 204) {
        setAgentError(model, "Tier 2 result could not be discarded");
        return;
    }
    clearAgentTier2Preview(model);
    requestAgentTier2Results(model, session_id, fx);
}

fn findAgentTier2Result(model: *const Model, operation_id: []const u8) ?*const AgentTier2ResultView {
    for (model.agentTier2Results()) |*result| {
        if (std.mem.eql(u8, result.sourceOperationId(), operation_id)) return result;
    }
    return null;
}

fn copyAgentTier2PreviewSource(model: *Model, operation_id: []const u8) void {
    copyTier2Id(
        &model.agent_tier2_preview_source_storage,
        &model.agent_tier2_preview_source_len,
        operation_id,
    );
}

fn clearAgentTier2Preview(model: *Model) void {
    model.agent_tier2_preview_source_len = 0;
    model.agent_tier2_diff_len = 0;
    model.agent_tier2_diff_truncated = false;
    model.agent_tier2_preview_ready = false;
}

fn appendAgentTier2Diff(model: *Model, value: []const u8) void {
    if (model.agent_tier2_diff_len == model.agent_tier2_diff_storage.len) {
        model.agent_tier2_diff_truncated = true;
        return;
    }
    const available = model.agent_tier2_diff_storage.len - model.agent_tier2_diff_len;
    const length = utf8BoundedLength(value, available);
    @memcpy(model.agent_tier2_diff_storage[model.agent_tier2_diff_len..][0..length], value[0..length]);
    model.agent_tier2_diff_len += length;
    model.agent_tier2_diff_truncated = model.agent_tier2_diff_truncated or length < value.len;
}

fn copyTier2Id(storage: *[max_agent_operation_id_bytes]u8, length: *usize, value: []const u8) void {
    const retained = @min(value.len, storage.len);
    @memcpy(storage[0..retained], value[0..retained]);
    length.* = retained;
}

fn copyTier2Text(storage: anytype, length: *usize, value: []const u8) void {
    const retained = utf8BoundedLength(value, storage.len);
    @memcpy(storage[0..retained], value[0..retained]);
    length.* = retained;
}

fn applyAgentSnapshotResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_snapshot_effect_key_base) orelse return;
    if (model.agent_snapshot_in_flight_session_id == session_id) {
        model.agent_snapshot_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) {
        requestActiveAgentStream(model, fx);
        return;
    }
    if (response.outcome != .ok or response.status != 200 or response.truncated) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent history could not be refreshed");
        return;
    }
    if (!applyAgentSnapshotPayload(model, session_id, response.body)) return;
    requestAgentTier2Results(model, session_id, fx);
    if (model.agent_document_revision < model.agent_snapshot_resync_revision) {
        requestAgentSnapshot(model, session_id, fx);
    } else {
        model.agent_snapshot_resync_revision = 0;
    }
    if (model.agent_turn_status == .running or model.agent_turn_status == .cancelling or
        model.agent_stream_session_id == 0)
    {
        scheduleAgentRefresh(session_id, fx);
    }
}

fn applyAgentStreamLine(model: *Model, line: native_sdk.EffectLine, fx: *Effects) void {
    const session_id = effectSessionId(line.key, agent_stream_effect_key_base) orelse return;
    if (session_id != model.active_session_id or model.agent_stream_session_id != session_id) return;
    if (line.truncated or line.dropped_before != 0) {
        setAgentError(model, "Agent live updates exceeded the bounded stream");
        cancelAgentStream(model, session_id, fx);
        scheduleAgentRefresh(session_id, fx);
        return;
    }
    const parsed = std.json.parseFromSlice(
        AgentStreamFrameWire,
        std.heap.page_allocator,
        line.line,
        .{ .ignore_unknown_fields = true },
    ) catch {
        setAgentError(model, "Agent live update was invalid; refreshing history");
        requestAgentSnapshot(model, session_id, fx);
        return;
    };
    defer parsed.deinit();
    const frame = parsed.value;
    if (std.mem.eql(u8, frame.type, "state")) {
        if (frame.history_restored) |restored| model.agent_history_restored = restored;
        projectPendingAgentOperation(model, frame.pending_operation_id);
        projectAgentCapabilities(model, frame.capabilities);
        projectAgentGoal(model, frame.goal);
        if (frame.status) |status| model.agent_turn_status = parseAgentTurnStatus(status);
        if (frame.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
        if (frame.document_revision) |revision| {
            if (revision > model.agent_document_revision) {
                requestAgentPatchResync(model, session_id, revision, fx);
            }
        }
        reconcilePendingAgentPrompt(model, session_id);
        if (model.agent_turn_status == .completed or model.agent_turn_status == .failed or
            model.agent_turn_status == .ready)
        {
            requestAgentTier2Results(model, session_id, fx);
        }
        return;
    }
    if (std.mem.eql(u8, frame.type, "resync")) {
        if (frame.status) |status| model.agent_turn_status = parseAgentTurnStatus(status);
        requestAgentPatchResync(model, session_id, frame.target_revision orelse 0, fx);
        return;
    }
    if (!std.mem.eql(u8, frame.type, "patch") or frame.patch == null) {
        setAgentError(model, "Agent live update type was unsupported; refreshing history");
        requestAgentSnapshot(model, session_id, fx);
        return;
    }
    if (frame.status) |status| model.agent_turn_status = parseAgentTurnStatus(status);
    applyAgentPatch(model, session_id, frame.patch.?, fx);
}

fn applyAgentPatch(model: *Model, session_id: u8, patch: AgentPatchWire, fx: *Effects) void {
    if (patch.target_revision <= model.agent_document_revision) {
        model.agent_stream_sequence = @max(model.agent_stream_sequence, patch.stream_sequence);
        return;
    }
    if (model.agent_snapshot_in_flight_session_id != 0) {
        requestAgentPatchResync(model, session_id, patch.target_revision, fx);
        return;
    }
    if (patch.base_revision != model.agent_document_revision or
        (model.agent_stream_sequence != 0 and patch.stream_sequence <= model.agent_stream_sequence))
    {
        requestAgentPatchResync(model, session_id, patch.target_revision, fx);
        return;
    }
    for (patch.operations) |operation| {
        if (!std.mem.eql(u8, operation.type, "append_content") or
            operation.block_id == null or operation.text == null)
        {
            requestAgentPatchResync(model, session_id, patch.target_revision, fx);
            return;
        }
        const block = findAgentAppendTarget(model, operation.block_id.?) orelse {
            requestAgentPatchResync(model, session_id, patch.target_revision, fx);
            return;
        };
        if (block.role != .agent and block.role != .thought) {
            requestAgentPatchResync(model, session_id, patch.target_revision, fx);
            return;
        }
    }
    for (patch.operations) |operation| {
        const block = findAgentAppendTarget(model, operation.block_id.?).?;
        appendAgentBlockContent(block, operation.text.?);
    }
    model.agent_document_revision = patch.target_revision;
    model.agent_stream_sequence = patch.stream_sequence;
}

fn requestAgentPatchResync(model: *Model, session_id: u8, target_revision: u64, fx: *Effects) void {
    model.agent_snapshot_resync_revision = @max(model.agent_snapshot_resync_revision, target_revision);
    requestAgentSnapshot(model, session_id, fx);
}

fn findAgentAppendTarget(model: *Model, block_id: []const u8) ?*AgentBlockView {
    const projected_id = stableAgentBlockId(block_id, 0);
    for (model.agent_blocks[0..model.agent_block_count]) |*block| {
        if (block.id != projected_id) continue;
        if (block.kind == .message or
            (block.kind == .tool_call and block.has_reasoning and block.activity_count == 0)) return block;
    }
    return null;
}

fn appendAgentBlockContent(view: *AgentBlockView, value: []const u8) void {
    const remaining = view.content_storage.len - view.content_len;
    const length = utf8BoundedLength(value, remaining);
    @memcpy(view.content_storage[view.content_len..][0..length], value[0..length]);
    view.content_len += length;
    view.truncated = view.truncated or length < value.len;
}

fn applyAgentStreamClosed(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_stream_effect_key_base) orelse return;
    if (model.agent_stream_session_id == session_id) model.agent_stream_session_id = 0;
    if (session_id != model.active_session_id or
        model.activeSession().mode != .agent or
        model.activeSession().agent_connection != .ready or
        response.outcome == .cancelled) return;
    if (response.outcome != .ok or response.status != 200 or response.dropped_before != 0) {
        setAgentError(model, "Agent live updates disconnected; reconnecting");
    }
    requestAgentSnapshot(model, session_id, fx);
    scheduleAgentRefresh(session_id, fx);
}

fn applyAgentSnapshotPayload(model: *Model, session_id: u8, body: []const u8) bool {
    const parsed = std.json.parseFromSlice(
        AgentSnapshotWire,
        std.heap.page_allocator,
        body,
        .{ .ignore_unknown_fields = true },
    ) catch {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent history response was invalid");
        return false;
    };
    defer parsed.deinit();
    if (parsed.value.session_id) |wire_session_id| {
        if (wire_session_id != session_id) return false;
    }
    if (!projectAgentExecutionContext(model, session_id, parsed.value.context)) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent execution context evidence was invalid");
        return false;
    }
    projectAgentCapabilities(model, parsed.value.capabilities);
    projectAgentGoal(model, parsed.value.goal);
    projectAgentBlocks(model, parsed.value.document.blocks);
    model.agent_history_restored = parsed.value.history_restored;
    projectPendingAgentOperation(model, parsed.value.pending_operation_id);
    model.agent_document_revision = parsed.value.document.revision;
    model.agent_stream_sequence = parsed.value.document.revision;
    model.agent_turn_status = parseAgentTurnStatus(parsed.value.status);
    if (parsed.value.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
    reconcilePendingAgentPrompt(model, session_id);
    return true;
}

fn projectPendingAgentOperation(model: *Model, operation_id: ?[]const u8) void {
    model.agent_pending_operation_len = 0;
    const value = operation_id orelse return;
    if (!validOperationId(value)) return;
    @memcpy(model.agent_pending_operation_storage[0..value.len], value);
    model.agent_pending_operation_len = value.len;
}

fn projectAgentExecutionContext(
    model: *Model,
    session_id: u8,
    event: ?AgentExecutionContextEventWire,
) bool {
    const wire = event orelse {
        clearAgentExecutionContext(model, session_id);
        return true;
    };
    const causation_id = wire.causation_id orelse return false;
    const correlation_id = wire.correlation_id orelse return false;
    if (wire.event_id.len == 0 or
        !std.mem.eql(u8, causation_id, correlation_id) or
        !std.mem.eql(u8, wire.payload.type, "agent_execution_context_recorded") or
        wire.payload.context.provider_id.len == 0 or
        wire.payload.context.provider_id.len > max_agent_capability_id_bytes or
        !std.mem.eql(u8, wire.payload.context.provider_id, model.activeSession().agent_provider.id()) or
        wire.payload.context.protocol.len == 0 or
        wire.payload.context.protocol.len > max_agent_capability_id_bytes or
        wire.payload.context.thread_id.len == 0 or
        wire.payload.context.thread_id.len > 4096 or
        wire.payload.context.receipts.len == 0 or
        wire.payload.context.receipts.len > max_agent_execution_contexts)
    {
        return false;
    }
    for (wire.payload.context.receipts, 0..) |receipt, index| {
        if (receipt.schema_version != 1 or
            receipt.context_revision == 0 or
            receipt.context_id.len == 0 or
            receipt.context_id.len > max_agent_context_id_bytes or
            !isContextDigest(receipt.context_digest) or
            !isContextDigest(receipt.environment_digest) or
            receipt.bindings.len > 128 or
            receipt.credential_bindings.len > 32)
        {
            return false;
        }
        const mode = parseAgentExecutionMode(receipt.mode) orelse return false;
        if (mode == .hermetic and !receipt.clear_inherited) return false;
        for (wire.payload.context.receipts[0..index]) |prior| {
            if (std.mem.eql(u8, prior.context_id, receipt.context_id)) return false;
        }
    }

    clearAgentExecutionContext(model, session_id);
    for (wire.payload.context.receipts) |receipt| {
        const projected = &model.agent_execution_contexts[model.agent_execution_context_count];
        copyCapabilityText(&projected.context_id_storage, &projected.context_id_len, receipt.context_id);
        projected.mode = parseAgentExecutionMode(receipt.mode).?;
        @memcpy(&projected.digest_storage, receipt.context_digest[0..agent_context_digest_bytes]);
        projected.binding_count = receipt.bindings.len;
        projected.credential_count = receipt.credential_bindings.len;
        model.agent_execution_context_count += 1;
    }
    const first_mode = model.agent_execution_contexts[0].mode;
    const same_mode = for (model.agent_execution_contexts[1..model.agent_execution_context_count]) |context| {
        if (context.mode != first_mode) break false;
    } else true;
    const summary = if (same_mode)
        std.fmt.bufPrint(
            &model.agent_execution_context_summary_storage,
            "{s} · {d} context{s}",
            .{
                first_mode.label(),
                model.agent_execution_context_count,
                if (model.agent_execution_context_count == 1) "" else "s",
            },
        ) catch return false
    else
        std.fmt.bufPrint(
            &model.agent_execution_context_summary_storage,
            "Mixed · {d} contexts",
            .{model.agent_execution_context_count},
        ) catch return false;
    model.agent_execution_context_summary_len = summary.len;
    return true;
}

fn clearAgentExecutionContext(model: *Model, session_id: u8) void {
    for (&model.agent_execution_contexts) |*context| context.* = .{};
    model.agent_execution_context_count = 0;
    model.agent_execution_context_session_id = session_id;
    model.agent_execution_context_expanded = false;
    model.agent_execution_context_summary_len = 0;
}

fn parseAgentExecutionMode(value: []const u8) ?AgentExecutionMode {
    if (std.mem.eql(u8, value, "hermetic")) return .hermetic;
    if (std.mem.eql(u8, value, "project")) return .project;
    if (std.mem.eql(u8, value, "user")) return .user;
    return null;
}

fn isContextDigest(value: []const u8) bool {
    return value.len == agent_context_digest_bytes and for (value) |byte| {
        if (!std.ascii.isDigit(byte) and !(byte >= 'a' and byte <= 'f')) break false;
    } else true;
}

fn projectAgentCapabilities(model: *Model, capabilities: AgentCapabilitiesWire) void {
    agent_capabilities.project(model, &model.session_slots[activeSessionIndex(model)].agent_capabilities, capabilities);
}

fn copyCapabilityText(destination: []u8, length: *usize, value: []const u8) void {
    const bounded_length = utf8BoundedLength(value, destination.len);
    @memcpy(destination[0..bounded_length], value[0..bounded_length]);
    length.* = bounded_length;
}

fn appendCapabilityText(destination: []u8, length: *usize, value: []const u8) void {
    if (length.* >= destination.len) return;
    const bounded_length = utf8BoundedLength(value, destination.len - length.*);
    @memcpy(destination[length.*..][0..bounded_length], value[0..bounded_length]);
    length.* += bounded_length;
}

const AgentProjectionGroup = enum { none, activity, single };

fn agentProjectionGroup(block: AgentBlockWire) AgentProjectionGroup {
    if (!renderableAgentBlock(block) or std.mem.eql(u8, block.kind, "agent_plan")) return .none;
    if (std.mem.eql(u8, block.kind, "agent_tool_call")) return .activity;
    if (std.mem.eql(u8, block.kind, "message") and
        parseAgentMessageRole(block.payload.role.?) == .thought) return .activity;
    return .single;
}

fn projectedAgentBlockCount(blocks: []const AgentBlockWire) usize {
    var count: usize = 0;
    var previous: AgentProjectionGroup = .none;
    for (blocks) |block| {
        const group = agentProjectionGroup(block);
        if (group == .none) continue;
        const continues = group != .single and group == previous;
        count += @intFromBool(!continues);
        previous = if (group == .single) .none else group;
    }
    return count;
}

fn projectAgentBlocks(model: *Model, blocks: []const AgentBlockWire) void {
    const previous_plan_id = model.agent_plan.id;
    const previous_plan_expanded = model.agent_plan.expanded;
    var previous_ids = [_]u64{0} ** max_agent_blocks;
    var previous_expanded = [_]bool{false} ** max_agent_blocks;
    for (model.agent_blocks[0..model.agent_block_count], 0..) |block, index| {
        previous_ids[index] = block.id;
        previous_expanded[index] = block.expanded;
    }
    for (&model.agent_blocks) |*block| block.* = .{};
    model.agent_block_count = 0;
    model.agent_plan = .{};
    model.agent_plan_visible = false;
    clearGenUiArtifact(model);
    for (blocks, 0..) |block, block_index| {
        if (validGenUiArtifactBlock(block)) {
            projectGenUiArtifact(model, block);
        }
        if (renderableAgentBlock(block) and std.mem.eql(u8, block.kind, "agent_plan")) {
            model.agent_plan.id = stableAgentBlockId(block.block_id, block_index);
            projectAgentPlan(&model.agent_plan, block.payload.entries.?);
            if (model.agent_plan.id == previous_plan_id) {
                model.agent_plan.expanded = previous_plan_expanded;
            }
            model.agent_plan_visible = true;
        }
    }
    const block_total = projectedAgentBlockCount(blocks);
    var skip = block_total -| max_agent_blocks;
    model.agent_block_index_base = @intCast(skip);
    model.agent_history_clipped = skip > 0;
    var previous_group: AgentProjectionGroup = .none;
    var retained_group = false;
    for (blocks, 0..) |block, block_index| {
        const group = agentProjectionGroup(block);
        if (group == .none) continue;
        const continues = group != .single and group == previous_group;
        if (!continues) {
            retained_group = skip == 0;
            if (skip > 0) skip -= 1;
        }
        previous_group = if (group == .single) .none else group;
        if (!retained_group) continue;
        if (!continues and model.agent_block_count == max_agent_blocks) break;
        const view = if (continues)
            &model.agent_blocks[model.agent_block_count - 1]
        else
            &model.agent_blocks[model.agent_block_count];
        if (!continues) view.id = stableAgentBlockId(block.block_id, block_index);
        if (std.mem.eql(u8, block.kind, "message")) {
            const role = parseAgentMessageRole(block.payload.role.?);
            if (role == .thought) {
                if (view.content_len > 0) appendActivitySlice(view, "\n\n");
                view.kind = .tool_call;
                view.role = .thought;
                view.has_reasoning = true;
                appendActivitySlice(view, visibleAgentMessageText(block.payload.text.?));
                copyActivityTitle(view, "Processed");
            } else {
                view.kind = .message;
                view.role = role;
                copyAgentBlockContent(view, visibleAgentMessageText(block.payload.text.?));
            }
        } else if (std.mem.eql(u8, block.kind, "operation")) {
            projectOperationBlock(view, block);
        } else if (std.mem.eql(u8, block.kind, "approval")) {
            projectApprovalBlock(view, block, blocks);
        } else if (std.mem.eql(u8, block.kind, "agent_tool_call")) {
            appendAgentToolCall(view, block.payload.call.?);
        }
        if (!continues) model.agent_block_count += 1;
    }
    for (model.agent_blocks[0..model.agent_block_count]) |*view| {
        for (previous_ids, 0..) |previous_id, previous_index| {
            if (previous_id == view.id) {
                view.expanded = previous_expanded[previous_index];
                break;
            }
        }
    }
    model.agent_projection_session_id = model.active_session_id;
}

fn validGenUiArtifactBlock(block: AgentBlockWire) bool {
    if (!std.mem.eql(u8, block.kind, "artifact") or
        !std.mem.eql(u8, block.payload.type, "artifact") or
        block.trust_class == null or
        !std.mem.eql(u8, block.trust_class.?, "isolated_artifact") or
        block.payload.artifact == null) return false;
    const artifact = block.payload.artifact.?;
    return validOperationId(artifact.artifact_id) and
        artifact.source_revision > 0 and
        artifact.entrypoint.len > 0 and
        artifact.compiler.name.len > 0 and
        artifact.compiler.version.len > 0 and
        isSha256(artifact.content_digest);
}

fn projectGenUiArtifact(model: *Model, block: AgentBlockWire) void {
    const artifact = block.payload.artifact.?;
    @memcpy(
        model.genui_artifact_id_storage[0..artifact.artifact_id.len],
        artifact.artifact_id,
    );
    model.genui_artifact_id_len = artifact.artifact_id.len;
    model.genui_source_revision = artifact.source_revision;
    refreshGenUiWorkbenchUrl(model);
}

fn clearGenUiArtifact(model: *Model) void {
    model.genui_workbench_url_len = 0;
    model.genui_artifact_id_len = 0;
    model.genui_source_revision = 0;
}

fn isSha256(value: []const u8) bool {
    if (value.len != 64) return false;
    for (value) |byte| {
        if (!std.ascii.isDigit(byte) and !(byte >= 'a' and byte <= 'f')) return false;
    }
    return true;
}

fn renderableAgentBlock(block: AgentBlockWire) bool {
    if (std.mem.eql(u8, block.kind, "message")) {
        return std.mem.eql(u8, block.payload.type, "message") and
            block.payload.role != null and block.payload.text != null and
            (!std.mem.eql(u8, block.payload.role.?, "agent") or
                visibleAgentMessageText(block.payload.text.?).len > 0);
    }
    if (std.mem.eql(u8, block.kind, "agent_tool_call")) {
        return std.mem.eql(u8, block.payload.type, "agent_tool_call") and
            block.payload.call != null and block.payload.call.?.title.len > 0;
    }
    if (std.mem.eql(u8, block.kind, "agent_plan")) {
        return std.mem.eql(u8, block.payload.type, "agent_plan") and
            block.payload.entries != null and block.payload.entries.?.len > 0;
    }
    const trusted = block.trust_class != null and
        std.mem.eql(u8, block.trust_class.?, "trusted_chrome");
    if (!trusted or block.payload.operation_id == null or
        !validOperationId(block.payload.operation_id.?)) return false;
    if (std.mem.eql(u8, block.kind, "operation")) {
        return std.mem.eql(u8, block.payload.type, "operation") and
            block.payload.summary != null and block.payload.risk != null and
            block.payload.state != null;
    }
    if (std.mem.eql(u8, block.kind, "approval")) {
        return std.mem.eql(u8, block.payload.type, "approval") and
            block.payload.prompt != null and block.payload.operation_revision != null;
    }
    return false;
}

fn visibleAgentMessageText(text: []const u8) []const u8 {
    const low_signal_prefixes = [_][]const u8{
        "Warning: Skill descriptions were shortened to fit",
        "Model metadata for ",
    };
    var low_signal = false;
    for (low_signal_prefixes) |prefix| {
        if (std.mem.startsWith(u8, text, prefix)) {
            low_signal = true;
            break;
        }
    }
    if (!low_signal) return text;
    const separator = std.mem.indexOf(u8, text, "\n\n") orelse return "";
    return std.mem.trimStart(u8, text[separator + 2 ..], "\r\n ");
}

fn copyActivityTitle(view: *AgentBlockView, value: []const u8) void {
    const length = utf8BoundedLength(value, view.title_storage.len);
    @memcpy(view.title_storage[0..length], value[0..length]);
    view.title_len = length;
}

fn appendActivityTitle(view: *AgentBlockView, value: []const u8) void {
    if (view.title_len >= view.title_storage.len) return;
    const available = view.title_storage.len - view.title_len;
    const length = utf8BoundedLength(value, available);
    @memcpy(view.title_storage[view.title_len..][0..length], value[0..length]);
    view.title_len += length;
}

fn copyActivityMeta(view: *AgentBlockView, value: []const u8) void {
    const length = utf8BoundedLength(value, view.meta_storage.len);
    @memcpy(view.meta_storage[0..length], value[0..length]);
    view.meta_len = length;
}

fn appendActivitySlice(view: *AgentBlockView, value: []const u8) void {
    const available = view.content_storage.len - view.content_len;
    const length = utf8BoundedLength(value, available);
    @memcpy(view.content_storage[view.content_len..][0..length], value[0..length]);
    view.content_len += length;
    view.truncated = view.truncated or length < value.len;
}

fn appendActivityFmt(view: *AgentBlockView, comptime format: []const u8, args: anytype) void {
    const rendered = std.fmt.bufPrint(view.content_storage[view.content_len..], format, args) catch {
        view.truncated = true;
        return;
    };
    view.content_len += rendered.len;
}

fn recordAgentDiffFile(view: *AgentBlockView, path: []const u8, added_lines: u32, removed_lines: u32) void {
    for (view.diff_files[0..view.diff_file_count]) |*file| {
        if (!std.mem.eql(u8, file.path(), path)) continue;
        file.added_lines +|= added_lines;
        file.removed_lines +|= removed_lines;
        return;
    }
    if (view.diff_file_count == view.diff_files.len) {
        view.diff_files_truncated = true;
        return;
    }
    const file = &view.diff_files[view.diff_file_count];
    const path_len = utf8BoundedLength(path, file.path_storage.len);
    @memcpy(file.path_storage[0..path_len], path[0..path_len]);
    file.path_len = path_len;
    file.added_lines = added_lines;
    file.removed_lines = removed_lines;
    view.diff_file_count += 1;
    view.diff_files_truncated = view.diff_files_truncated or path_len < path.len;
}

fn appendAgentToolCall(view: *AgentBlockView, call: AgentToolCallWire) void {
    const call_status = parseAgentToolStatus(call.status);
    if (view.activity_count == 0) {
        view.kind = .tool_call;
        view.tool_status = call_status;
        if (view.content_len > 0) appendActivitySlice(view, "\n---\n\n");
    } else {
        view.tool_status = mergedAgentToolStatus(view.tool_status, call_status);
        appendActivitySlice(view, "\n---\n\n");
    }
    view.activity_count +|= 1;
    if (std.mem.eql(u8, call.kind, "execute")) {
        view.execute_count +|= 1;
    } else if (std.mem.eql(u8, call.kind, "edit")) {
        view.edit_count +|= 1;
    } else if (std.mem.eql(u8, call.kind, "read") or
        std.mem.eql(u8, call.kind, "search") or
        std.mem.eql(u8, call.kind, "fetch"))
    {
        view.read_count +|= 1;
    } else {
        view.other_tool_count +|= 1;
    }
    appendActivityFmt(
        view,
        "**{s}** · _{s}_\n\n",
        .{ displayAgentToolTitle(call.kind, call.title), agentToolStatusLabel(call_status) },
    );
    for (call.locations) |location| {
        if (location.line) |line| {
            appendActivityFmt(view, "- `{s}:{d}`\n", .{ location.path, line });
        } else {
            appendActivityFmt(view, "- `{s}`\n", .{location.path});
        }
    }
    if (call.locations.len > 0) appendActivitySlice(view, "\n");
    for (call.content) |content| {
        if (std.mem.eql(u8, content.type, "text") and content.text != null) {
            appendActivitySlice(view, content.text.?);
            appendActivitySlice(view, "\n\n");
        } else if (std.mem.eql(u8, content.type, "diff") and
            content.path != null and content.patch != null)
        {
            view.diff_count +|= 1;
            view.added_lines +|= content.added_lines orelse 0;
            view.removed_lines +|= content.removed_lines orelse 0;
            recordAgentDiffFile(
                view,
                content.path.?,
                content.added_lines orelse 0,
                content.removed_lines orelse 0,
            );
            appendActivityFmt(view, "**{s}**\n\n```diff\n", .{content.path.?});
            appendActivitySlice(view, content.patch.?);
            appendActivitySlice(view, "\n```\n\n");
        } else if (std.mem.eql(u8, content.type, "terminal") and content.terminal_id != null) {
            view.terminal_count +|= 1;
            appendActivityFmt(view, "**Terminal** `{s}`\n\n", .{content.terminal_id.?});
        } else if (std.mem.eql(u8, content.type, "media")) {
            appendActivityFmt(
                view,
                "**{s}** · {s} · {d} encoded bytes\n\n",
                .{ content.kind orelse "media", content.mime_type orelse "application/octet-stream", content.encoded_bytes orelse 0 },
            );
        } else if (std.mem.eql(u8, content.type, "resource")) {
            appendActivityFmt(view, "**{s}** · `{s}`\n\n", .{ content.name orelse "Resource", content.uri orelse "" });
            if (content.text) |text| {
                appendActivitySlice(view, text);
                appendActivitySlice(view, "\n\n");
            }
        }
    }
    if (call.raw_input) |raw| {
        appendActivitySlice(view, "**Input**\n\n```json\n");
        appendActivitySlice(view, raw);
        appendActivitySlice(view, "\n```\n\n");
    }
    if (call.raw_output) |raw| {
        appendActivitySlice(view, "**Output**\n\n```json\n");
        appendActivitySlice(view, raw);
        appendActivitySlice(view, "\n```\n");
    }
    updateAgentToolSummary(view, call);
    if (view.tool_status == .failed) view.expanded = true;
}

fn mergedAgentToolStatus(left: AgentToolStatus, right: AgentToolStatus) AgentToolStatus {
    if (left == .failed or right == .failed) return .failed;
    if (left == .in_progress or right == .in_progress) return .in_progress;
    if (left == .pending or right == .pending) return .pending;
    return .completed;
}

fn updateAgentToolSummary(view: *AgentBlockView, last_call: AgentToolCallWire) void {
    var title: [max_agent_activity_title_bytes]u8 = undefined;
    const rendered_title = if (view.has_reasoning)
        "Processed"
    else if (view.activity_count == 1)
        displayAgentToolTitle(last_call.kind, last_call.title)
    else if (view.other_tool_count == 0 and view.edit_count == 0 and view.execute_count > 0)
        if (view.read_count > 0)
            std.fmt.bufPrint(&title, "Read files and ran {d} command{s}", .{ view.execute_count, if (view.execute_count == 1) "" else "s" }) catch "Used tools"
        else
            std.fmt.bufPrint(&title, "Ran {d} commands", .{view.execute_count}) catch "Used tools"
    else if (view.other_tool_count == 0 and view.execute_count == 0 and view.read_count == 0 and view.edit_count > 0)
        std.fmt.bufPrint(&title, "Made {d} edits", .{view.edit_count}) catch "Used tools"
    else if (view.other_tool_count == 0 and view.execute_count == 0 and view.edit_count == 0 and view.read_count > 0)
        std.fmt.bufPrint(&title, "Read files with {d} tools", .{view.read_count}) catch "Used tools"
    else
        std.fmt.bufPrint(&title, "Used {d} tools", .{view.activity_count}) catch "Used tools";
    copyActivityTitle(view, rendered_title);

    var meta: [max_agent_activity_meta_bytes]u8 = undefined;
    const status = agentToolStatusLabel(view.tool_status);
    const rendered = if (view.activity_count == 1 and view.diff_count > 0)
        std.fmt.bufPrint(&meta, "{s} · {d} file{s} · +{d} −{d}", .{ status, view.diff_count, if (view.diff_count == 1) "" else "s", view.added_lines, view.removed_lines }) catch status
    else if (view.activity_count == 1 and view.terminal_count > 0)
        std.fmt.bufPrint(&meta, "{s} · {d} terminal{s}", .{ status, view.terminal_count, if (view.terminal_count == 1) "" else "s" }) catch status
    else if (view.activity_count == 1)
        std.fmt.bufPrint(&meta, "{s} · {s}", .{ status, last_call.kind }) catch status
    else if (view.diff_count > 0)
        std.fmt.bufPrint(&meta, "{s} · {d} tool{s} · {d} file{s} · +{d} −{d}", .{ status, view.activity_count, if (view.activity_count == 1) "" else "s", view.diff_count, if (view.diff_count == 1) "" else "s", view.added_lines, view.removed_lines }) catch status
    else if (view.terminal_count > 0)
        std.fmt.bufPrint(&meta, "{s} · {d} tool{s} · {d} terminal{s}", .{ status, view.activity_count, if (view.activity_count == 1) "" else "s", view.terminal_count, if (view.terminal_count == 1) "" else "s" }) catch status
    else
        std.fmt.bufPrint(&meta, "{s} · {d} tool{s}", .{ status, view.activity_count, if (view.activity_count == 1) "" else "s" }) catch status;
    copyActivityMeta(view, rendered);
}

fn displayAgentToolTitle(kind: []const u8, title: []const u8) []const u8 {
    if (std.mem.eql(u8, title, "mcp__hyper_term__startup")) return "Start Hyper Term tools";
    if (std.mem.eql(u8, kind, "execute") and
        (title.len > 80 or std.mem.indexOfAny(u8, title, "|;&\n") != null)) return "Run shell command";
    return title;
}

fn projectAgentPlan(view: *AgentBlockView, entries: []const AgentPlanEntryWire) void {
    view.kind = .plan;
    var completed: usize = 0;
    var active_step: ?[]const u8 = null;
    var next_step: ?[]const u8 = null;
    for (entries) |entry| {
        const marker: []const u8 = if (std.mem.eql(u8, entry.status, "completed")) "x" else " ";
        completed += @intFromBool(std.mem.eql(u8, entry.status, "completed"));
        if (active_step == null and std.mem.eql(u8, entry.status, "in_progress")) active_step = entry.content;
        if (next_step == null and !std.mem.eql(u8, entry.status, "completed")) next_step = entry.content;
        // ACP priorities help the runtime order work, but they are noisy in the
        // compact Plan disclosure and are not a user-facing status. Keep the
        // native projection focused on completion and the current step.
        appendActivityFmt(view, "- [{s}] {s}\n", .{ marker, entry.content });
    }
    var meta: [max_agent_activity_meta_bytes]u8 = undefined;
    const rendered = std.fmt.bufPrint(&meta, "{d} / {d}", .{ completed, entries.len }) catch "Plan";
    copyActivityMeta(view, rendered);
    if (completed == entries.len) {
        copyActivityTitle(view, "Plan complete");
    } else {
        copyActivityTitle(view, "Plan · ");
        const step = active_step orelse next_step orelse entries[0].content;
        const step_length = utf8DisplayColumnPrefixLength(step, max_agent_goal_step_columns);
        appendActivityTitle(view, step[0..step_length]);
        if (step_length < step.len) appendActivityTitle(view, "…");
    }
    view.expanded = false;
}

fn projectAgentGoal(model: *Model, wire: ?AgentGoalWire) void {
    const was_expanded = model.agent_goal.expanded;
    model.agent_goal = .{};
    model.agent_goal.expanded = was_expanded;
    model.agent_goal_visible = false;
    const goal = wire orelse {
        model.agent_goal_menu_open = false;
        return;
    };
    const objective = std.mem.trim(u8, goal.objective, " \t\r\n");
    if (objective.len == 0) return;
    model.agent_goal.status = parseAgentGoalStatus(goal.status) orelse return;
    copyCapabilityText(
        &model.agent_goal.objective_storage,
        &model.agent_goal.objective_len,
        objective,
    );
    copyCapabilityText(
        &model.agent_goal.meta_storage,
        &model.agent_goal.meta_len,
        agentGoalStatusLabel(model.agent_goal.status),
    );
    var buffer: [64]u8 = undefined;
    if (goal.time_used_seconds > 0) {
        const elapsed = formatAgentGoalElapsed(&buffer, @intCast(goal.time_used_seconds)) catch "";
        if (elapsed.len > 0) {
            appendCapabilityText(&model.agent_goal.meta_storage, &model.agent_goal.meta_len, " · ");
            appendCapabilityText(&model.agent_goal.meta_storage, &model.agent_goal.meta_len, elapsed);
        }
    }
    if (goal.token_budget) |budget| {
        if (budget > 0) {
            const tokens = std.fmt.bufPrint(&buffer, "{d} / {d} tokens", .{ @max(goal.tokens_used, 0), budget }) catch "";
            if (tokens.len > 0) {
                appendCapabilityText(&model.agent_goal.meta_storage, &model.agent_goal.meta_len, " · ");
                appendCapabilityText(&model.agent_goal.meta_storage, &model.agent_goal.meta_len, tokens);
            }
        }
    }
    model.agent_goal_visible = true;
}

fn parseAgentGoalStatus(value: []const u8) ?AgentGoalStatus {
    if (std.mem.eql(u8, value, "active")) return .active;
    if (std.mem.eql(u8, value, "paused")) return .paused;
    if (std.mem.eql(u8, value, "blocked")) return .blocked;
    if (std.mem.eql(u8, value, "usage_limited")) return .usage_limited;
    if (std.mem.eql(u8, value, "budget_limited")) return .budget_limited;
    if (std.mem.eql(u8, value, "complete")) return .complete;
    return null;
}

fn agentGoalStatusLabel(status: AgentGoalStatus) []const u8 {
    return switch (status) {
        .active => "active",
        .paused => "paused",
        .blocked => "blocked",
        .usage_limited => "usage limited",
        .budget_limited => "budget limited",
        .complete => "complete",
    };
}

fn formatAgentGoalElapsed(buffer: []u8, seconds: u64) ![]u8 {
    if (seconds < 60) return std.fmt.bufPrint(buffer, "{d}s", .{seconds});
    const minutes = seconds / 60;
    if (minutes < 60) return std.fmt.bufPrint(buffer, "{d}m", .{minutes});
    const hours = minutes / 60;
    const remaining_minutes = minutes % 60;
    if (remaining_minutes == 0) return std.fmt.bufPrint(buffer, "{d}h", .{hours});
    return std.fmt.bufPrint(buffer, "{d}h {d}m", .{ hours, remaining_minutes });
}

fn utf8DisplayColumnPrefixLength(value: []const u8, maximum_columns: usize) usize {
    var index: usize = 0;
    var columns: usize = 0;
    while (index < value.len) {
        const sequence_length = std.unicode.utf8ByteSequenceLength(value[index]) catch break;
        if (index + sequence_length > value.len) break;
        const width: usize = if (sequence_length == 1) 1 else 2;
        if (columns + width > maximum_columns) break;
        columns += width;
        index += sequence_length;
    }
    return index;
}

fn utf8VisualLineCount(value: []const u8, maximum_columns: usize) usize {
    if (maximum_columns == 0) return 1;
    var index: usize = 0;
    var lines: usize = 1;
    var columns: usize = 0;
    while (index < value.len) {
        if (value[index] == '\n') {
            lines += 1;
            columns = 0;
            index += 1;
            continue;
        }
        const sequence_length = std.unicode.utf8ByteSequenceLength(value[index]) catch {
            index += 1;
            continue;
        };
        if (index + sequence_length > value.len) break;
        const width: usize = if (sequence_length == 1) 1 else 2;
        if (columns > 0 and columns + width > maximum_columns) {
            lines += 1;
            columns = 0;
        }
        columns += width;
        index += sequence_length;
    }
    return lines;
}

fn parseAgentToolStatus(value: []const u8) AgentToolStatus {
    if (std.mem.eql(u8, value, "in_progress")) return .in_progress;
    if (std.mem.eql(u8, value, "completed")) return .completed;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    return .pending;
}

fn agentToolStatusLabel(status: AgentToolStatus) []const u8 {
    return switch (status) {
        .pending => "pending",
        .in_progress => "running",
        .completed => "completed",
        .failed => "failed",
    };
}

fn projectOperationBlock(view: *AgentBlockView, block: AgentBlockWire) void {
    view.kind = .operation;
    copyAgentBlockContent(view, block.payload.summary.?);
    copyOperationId(view, block.payload.operation_id.?);
    copyOperationKind(view, operationKindLabel(block.payload.kind));
    view.risk = parseAgentRisk(block.payload.risk.?);
    view.tier2_isolated = stringListContains(block.payload.required_capabilities, "sandbox.isolated_task");
    view.state = parseAgentOperationState(block.payload.state.?);
}

fn projectApprovalBlock(
    view: *AgentBlockView,
    block: AgentBlockWire,
    blocks: []const AgentBlockWire,
) void {
    view.kind = .approval;
    copyAgentBlockContent(view, block.payload.prompt.?);
    copyOperationId(view, block.payload.operation_id.?);
    view.operation_revision = block.payload.operation_revision.?;
    view.allow_once_available = stringListContains(block.payload.options, "allow_once");
    view.decision = parseAgentDecision(block.payload.decision);
    view.state = if (view.decision == .none) .waiting_human else .cancelled;
    view.risk = .external_effect;
    copyOperationKind(view, "Agent effect");
    for (blocks) |candidate| {
        if (!renderableAgentBlock(candidate) or
            !std.mem.eql(u8, candidate.kind, "operation") or
            candidate.payload.operation_id == null or
            !std.mem.eql(u8, candidate.payload.operation_id.?, block.payload.operation_id.?)) continue;
        if (candidate.payload.kind) |kind| copyOperationKind(view, operationKindLabel(kind));
        if (candidate.payload.risk) |risk| view.risk = parseAgentRisk(risk);
        view.tier2_isolated = stringListContains(candidate.payload.required_capabilities, "sandbox.isolated_task");
        if (candidate.payload.state) |state| view.state = parseAgentOperationState(state);
        if (view.isWorkspaceReview() and candidate.payload.summary != null) {
            copyAgentBlockContent(view, candidate.payload.summary.?);
        }
        break;
    }
}

fn stringListContains(values: ?[]const []const u8, expected: []const u8) bool {
    const items = values orelse return false;
    for (items) |item| {
        if (std.mem.eql(u8, item, expected)) return true;
    }
    return false;
}

fn stableAgentBlockId(block_id: ?[]const u8, fallback_index: usize) u64 {
    const value = if (block_id) |id|
        std.hash.Wyhash.hash(0, id) & std.math.maxInt(i64)
    else
        fallback_index + 1;
    return if (value == 0) fallback_index + 1 else value;
}

fn copyAgentBlockContent(view: *AgentBlockView, value: []const u8) void {
    const length = utf8BoundedLength(value, view.content_storage.len);
    @memcpy(view.content_storage[0..length], value[0..length]);
    view.content_len = length;
    view.truncated = length < value.len;
}

fn copyOperationId(view: *AgentBlockView, value: []const u8) void {
    const length = @min(value.len, view.operation_id_storage.len);
    @memcpy(view.operation_id_storage[0..length], value[0..length]);
    view.operation_id_len = length;
}

fn copyOperationKind(view: *AgentBlockView, value: []const u8) void {
    const length = utf8BoundedLength(value, view.operation_kind_storage.len);
    @memcpy(view.operation_kind_storage[0..length], value[0..length]);
    view.operation_kind_len = length;
}

fn operationKindLabel(value: ?std.json.Value) []const u8 {
    const kind = value orelse return "Agent effect";
    const raw = switch (kind) {
        .string => |text| text,
        .object => |object| object_value: {
            const other = object.get("other") orelse break :object_value "Agent effect";
            break :object_value switch (other) {
                .string => |text| text,
                else => "Agent effect",
            };
        },
        else => return "Agent effect",
    };
    if (std.mem.eql(u8, raw, "agent_shell")) return "Agent shell request";
    if (std.mem.eql(u8, raw, "codex_shell")) return "Codex shell request";
    if (std.mem.eql(u8, raw, "file_edit")) return "Workspace edit";
    if (std.mem.eql(u8, raw, "agent_tool")) return "Agent tool";
    if (std.mem.eql(u8, raw, "mcp_tool")) return "MCP tool";
    if (std.mem.eql(u8, raw, "computer_use")) return "Computer Use";
    if (std.mem.eql(u8, raw, "artifact_build")) return "Artifact build";
    if (std.mem.eql(u8, raw, "shell")) return "Shell command";
    return raw;
}

fn validOperationId(value: []const u8) bool {
    if (value.len != max_agent_operation_id_bytes) return false;
    for (value, 0..) |byte, index| {
        if (index == 8 or index == 13 or index == 18 or index == 23) {
            if (byte != '-') return false;
        } else if (!std.ascii.isHex(byte)) return false;
    }
    return true;
}

fn parseAgentRisk(value: []const u8) AgentRisk {
    if (std.mem.eql(u8, value, "read_only")) return .read_only;
    if (std.mem.eql(u8, value, "workspace_write")) return .workspace_write;
    if (std.mem.eql(u8, value, "external_effect")) return .external_effect;
    if (std.mem.eql(u8, value, "destructive")) return .destructive;
    return .unknown;
}

fn parseAgentOperationState(value: []const u8) AgentOperationState {
    if (std.mem.eql(u8, value, "policy_check")) return .policy_check;
    if (std.mem.eql(u8, value, "waiting_human")) return .waiting_human;
    if (std.mem.eql(u8, value, "authorized")) return .authorized;
    if (std.mem.eql(u8, value, "dispatching")) return .dispatching;
    if (std.mem.eql(u8, value, "succeeded")) return .succeeded;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    if (std.mem.eql(u8, value, "cancelled")) return .cancelled;
    if (std.mem.eql(u8, value, "unknown_execution")) return .unknown_execution;
    return .proposed;
}

fn parseAgentDecision(value: ?[]const u8) AgentDecision {
    const decision = value orelse return .none;
    if (std.mem.eql(u8, decision, "allow_once")) return .allow_once;
    if (std.mem.eql(u8, decision, "reject_once")) return .reject_once;
    if (std.mem.eql(u8, decision, "cancelled")) return .cancelled;
    return .other;
}

fn parseAgentMessageRole(value: []const u8) AgentMessageRole {
    if (std.mem.eql(u8, value, "user")) return .user;
    if (std.mem.eql(u8, value, "system")) return .system;
    if (std.mem.eql(u8, value, "thought")) return .thought;
    return .agent;
}

fn parseAgentTurnStatus(value: []const u8) AgentTurnStatus {
    return parseAgentTurnStatusStrict(value) orelse .idle;
}

fn parseAgentTurnStatusStrict(value: []const u8) ?AgentTurnStatus {
    if (std.mem.eql(u8, value, "ready")) return .ready;
    if (std.mem.eql(u8, value, "running")) return .running;
    if (std.mem.eql(u8, value, "cancelling")) return .cancelling;
    if (std.mem.eql(u8, value, "completed")) return .completed;
    if (std.mem.eql(u8, value, "waiting_approval")) return .waiting_approval;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    return null;
}

fn scheduleAgentRefresh(session_id: u8, fx: *Effects) void {
    fx.startTimer(.{
        .key = agent_poll_timer_key_base + session_id,
        .interval_ms = 500,
        .on_fire = Effects.timerMsg(.agent_poll),
    });
}

fn effectSessionId(key: u64, base: u64) ?u8 {
    if (key <= base) return null;
    const raw_session_id = key - base;
    if (raw_session_id > std.math.maxInt(u8)) return null;
    return @intCast(raw_session_id);
}

fn resetAgentProjection(model: *Model, session_id: u8) void {
    for (&model.agent_blocks) |*block| block.* = .{};
    model.agent_block_count = 0;
    model.agent_block_index_base = 0;
    model.agent_history_clipped = false;
    model.agent_history_restored = false;
    model.agent_plan = .{};
    model.agent_plan_visible = false;
    model.agent_goal = .{};
    model.agent_goal_visible = false;
    model.agent_goal_menu_open = false;
    model.agent_goal_editing = false;
    model.agent_goal_in_flight_session_id = 0;
    model.agent_projection_session_id = session_id;
    clearAgentExecutionContext(model, session_id);
    model.agent_document_revision = 0;
    model.agent_stream_sequence = 0;
    model.agent_turn_status = .idle;
    model.agent_error_len = 0;
    model.agent_pending_operation_len = 0;
    model.agent_snapshot_resync_revision = 0;
    model.agent_permission_in_flight_session_id = 0;
    for (&model.agent_config_options) |*option| option.* = .{};
    for (&model.agent_commands) |*entry| entry.* = .{};
    model.agent_config_option_count = 0;
    model.agent_command_count = 0;
    model.agent_config_in_flight_session_id = 0;
    model.agent_command_picker_open = false;
    for (&model.agent_tier2_results) |*result| result.* = .{};
    model.agent_tier2_result_count = 0;
    model.agent_tier2_projection_session_id = session_id;
    model.agent_tier2_results_in_flight_session_id = 0;
    model.agent_tier2_action_in_flight_session_id = 0;
    model.agent_tier2_preview_source_len = 0;
    model.agent_tier2_diff_len = 0;
    model.agent_tier2_diff_truncated = false;
    model.agent_tier2_preview_ready = false;
    clearGenUiArtifact(model);
}

fn setAgentError(model: *Model, message: []const u8) void {
    const length = utf8BoundedLength(message, model.agent_error_storage.len);
    @memcpy(model.agent_error_storage[0..length], message[0..length]);
    model.agent_error_len = length;
}

fn pendingAgentPrompt(model: *Model, session_id: u8) ?*PendingAgentPrompt {
    for (model.session_slots[0..model.session_count], 0..) |session, index| {
        if (session.id == session_id) return &model.agent_pending_prompts[index];
    }
    return null;
}

fn reconcilePendingAgentPrompt(model: *Model, session_id: u8) void {
    switch (model.agent_turn_status) {
        .failed => restorePendingAgentPrompt(model, session_id),
        .completed => if (pendingAgentPrompt(model, session_id)) |pending| pending.clear(),
        else => {},
    }
}

fn restorePendingAgentPrompt(model: *Model, session_id: u8) void {
    const pending = pendingAgentPrompt(model, session_id) orelse return;
    defer pending.clear();
    if (session_id != model.active_session_id or pending.len == 0) return;
    if (model.agent_composer_buffer.text().len == 0) {
        model.agent_composer_buffer.set(pending.text());
    }
}

fn utf8BoundedLength(value: []const u8, maximum: usize) usize {
    var end = @min(value.len, maximum);
    while (end > 0 and !std.unicode.utf8ValidateSlice(value[0..end])) end -= 1;
    return end;
}

fn setAgentConnection(model: *Model, session_id: u8, connection: AgentConnection) void {
    for (model.session_slots[0..model.session_count]) |*session| {
        if (session.id == session_id and session.mode == .agent) {
            session.agent_connection = connection;
            return;
        }
    }
}

fn markDesktopWorkspaceChanged(model: *Model, fx: *Effects) void {
    if (!model.desktop_workspace_enabled or
        model.desktop_workspace_revision == std.math.maxInt(u64)) return;
    model.desktop_workspace_revision += 1;
    requestDesktopWorkspacePersist(model, fx);
}

fn requestDesktopWorkspacePersist(model: *Model, fx: *Effects) void {
    if (!model.desktop_workspace_enabled or
        model.desktop_workspace_in_flight_revision != 0 or
        model.desktop_workspace_revision <= model.desktop_workspace_persisted_revision) return;
    var url_storage: [terminal_close_url_capacity]u8 = undefined;
    const url = writeDesktopWorkspaceUrl(model, url_storage[0..]) orelse return;
    var session_wires: [max_sessions]DesktopSessionWire = undefined;
    for (model.openSessions(), 0..) |session, index| {
        session_wires[index] = .{
            .id = session.id,
            .mode = if (session.mode == .agent) "agent" else "terminal",
            .agent_provider = if (session.mode == .agent) session.agent_provider.id() else null,
        };
    }
    const snapshot = DesktopWorkspaceWire{
        .version = 1,
        .revision = model.desktop_workspace_revision,
        .active_session_id = model.active_session_id,
        .next_session_id = model.next_session_id,
        .selected_agent_provider = model.selected_agent_provider.id(),
        .sessions = session_wires[0..model.session_count],
    };
    const body = std.json.Stringify.valueAlloc(std.heap.page_allocator, snapshot, .{}) catch return;
    defer std.heap.page_allocator.free(body);
    if (body.len > max_desktop_workspace_bytes) return;
    fx.fetch(.{
        .key = desktop_workspace_effect_key,
        .method = .POST,
        .url = url,
        .body = body,
        .timeout_ms = 2_000,
        .on_response = Effects.responseMsg(.desktop_workspace_persisted),
    });
    model.desktop_workspace_in_flight_revision = model.desktop_workspace_revision;
}

fn requestTerminalMetadata(model: *Model, fx: *Effects) void {
    if (!model.desktop_workspace_enabled or model.terminal_metadata_in_flight) return;
    var storage: [terminal_close_url_capacity]u8 = undefined;
    const url = writeTerminalMetadataUrl(model, storage[0..]) orelse return;
    model.terminal_metadata_in_flight = true;
    fx.fetch(.{
        .key = terminal_metadata_effect_key,
        .url = url,
        .timeout_ms = 2_000,
        .on_response = Effects.responseMsg(.terminal_metadata_received),
    });
}

fn writeTerminalMetadataUrl(model: *const Model, storage: []u8) ?[]const u8 {
    const prefix = terminal_gateway_origin ++ "/?token=";
    const base_url = model.terminal_base_url_storage[0..model.terminal_base_url_len];
    if (!std.mem.startsWith(u8, base_url, prefix)) return null;
    return std.fmt.bufPrint(
        storage,
        terminal_gateway_origin ++ "/terminal/sessions/metadata?token={s}",
        .{base_url[prefix.len..]},
    ) catch null;
}

fn applyTerminalMetadataResponse(
    model: *Model,
    response: native_sdk.EffectResponse,
    fx: *Effects,
) void {
    if (response.key != terminal_metadata_effect_key) return;
    model.terminal_metadata_in_flight = false;
    defer scheduleTerminalMetadataRefresh(model, fx);
    if (response.outcome != .ok or response.status != 200 or response.truncated or
        response.body.len == 0 or response.body.len > max_terminal_metadata_bytes) return;
    const parsed = std.json.parseFromSlice(
        TerminalMetadataResponseWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = false },
    ) catch return;
    defer parsed.deinit();
    if (parsed.value.version != 1 or parsed.value.sessions.len > max_sessions) return;

    var seen: [256]bool = @splat(false);
    for (parsed.value.sessions) |incoming| {
        if (incoming.session_id == 0 or incoming.session_id > std.math.maxInt(u8)) return;
        const session_id: u8 = @intCast(incoming.session_id);
        if (seen[session_id] or incoming.revision == 0 or
            !validTerminalMetadataText(incoming.title, max_terminal_title_bytes, false) or
            !validTerminalMetadataText(incoming.cwd, max_terminal_cwd_bytes, true)) return;
        seen[session_id] = true;
        const session = findSession(model, session_id) orelse continue;
        if (session.mode != .terminal) return;
    }
    for (parsed.value.sessions) |incoming| {
        const session_id: u8 = @intCast(incoming.session_id);
        const session = findSession(model, session_id) orelse continue;
        if (incoming.revision <= session.terminal_metadata_revision) continue;
        session.terminal_metadata_revision = incoming.revision;
        copyTerminalMetadataText(
            session.terminal_title_storage[0..],
            &session.terminal_title_len,
            incoming.title,
        );
        copyTerminalMetadataText(
            session.terminal_cwd_storage[0..],
            &session.terminal_cwd_len,
            incoming.cwd,
        );
    }
}

fn validTerminalMetadataText(value: ?[]const u8, maximum: usize, absolute_path: bool) bool {
    const text = value orelse return true;
    if (text.len == 0 or text.len > maximum or (absolute_path and text[0] != '/')) return false;
    for (text) |byte| {
        if (byte < 0x20 or byte == 0x7f) return false;
    }
    return std.unicode.utf8ValidateSlice(text);
}

fn copyTerminalMetadataText(storage: []u8, length: *usize, value: ?[]const u8) void {
    const text = value orelse {
        length.* = 0;
        return;
    };
    @memcpy(storage[0..text.len], text);
    length.* = text.len;
}

fn scheduleTerminalMetadataRefresh(model: *const Model, fx: *Effects) void {
    if (!model.desktop_workspace_enabled) return;
    fx.startTimer(.{
        .key = terminal_metadata_poll_timer_key,
        .interval_ms = 1_000,
        .on_fire = Effects.timerMsg(.terminal_metadata_poll),
    });
}

fn applyDesktopWorkspacePersisted(
    model: *Model,
    response: native_sdk.EffectResponse,
    fx: *Effects,
) void {
    if (response.key != desktop_workspace_effect_key or
        model.desktop_workspace_in_flight_revision == 0) return;
    const submitted_revision = model.desktop_workspace_in_flight_revision;
    model.desktop_workspace_in_flight_revision = 0;
    if (response.outcome == .ok and response.status == 204) {
        model.desktop_workspace_persisted_revision = @max(
            model.desktop_workspace_persisted_revision,
            submitted_revision,
        );
    }
    if (model.desktop_workspace_revision > submitted_revision) {
        requestDesktopWorkspacePersist(model, fx);
    }
}

fn writeDesktopWorkspaceUrl(model: *const Model, storage: []u8) ?[]const u8 {
    const prefix = terminal_gateway_origin ++ "/?token=";
    const base_url = model.terminal_base_url_storage[0..model.terminal_base_url_len];
    if (!std.mem.startsWith(u8, base_url, prefix)) return null;
    return std.fmt.bufPrint(
        storage,
        terminal_gateway_origin ++ "/desktop/workspace?token={s}",
        .{base_url[prefix.len..]},
    ) catch null;
}

fn requestTerminalClose(model: *const Model, session_id: u8, fx: *Effects) void {
    const prefix = terminal_gateway_origin ++ "/?token=";
    const base_url = model.terminal_base_url_storage[0..model.terminal_base_url_len];
    if (!std.mem.startsWith(u8, base_url, prefix)) return;
    const token = base_url[prefix.len..];
    var storage: [terminal_close_url_capacity]u8 = undefined;
    const url = std.fmt.bufPrint(
        storage[0..],
        "{s}/terminal/session/close?token={s}&session_id={d}",
        .{ terminal_gateway_origin, token, session_id },
    ) catch return;
    fx.fetch(.{
        .key = terminal_close_effect_key_base + session_id,
        .method = .POST,
        .url = url,
        .body = "{}",
        .timeout_ms = 2_000,
        .on_response = Effects.responseMsg(.terminal_session_closed),
    });
}

fn selectSession(model: *Model, session_id: u8) void {
    for (model.openSessions()) |session| {
        if (session.id == session_id) {
            model.active_session_id = session_id;
            refreshTerminalUrl(model);
            return;
        }
    }
}

fn activeSessionIndex(model: *const Model) usize {
    for (model.openSessions(), 0..) |session, index| {
        if (session.id == model.active_session_id) return index;
    }
    return 0;
}

fn saveActiveAgentComposer(model: *Model) void {
    model.agent_composer_drafts[activeSessionIndex(model)] = model.agent_composer_buffer;
}

fn loadActiveAgentComposer(model: *Model) void {
    model.agent_composer_buffer = model.agent_composer_drafts[activeSessionIndex(model)];
    model.agent_goal_editing = false;
    model.agent_composer_focus_requested = false;
}

fn clearAgentSearch(model: *Model) void {
    model.agent_search_open = false;
    model.agent_search_buffer.clear();
}

fn requestAgentComposerFocus(model: *Model) void {
    model.agent_composer_focus_requested =
        model.activeSession().mode == .agent and
        model.activeSession().agent_connection == .ready and
        !model.agent_search_open;
}

pub fn command(name: []const u8) ?Msg {
    if (std.mem.eql(u8, name, "hyper-term.new-terminal")) return .choose_terminal;
    if (std.mem.eql(u8, name, "hyper-term.new-agent")) return .choose_agent;
    if (std.mem.eql(u8, name, "hyper-term.new-codex-agent")) return .choose_codex_agent;
    if (std.mem.eql(u8, name, "hyper-term.new-codex-acp-agent")) return .choose_codex_acp_agent;
    if (std.mem.eql(u8, name, "hyper-term.new-claude-acp-agent")) return .choose_claude_acp_agent;
    if (std.mem.eql(u8, name, "hyper-term.new-copilot-acp-agent")) return .choose_copilot_acp_agent;
    if (std.mem.eql(u8, name, "hyper-term.close-session")) return .close_active_session;
    return null;
}

/// Canvas-level fallback for macOS application shortcuts. The AppKit menu
/// monitor remains the primary path; this keeps the same lifecycle available
/// when a retained-canvas host injects a key event directly (including the
/// Native SDK automation harness). Control-only chords remain available to
/// zsh and terminal applications.
pub fn onKey(keyboard: canvas.WidgetKeyboardEvent) ?Msg {
    if (keyboard.phase != .key_down or
        !keyboard.modifiers.super or
        keyboard.modifiers.control or
        keyboard.modifiers.alt)
    {
        return null;
    }
    if (keyboard.modifiers.shift) {
        if (std.ascii.eqlIgnoreCase(keyboard.key, "n")) return .choose_agent;
        return null;
    }
    if (std.ascii.eqlIgnoreCase(keyboard.key, "t")) return .choose_terminal;
    if (std.ascii.eqlIgnoreCase(keyboard.key, "w")) return .close_active_session;
    if (std.ascii.eqlIgnoreCase(keyboard.key, "f")) return .open_agent_search;
    return null;
}

pub fn onAppearance(appearance: native_sdk.Appearance) ?Msg {
    return .{ .system_appearance = .{
        .scheme = switch (appearance.color_scheme) {
            .light => .light,
            .dark => .dark,
        },
        .high_contrast = appearance.high_contrast,
        .reduce_motion = appearance.reduce_motion,
    } };
}

pub fn onChrome(chrome: native_sdk.WindowChrome) ?Msg {
    return .{ .chrome_changed = chrome };
}

/// Native adapter for the normative values in the repository root DESIGN.md.
pub fn hyperTermTokens(model: *const Model) canvas.DesignTokens {
    const contrast: canvas.ColorContrast = if (model.high_contrast) .high else .standard;
    if (model.high_contrast) {
        var tokens = canvas.DesignTokens.theme(.{
            .color_scheme = model.system_scheme,
            .contrast = contrast,
            .density = .compact,
            .reduce_motion = model.reduce_motion,
        });
        tokens.controls.tabs_indicator = .underline;
        tokens.metrics.tabs_gap = 4;
        tokens.controls.button_group_style = .segmented;
        tokens.metrics.button_group_gap = 0;
        if (model.ui_font_registered) tokens.typography.font_id = ui_font_id;
        return tokens;
    }

    var tokens = canvas.DesignTokens.theme(.{
        .color_scheme = model.system_scheme,
        .contrast = contrast,
        .density = .compact,
        .reduce_motion = model.reduce_motion,
    });
    tokens.colors = switch (model.system_scheme) {
        .light => .{
            .background = canvas.Color.rgb8(247, 249, 241),
            .surface = canvas.Color.rgb8(255, 255, 255),
            .surface_subtle = canvas.Color.rgb8(238, 242, 229),
            .surface_pressed = canvas.Color.rgb8(225, 231, 212),
            .text = canvas.Color.rgb8(23, 26, 20),
            .text_muted = canvas.Color.rgb8(98, 106, 91),
            .border = canvas.Color.rgb8(213, 220, 201),
            .accent = canvas.Color.rgb8(69, 97, 9),
            .accent_text = canvas.Color.rgb8(247, 255, 217),
            .destructive = canvas.Color.rgb8(166, 49, 43),
            .destructive_text = canvas.Color.rgb8(255, 255, 255),
            .success = canvas.Color.rgb8(55, 103, 13),
            .success_text = canvas.Color.rgb8(255, 255, 255),
            .warning = canvas.Color.rgb8(138, 85, 0),
            .warning_text = canvas.Color.rgb8(255, 255, 255),
            .info = canvas.Color.rgb8(24, 94, 139),
            .info_text = canvas.Color.rgb8(255, 255, 255),
            .focus_ring = canvas.Color.rgb8(92, 125, 16),
            .shadow = canvas.Color.rgba8(13, 15, 11, 32),
            .disabled = canvas.Color.rgb8(238, 242, 229),
        },
        .dark => .{
            .background = canvas.Color.rgb8(13, 15, 11),
            .surface = canvas.Color.rgb8(18, 21, 15),
            .surface_subtle = canvas.Color.rgb8(24, 28, 21),
            .surface_pressed = canvas.Color.rgb8(36, 43, 29),
            .text = canvas.Color.rgb8(230, 233, 221),
            .text_muted = canvas.Color.rgb8(137, 145, 126),
            .border = canvas.Color.rgb8(41, 47, 36),
            .accent = canvas.Color.rgb8(215, 255, 114),
            .accent_text = canvas.Color.rgb8(17, 20, 13),
            .destructive = canvas.Color.rgb8(255, 141, 131),
            .destructive_text = canvas.Color.rgb8(17, 20, 13),
            .success = canvas.Color.rgb8(155, 207, 93),
            .success_text = canvas.Color.rgb8(17, 20, 13),
            .warning = canvas.Color.rgb8(240, 191, 104),
            .warning_text = canvas.Color.rgb8(17, 20, 13),
            .info = canvas.Color.rgb8(139, 198, 255),
            .info_text = canvas.Color.rgb8(17, 20, 13),
            .focus_ring = canvas.Color.rgb8(168, 213, 88),
            .shadow = canvas.Color.rgba8(0, 0, 0, 150),
            .disabled = canvas.Color.rgb8(36, 43, 29),
        },
    };
    tokens.typography.body_size = 14;
    tokens.typography.label_size = 12;
    tokens.typography.title_size = 18;
    tokens.typography.heading_size = 24;
    tokens.typography.display_size = 40;
    tokens.spacing = .{ .xs = 4, .sm = 8, .md = 12, .lg = 16, .xl = 24 };
    tokens.radius = .{ .sm = 4, .md = 6, .lg = 8, .xl = 12 };
    tokens.controls.tabs_indicator = .underline;
    tokens.metrics.tabs_gap = 4;
    tokens.controls.button_group_style = .segmented;
    tokens.metrics.button_group_gap = 0;
    if (model.ui_font_registered) tokens.typography.font_id = ui_font_id;
    return tokens;
}

/// The Native canvas renderer does not provide per-glyph fallback on every
/// render path. Hyper Term therefore registers one broad-coverage UI face at
/// boot. Packaging may override the path with a bundled OFL font; the macOS
/// development fallback is a system-owned font and is never copied into the
/// repository or read by a WebView.
pub fn preferredUiFontPath(configured: ?[]const u8) []const u8 {
    if (configured) |path| {
        if (path.len > 0) return path;
    }
    return default_macos_ui_font_path;
}

pub const HyperTermUi = canvas.Ui(Msg);
pub const app_markup = @embedFile("app.native");
pub const agent_block_contract_markup = @embedFile("agent_block_contract.native");
pub const CompiledHyperTermView = canvas.CompiledMarkupView(Model, Msg, app_markup);
pub const CompiledAgentBlockContractView = canvas.CompiledMarkupView(Model, Msg, agent_block_contract_markup);
pub const hyper_term_fragments = [_]canvas.MarkupFragment{
    CompiledHyperTermView.fragment("src/app.native"),
    CompiledAgentBlockContractView.fragment("src/agent_block_contract.native"),
};
const DesktopView = desktop_view.DesktopView(
    Model,
    Msg,
    CompiledHyperTermView,
    utf8DisplayColumnPrefixLength,
    .{
        .max_sessions = max_sessions,
        .max_inline_session_tabs = max_inline_session_tabs,
        .max_agent_blocks = max_agent_blocks,
        .max_agent_goal_step_columns = max_agent_goal_step_columns,
    },
);
pub const agentTimelineOptions = DesktopView.agentTimelineOptions;
pub const rootView = DesktopView.rootView;

pub fn initialModel() Model {
    return .{};
}

pub fn initialModelWithTerminalUrl(url: []const u8) Model {
    return initialModelWithServices(url, "");
}

pub fn initialModelWithServices(terminal_url: []const u8, agent_url: []const u8) Model {
    return initialModelWithProviders(terminal_url, agent_url, "codex");
}

pub fn initialModelWithProviders(terminal_url: []const u8, agent_url: []const u8, providers: []const u8) Model {
    return initialModelWithProviderStatus(terminal_url, agent_url, providers, "");
}

pub fn initialModelWithProviderStatus(
    terminal_url: []const u8,
    agent_url: []const u8,
    providers: []const u8,
    provider_status: []const u8,
) Model {
    var model: Model = undefined;
    initializeModelWithProviderStatus(&model, terminal_url, agent_url, providers, provider_status);
    return model;
}

fn initializeModelWithProviderStatus(
    model: *Model,
    terminal_url: []const u8,
    agent_url: []const u8,
    providers: []const u8,
    provider_status: []const u8,
) void {
    model.* = .{};
    if (trustedTerminalUrl(terminal_url)) {
        @memcpy(model.terminal_base_url_storage[0..terminal_url.len], terminal_url);
        model.terminal_base_url_len = terminal_url.len;
        refreshTerminalUrl(model);
    }
    if (trustedAgentUrl(agent_url)) {
        @memcpy(model.agent_base_url_storage[0..agent_url.len], agent_url);
        model.agent_base_url_len = agent_url.len;
        const legacy_providers = parseAgentProviders(providers);
        if (provider_status.len == 0) {
            model.available_agent_providers = legacy_providers;
            model.authenticated_agent_providers = legacy_providers;
        } else if (!applyAgentProviderStatus(model, provider_status)) {
            // The status document crosses a process boundary. Fail closed if
            // it is malformed instead of trusting the legacy ready list.
            clearAgentProviderStatus(model);
        }
        const ready = model.authenticated_agent_providers | model.session_auth_agent_providers;
        model.selected_agent_provider = firstAvailableAgentProvider(ready) orelse
            firstAvailableAgentProvider(model.available_agent_providers) orelse .codex;
    }
}

pub fn initializeModelWithDesktopServices(
    model: *Model,
    terminal_url: []const u8,
    agent_url: []const u8,
    providers: []const u8,
    provider_status: []const u8,
    bug_capsule_url: []const u8,
) void {
    initializeModelWithProviderStatus(
        model,
        terminal_url,
        agent_url,
        providers,
        provider_status,
    );
    if (trustedBugCapsuleUrl(bug_capsule_url, agent_url)) {
        model.session_slots[0] = .{
            .id = 1,
            .mode = .capsule,
            .title = "Capsule",
            .icon = "circle-dot",
        };
        @memcpy(
            model.genui_workbench_url_storage[0..bug_capsule_url.len],
            bug_capsule_url,
        );
        model.genui_workbench_url_len = bug_capsule_url.len;
        return;
    }
    model.desktop_workspace_enabled = model.terminal_base_url_len > 0;
}

pub fn restoreDesktopWorkspace(model: *Model, document: []const u8) bool {
    if (!model.desktop_workspace_enabled or document.len == 0 or
        document.len > max_desktop_workspace_bytes or model.isCapsule()) return false;
    const parsed = std.json.parseFromSlice(
        DesktopWorkspaceWire,
        std.heap.page_allocator,
        document,
        .{},
    ) catch return false;
    defer parsed.deinit();
    const wire = parsed.value;
    if (wire.version != 1 or wire.sessions.len == 0 or wire.sessions.len > max_sessions or
        wire.active_session_id == 0 or wire.next_session_id == 0) return false;
    const selected_provider = agentProviderFromId(wire.selected_agent_provider) orelse return false;
    var seen = [_]bool{false} ** 256;
    var sessions = [_]Session{.{}} ** max_sessions;
    var active_found = false;
    for (wire.sessions, 0..) |session, index| {
        if (session.id == 0 or seen[session.id]) return false;
        seen[session.id] = true;
        active_found = active_found or session.id == wire.active_session_id;
        if (std.mem.eql(u8, session.mode, "terminal")) {
            if (session.agent_provider != null) return false;
            sessions[index] = .{ .id = session.id };
            continue;
        }
        if (!std.mem.eql(u8, session.mode, "agent")) return false;
        const provider = agentProviderFromId(session.agent_provider orelse return false) orelse return false;
        sessions[index] = .{
            .id = session.id,
            .mode = .agent,
            .title = provider.label(),
            .icon = "circle-dot",
            .agent_provider = provider,
            .agent_connection = if (model.agent_base_url_len > 0 and model.agentProviderReady(provider))
                .connecting
            else
                .unavailable,
        };
    }
    if (!active_found or seen[wire.next_session_id]) return false;
    model.session_slots = sessions;
    model.agent_composer_buffer = .{};
    for (&model.agent_composer_drafts) |*draft| draft.* = .{};
    for (&model.agent_pending_prompts) |*pending| pending.* = .{};
    model.session_count = wire.sessions.len;
    model.active_session_id = wire.active_session_id;
    model.next_session_id = wire.next_session_id;
    model.selected_agent_provider = selected_provider;
    model.desktop_workspace_restored = true;
    model.desktop_workspace_revision = wire.revision;
    model.desktop_workspace_persisted_revision = wire.revision;
    model.desktop_workspace_in_flight_revision = 0;
    refreshTerminalUrl(model);
    return true;
}

fn providerBit(provider: AgentProvider) u8 {
    return switch (provider) {
        .codex => 1,
        .codex_acp => 2,
        .claude_acp => 4,
        .copilot_acp => 8,
    };
}

fn parseAgentProviders(value: []const u8) u8 {
    var providers: u8 = 0;
    var iterator = std.mem.splitScalar(u8, value, ',');
    while (iterator.next()) |provider| {
        if (std.mem.eql(u8, provider, "codex")) providers |= providerBit(.codex);
        if (std.mem.eql(u8, provider, "codex-acp")) providers |= providerBit(.codex_acp);
        if (std.mem.eql(u8, provider, "claude-acp")) providers |= providerBit(.claude_acp);
        if (std.mem.eql(u8, provider, "copilot-acp")) providers |= providerBit(.copilot_acp);
    }
    return providers;
}

fn agentProviderFromId(value: []const u8) ?AgentProvider {
    if (std.mem.eql(u8, value, "codex")) return .codex;
    if (std.mem.eql(u8, value, "codex-acp")) return .codex_acp;
    if (std.mem.eql(u8, value, "claude-acp")) return .claude_acp;
    if (std.mem.eql(u8, value, "copilot-acp")) return .copilot_acp;
    return null;
}

fn firstAvailableAgentProvider(providers: u8) ?AgentProvider {
    inline for (.{ AgentProvider.codex, AgentProvider.codex_acp, AgentProvider.claude_acp, AgentProvider.copilot_acp }) |provider| {
        if (providers & providerBit(provider) != 0) return provider;
    }
    return null;
}

fn applyAgentProviderStatus(model: *Model, source: []const u8) bool {
    if (source.len == 0 or source.len > max_agent_provider_status_bytes) return false;
    const parsed = std.json.parseFromSlice(
        []const AgentProviderStatusWire,
        std.heap.page_allocator,
        source,
        .{ .ignore_unknown_fields = false },
    ) catch return false;
    defer parsed.deinit();
    if (parsed.value.len == 0 or parsed.value.len > 4) return false;

    var detected: u8 = 0;
    var authenticated: u8 = 0;
    var session_auth: u8 = 0;
    var login_required: u8 = 0;
    var provider_missing: u8 = 0;
    var probe_failed: u8 = 0;
    var contained: u8 = 0;
    for (parsed.value) |status| {
        const provider = parseAgentProvider(status.id) orelse return false;
        const bit = providerBit(provider);
        if (detected & bit != 0 or !std.mem.eql(u8, status.protocol, expectedAgentProtocol(provider))) return false;
        detected |= bit;
        if (!std.mem.eql(u8, status.containment, "native_seatbelt")) return false;
        contained |= bit;
        if (std.mem.eql(u8, status.readiness, "authenticated")) {
            authenticated |= bit;
        } else if (std.mem.eql(u8, status.readiness, "available")) {
            session_auth |= bit;
        } else if (std.mem.eql(u8, status.readiness, "login_required")) {
            login_required |= bit;
        } else if (std.mem.eql(u8, status.readiness, "provider_missing")) {
            provider_missing |= bit;
        } else if (std.mem.eql(u8, status.readiness, "probe_failed")) {
            probe_failed |= bit;
        } else return false;
    }
    model.available_agent_providers = detected;
    model.authenticated_agent_providers = authenticated;
    model.session_auth_agent_providers = session_auth;
    model.login_required_agent_providers = login_required;
    model.provider_missing_agent_providers = provider_missing;
    model.provider_probe_failed_agent_providers = probe_failed;
    model.contained_agent_providers = contained;
    return true;
}

fn clearAgentProviderStatus(model: *Model) void {
    model.available_agent_providers = 0;
    model.authenticated_agent_providers = 0;
    model.session_auth_agent_providers = 0;
    model.login_required_agent_providers = 0;
    model.provider_missing_agent_providers = 0;
    model.provider_probe_failed_agent_providers = 0;
    model.contained_agent_providers = 0;
}

fn parseAgentProvider(id: []const u8) ?AgentProvider {
    return agent_provider.parse(id);
}

fn expectedAgentProtocol(provider: AgentProvider) []const u8 {
    return provider.protocol();
}

fn providerMenuLabel(model: *const Model, provider: AgentProvider) []const u8 {
    return agent_provider.menuLabel(provider, model.agentProviderReadiness(provider));
}

fn refreshTerminalUrl(model: *Model) void {
    if (model.terminal_base_url_len == 0) {
        model.terminal_url_len = 0;
        return;
    }
    const formatted = std.fmt.bufPrint(
        model.terminal_url_storage[0..],
        "{s}&tab={d}",
        .{
            model.terminal_base_url_storage[0..model.terminal_base_url_len],
            model.active_session_id,
        },
    ) catch {
        model.terminal_url_len = 0;
        return;
    };
    model.terminal_url_len = formatted.len;
}

pub fn terminalPanes(model: *const Model, out: []HyperTermApp.WebViewPane) usize {
    if (!model.isTerminal() or !model.terminalReady() or out.len == 0) return 0;
    out[0] = .{
        .label = terminal_view_label,
        .anchor = terminal_view_anchor,
        .url = model.terminalUrl(),
    };
    return 1;
}

pub fn desktopPanes(model: *const Model, out: []HyperTermApp.WebViewPane) usize {
    var count: usize = 0;
    if (model.terminal_webview_mounted and count < out.len) {
        out[count] = if (model.isTerminal() and model.terminalReady()) .{
            .label = terminal_view_label,
            .anchor = terminal_view_anchor,
            .url = model.terminalUrl(),
        } else .{
            .label = terminal_view_label,
            .frame = geometry.RectF.init(0, 0, 1, 1),
            .url = "zero://inline",
        };
        count += 1;
    }
    if (model.genui_webview_mounted and count < out.len) {
        out[count] = if (model.isCapsule() or model.hasAgentEditor()) .{
            .label = genui_view_label,
            .anchor = genui_view_anchor,
            .url = model.genUiWorkbenchUrl(),
            .reload_token = model.genui_source_revision,
        } else .{
            .label = genui_view_label,
            .frame = geometry.RectF.init(0, 0, 1, 1),
            .url = "zero://inline",
        };
        count += 1;
    }
    return count;
}

fn refreshGenUiWorkbenchUrl(model: *Model) void {
    if (model.agent_base_url_len == 0 or model.genui_artifact_id_len == 0) {
        model.genui_workbench_url_len = 0;
        return;
    }
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse {
        model.genui_workbench_url_len = 0;
        return;
    };
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    const artifact_id = model.genui_artifact_id_storage[0..model.genui_artifact_id_len];
    const url = std.fmt.bufPrint(
        model.genui_workbench_url_storage[0..],
        "{s}/agent/workbench/?surface=artifact&artifact_id={s}&session_id={d}&token={s}",
        .{ origin, artifact_id, model.active_session_id, token },
    ) catch {
        model.genui_workbench_url_len = 0;
        return;
    };
    model.genui_workbench_url_len = url.len;
}

pub fn trustedTerminalUrl(url: []const u8) bool {
    const prefix = terminal_gateway_origin ++ "/?token=";
    if (!std.mem.startsWith(u8, url, prefix)) return false;
    const token = url[prefix.len..];
    return trustedGatewayToken(token);
}

pub fn trustedAgentUrl(url: []const u8) bool {
    const prefix = "http://127.0.0.1:";
    if (!std.mem.startsWith(u8, url, prefix)) return false;
    const remainder = url[prefix.len..];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, remainder, marker) orelse return false;
    const port_text = remainder[0..marker_index];
    if (port_text.len == 0 or port_text.len > 5) return false;
    const port = std.fmt.parseInt(u16, port_text, 10) catch return false;
    if (port == 0) return false;
    return trustedGatewayToken(remainder[marker_index + marker.len ..]);
}

pub fn trustedAgentOrigin(url: []const u8) ?[]const u8 {
    if (!trustedAgentUrl(url)) return null;
    const marker_index = std.mem.indexOf(u8, url, "/?token=") orelse return null;
    return url[0..marker_index];
}

pub fn trustedBugCapsuleUrl(url: []const u8, agent_url: []const u8) bool {
    if (url.len == 0 or url.len > genui_url_capacity) return false;
    const origin = trustedAgentOrigin(agent_url) orelse return false;
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, agent_url, marker) orelse return false;
    const token = agent_url[marker_index + marker.len ..];
    var expected_storage: [genui_url_capacity]u8 = undefined;
    const expected = std.fmt.bufPrint(
        expected_storage[0..],
        "{s}/agent/workbench/?surface=capsule&token={s}",
        .{ origin, token },
    ) catch return false;
    return std.mem.eql(u8, url, expected);
}

/// Rebinds Rust-owned Agent sessions after a Native renderer replacement.
/// Session creation is idempotent at the gateway, so the same boot path
/// covers both an already-running Agent and a crash between tab creation and
/// the first ready response.
pub fn initEffects(model: *Model, fx: *Effects) void {
    requestTerminalMetadata(model, fx);
    if (!model.desktop_workspace_restored) return;
    for (model.openSessions()) |session| {
        if (session.mode == .agent and model.agentProviderReady(session.agent_provider)) {
            requestAgentStart(model, session.id, fx);
        }
    }
}

fn trustedGatewayToken(token: []const u8) bool {
    if (token.len < 32 or token.len > max_gateway_token_bytes) return false;
    for (token) |character| {
        if (!std.ascii.isAlphanumeric(character) and character != '-' and character != '_') return false;
    }
    return true;
}

/// Keeps launch-to-glass native-first. The scene installs only the Metal
/// canvas; the ordinary Terminal WebView joins on the next run-loop turn, and
/// the Artifact Workbench does not exist until an editor or Capsule needs it.
/// Both views remain presentation-only children of the trusted canvas.
pub const DeferredWebViewApp = struct {
    const FocusSurface = enum { none, canvas, terminal, genui };

    inner: native_sdk.App,
    model: *Model,
    primary_window_id: native_sdk.WindowId = 0,
    mount_timer_armed: bool = false,
    mounting_enabled: bool = false,
    app_active: bool = true,
    acknowledged_attention: ?AgentAttention = null,
    focused_surface: FocusSurface = .none,
    focused_terminal_session_id: u8 = 0,

    pub fn init(app_state: *HyperTermApp) DeferredWebViewApp {
        return .{
            .inner = app_state.app(),
            .model = &app_state.model,
        };
    }

    pub fn app(self: *DeferredWebViewApp) native_sdk.App {
        return .{
            .context = self,
            .name = self.inner.name,
            .source = self.inner.source,
            .scene_fn = scene,
            .start_fn = start,
            .event_fn = event,
            .stop_fn = stop,
            .replay_fn = replay,
        };
    }

    fn scene(context: *anyopaque) anyerror!native_sdk.ShellConfig {
        const self: *DeferredWebViewApp = @ptrCast(@alignCast(context));
        return (try self.inner.scene()) orelse error.MissingScene;
    }

    fn start(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *DeferredWebViewApp = @ptrCast(@alignCast(context));
        try self.inner.start(runtime);
    }

    fn event(context: *anyopaque, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) anyerror!void {
        const self: *DeferredWebViewApp = @ptrCast(@alignCast(context));
        try self.inner.event(runtime, event_value);

        switch (event_value) {
            .lifecycle => |lifecycle| switch (lifecycle) {
                .activate => self.app_active = true,
                .deactivate => self.app_active = false,
                else => {},
            },
            .gpu_surface_frame => |frame| {
                if (!std.mem.eql(u8, frame.label, canvas_label)) return;
                self.primary_window_id = frame.window_id;
                if (!self.mount_timer_armed and !self.mounting_enabled) {
                    try runtime.startTimer(deferred_webview_timer_id, deferred_webview_delay_ns, false);
                    self.mount_timer_armed = true;
                }
            },
            .timer => |timer| {
                if (timer.id == deferred_webview_timer_id) {
                    self.mount_timer_armed = false;
                    self.mounting_enabled = true;
                }
            },
            else => {},
        }

        self.projectAttention(runtime);

        if (!self.mounting_enabled or self.primary_window_id == 0) return;
        try self.mountTerminal(runtime);
        if (self.model.isCapsule() or self.model.hasAgentEditor()) {
            try self.mountGenUi(runtime);
        }
        self.projectInputFocus(runtime);
    }

    fn stop(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *DeferredWebViewApp = @ptrCast(@alignCast(context));
        try self.inner.stop(runtime);
    }

    fn replay(context: *anyopaque, control: native_sdk.runtime.ReplayControl) anyerror!void {
        const self: *DeferredWebViewApp = @ptrCast(@alignCast(context));
        try self.inner.replayControl(control);
    }

    /// Foreground attention is acknowledged by the visible Agent surface.
    /// Background attention is emitted once per semantic transition. Returning
    /// to a non-alert state clears the fingerprint so a later turn with the
    /// same document revision can still notify.
    fn projectAttention(self: *DeferredWebViewApp, runtime: *native_sdk.Runtime) void {
        const attention = agentAttention(self.model);
        if (self.app_active) {
            self.acknowledged_attention = attention;
            return;
        }
        const next = attention orelse {
            self.acknowledged_attention = null;
            return;
        };
        if (self.acknowledged_attention) |previous| {
            if (std.meta.eql(previous, next)) return;
        }
        runtime.showNotification(.{
            .title = next.title(),
            .subtitle = next.provider.label(),
            .body = next.body(),
        }) catch |err| {
            std.log.warn("native Agent attention notification failed: {s}", .{@errorName(err)});
        };
        // A platform failure must not become a notification storm on every
        // stream frame; the UI remains the durable source of the alert.
        self.acknowledged_attention = next;
    }

    fn mountTerminal(self: *DeferredWebViewApp, runtime: *native_sdk.Runtime) !void {
        if (self.model.terminal_webview_mounted) return;
        const url = if (self.model.terminalReady()) self.model.terminalUrl() else "zero://inline";
        _ = try runtime.createView(.{
            .window_id = self.primary_window_id,
            .label = terminal_view_label,
            .kind = .webview,
            .parent = canvas_label,
            .frame = geometry.RectF.init(0, 0, 1, 1),
            .layer = 20,
            .url = url,
        });
        self.model.terminal_webview_mounted = true;
    }

    fn mountGenUi(self: *DeferredWebViewApp, runtime: *native_sdk.Runtime) !void {
        if (self.model.genui_webview_mounted) return;
        const url = if (self.model.genUiWorkbenchUrl().len > 0) self.model.genUiWorkbenchUrl() else "zero://inline";
        _ = try runtime.createView(.{
            .window_id = self.primary_window_id,
            .label = genui_view_label,
            .kind = .webview,
            .parent = canvas_label,
            .frame = geometry.RectF.init(0, 0, 1, 1),
            .layer = 21,
            .url = url,
        });
        self.model.genui_webview_mounted = true;
    }

    /// Transfers the platform first responder only when the active interactive
    /// surface changes. DOM/widget-local focus remains owned by that surface,
    /// so ordinary rebuilds cannot steal an in-progress IME composition.
    fn projectInputFocus(self: *DeferredWebViewApp, runtime: *native_sdk.Runtime) void {
        const next: FocusSurface = if (self.model.isTerminal())
            .terminal
        else if (self.model.isCapsule() or self.model.hasAgentEditor())
            .genui
        else
            .canvas;
        const terminal_session_changed = next == .terminal and
            self.focused_terminal_session_id != self.model.active_session_id;
        if (next == self.focused_surface and !terminal_session_changed) return;

        const label = switch (next) {
            .none => return,
            .canvas => canvas_label,
            .terminal => terminal_view_label,
            .genui => genui_view_label,
        };
        runtime.focusView(self.primary_window_id, label) catch |err| {
            std.log.warn("native input focus lease could not move to '{s}': {s}", .{ label, @errorName(err) });
            return;
        };
        self.focused_surface = next;
        self.focused_terminal_session_id = if (next == .terminal) self.model.active_session_id else 0;
    }
};

pub fn main(init: std.process.Init) !void {
    const terminal_url = init.environ_map.get("HYPER_TERM_TERMINAL_URL") orelse "";
    const agent_url = init.environ_map.get("HYPER_TERM_AGENT_URL") orelse "";
    const agent_providers = init.environ_map.get("HYPER_TERM_AGENT_PROVIDERS") orelse "";
    const agent_provider_status = init.environ_map.get("HYPER_TERM_AGENT_PROVIDER_STATUS") orelse "";
    const bug_capsule_url = init.environ_map.get("HYPER_TERM_BUG_CAPSULE_URL") orelse "";
    const desktop_workspace = init.environ_map.get("HYPER_TERM_DESKTOP_WORKSPACE") orelse "";
    const ui_font_path = preferredUiFontPath(init.environ_map.get("HYPER_TERM_UI_FONT_PATH"));
    const ui_font_bytes = std.Io.Dir.cwd().readFileAlloc(
        init.io,
        ui_font_path,
        std.heap.page_allocator,
        .limited(max_ui_font_bytes),
    ) catch null;
    defer if (ui_font_bytes) |bytes| std.heap.page_allocator.free(bytes);
    var font_registration: [1]HyperTermApp.FontRegistration = undefined;
    const app_fonts: []const HyperTermApp.FontRegistration = if (ui_font_bytes) |bytes| blk: {
        font_registration[0] = .{
            .id = ui_font_id,
            .name = std.fs.path.basename(ui_font_path),
            .ttf = bytes,
        };
        break :blk &font_registration;
    } else &.{};
    var allowed_origins = [_][]const u8{
        "zero://app",
        "zero://inline",
        terminal_gateway_origin,
        "",
    };
    var allowed_origin_count: usize = 3;
    if (trustedAgentOrigin(agent_url)) |origin| {
        allowed_origins[allowed_origin_count] = origin;
        allowed_origin_count += 1;
    }
    const app_state = try HyperTermApp.create(std.heap.page_allocator, .{
        .name = "hyper-term",
        .scene = shell_scene,
        .canvas_label = canvas_label,
        .update_fx = update,
        .init_fx = initEffects,
        .tokens_fn = hyperTermTokens,
        .fonts = app_fonts,
        .on_command = command,
        .on_key = onKey,
        .on_appearance = onAppearance,
        .on_chrome = onChrome,
        .view = rootView,
        .web_panes = desktopPanes,
        // Whole-document `.markup` would replace `rootView` after the first
        // edit. A watched fragment reloads the compiled shell in place, then
        // still passes it through the Agent timeline composition seam.
        .markup = null,
        .fragment_watch = if (dev_markup_reload)
            .{ .fragments = &hyper_term_fragments, .io = init.io }
        else
            null,
    });
    defer app_state.destroy();
    initializeModelWithDesktopServices(
        &app_state.model,
        terminal_url,
        agent_url,
        agent_providers,
        agent_provider_status,
        bug_capsule_url,
    );
    _ = restoreDesktopWorkspace(&app_state.model, desktop_workspace);
    app_state.model.ui_font_registered = app_fonts.len > 0;

    var deferred_webview_app = DeferredWebViewApp.init(app_state);
    try runner.runWithOptions(deferred_webview_app.app(), .{
        .app_name = "hyper-term",
        .window_title = "Hyper Term",
        .bundle_id = "dev.hyperterm.desktop",
        .icon_path = "assets/icon.png",
        .default_frame = geometry.RectF.init(0, 0, window_width, window_height),
        .restore_state = true,
        .js_window_api = false,
        .security = .{
            .permissions = &app_permissions,
            .navigation = .{ .allowed_origins = allowed_origins[0..allowed_origin_count] },
        },
    }, init);
}

test {
    _ = @import("tests.zig");
    _ = @import("agent_projection_tests.zig");
    _ = @import("provider_login_tests.zig");
}
