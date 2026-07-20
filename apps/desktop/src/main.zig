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

pub const panic = std.debug.FullPanic(native_sdk.debug.capturePanic);

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;

pub const canvas_label = "hyper-term-canvas";
pub const terminal_view_label = "hyper-term-terminal-view";
pub const terminal_view_anchor = "Terminal viewport";
pub const genui_view_label = "hyper-term-genui-view";
pub const genui_view_anchor = "Agent artifact editor viewport";
pub const terminal_gateway_origin = "http://127.0.0.1:47437";
pub const max_sessions: usize = 8;
const terminal_url_capacity: usize = 256;
const agent_url_capacity: usize = 256;
const genui_url_capacity: usize = 512;
const max_gateway_token_bytes: usize = 128;
const max_agent_provider_status_bytes: usize = 4 * 1024;
const terminal_close_url_capacity: usize = terminal_url_capacity + 64;
const agent_effect_url_capacity: usize = agent_url_capacity + 64;
pub const max_agent_blocks: usize = 128;
const max_agent_block_bytes: usize = 8 * 1024;
const max_agent_operation_id_bytes: usize = 36;
const max_agent_operation_kind_bytes: usize = 96;
const max_agent_activity_title_bytes: usize = 512;
const max_agent_activity_meta_bytes: usize = 512;
const max_agent_goal_step_columns: usize = 42;
const max_agent_error_bytes: usize = 512;
const max_agent_prompt_bytes: usize = 16 * 1024;
const max_agent_config_options: usize = 4;
const max_agent_config_choices: usize = 24;
const max_agent_commands: usize = 24;
const max_agent_tier2_results: usize = 4;
const max_agent_tier2_files: usize = 12;
const max_agent_tier2_path_bytes: usize = 256;
const max_agent_tier2_diff_bytes: usize = 6 * 1024;
const max_agent_capability_id_bytes: usize = 128;
const max_agent_capability_label_bytes: usize = 192;
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
pub const window_width: f32 = 1180;
pub const window_height: f32 = 760;
pub const window_min_width: f32 = 840;
pub const window_min_height: f32 = 520;
pub const titlebar_natural_height: f32 = 44;

const app_permissions = [_][]const u8{
    native_sdk.security.permission_command,
    native_sdk.security.permission_network,
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
    .{
        .label = terminal_view_label,
        .kind = .webview,
        .parent = canvas_label,
        .url = "zero://inline",
        .x = 0,
        .y = 0,
        .width = 1,
        .height = 1,
        .layer = 20,
    },
    .{
        .label = genui_view_label,
        .kind = .webview,
        .parent = canvas_label,
        .url = "zero://inline",
        .x = 0,
        .y = 0,
        .width = 1,
        .height = 1,
        .layer = 21,
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

pub const SessionMode = enum {
    terminal,
    agent,
    capsule,
};

pub const AgentProvider = enum {
    codex,
    codex_acp,
    claude_acp,
    copilot_acp,

    pub fn id(provider: AgentProvider) []const u8 {
        return switch (provider) {
            .codex => "codex",
            .codex_acp => "codex-acp",
            .claude_acp => "claude-acp",
            .copilot_acp => "copilot-acp",
        };
    }

    pub fn label(provider: AgentProvider) []const u8 {
        return switch (provider) {
            .codex => "Codex",
            .codex_acp => "Codex ACP",
            .claude_acp => "Claude ACP",
            .copilot_acp => "Copilot ACP",
        };
    }
};

pub const AgentProviderReadiness = enum {
    unavailable,
    authenticated,
    available,
    login_required,
    provider_missing,
    probe_failed,
};

pub const AgentConnection = enum {
    unavailable,
    connecting,
    ready,
    failed,
};

pub const AgentTurnStatus = enum {
    idle,
    ready,
    running,
    cancelling,
    completed,
    waiting_approval,
    failed,
};

pub const AgentMessageRole = enum { user, agent, system, thought };

pub const AgentBlockKind = enum { message, operation, approval, tool_call, plan };

pub const AgentToolStatus = enum { pending, in_progress, completed, failed };

pub const AgentRisk = enum {
    read_only,
    workspace_write,
    external_effect,
    destructive,
    unknown,
};

pub const AgentOperationState = enum {
    proposed,
    policy_check,
    waiting_human,
    authorized,
    dispatching,
    succeeded,
    failed,
    cancelled,
    unknown_execution,
};

pub const AgentDecision = enum { none, reject_once, cancelled, other };

pub const AgentTier2FileView = struct {
    path_storage: [max_agent_tier2_path_bytes]u8 = [_]u8{0} ** max_agent_tier2_path_bytes,
    path_len: usize = 0,
    kind_storage: [16]u8 = [_]u8{0} ** 16,
    kind_len: usize = 0,
    bytes: u64 = 0,

    pub fn path(file: *const AgentTier2FileView) []const u8 {
        return file.path_storage[0..file.path_len];
    }

    pub fn kind(file: *const AgentTier2FileView) []const u8 {
        return file.kind_storage[0..file.kind_len];
    }
};

pub const AgentTier2ResultView = struct {
    source_operation_id_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    source_operation_id_len: usize = 0,
    changed_bytes: u64 = 0,
    files: [max_agent_tier2_files]AgentTier2FileView = [_]AgentTier2FileView{.{}} ** max_agent_tier2_files,
    file_count: usize = 0,
    files_truncated: bool = false,
    has_acceptance: bool = false,
    acceptance_operation_id_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    acceptance_operation_id_len: usize = 0,
    acceptance_revision: u64 = 0,
    acceptance_state: AgentOperationState = .proposed,

    pub fn sourceOperationId(result: *const AgentTier2ResultView) []const u8 {
        return result.source_operation_id_storage[0..result.source_operation_id_len];
    }

    pub fn acceptanceOperationId(result: *const AgentTier2ResultView) []const u8 {
        return result.acceptance_operation_id_storage[0..result.acceptance_operation_id_len];
    }

    pub fn deletedFileCount(result: *const AgentTier2ResultView) usize {
        var count: usize = 0;
        for (result.files[0..result.file_count]) |*file| {
            if (std.mem.eql(u8, file.kind(), "deleted")) count += 1;
        }
        return count;
    }
};

pub const AgentBlockView = struct {
    id: u64 = 0,
    kind: AgentBlockKind = .message,
    role: AgentMessageRole = .agent,
    content_storage: [max_agent_block_bytes]u8 = [_]u8{0} ** max_agent_block_bytes,
    content_len: usize = 0,
    truncated: bool = false,
    operation_id_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    operation_id_len: usize = 0,
    operation_kind_storage: [max_agent_operation_kind_bytes]u8 = [_]u8{0} ** max_agent_operation_kind_bytes,
    operation_kind_len: usize = 0,
    title_storage: [max_agent_activity_title_bytes]u8 = [_]u8{0} ** max_agent_activity_title_bytes,
    title_len: usize = 0,
    meta_storage: [max_agent_activity_meta_bytes]u8 = [_]u8{0} ** max_agent_activity_meta_bytes,
    meta_len: usize = 0,
    expanded: bool = false,
    tool_status: AgentToolStatus = .pending,
    activity_count: u16 = 0,
    has_reasoning: bool = false,
    execute_count: u16 = 0,
    read_count: u16 = 0,
    edit_count: u16 = 0,
    other_tool_count: u16 = 0,
    diff_count: u16 = 0,
    terminal_count: u16 = 0,
    added_lines: u64 = 0,
    removed_lines: u64 = 0,
    operation_revision: u64 = 0,
    risk: AgentRisk = .unknown,
    state: AgentOperationState = .proposed,
    decision: AgentDecision = .none,

    pub fn content(block: *const AgentBlockView) []const u8 {
        return block.content_storage[0..block.content_len];
    }

    pub fn roleLabel(block: *const AgentBlockView) []const u8 {
        return switch (block.role) {
            .user => "You",
            .agent => "Agent",
            .system => "System",
            .thought => "Plan",
        };
    }

    pub fn isMessage(block: *const AgentBlockView) bool {
        return block.kind == .message;
    }

    pub fn isUserMessage(block: *const AgentBlockView) bool {
        return block.kind == .message and block.role == .user;
    }

    pub fn isThoughtMessage(block: *const AgentBlockView) bool {
        return block.kind == .message and block.role == .thought;
    }

    pub fn isSystemMessage(block: *const AgentBlockView) bool {
        return block.kind == .message and block.role == .system;
    }

    pub fn isOperation(block: *const AgentBlockView) bool {
        return block.kind == .operation;
    }

    pub fn isApproval(block: *const AgentBlockView) bool {
        return block.kind == .approval;
    }

    pub fn isActivity(block: *const AgentBlockView) bool {
        return block.kind == .tool_call or block.kind == .plan;
    }

    pub fn activityTitle(block: *const AgentBlockView) []const u8 {
        return block.title_storage[0..block.title_len];
    }

    pub fn activityMeta(block: *const AgentBlockView) []const u8 {
        return block.meta_storage[0..block.meta_len];
    }

    pub fn hasActivityDetails(block: *const AgentBlockView) bool {
        return block.content_len > 0;
    }

    pub fn isApprovalPending(block: *const AgentBlockView) bool {
        return block.kind == .approval and block.decision == .none;
    }

    pub fn canAllowOnce(block: *const AgentBlockView) bool {
        return block.isApprovalPending() and (block.isBrokeredMcpReview() or block.isWorkspaceReview());
    }

    pub fn isBrokeredMcpReview(block: *const AgentBlockView) bool {
        return block.risk == .read_only and
            std.mem.eql(u8, block.operationKindLabel(), "MCP tool");
    }

    pub fn isWorkspaceReview(block: *const AgentBlockView) bool {
        return block.risk == .workspace_write and
            std.mem.eql(u8, block.operationKindLabel(), "Workspace edit");
    }

    pub fn operationId(block: *const AgentBlockView) []const u8 {
        return block.operation_id_storage[0..block.operation_id_len];
    }

    pub fn operationKindLabel(block: *const AgentBlockView) []const u8 {
        return block.operation_kind_storage[0..block.operation_kind_len];
    }

    pub fn riskLabel(block: *const AgentBlockView) []const u8 {
        return switch (block.risk) {
            .read_only => "read only",
            .workspace_write => "workspace write",
            .external_effect => "external effect",
            .destructive => "destructive",
            .unknown => "unknown risk",
        };
    }

    pub fn stateLabel(block: *const AgentBlockView) []const u8 {
        return switch (block.state) {
            .proposed => "proposed",
            .policy_check => "policy check",
            .waiting_human => "waiting for you",
            .authorized => "authorized",
            .dispatching => "dispatching",
            .succeeded => "succeeded",
            .failed => "failed",
            .cancelled => "cancelled",
            .unknown_execution => "execution unknown",
        };
    }

    pub fn approvalTitle(block: *const AgentBlockView) []const u8 {
        return switch (block.decision) {
            .none => "Approval required",
            .reject_once => "Request rejected",
            .cancelled => "Request cancelled",
            .other => "Approval resolved",
        };
    }

    pub fn decisionLabel(block: *const AgentBlockView) []const u8 {
        return switch (block.decision) {
            .none => "pending",
            .reject_once => "rejected once",
            .cancelled => "cancelled",
            .other => "resolved",
        };
    }
};

pub const AgentConfigChoiceView = struct {
    action_id: u16 = 0,
    value_storage: [max_agent_capability_id_bytes]u8 = [_]u8{0} ** max_agent_capability_id_bytes,
    value_len: usize = 0,
    name_storage: [max_agent_capability_label_bytes]u8 = [_]u8{0} ** max_agent_capability_label_bytes,
    name_len: usize = 0,
    selected: bool = false,

    pub fn value(choice: *const AgentConfigChoiceView) []const u8 {
        return choice.value_storage[0..choice.value_len];
    }

    pub fn name(choice: *const AgentConfigChoiceView) []const u8 {
        return choice.name_storage[0..choice.name_len];
    }
};

pub const AgentConfigOptionView = struct {
    index: u8 = 0,
    id_storage: [max_agent_capability_id_bytes]u8 = [_]u8{0} ** max_agent_capability_id_bytes,
    id_len: usize = 0,
    name_storage: [max_agent_capability_label_bytes]u8 = [_]u8{0} ** max_agent_capability_label_bytes,
    name_len: usize = 0,
    current_storage: [max_agent_capability_label_bytes]u8 = [_]u8{0} ** max_agent_capability_label_bytes,
    current_len: usize = 0,
    picker_open: bool = false,
    is_boolean: bool = false,
    choices: [max_agent_config_choices]AgentConfigChoiceView = [_]AgentConfigChoiceView{.{}} ** max_agent_config_choices,
    choice_count: usize = 0,

    pub fn id(option: *const AgentConfigOptionView) []const u8 {
        return option.id_storage[0..option.id_len];
    }

    pub fn name(option: *const AgentConfigOptionView) []const u8 {
        return option.name_storage[0..option.name_len];
    }

    pub fn currentLabel(option: *const AgentConfigOptionView) []const u8 {
        return option.current_storage[0..option.current_len];
    }

    pub fn compactWidth(option: *const AgentConfigOptionView) f32 {
        const label = option.currentLabel();
        const codepoints = std.unicode.utf8CountCodepoints(label) catch label.len;
        const estimated = 48 + @as(f32, @floatFromInt(codepoints)) * 7;
        return std.math.clamp(estimated, 76, 128);
    }

    pub fn visibleChoices(option: *const AgentConfigOptionView) []const AgentConfigChoiceView {
        return option.choices[0..option.choice_count];
    }
};

pub const AgentCommandView = struct {
    index: u8 = 0,
    name_storage: [max_agent_capability_id_bytes]u8 = [_]u8{0} ** max_agent_capability_id_bytes,
    name_len: usize = 0,
    label_storage: [max_agent_capability_label_bytes]u8 = [_]u8{0} ** max_agent_capability_label_bytes,
    label_len: usize = 0,

    pub fn name(entry: *const AgentCommandView) []const u8 {
        return entry.name_storage[0..entry.name_len];
    }

    pub fn menuLabel(entry: *const AgentCommandView) []const u8 {
        return entry.label_storage[0..entry.label_len];
    }
};

pub const Session = struct {
    id: u8 = 0,
    mode: SessionMode = .terminal,
    title: []const u8 = "zsh",
    icon: []const u8 = "terminal",
    agent_provider: AgentProvider = .codex,
    agent_connection: AgentConnection = .unavailable,

    pub fn closeLabel(session: *const Session, arena: std.mem.Allocator) []const u8 {
        return std.fmt.allocPrint(arena, "Close {s} {d}", .{ session.title, session.id }) catch "Close tab";
    }

    pub fn tabGroupLabel(session: *const Session, arena: std.mem.Allocator) []const u8 {
        return std.fmt.allocPrint(arena, "{s} tab {d}", .{ session.title, session.id }) catch "Session tab";
    }
};

const PendingAgentPrompt = struct {
    storage: [max_agent_prompt_bytes]u8 = [_]u8{0} ** max_agent_prompt_bytes,
    len: usize = 0,

    fn set(pending: *PendingAgentPrompt, value: []const u8) void {
        const length = utf8BoundedLength(value, pending.storage.len);
        @memcpy(pending.storage[0..length], value[0..length]);
        pending.len = length;
    }

    fn text(pending: *const PendingAgentPrompt) []const u8 {
        return pending.storage[0..pending.len];
    }

    fn clear(pending: *PendingAgentPrompt) void {
        pending.len = 0;
    }
};

pub const Model = struct {
    system_scheme: canvas.ColorScheme = .dark,
    high_contrast: bool = false,
    reduce_motion: bool = false,
    chrome_leading: f32 = 0,
    chrome_trailing: f32 = 0,
    titlebar_height: f32 = titlebar_natural_height,
    agent_split: f32 = 0.64,
    session_slots: [max_sessions]Session = .{
        .{ .id = 1 }, .{}, .{}, .{}, .{}, .{}, .{}, .{},
    },
    session_count: usize = 1,
    active_session_id: u8 = 1,
    next_session_id: u8 = 2,
    agent_provider_picker_open: bool = false,
    agent_provider_refresh_in_flight: bool = false,
    selected_agent_provider: AgentProvider = .codex,
    available_agent_providers: u8 = 0,
    authenticated_agent_providers: u8 = 0,
    session_auth_agent_providers: u8 = 0,
    login_required_agent_providers: u8 = 0,
    provider_missing_agent_providers: u8 = 0,
    provider_probe_failed_agent_providers: u8 = 0,
    contained_agent_providers: u8 = 0,
    terminal_base_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_base_url_len: usize = 0,
    terminal_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_url_len: usize = 0,
    agent_base_url_storage: [agent_url_capacity]u8 = [_]u8{0} ** agent_url_capacity,
    agent_base_url_len: usize = 0,
    genui_workbench_url_storage: [genui_url_capacity]u8 = [_]u8{0} ** genui_url_capacity,
    genui_workbench_url_len: usize = 0,
    genui_artifact_id_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    genui_artifact_id_len: usize = 0,
    genui_source_revision: u64 = 0,
    agent_editor_open_session_id: u8 = 0,
    agent_composer_buffer: canvas.TextBuffer(max_agent_prompt_bytes) = .{},
    agent_pending_prompts: [max_sessions]PendingAgentPrompt = [_]PendingAgentPrompt{.{}} ** max_sessions,
    ui_font_registered: bool = false,
    agent_blocks: [max_agent_blocks]AgentBlockView = [_]AgentBlockView{.{}} ** max_agent_blocks,
    agent_block_count: usize = 0,
    agent_block_index_base: u64 = 0,
    agent_history_clipped: bool = false,
    agent_plan: AgentBlockView = .{},
    agent_plan_visible: bool = false,
    agent_projection_session_id: u8 = 0,
    agent_document_revision: u64 = 0,
    agent_stream_sequence: u64 = 0,
    agent_turn_status: AgentTurnStatus = .idle,
    agent_error_storage: [max_agent_error_bytes]u8 = [_]u8{0} ** max_agent_error_bytes,
    agent_error_len: usize = 0,
    agent_snapshot_in_flight_session_id: u8 = 0,
    agent_snapshot_resync_revision: u64 = 0,
    agent_stream_session_id: u8 = 0,
    agent_permission_in_flight_session_id: u8 = 0,
    agent_config_options: [max_agent_config_options]AgentConfigOptionView = [_]AgentConfigOptionView{.{}} ** max_agent_config_options,
    agent_config_option_count: usize = 0,
    agent_commands: [max_agent_commands]AgentCommandView = [_]AgentCommandView{.{}} ** max_agent_commands,
    agent_command_count: usize = 0,
    agent_config_in_flight_session_id: u8 = 0,
    agent_command_picker_open: bool = false,
    agent_tier2_results: [max_agent_tier2_results]AgentTier2ResultView = [_]AgentTier2ResultView{.{}} ** max_agent_tier2_results,
    agent_tier2_result_count: usize = 0,
    agent_tier2_projection_session_id: u8 = 0,
    agent_tier2_results_in_flight_session_id: u8 = 0,
    agent_tier2_action_in_flight_session_id: u8 = 0,
    agent_tier2_preview_source_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    agent_tier2_preview_source_len: usize = 0,
    agent_tier2_diff_storage: [max_agent_tier2_diff_bytes]u8 = [_]u8{0} ** max_agent_tier2_diff_bytes,
    agent_tier2_diff_len: usize = 0,
    agent_tier2_diff_truncated: bool = false,
    agent_tier2_preview_ready: bool = false,

    /// Read by update, token, and derived-binding code rather than bound
    /// directly by the declarative view.
    pub const view_unbound = .{
        "system_scheme",
        "high_contrast",
        "reduce_motion",
        "session_slots",
        "session_count",
        "next_session_id",
        "agentProviderUnavailable",
        "available_agent_providers",
        "authenticated_agent_providers",
        "session_auth_agent_providers",
        "login_required_agent_providers",
        "provider_missing_agent_providers",
        "provider_probe_failed_agent_providers",
        "contained_agent_providers",
        "terminal_base_url_storage",
        "terminal_base_url_len",
        "terminal_url_storage",
        "terminal_url_len",
        "agent_base_url_storage",
        "agent_base_url_len",
        "genui_workbench_url_storage",
        "genui_workbench_url_len",
        "genui_artifact_id_storage",
        "genui_artifact_id_len",
        "genui_source_revision",
        "agent_editor_open_session_id",
        "agent_composer_buffer",
        "agent_pending_prompts",
        "ui_font_registered",
        "agent_blocks",
        "agent_block_count",
        "agent_block_index_base",
        "agent_plan",
        "agent_plan_visible",
        "agent_projection_session_id",
        "agent_document_revision",
        "agent_stream_sequence",
        "agent_turn_status",
        "agent_error_storage",
        "agent_error_len",
        "agent_snapshot_in_flight_session_id",
        "agent_snapshot_resync_revision",
        "agent_stream_session_id",
        "agent_permission_in_flight_session_id",
        "agent_config_options",
        "agent_config_option_count",
        "agent_commands",
        "agent_config_in_flight_session_id",
        "agent_tier2_results",
        "agent_tier2_result_count",
        "agent_tier2_projection_session_id",
        "agent_tier2_results_in_flight_session_id",
        "agent_tier2_action_in_flight_session_id",
        "agent_tier2_preview_source_storage",
        "agent_tier2_preview_source_len",
        "agent_tier2_diff_storage",
        "agent_tier2_diff_len",
        "agent_tier2_diff_truncated",
        "agent_tier2_preview_ready",
        "terminalReady",
        "terminalUrl",
        "genUiWorkbenchUrl",
        "hasGenUiArtifact",
        "hasEditableAgentArtifact",
        "agentError",
        "agentTier2Results",
        "agentTier2Diff",
    };

    pub fn openSessions(model: *const Model) []const Session {
        return model.session_slots[0..model.session_count];
    }

    pub fn activeSession(model: *const Model) Session {
        for (model.openSessions()) |session| {
            if (session.id == model.active_session_id) return session;
        }
        return model.session_slots[0];
    }

    pub fn isTerminal(model: *const Model) bool {
        return model.activeSession().mode == .terminal;
    }

    pub fn isCapsule(model: *const Model) bool {
        return model.activeSession().mode == .capsule;
    }

    pub fn terminalReady(model: *const Model) bool {
        return model.terminal_url_len > 0;
    }

    pub fn terminalDisconnected(model: *const Model) bool {
        return !model.terminalReady();
    }

    pub fn terminalStatus(model: *const Model) []const u8 {
        return if (model.terminalReady())
            "zsh · ordered Rust PTY plane"
        else
            "zsh · hyperd disconnected";
    }

    pub fn terminalUrl(model: *const Model) []const u8 {
        return model.terminal_url_storage[0..model.terminal_url_len];
    }

    pub fn agentProviderUnavailable(model: *const Model) bool {
        return model.agent_base_url_len == 0 or model.available_agent_providers == 0;
    }

    pub fn agentProviderPickerUnavailable(model: *const Model) bool {
        return model.agent_base_url_len == 0;
    }

    pub fn agentProviderRefreshLabel(model: *const Model) []const u8 {
        return if (model.agent_provider_refresh_in_flight)
            "Checking providers…"
        else
            "Refresh providers";
    }

    pub fn codexProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.codex);
    }

    pub fn codexAcpProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.codex_acp);
    }

    pub fn claudeAcpProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.claude_acp);
    }

    pub fn copilotAcpProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.copilot_acp);
    }

    pub fn agentProviderAvailable(model: *const Model, provider: AgentProvider) bool {
        return model.available_agent_providers & providerBit(provider) != 0;
    }

    pub fn agentProviderReady(model: *const Model, provider: AgentProvider) bool {
        const bit = providerBit(provider);
        return (model.authenticated_agent_providers | model.session_auth_agent_providers) & bit != 0;
    }

    pub fn agentProviderReadiness(model: *const Model, provider: AgentProvider) AgentProviderReadiness {
        const bit = providerBit(provider);
        if (model.authenticated_agent_providers & bit != 0) return .authenticated;
        if (model.session_auth_agent_providers & bit != 0) return .available;
        if (model.login_required_agent_providers & bit != 0) return .login_required;
        if (model.provider_missing_agent_providers & bit != 0) return .provider_missing;
        if (model.provider_probe_failed_agent_providers & bit != 0) return .probe_failed;
        return .unavailable;
    }

    pub fn codexProviderDisabled(model: *const Model) bool {
        return !model.agentProviderReady(.codex);
    }

    pub fn codexAcpProviderDisabled(model: *const Model) bool {
        return !model.agentProviderReady(.codex_acp);
    }

    pub fn claudeAcpProviderDisabled(model: *const Model) bool {
        return !model.agentProviderReady(.claude_acp);
    }

    pub fn copilotAcpProviderDisabled(model: *const Model) bool {
        return !model.agentProviderReady(.copilot_acp);
    }

    pub fn codexProviderMenuLabel(model: *const Model) []const u8 {
        return providerMenuLabel(model, .codex);
    }

    pub fn codexAcpProviderMenuLabel(model: *const Model) []const u8 {
        return providerMenuLabel(model, .codex_acp);
    }

    pub fn claudeAcpProviderMenuLabel(model: *const Model) []const u8 {
        return providerMenuLabel(model, .claude_acp);
    }

    pub fn copilotAcpProviderMenuLabel(model: *const Model) []const u8 {
        return providerMenuLabel(model, .copilot_acp);
    }

    pub fn agentStatus(model: *const Model) []const u8 {
        const session = model.activeSession();
        if (session.mode == .agent and !model.agentProviderReady(session.agent_provider)) {
            return switch (model.agentProviderReadiness(session.agent_provider)) {
                .login_required => "Provider sign-in required · no command executed",
                .provider_missing => "Provider executable missing · no command executed",
                .probe_failed => "Provider readiness check failed · no command executed",
                .unavailable => "Agent unavailable · no command executed",
                .authenticated => unreachable,
                .available => unreachable,
            };
        }
        if (model.activeSession().agent_connection == .ready) {
            return switch (model.agent_turn_status) {
                .running => "Agent working",
                .cancelling => "Stopping Agent",
                .waiting_approval => "Needs approval",
                .failed => if (model.agent_error_len > 0) model.agentError() else "Agent turn failed",
                .completed => "Turn complete",
                else => "Agent ready",
            };
        }
        return switch (model.activeSession().agent_connection) {
            .unavailable => "Agent unavailable · no command executed",
            .connecting => "Agent connecting",
            .ready => "Agent ready",
            .failed => if (model.agent_error_len > 0) model.agentError() else "Agent start failed · no command executed",
        };
    }

    pub fn hasAgentStatusNotice(model: *const Model) bool {
        const session = model.activeSession();
        if (session.mode != .agent) return false;
        return !model.agentProviderReady(session.agent_provider) or
            session.agent_connection == .failed or
            model.agent_turn_status == .failed;
    }

    pub fn agentComposerHeight(model: *const Model) f32 {
        const text = model.agent_composer_buffer.text();
        var visual_lines: usize = 1;
        for (text) |byte| visual_lines += @intFromBool(byte == '\n');
        visual_lines += text.len / 96;
        const extra_lines = @min(visual_lines - 1, 4);
        return 66 + @as(f32, @floatFromInt(extra_lines)) * 18;
    }

    pub fn agentComposerText(model: *const Model) []const u8 {
        return model.agent_composer_buffer.text();
    }

    pub fn agentComposerInputDisabled(model: *const Model) bool {
        return model.activeSession().agent_connection != .ready or
            model.agent_base_url_len == 0;
    }

    pub fn agentSubmitDisabled(model: *const Model) bool {
        return model.agentComposerInputDisabled() or
            model.agent_turn_status == .running or
            model.agent_turn_status == .cancelling or
            model.agent_turn_status == .waiting_approval;
    }

    pub fn hasAgentComposerStatus(model: *const Model) bool {
        return model.activeSession().agent_connection == .connecting or switch (model.agent_turn_status) {
            .running, .cancelling, .waiting_approval => true,
            else => false,
        };
    }

    pub fn agentComposerStatus(model: *const Model) []const u8 {
        if (model.activeSession().agent_connection == .connecting) return "Connecting…";
        return switch (model.agent_turn_status) {
            .running => "Working",
            .cancelling => "Stopping…",
            .waiting_approval => "Needs approval",
            else => "",
        };
    }

    pub fn hasAgentStopControl(model: *const Model) bool {
        return switch (model.agent_turn_status) {
            .running, .cancelling, .waiting_approval => true,
            else => false,
        };
    }

    pub fn agentCancelDisabled(model: *const Model) bool {
        return model.agent_turn_status == .cancelling or
            model.activeSession().agent_connection != .ready;
    }

    pub fn agentBlocks(model: *const Model) []const AgentBlockView {
        return model.agent_blocks[0..model.agent_block_count];
    }

    pub fn agentPlan(model: *const Model) ?*const AgentBlockView {
        return if (model.agent_plan_visible) &model.agent_plan else null;
    }

    pub fn hasAgentBlocks(model: *const Model) bool {
        return model.agent_block_count > 0;
    }

    pub fn agentPermissionBusy(model: *const Model) bool {
        return model.agent_permission_in_flight_session_id != 0;
    }

    pub fn agentConfigOptions(model: *const Model) []const AgentConfigOptionView {
        return model.agent_config_options[0..model.agent_config_option_count];
    }

    pub fn agentCommands(model: *const Model) []const AgentCommandView {
        return model.agent_commands[0..model.agent_command_count];
    }

    pub fn agentTier2Results(model: *const Model) []const AgentTier2ResultView {
        return model.agent_tier2_results[0..model.agent_tier2_result_count];
    }

    pub fn agentTier2Diff(model: *const Model) []const u8 {
        return model.agent_tier2_diff_storage[0..model.agent_tier2_diff_len];
    }

    pub fn openAgentConfigChoices(model: *const Model) []const AgentConfigChoiceView {
        for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
            if (option.picker_open) return option.choices[0..option.choice_count];
        }
        return &.{};
    }

    pub fn agentCapabilityBusy(model: *const Model) bool {
        return model.agent_config_in_flight_session_id != 0 or model.agentSubmitDisabled();
    }

    pub fn agentError(model: *const Model) []const u8 {
        return model.agent_error_storage[0..model.agent_error_len];
    }

    pub fn hasGenUiArtifact(model: *const Model) bool {
        return model.activeSession().mode == .agent and model.genui_workbench_url_len > 0;
    }

    pub fn hasEditableAgentArtifact(model: *const Model) bool {
        if (!model.hasGenUiArtifact()) return false;
        return switch (model.activeSession().agent_provider) {
            .codex => false,
            .codex_acp, .claude_acp, .copilot_acp => true,
        };
    }

    pub fn canOpenAgentEditor(model: *const Model) bool {
        return model.hasEditableAgentArtifact() and !model.hasAgentEditor();
    }

    pub fn hasAgentEditor(model: *const Model) bool {
        return model.hasEditableAgentArtifact() and
            model.agent_editor_open_session_id == model.active_session_id;
    }

    pub fn genUiArtifactLabel(model: *const Model) []const u8 {
        return model.genui_artifact_id_storage[0..@min(model.genui_artifact_id_len, 8)];
    }

    pub fn genUiWorkbenchUrl(model: *const Model) []const u8 {
        return model.genui_workbench_url_storage[0..model.genui_workbench_url_len];
    }
};

pub const Msg = union(enum) {
    choose_terminal,
    choose_agent,
    toggle_agent_provider_picker,
    dismiss_agent_provider_picker,
    refresh_agent_providers,
    agent_providers_refreshed: native_sdk.EffectResponse,
    choose_codex_agent,
    choose_codex_acp_agent,
    choose_claude_acp_agent,
    choose_copilot_acp_agent,
    select_session: u8,
    close_session: u8,
    close_active_session,
    terminal_session_closed: native_sdk.EffectResponse,
    agent_session_started: native_sdk.EffectResponse,
    agent_session_closed: native_sdk.EffectResponse,
    agent_composer_changed: canvas.TextInputEvent,
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
    pub const view_unbound = .{ "close_active_session", "terminal_session_closed", "agent_providers_refreshed", "agent_session_started", "agent_session_closed", "agent_turn_started", "agent_turn_cancelled", "agent_snapshot_received", "agent_stream_line", "agent_stream_closed", "agent_config_updated", "agent_permission_decided", "agent_poll", "agent_tier2_results_received", "preview_agent_tier2_result", "agent_tier2_preview_received", "request_agent_tier2_review", "agent_tier2_review_requested", "discard_agent_tier2_result", "agent_tier2_result_discarded", "system_appearance", "chrome_changed" };
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
            _ = appendSession(model, .terminal);
        },
        .choose_agent => createAgentSession(model, model.selected_agent_provider, fx),
        .toggle_agent_provider_picker => toggleAgentProviderPicker(model, fx),
        .dismiss_agent_provider_picker => model.agent_provider_picker_open = false,
        .refresh_agent_providers => requestAgentProviderRefresh(model, fx),
        .agent_providers_refreshed => |response| applyAgentProviderRefresh(model, response),
        .choose_codex_agent => createAgentSession(model, .codex, fx),
        .choose_codex_acp_agent => createAgentSession(model, .codex_acp, fx),
        .choose_claude_acp_agent => createAgentSession(model, .claude_acp, fx),
        .choose_copilot_acp_agent => createAgentSession(model, .copilot_acp, fx),
        .select_session => |session_id| {
            const previous = model.active_session_id;
            selectSession(model, session_id);
            if (previous != model.active_session_id) {
                cancelAgentStream(model, previous, fx);
                resetAgentProjection(model, model.active_session_id);
                requestActiveAgentStream(model, fx);
            }
        },
        .close_session => |session_id| closeSession(model, session_id, fx),
        .close_active_session => closeSession(model, model.active_session_id, fx),
        .terminal_session_closed => {},
        .agent_session_started => |response| applyAgentStartResponse(model, response, fx),
        .agent_session_closed => {},
        .agent_composer_changed => |edit| model.agent_composer_buffer.apply(edit),
        .send_agent_prompt => requestAgentTurn(model, fx),
        .agent_turn_started => |response| applyAgentTurnResponse(model, response, fx),
        .cancel_agent_turn => requestAgentCancel(model, fx),
        .agent_turn_cancelled => |response| applyAgentCancelResponse(model, response),
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
                    (block.isActivity() or block.isThoughtMessage() or block.isSystemMessage()))
                {
                    block.expanded = !block.expanded;
                    break;
                }
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
    const session_id = model.next_session_id;
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
    }
}

fn closeSession(model: *Model, session_id: u8, fx: *Effects) void {
    var closing_index: ?usize = null;
    for (model.openSessions(), 0..) |session, index| {
        if (session.id == session_id) {
            closing_index = index;
            break;
        }
    }
    const index = closing_index orelse return;
    const session = model.session_slots[index];
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
        model.agent_pending_prompts[cursor] = model.agent_pending_prompts[cursor + 1];
    }
    model.session_count -= 1;
    model.session_slots[model.session_count] = .{};
    model.agent_pending_prompts[model.session_count] = .{};
    refreshTerminalUrl(model);
    if (was_active) {
        resetAgentProjection(model, model.active_session_id);
        requestActiveAgentStream(model, fx);
    }
}

fn toggleAgentProviderPicker(model: *Model, fx: *Effects) void {
    model.agent_provider_picker_open = !model.agent_provider_picker_open;
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
    const ready = model.authenticated_agent_providers | model.session_auth_agent_providers;
    if (!model.agentProviderReady(model.selected_agent_provider)) {
        model.selected_agent_provider = firstAvailableAgentProvider(ready) orelse
            firstAvailableAgentProvider(model.available_agent_providers) orelse .codex;
    }
}

fn requestAgentStart(model: *Model, session_id: u8, fx: *Effects) void {
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
        .timeout_ms = 12_000,
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
            requestAgentSnapshot(model, session_id, fx);
            requestAgentStream(model, session_id, fx);
        } else {
            setAgentError(model, agentStartFailureMessage(response));
        }
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
    model.agent_turn_status = .running;
    model.agent_error_len = 0;
}

fn applyAgentTurnResponse(model: *Model, response: native_sdk.EffectResponse, _: *Effects) void {
    const session_id = effectSessionId(response.key, agent_turn_effect_key_base) orelse return;
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent turn could not be started");
        restorePendingAgentPrompt(model, session_id);
        return;
    }
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

fn applyAgentCancelResponse(model: *Model, response: native_sdk.EffectResponse) void {
    const session_id = effectSessionId(response.key, agent_cancel_effect_key_base) orelse return;
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent turn could not be stopped safely");
    }
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

const AgentSnapshotWire = struct {
    session_id: ?u8 = null,
    status: []const u8,
    @"error": ?[]const u8 = null,
    capabilities: AgentCapabilitiesWire = .{},
    document: struct {
        revision: u64 = 0,
        blocks: []const AgentBlockWire,
    },
};

const AgentConfigChoiceWire = struct {
    value: []const u8,
    name: []const u8,
};

const AgentConfigOptionWire = struct {
    id: []const u8,
    name: []const u8,
    kind: std.json.Value,
    choices: []const AgentConfigChoiceWire = &.{},
};

const AgentCommandWire = struct {
    name: []const u8,
    description: ?[]const u8 = null,
};

const AgentCapabilitiesWire = struct {
    config_options: []const AgentConfigOptionWire = &.{},
    available_commands: []const AgentCommandWire = &.{},
};

const AgentPatchOperationWire = struct {
    type: []const u8,
    block_id: ?[]const u8 = null,
    text: ?[]const u8 = null,
};

const AgentPatchWire = struct {
    stream_sequence: u64,
    base_revision: u64,
    target_revision: u64,
    operations: []const AgentPatchOperationWire,
};

const AgentStreamFrameWire = struct {
    type: []const u8,
    status: ?[]const u8 = null,
    @"error": ?[]const u8 = null,
    capabilities: AgentCapabilitiesWire = .{},
    patch: ?AgentPatchWire = null,
    target_revision: ?u64 = null,
    reason: ?[]const u8 = null,
};

const AgentCapabilitiesResponseWire = struct {
    session_id: u8,
    capabilities: AgentCapabilitiesWire,
};

const AgentTier2FileWire = struct {
    kind: []const u8,
    path: []const u8,
    bytes: u64,
};

const AgentTier2AcceptanceWire = struct {
    operation_id: []const u8,
    operation_revision: u64,
    state: []const u8,
};

const AgentTier2ResultWire = struct {
    source_operation_id: []const u8,
    changed_bytes: u64,
    changed_files: []const AgentTier2FileWire,
    acceptance: ?AgentTier2AcceptanceWire = null,
};

const AgentTier2ResultsWire = struct {
    results: []const AgentTier2ResultWire,
};

const AgentTier2PreviewHunkWire = struct {
    patch: []const u8,
    truncated: bool = false,
};

const AgentTier2PreviewChangeWire = struct {
    target_path: []const u8,
    deleted: bool = false,
    binary: bool = false,
    base_bytes: u64 = 0,
    proposed_bytes: u64 = 0,
    proposed_digest: []const u8 = "",
    hunks: []const AgentTier2PreviewHunkWire,
    truncated: bool = false,
};

const AgentTier2PreviewWire = struct {
    source_operation_id: []const u8,
    changes: []const AgentTier2PreviewChangeWire,
    truncated: bool = false,
};

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

const AgentToolContentWire = struct {
    type: []const u8,
    text: ?[]const u8 = null,
    path: ?[]const u8 = null,
    patch: ?[]const u8 = null,
    added_lines: ?u32 = null,
    removed_lines: ?u32 = null,
    terminal_id: ?[]const u8 = null,
    kind: ?[]const u8 = null,
    mime_type: ?[]const u8 = null,
    uri: ?[]const u8 = null,
    name: ?[]const u8 = null,
    byte_count: ?u64 = null,
    encoded_bytes: ?u64 = null,
};

const AgentToolLocationWire = struct {
    path: []const u8,
    line: ?u32 = null,
};

const AgentToolCallWire = struct {
    tool_call_id: []const u8,
    title: []const u8,
    kind: []const u8,
    status: []const u8,
    content: []const AgentToolContentWire,
    locations: []const AgentToolLocationWire,
    raw_input: ?[]const u8 = null,
    raw_output: ?[]const u8 = null,
};

const AgentPlanEntryWire = struct {
    content: []const u8,
    priority: []const u8,
    status: []const u8,
};

const AgentBlockWire = struct {
    block_id: ?[]const u8 = null,
    block_revision: u64 = 0,
    kind: []const u8,
    trust_class: ?[]const u8 = null,
    payload: struct {
        type: []const u8,
        role: ?[]const u8 = null,
        text: ?[]const u8 = null,
        operation_id: ?[]const u8 = null,
        operation_revision: ?u64 = null,
        kind: ?std.json.Value = null,
        summary: ?[]const u8 = null,
        risk: ?[]const u8 = null,
        state: ?[]const u8 = null,
        prompt: ?[]const u8 = null,
        decision: ?[]const u8 = null,
        artifact: ?struct {
            artifact_id: []const u8,
            source_revision: u64,
            entrypoint: []const u8,
            content_digest: []const u8,
            compiler: struct {
                name: []const u8,
                version: []const u8,
            },
        } = null,
        call: ?AgentToolCallWire = null,
        entries: ?[]const AgentPlanEntryWire = null,
    },
};

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
    if (model.agent_stream_session_id == 0) scheduleAgentStreamRetry(session_id, fx);
}

fn applyAgentStreamLine(model: *Model, line: native_sdk.EffectLine, fx: *Effects) void {
    const session_id = effectSessionId(line.key, agent_stream_effect_key_base) orelse return;
    if (session_id != model.active_session_id or model.agent_stream_session_id != session_id) return;
    if (line.truncated or line.dropped_before != 0) {
        setAgentError(model, "Agent live updates exceeded the bounded stream");
        cancelAgentStream(model, session_id, fx);
        scheduleAgentStreamRetry(session_id, fx);
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
        projectAgentCapabilities(model, frame.capabilities);
        if (frame.status) |status| model.agent_turn_status = parseAgentTurnStatus(status);
        if (frame.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
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
    scheduleAgentStreamRetry(session_id, fx);
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
    projectAgentCapabilities(model, parsed.value.capabilities);
    projectAgentBlocks(model, parsed.value.document.blocks);
    model.agent_document_revision = parsed.value.document.revision;
    model.agent_stream_sequence = parsed.value.document.revision;
    model.agent_turn_status = parseAgentTurnStatus(parsed.value.status);
    if (parsed.value.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
    reconcilePendingAgentPrompt(model, session_id);
    return true;
}

fn projectAgentCapabilities(model: *Model, capabilities: AgentCapabilitiesWire) void {
    for (&model.agent_config_options) |*option| option.* = .{};
    for (&model.agent_commands) |*entry| entry.* = .{};
    model.agent_config_option_count = 0;
    model.agent_command_count = 0;
    var next_action_id: u16 = 1;
    for (capabilities.config_options) |wire| {
        if (model.agent_config_option_count == max_agent_config_options) break;
        if (wire.id.len == 0 or wire.id.len > max_agent_capability_id_bytes or
            wire.name.len == 0 or wire.name.len > max_agent_capability_label_bytes) continue;
        const kind = switch (wire.kind) {
            .object => |object| object,
            else => continue,
        };
        const type_value = kind.get("type") orelse continue;
        const type_name = switch (type_value) {
            .string => |value| value,
            else => continue,
        };
        const option = &model.agent_config_options[model.agent_config_option_count];
        option.index = @intCast(model.agent_config_option_count);
        copyCapabilityText(&option.id_storage, &option.id_len, wire.id);
        copyCapabilityText(&option.name_storage, &option.name_len, wire.name);
        if (std.mem.eql(u8, type_name, "select")) {
            const current_value = kind.get("current_value") orelse continue;
            const current = switch (current_value) {
                .string => |value| value,
                else => continue,
            };
            for (wire.choices) |choice| {
                if (option.choice_count == max_agent_config_choices) break;
                if (choice.value.len == 0 or choice.value.len > max_agent_capability_id_bytes or
                    choice.name.len == 0 or choice.name.len > max_agent_capability_label_bytes) continue;
                const projected = &option.choices[option.choice_count];
                projected.action_id = next_action_id;
                next_action_id +%= 1;
                copyCapabilityText(&projected.value_storage, &projected.value_len, choice.value);
                copyCapabilityText(&projected.name_storage, &projected.name_len, choice.name);
                projected.selected = std.mem.eql(u8, choice.value, current);
                if (projected.selected) {
                    copyCapabilityText(&option.current_storage, &option.current_len, choice.name);
                }
                option.choice_count += 1;
            }
            if (option.choice_count == 0) continue;
            if (option.current_len == 0) {
                copyCapabilityText(&option.current_storage, &option.current_len, current);
            }
        } else if (std.mem.eql(u8, type_name, "boolean")) {
            const current_value = kind.get("current_value") orelse continue;
            const current = switch (current_value) {
                .bool => |value| value,
                else => continue,
            };
            option.is_boolean = true;
            const values = [_]struct { value: []const u8, name: []const u8, selected: bool }{
                .{ .value = "true", .name = "On", .selected = current },
                .{ .value = "false", .name = "Off", .selected = !current },
            };
            for (values) |value| {
                const projected = &option.choices[option.choice_count];
                projected.action_id = next_action_id;
                next_action_id +%= 1;
                copyCapabilityText(&projected.value_storage, &projected.value_len, value.value);
                copyCapabilityText(&projected.name_storage, &projected.name_len, value.name);
                projected.selected = value.selected;
                if (value.selected) {
                    copyCapabilityText(&option.current_storage, &option.current_len, value.name);
                }
                option.choice_count += 1;
            }
        } else continue;
        model.agent_config_option_count += 1;
    }
    for (capabilities.available_commands) |wire| {
        if (model.agent_command_count == max_agent_commands) break;
        if (wire.name.len == 0 or wire.name.len > max_agent_capability_id_bytes) continue;
        const entry = &model.agent_commands[model.agent_command_count];
        entry.index = @intCast(model.agent_command_count);
        copyCapabilityText(&entry.name_storage, &entry.name_len, wire.name);
        if (std.mem.startsWith(u8, wire.name, "$")) {
            copyCapabilityText(&entry.label_storage, &entry.label_len, "Skill · ");
            appendCapabilityText(&entry.label_storage, &entry.label_len, wire.name[1..]);
        } else if (std.mem.eql(u8, wire.name, "skills")) {
            copyCapabilityText(&entry.label_storage, &entry.label_len, "Skills");
        } else {
            copyCapabilityText(&entry.label_storage, &entry.label_len, "/");
            appendCapabilityText(&entry.label_storage, &entry.label_len, wire.name);
        }
        if (wire.description) |description| {
            appendCapabilityText(&entry.label_storage, &entry.label_len, " · ");
            appendCapabilityText(&entry.label_storage, &entry.label_len, description);
        }
        model.agent_command_count += 1;
    }
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
        // compact Goal disclosure and are not a user-facing status. Keep the
        // native projection focused on completion and the current step.
        appendActivityFmt(view, "- [{s}] {s}\n", .{ marker, entry.content });
    }
    var meta: [max_agent_activity_meta_bytes]u8 = undefined;
    const rendered = std.fmt.bufPrint(&meta, "{d} / {d}", .{ completed, entries.len }) catch "Goal";
    copyActivityMeta(view, rendered);
    if (completed == entries.len) {
        copyActivityTitle(view, "Goal complete");
    } else {
        copyActivityTitle(view, "Goal · ");
        const step = active_step orelse next_step orelse entries[0].content;
        const step_length = utf8DisplayColumnPrefixLength(step, max_agent_goal_step_columns);
        appendActivityTitle(view, step[0..step_length]);
        if (step_length < step.len) appendActivityTitle(view, "…");
    }
    view.expanded = false;
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
        if (candidate.payload.state) |state| view.state = parseAgentOperationState(state);
        if (view.isWorkspaceReview() and candidate.payload.summary != null) {
            copyAgentBlockContent(view, candidate.payload.summary.?);
        }
        break;
    }
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
    if (std.mem.eql(u8, value, "ready")) return .ready;
    if (std.mem.eql(u8, value, "running")) return .running;
    if (std.mem.eql(u8, value, "cancelling")) return .cancelling;
    if (std.mem.eql(u8, value, "completed")) return .completed;
    if (std.mem.eql(u8, value, "waiting_approval")) return .waiting_approval;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    return .idle;
}

fn scheduleAgentStreamRetry(session_id: u8, fx: *Effects) void {
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
    model.agent_plan = .{};
    model.agent_plan_visible = false;
    model.agent_projection_session_id = session_id;
    model.agent_document_revision = 0;
    model.agent_stream_sequence = 0;
    model.agent_turn_status = .idle;
    model.agent_error_len = 0;
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
pub const CompiledHyperTermView = canvas.CompiledMarkupView(Model, Msg, app_markup);
pub const hyper_term_fragments = [_]canvas.MarkupFragment{
    CompiledHyperTermView.fragment("src/app.native"),
};
const AgentMarkdown = native_sdk.markdown.Markdown(Msg);

const agent_timeline_id = "agent-blocks";
const agent_timeline_estimated_width: usize = 84;
const agent_timeline_line_height: f32 = 19;
const agent_timeline_viewport_fallback: f32 = 480;
fn agentBlockExtentEstimate(context: ?*const anyopaque, logical_index: u64) f32 {
    const pointer = context orelse return 36;
    const model: *const Model = @ptrCast(@alignCast(pointer));
    if (logical_index < model.agent_block_index_base) return 36;
    const physical_u64 = logical_index - model.agent_block_index_base;
    if (physical_u64 >= model.agent_block_count) return 36;
    const block = &model.agent_blocks[@intCast(physical_u64)];
    const lines = @max(@as(usize, 1), (block.content_len + agent_timeline_estimated_width - 1) / agent_timeline_estimated_width);
    const text_extent = @as(f32, @floatFromInt(@min(lines, 96))) * agent_timeline_line_height;
    return switch (block.kind) {
        .message => if (block.role == .system and !block.expanded)
            28
        else if (block.role == .user)
            24 + text_extent
        else
            10 + text_extent,
        .tool_call, .plan => if (block.expanded) 42 + text_extent else 30,
        .operation => 36 + @min(text_extent, agent_timeline_line_height),
        .approval => 118 + @min(text_extent, agent_timeline_line_height * 5),
    };
}

pub fn agentTimelineOptions(model: *const Model) HyperTermUi.VirtualListOptions {
    return .{
        .id = agent_timeline_id,
        .item_count = model.agent_block_count,
        .index_base = model.agent_block_index_base,
        .item_extent = 36,
        .extent_estimate = agentBlockExtentEstimate,
        .extent_context = model,
        .gap = 2,
        .overscan = 4,
        .grow = 1,
        .viewport_fallback = agent_timeline_viewport_fallback,
        .anchor = .trailing,
        .semantics = .{ .label = "Agent blocks" },
    };
}

fn agentTimeline(ui: *HyperTermUi, model: *const Model) HyperTermUi.Node {
    const options = agentTimelineOptions(model);
    const window = ui.virtualWindow(options);
    const rows = ui.arena.alloc(HyperTermUi.Node, window.itemCount()) catch {
        ui.failed = true;
        return ui.column(.{ .grow = 1 }, .{});
    };
    for (rows, 0..) |*row, offset| {
        const physical = window.start_index + offset;
        var node = agentBlockNode(ui, model, &model.agent_blocks[physical]);
        node.key = .{ .int = model.agent_blocks[physical].id };
        row.* = node;
    }
    const timeline = ui.virtualList(options, window, .{rows});
    const transcript = if (!model.agent_history_clipped)
        timeline
    else
        ui.column(.{ .grow = 1 }, .{
            ui.text(.{ .padding = 6, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("Older activity is compacted · showing the latest {d} blocks", .{max_agent_blocks})),
            timeline,
        });
    const content = if (model.agent_tier2_result_count == 0 and !model.agent_plan_visible)
        transcript
    else blk: {
        break :blk ui.column(.{ .grow = 1 }, .{
            transcript,
            if (model.agent_tier2_result_count > 0)
                agentTier2ResultsNode(ui, model)
            else
                ui.el(.stack, .{}, .{}),
            if (model.agent_plan_visible)
                ui.row(.{ .gap = 4, .padding = 4, .cross = .center }, .{
                    agentGoalNode(ui, &model.agent_plan),
                })
            else
                ui.el(.stack, .{}, .{}),
        });
    };
    if (model.hasAgentEditor()) return content;
    return ui.column(.{
        .grow = 1,
        .padding = 10,
        .semantics = .{ .label = "Agent reading rail" },
    }, .{content});
}

fn agentTier2ResultsNode(ui: *HyperTermUi, model: *const Model) HyperTermUi.Node {
    const results = model.agentTier2Results();
    const nodes = ui.arena.alloc(HyperTermUi.Node, results.len) catch {
        ui.failed = true;
        return ui.column(.{}, .{});
    };
    for (results, nodes) |*result, *node| {
        node.* = agentTier2ResultNode(ui, model, result);
        node.key = .{ .str = result.sourceOperationId() };
    }
    return ui.column(.{ .gap = 4, .padding = 4, .semantics = .{ .label = "Tier 2 review results" } }, .{nodes});
}

fn agentTier2ResultNode(
    ui: *HyperTermUi,
    model: *const Model,
    result: *const AgentTier2ResultView,
) HyperTermUi.Node {
    const preview_selected = std.mem.eql(
        u8,
        result.sourceOperationId(),
        model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len],
    );
    const preview_ready = preview_selected and model.agent_tier2_preview_ready;
    const busy = model.agent_tier2_action_in_flight_session_id != 0;
    const first_file = if (result.file_count > 0) result.files[0].path() else "No accepted text files";
    const deleted_files = result.deletedFileCount();
    const first_file_deleted = result.file_count > 0 and std.mem.eql(u8, result.files[0].kind(), "deleted");
    const more_files = if (result.file_count > 1)
        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("+{d} more", .{result.file_count - 1}))
    else
        ui.el(.stack, .{}, .{});
    const preview = if (!preview_ready)
        ui.el(.stack, .{}, .{})
    else
        ui.column(.{ .gap = 5 }, .{
            ui.scroll(.{
                .height = 180,
                .semantics = .{ .label = "Rust-verified Tier 2 Diff" },
                .style_tokens = .{ .background = .surface_subtle, .radius = .md },
            }, if (model.agent_tier2_diff_len == 0)
                ui.text(.{ .padding = 7, .style_tokens = .{ .foreground = .text_muted } }, "No textual Diff was produced.")
            else
                ui.paragraph(.{ .padding = 7, .wrap = true }, &.{.{
                    .text = model.agentTier2Diff(),
                    .monospace = true,
                }})),
            if (model.agent_tier2_diff_truncated)
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Diff preview clipped to the bounded desktop budget.")
            else
                ui.el(.stack, .{}, .{}),
            if (!result.has_acceptance)
                ui.row(.{ .gap = 6, .cross = .center }, .{
                    ui.text(.{ .grow = 1, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, "Preview only · no workspace permission created"),
                    ui.button(.{
                        .size = .sm,
                        .variant = .primary,
                        .disabled = busy,
                        .on_press = Msg{ .request_agent_tier2_review = result.sourceOperationId() },
                    }, "Request apply approval"),
                })
            else
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "WorkspaceWrite approval is waiting in the transcript."),
        });
    return ui.el(.card, .{ .style_tokens = .{ .border_color = if (result.has_acceptance) .warning else .border } }, .{
        ui.column(.{ .gap = 5, .padding = 7 }, .{
            ui.row(.{ .gap = 6, .cross = .center }, .{
                ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .info } }, "edit"),
                ui.text(.{ .grow = 1 }, "Tier 2 changes retained for review"),
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, if (deleted_files == 0)
                    ui.fmt("{d} files · {d} bytes", .{ result.file_count, result.changed_bytes })
                else
                    ui.fmt("{d} files · {d} deleted · {d} bytes", .{ result.file_count, deleted_files, result.changed_bytes })),
                ui.el(.badge, .{ .variant = .secondary, .text = if (result.has_acceptance) "approval pending" else "not applied" }, .{}),
            }),
            ui.row(.{ .gap = 6, .cross = .center }, .{
                ui.text(.{ .grow = 1, .size = .sm }, first_file),
                if (first_file_deleted)
                    ui.el(.badge, .{ .variant = .secondary, .text = "delete" }, .{})
                else
                    ui.el(.stack, .{}, .{}),
                more_files,
                if (!result.has_acceptance)
                    ui.button(.{
                        .size = .sm,
                        .variant = .ghost,
                        .disabled = busy,
                        .on_press = Msg{ .discard_agent_tier2_result = result.sourceOperationId() },
                    }, "Discard")
                else
                    ui.el(.stack, .{}, .{}),
                ui.button(.{
                    .size = .sm,
                    .variant = .outline,
                    .disabled = busy,
                    .on_press = Msg{ .preview_agent_tier2_result = result.sourceOperationId() },
                }, if (preview_ready) "Hide Diff" else if (preview_selected) "Loading Diff" else "Review Diff"),
            }),
            preview,
        }),
    });
}

fn agentBlockNode(ui: *HyperTermUi, model: *const Model, block: *const AgentBlockView) HyperTermUi.Node {
    return switch (block.kind) {
        .message => agentMessageNode(ui, model, block),
        .tool_call, .plan => agentActivityNode(ui, block),
        .operation => agentOperationNode(ui, block),
        .approval => agentApprovalNode(ui, model, block),
    };
}

fn agentMessageNode(ui: *HyperTermUi, model: *const Model, block: *const AgentBlockView) HyperTermUi.Node {
    const clipped = if (block.truncated)
        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, if (block.isUserMessage()) "Block clipped to 8 KiB in this view." else "Response clipped to 8 KiB in this view.")
    else
        ui.el(.stack, .{}, .{});
    if (block.isUserMessage()) {
        return ui.row(.{ .padding = 1 }, .{
            ui.spacer(1),
            ui.el(.bubble, .{}, .{
                ui.column(.{ .gap = 3, .padding = 6 }, .{
                    ui.text(.{ .wrap = true }, block.content()),
                    clipped,
                }),
            }),
        });
    }
    if (block.isSystemMessage()) {
        return ui.column(.{ .grow = 1 }, .{
            ui.row(.{ .gap = 4, .padding = 1, .cross = .center }, .{
                ui.button(.{
                    .size = .sm,
                    .variant = .ghost,
                    .icon = if (block.expanded) "chevron-down" else "chevron-right",
                    .on_press = Msg{ .toggle_agent_block = block.id },
                }, "Session notice"),
            }),
            if (block.expanded)
                ui.column(.{ .gap = 4, .padding = 6 }, .{
                    AgentMarkdown.view(ui, block.content(), .{}),
                    clipped,
                })
            else
                ui.el(.stack, .{}, .{}),
        });
    }
    if (block.isThoughtMessage()) {
        const label = switch (model.agent_turn_status) {
            .running, .waiting_approval => "Reasoning",
            else => "Processed",
        };
        return ui.column(.{ .grow = 1 }, .{
            ui.row(.{ .gap = 4, .padding = 2, .cross = .center }, .{
                ui.button(.{
                    .size = .sm,
                    .variant = .ghost,
                    .icon = if (block.expanded) "chevron-down" else "chevron-right",
                    .on_press = Msg{ .toggle_agent_block = block.id },
                }, label),
            }),
            if (block.expanded)
                ui.column(.{ .gap = 4, .padding = 7 }, .{
                    AgentMarkdown.view(ui, block.content(), .{}),
                    clipped,
                })
            else
                ui.el(.stack, .{}, .{}),
        });
    }
    return ui.column(.{ .gap = 3, .padding = 1 }, .{
        ui.column(.{ .padding = 1 }, .{AgentMarkdown.view(ui, block.content(), .{})}),
        clipped,
    });
}

fn agentGoalNode(ui: *HyperTermUi, block: *const AgentBlockView) HyperTermUi.Node {
    return ui.column(.{ .grow = 1, .semantics = .{ .label = "Active Agent goal" } }, .{
        ui.el(.bubble, .{}, .{
            ui.row(.{ .gap = 5, .padding = 3, .cross = .center }, .{
                ui.icon(.{ .width = 12, .height = 12, .style_tokens = .{ .foreground = .accent } }, "circle-dot"),
                ui.button(.{
                    .size = .sm,
                    .variant = .ghost,
                    .icon = if (block.expanded) "chevron-down" else "chevron-right",
                    .on_press = Msg{ .toggle_agent_block = block.id },
                }, block.activityTitle()),
                ui.spacer(1),
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.activityMeta()),
            }),
        }),
        if (block.expanded)
            ui.column(.{ .gap = 4, .padding = 6 }, .{
                AgentMarkdown.view(ui, block.content(), .{}),
            })
        else
            ui.el(.stack, .{}, .{}),
    });
}

fn agentActivityNode(ui: *HyperTermUi, block: *const AgentBlockView) HyperTermUi.Node {
    return agentActivityNodeWithWidth(ui, block, 0);
}

fn agentActivityNodeWithWidth(ui: *HyperTermUi, block: *const AgentBlockView, width: f32) HyperTermUi.Node {
    return ui.column(.{
        .width = width,
        .grow = if (width == 0) 1 else 0,
    }, .{
        ui.row(.{ .gap = 5, .padding = 2, .cross = .center }, .{
            ui.button(.{
                .size = .sm,
                .variant = .ghost,
                .icon = if (block.expanded) "chevron-down" else "chevron-right",
                .on_press = Msg{ .toggle_agent_block = block.id },
            }, block.activityTitle()),
            ui.spacer(1),
            ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.activityMeta()),
        }),
        if (block.expanded)
            ui.column(.{ .gap = 5, .padding = 7 }, .{
                if (block.hasActivityDetails()) AgentMarkdown.view(ui, block.content(), .{}) else ui.el(.stack, .{}, .{}),
                if (block.truncated)
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Tool details clipped to 8 KiB in this view.")
                else
                    ui.el(.stack, .{}, .{}),
            })
        else
            ui.el(.stack, .{}, .{}),
    });
}

fn agentOperationNode(ui: *HyperTermUi, block: *const AgentBlockView) HyperTermUi.Node {
    return ui.row(.{ .gap = 7, .padding = 5, .cross = .center }, .{
        ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .info } }, "wrench"),
        ui.text(.{ .grow = 1, .wrap = true }, block.content()),
        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{s} · {s}", .{ block.operationKindLabel(), block.riskLabel() })),
        ui.el(.badge, .{ .text = block.stateLabel(), .variant = .secondary }, .{}),
    });
}

fn agentApprovalNode(ui: *HyperTermUi, model: *const Model, block: *const AgentBlockView) HyperTermUi.Node {
    const decision = if (!block.isApprovalPending())
        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("Decision: {s}", .{block.decisionLabel()}))
    else if (block.canAllowOnce())
        ui.row(.{ .gap = 6, .cross = .center }, .{
            ui.text(.{ .grow = 1, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, if (block.isWorkspaceReview()) "Rust-verified Diff · durable apply" else "Brokered read-only tool · receipt recorded"),
            ui.button(.{ .size = .sm, .variant = .outline, .on_press = Msg{ .cancel_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Cancel"),
            ui.button(.{ .size = .sm, .variant = .destructive, .on_press = Msg{ .reject_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Reject"),
            ui.button(.{ .size = .sm, .variant = .primary, .on_press = Msg{ .allow_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Allow once"),
        })
    else
        ui.row(.{ .gap = 6, .cross = .center }, .{
            ui.text(.{ .grow = 1, .wrap = true, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, "Allow unavailable until Rust can enforce this effect."),
            ui.button(.{ .size = .sm, .variant = .outline, .on_press = Msg{ .cancel_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Cancel"),
            ui.button(.{ .size = .sm, .variant = .destructive, .on_press = Msg{ .reject_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Reject"),
        });
    return ui.el(.card, .{ .style_tokens = .{ .border_color = .warning } }, .{
        ui.column(.{ .gap = 6, .padding = 8 }, .{
            ui.row(.{ .gap = 6, .cross = .center }, .{
                ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .warning } }, "alert"),
                ui.text(.{ .grow = 1 }, block.approvalTitle()),
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{s} · {s}", .{ block.operationKindLabel(), block.riskLabel() })),
            }),
            ui.text(.{ .wrap = true }, block.content()),
            decision,
        }),
    });
}

fn replaceNodeByLabel(ui: *HyperTermUi, source: HyperTermUi.Node, label: []const u8, replacement: HyperTermUi.Node) HyperTermUi.Node {
    if (std.mem.eql(u8, source.widget.semantics.label, label)) return replacement;
    if (source.nodes.len == 0) return source;
    var result = source;
    const children = ui.arena.alloc(HyperTermUi.Node, source.nodes.len) catch {
        ui.failed = true;
        return source;
    };
    for (source.nodes, children) |child, *output| {
        output.* = replaceNodeByLabel(ui, child, label, replacement);
    }
    result.nodes = children;
    return result;
}

/// Stable Zig composition seam for the product shell. Today the complete
/// shell is one compiled Native markup document; builder-owned surfaces such
/// as the windowed Agent transcript can replace individual branches here
/// without moving the rest of the design system out of `.native` fragments.
pub fn rootView(ui: *HyperTermUi, model: *const Model) HyperTermUi.Node {
    const shell = CompiledHyperTermView.build(ui, model);
    if (model.isTerminal()) return shell;
    return replaceNodeByLabel(ui, shell, "Agent blocks", agentTimeline(ui, model));
}

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
    var model = initialModel();
    if (trustedTerminalUrl(terminal_url)) {
        @memcpy(model.terminal_base_url_storage[0..terminal_url.len], terminal_url);
        model.terminal_base_url_len = terminal_url.len;
        refreshTerminalUrl(&model);
    }
    if (trustedAgentUrl(agent_url)) {
        @memcpy(model.agent_base_url_storage[0..agent_url.len], agent_url);
        model.agent_base_url_len = agent_url.len;
        const legacy_providers = parseAgentProviders(providers);
        if (provider_status.len == 0) {
            model.available_agent_providers = legacy_providers;
            model.authenticated_agent_providers = legacy_providers;
        } else if (!applyAgentProviderStatus(&model, provider_status)) {
            // The status document crosses a process boundary. Fail closed if
            // it is malformed instead of trusting the legacy ready list.
            clearAgentProviderStatus(&model);
        }
        const ready = model.authenticated_agent_providers | model.session_auth_agent_providers;
        model.selected_agent_provider = firstAvailableAgentProvider(ready) orelse
            firstAvailableAgentProvider(model.available_agent_providers) orelse .codex;
    }
    return model;
}

pub fn initialModelWithDesktopServices(
    terminal_url: []const u8,
    agent_url: []const u8,
    providers: []const u8,
    provider_status: []const u8,
    bug_capsule_url: []const u8,
) Model {
    var model = initialModelWithProviderStatus(
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
    }
    return model;
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

fn firstAvailableAgentProvider(providers: u8) ?AgentProvider {
    inline for (.{ AgentProvider.codex, AgentProvider.codex_acp, AgentProvider.claude_acp, AgentProvider.copilot_acp }) |provider| {
        if (providers & providerBit(provider) != 0) return provider;
    }
    return null;
}

const AgentProviderStatusWire = struct {
    id: []const u8,
    protocol: []const u8,
    readiness: []const u8,
    containment: []const u8,
};

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
    inline for (.{ AgentProvider.codex, AgentProvider.codex_acp, AgentProvider.claude_acp, AgentProvider.copilot_acp }) |provider| {
        if (std.mem.eql(u8, id, provider.id())) return provider;
    }
    return null;
}

fn expectedAgentProtocol(provider: AgentProvider) []const u8 {
    return if (provider == .codex) "codex-app-server-v2" else "acp-v1";
}

fn providerMenuLabel(model: *const Model, provider: AgentProvider) []const u8 {
    return switch (provider) {
        .codex => switch (model.agentProviderReadiness(provider)) {
            .authenticated => "Codex · App Server · authenticated",
            .login_required => "Codex · App Server · sign in required",
            .probe_failed => "Codex · App Server · readiness failed",
            else => "Codex · App Server · unavailable",
        },
        .codex_acp => switch (model.agentProviderReadiness(provider)) {
            .authenticated => "Codex · ACP · authenticated",
            .login_required => "Codex · ACP · sign in required",
            .probe_failed => "Codex · ACP · readiness failed",
            else => "Codex · ACP · unavailable",
        },
        .claude_acp => switch (model.agentProviderReadiness(provider)) {
            .authenticated => "Claude · ACP · authenticated",
            .login_required => "Claude · ACP · sign in required",
            .probe_failed => "Claude · ACP · readiness failed",
            else => "Claude · ACP · unavailable",
        },
        .copilot_acp => switch (model.agentProviderReadiness(provider)) {
            .available => "Copilot · ACP · auth on session",
            .probe_failed => "Copilot · ACP · readiness failed",
            else => "Copilot · ACP · unavailable",
        },
    };
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
    if (out.len == 0) return 0;
    out[0] = if (model.isTerminal() and model.terminalReady()) .{
        .label = terminal_view_label,
        .anchor = terminal_view_anchor,
        .url = model.terminalUrl(),
    } else .{
        .label = terminal_view_label,
        .frame = geometry.RectF.init(0, 0, 1, 1),
        .url = "zero://inline",
    };
    if (out.len == 1) return 1;
    out[1] = if (model.isCapsule() or model.hasAgentEditor()) .{
        .label = genui_view_label,
        .anchor = genui_view_anchor,
        .url = model.genUiWorkbenchUrl(),
        .reload_token = model.genui_source_revision,
    } else .{
        .label = genui_view_label,
        .frame = geometry.RectF.init(0, 0, 1, 1),
        .url = "zero://inline",
    };
    return 2;
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

fn trustedGatewayToken(token: []const u8) bool {
    if (token.len < 32 or token.len > max_gateway_token_bytes) return false;
    for (token) |character| {
        if (!std.ascii.isAlphanumeric(character) and character != '-' and character != '_') return false;
    }
    return true;
}

pub fn main(init: std.process.Init) !void {
    const terminal_url = init.environ_map.get("HYPER_TERM_TERMINAL_URL") orelse "";
    const agent_url = init.environ_map.get("HYPER_TERM_AGENT_URL") orelse "";
    const agent_providers = init.environ_map.get("HYPER_TERM_AGENT_PROVIDERS") orelse "";
    const agent_provider_status = init.environ_map.get("HYPER_TERM_AGENT_PROVIDER_STATUS") orelse "";
    const bug_capsule_url = init.environ_map.get("HYPER_TERM_BUG_CAPSULE_URL") orelse "";
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
    app_state.model = initialModelWithDesktopServices(
        terminal_url,
        agent_url,
        agent_providers,
        agent_provider_status,
        bug_capsule_url,
    );
    app_state.model.ui_font_registered = app_fonts.len > 0;

    try runner.runWithOptions(app_state.app(), .{
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
}
