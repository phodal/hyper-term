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
const agent_effects = @import("agent_effects.zig");
const agent_projection = @import("agent_projection.zig");
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
pub const agent_start_effect_key_base = agent_effects.agent_start_effect_key_base;
pub const agent_turn_effect_key_base = agent_effects.agent_turn_effect_key_base;
pub const agent_cancel_effect_key_base = agent_effects.agent_cancel_effect_key_base;
pub const agent_snapshot_effect_key_base = agent_effects.agent_snapshot_effect_key_base;
pub const agent_poll_timer_key_base = agent_effects.agent_poll_timer_key_base;
pub const agent_permission_effect_key_base = agent_effects.agent_permission_effect_key_base;
pub const agent_config_effect_key_base = agent_effects.agent_config_effect_key_base;
pub const agent_stream_effect_key_base = agent_effects.agent_stream_effect_key_base;
pub const agent_tier2_results_effect_key_base = agent_effects.agent_tier2_results_effect_key_base;
pub const agent_tier2_preview_effect_key_base = agent_effects.agent_tier2_preview_effect_key_base;
pub const agent_tier2_review_effect_key_base = agent_effects.agent_tier2_review_effect_key_base;
pub const agent_tier2_discard_effect_key_base = agent_effects.agent_tier2_discard_effect_key_base;
pub const agent_provider_refresh_effect_key: u64 = 0x4854_4e00;
pub const agent_goal_effect_key_base = agent_effects.agent_goal_effect_key_base;
pub const deferred_webview_timer_id: u64 = 0x4854_5100;
pub const agent_attention_effect_key = agent_effects.agent_attention_effect_key;
pub const agent_attention_poll_timer_key = agent_effects.agent_attention_poll_timer_key;
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
const findAgentAppendTarget = agent_projection.findAgentAppendTarget;
const appendAgentBlockContent = agent_projection.appendAgentBlockContent;
const applyAgentSnapshotPayload = agent_projection.applyAgentSnapshotPayload;
const projectPendingAgentOperation = agent_projection.projectPendingAgentOperation;
const projectAgentCapabilities = agent_projection.projectAgentCapabilities;
const projectAgentGoal = agent_projection.projectAgentGoal;
const validOperationId = agent_projection.validOperationId;
const parseAgentOperationState = agent_projection.parseAgentOperationState;
const parseAgentTurnStatus = agent_projection.parseAgentTurnStatus;
const parseAgentTurnStatusStrict = agent_projection.parseAgentTurnStatusStrict;
const resetAgentProjection = agent_projection.resetAgentProjection;
const setAgentError = agent_projection.setAgentError;
const pendingAgentPrompt = agent_projection.pendingAgentPrompt;
const reconcilePendingAgentPrompt = agent_projection.reconcilePendingAgentPrompt;
const restorePendingAgentPrompt = agent_projection.restorePendingAgentPrompt;
const utf8BoundedLength = agent_projection.utf8BoundedLength;
const utf8DisplayColumnPrefixLength = agent_projection.utf8DisplayColumnPrefixLength;
const setAgentConnection = agent_projection.setAgentConnection;

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
const AgentEffectRouter = agent_effects.Router(Effects);
const hasAgentSessions = AgentEffectRouter.hasAgentSessions;
const requestAgentAttention = AgentEffectRouter.requestAgentAttention;
const applyAgentAttentionResponse = AgentEffectRouter.applyAgentAttentionResponse;
const findSession = AgentEffectRouter.findSession;
const acknowledgeSessionAttention = AgentEffectRouter.acknowledgeSessionAttention;
const requestAgentStart = AgentEffectRouter.requestAgentStart;
const requestAgentClose = AgentEffectRouter.requestAgentClose;
const requestAgentTier2Preview = AgentEffectRouter.requestAgentTier2Preview;
const requestAgentTier2Review = AgentEffectRouter.requestAgentTier2Review;
const requestAgentTier2Discard = AgentEffectRouter.requestAgentTier2Discard;
const applyAgentStartResponse = AgentEffectRouter.applyAgentStartResponse;
const requestAgentTurn = AgentEffectRouter.requestAgentTurn;
const editAgentGoal = AgentEffectRouter.editAgentGoal;
const requestAgentGoalAction = AgentEffectRouter.requestAgentGoalAction;
const applyAgentGoalResponse = AgentEffectRouter.applyAgentGoalResponse;
const applyAgentTurnResponse = AgentEffectRouter.applyAgentTurnResponse;
const requestAgentCancel = AgentEffectRouter.requestAgentCancel;
const applyAgentCancelResponse = AgentEffectRouter.applyAgentCancelResponse;
const requestAgentPermission = AgentEffectRouter.requestAgentPermission;
const applyAgentPermissionResponse = AgentEffectRouter.applyAgentPermissionResponse;
const toggleAgentConfigPicker = AgentEffectRouter.toggleAgentConfigPicker;
const closeAgentConfigPickers = AgentEffectRouter.closeAgentConfigPickers;
const requestAgentConfig = AgentEffectRouter.requestAgentConfig;
const applyAgentConfigResponse = AgentEffectRouter.applyAgentConfigResponse;
const insertAgentCommand = AgentEffectRouter.insertAgentCommand;
const requestActiveAgentStream = AgentEffectRouter.requestActiveAgentStream;
const cancelAgentStream = AgentEffectRouter.cancelAgentStream;
const applyAgentTier2ResultsResponse = AgentEffectRouter.applyAgentTier2ResultsResponse;
const applyAgentTier2PreviewResponse = AgentEffectRouter.applyAgentTier2PreviewResponse;
const applyAgentTier2ReviewResponse = AgentEffectRouter.applyAgentTier2ReviewResponse;
const applyAgentTier2DiscardResponse = AgentEffectRouter.applyAgentTier2DiscardResponse;
const applyAgentSnapshotResponse = AgentEffectRouter.applyAgentSnapshotResponse;
const applyAgentStreamLine = AgentEffectRouter.applyAgentStreamLine;
const applyAgentStreamClosed = AgentEffectRouter.applyAgentStreamClosed;
const requestAgentComposerFocus = AgentEffectRouter.requestAgentComposerFocus;

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
    if (model.genui_webview_mounted and
        (model.isCapsule() or model.hasAgentEditor()) and
        count < out.len)
    {
        out[count] = .{
            .label = genui_view_label,
            .anchor = genui_view_anchor,
            .url = model.genUiWorkbenchUrl(),
            .reload_token = model.genui_source_revision,
        };
        count += 1;
    }
    return count;
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
        } else {
            try self.unmountGenUi(runtime);
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

    fn unmountGenUi(self: *DeferredWebViewApp, runtime: *native_sdk.Runtime) !void {
        if (!self.model.genui_webview_mounted) return;
        try runtime.closeView(self.primary_window_id, genui_view_label);
        self.model.genui_webview_mounted = false;
        if (self.focused_surface == .genui) self.focused_surface = .none;
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
