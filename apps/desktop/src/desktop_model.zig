//! Bounded desktop state for Terminal and Agent tabs.
//!
//! This module owns presentation state and derived labels only. It has no
//! process, filesystem, PTY, network, or WebView effect authority.

const std = @import("std");
const native_sdk = @import("native_sdk");
const agent_capabilities = @import("agent_capabilities.zig");
const agent_block_view = @import("agent_block_view.zig");
const agent_provider = @import("agent_provider.zig");

const canvas = native_sdk.canvas;

pub const max_sessions: usize = 8;
pub const max_inline_session_tabs: usize = 2;
pub const max_session_tab_title_bytes: usize = 16;
pub const terminal_url_capacity: usize = 256;
pub const agent_url_capacity: usize = 256;
pub const genui_url_capacity: usize = 512;
pub const max_agent_blocks: usize = 128;
pub const max_agent_search_bytes: usize = 256;
pub const max_agent_operation_id_bytes = agent_block_view.max_operation_id_bytes;
pub const max_agent_goal_objective_bytes: usize = 1024;
pub const max_agent_goal_meta_bytes: usize = 128;
pub const max_agent_error_bytes: usize = 512;
pub const max_agent_prompt_bytes: usize = 16 * 1024;
pub const max_agent_config_options: usize = 4;
pub const max_agent_config_choices: usize = 24;
pub const max_agent_commands: usize = 24;
pub const max_agent_tier2_results: usize = 4;
pub const max_agent_tier2_files: usize = 12;
pub const max_agent_tier2_path_bytes: usize = 256;
pub const max_agent_tier2_diff_bytes: usize = 6 * 1024;
pub const max_agent_capability_id_bytes: usize = 128;
pub const max_agent_capability_label_bytes: usize = 192;
pub const max_agent_execution_contexts: usize = 4;
pub const max_agent_context_id_bytes: usize = 128;
pub const agent_context_digest_bytes: usize = 64;
pub const max_agent_context_summary_bytes: usize = 64;
pub const max_terminal_title_bytes: usize = 256;
pub const max_terminal_cwd_bytes: usize = 512;
pub const max_agent_goal_step_columns: usize = 42;
pub const titlebar_natural_height: f32 = 44;

pub const SessionMode = enum {
    terminal,
    agent,
    capsule,
};

pub const AgentMessageRole = agent_block_view.MessageRole;
pub const AgentBlockKind = agent_block_view.BlockKind;
pub const AgentToolStatus = agent_block_view.ToolStatus;
pub const AgentRisk = agent_block_view.Risk;
pub const AgentOperationState = agent_block_view.OperationState;
pub const AgentDecision = agent_block_view.Decision;
pub const AgentDiffFileView = agent_block_view.DiffFileView;
pub const AgentBlockView = agent_block_view.BlockView;

pub const AgentProvider = agent_provider.Provider;
pub const AgentProviderReadiness = agent_provider.Readiness;

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

/// A bounded, semantic request for the native shell to get the user's
/// attention. The source fields are Rust-authenticated Agent projection
/// state; provider prose and WebView content never enter this contract.
pub const AgentAttention = struct {
    kind: Kind,
    session_id: u8,
    provider: AgentProvider,
    document_revision: u64,
    stream_sequence: u64,

    pub const Kind = enum {
        approval,
        review_ready,
        failed,
    };

    pub fn title(attention: AgentAttention) []const u8 {
        return switch (attention.kind) {
            .approval => "Agent needs approval",
            .review_ready => "Agent finished",
            .failed => "Agent needs attention",
        };
    }

    pub fn body(attention: AgentAttention) []const u8 {
        return switch (attention.kind) {
            .approval => "Review the requested action in Hyper Term.",
            .review_ready => "The result is ready to review in Hyper Term.",
            .failed => "Open Hyper Term to review the failed Agent turn.",
        };
    }
};

pub const AgentGoalStatus = enum {
    active,
    paused,
    blocked,
    usage_limited,
    budget_limited,
    complete,
};

pub const AgentGoalAction = enum {
    pause_goal,
    resume_goal,
    clear_goal,

    pub fn command(action: AgentGoalAction) []const u8 {
        return switch (action) {
            .pause_goal => "/goal pause",
            .resume_goal => "/goal resume",
            .clear_goal => "/goal clear",
        };
    }
};

pub const AgentGoalView = struct {
    objective_storage: [max_agent_goal_objective_bytes]u8 = [_]u8{0} ** max_agent_goal_objective_bytes,
    objective_len: usize = 0,
    meta_storage: [max_agent_goal_meta_bytes]u8 = [_]u8{0} ** max_agent_goal_meta_bytes,
    meta_len: usize = 0,
    status: AgentGoalStatus = .active,
    expanded: bool = false,

    pub fn objective(goal: *const AgentGoalView) []const u8 {
        return goal.objective_storage[0..goal.objective_len];
    }

    pub fn meta(goal: *const AgentGoalView) []const u8 {
        return goal.meta_storage[0..goal.meta_len];
    }
};

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

pub const AgentExecutionMode = enum {
    hermetic,
    project,
    user,

    pub fn label(mode: AgentExecutionMode) []const u8 {
        return switch (mode) {
            .hermetic => "Hermetic",
            .project => "Project",
            .user => "User",
        };
    }
};

pub const AgentExecutionContextView = struct {
    context_id_storage: [max_agent_context_id_bytes]u8 = [_]u8{0} ** max_agent_context_id_bytes,
    context_id_len: usize = 0,
    mode: AgentExecutionMode = .hermetic,
    digest_storage: [agent_context_digest_bytes]u8 = [_]u8{0} ** agent_context_digest_bytes,
    binding_count: usize = 0,
    credential_count: usize = 0,

    pub fn contextId(context: *const AgentExecutionContextView) []const u8 {
        return context.context_id_storage[0..context.context_id_len];
    }

    pub fn modeLabel(context: *const AgentExecutionContextView) []const u8 {
        return context.mode.label();
    }

    pub fn digestPrefix(context: *const AgentExecutionContextView) []const u8 {
        return context.digest_storage[0..8];
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
    agent_capabilities: agent_capabilities.SessionState = .{},
    agent_connection: AgentConnection = .unavailable,
    agent_attention_status: AgentTurnStatus = .idle,
    agent_attention_revision: u64 = 0,
    acknowledged_attention_status: AgentTurnStatus = .idle,
    acknowledged_attention_revision: u64 = 0,
    terminal_metadata_revision: u64 = 0,
    terminal_title_storage: [max_terminal_title_bytes]u8 = [_]u8{0} ** max_terminal_title_bytes,
    terminal_title_len: usize = 0,
    terminal_cwd_storage: [max_terminal_cwd_bytes]u8 = [_]u8{0} ** max_terminal_cwd_bytes,
    terminal_cwd_len: usize = 0,

    pub fn terminalTitle(session: *const Session) []const u8 {
        return session.terminal_title_storage[0..session.terminal_title_len];
    }

    pub fn terminalCwd(session: *const Session) []const u8 {
        return session.terminal_cwd_storage[0..session.terminal_cwd_len];
    }

    pub fn displayTitle(session: *const Session) []const u8 {
        if (session.mode != .terminal) {
            const agent_title = session.agent_capabilities.title();
            return if (agent_title.len > 0) agent_title else session.title;
        }
        const terminal_title = session.terminalTitle();
        if (terminal_title.len > 0 and terminal_title.len <= 40) return terminal_title;
        if (terminal_title.len > 40) {
            if (std.mem.lastIndexOfScalar(u8, terminal_title, '/')) |slash| {
                const basename = terminal_title[slash + 1 ..];
                if (basename.len > 0 and basename.len <= 40) return basename;
            }
        }
        const cwd = session.terminalCwd();
        if (cwd.len > 1) {
            const basename = std.fs.path.basename(cwd);
            return if (basename.len <= 40) basename else utf8Prefix(basename, 40);
        }
        if (cwd.len == 1) return "/";
        if (terminal_title.len > 0) return utf8Prefix(terminal_title, 40);
        return session.title;
    }

    pub fn tabTitle(session: *const Session) []const u8 {
        return utf8Prefix(session.displayTitle(), max_session_tab_title_bytes);
    }

    pub fn hasUnacknowledgedAttention(session: *const Session) bool {
        const attention = switch (session.agent_attention_status) {
            .waiting_approval, .completed, .failed => true,
            else => false,
        };
        return attention and
            (session.agent_attention_status != session.acknowledged_attention_status or
                session.agent_attention_revision != session.acknowledged_attention_revision);
    }

    pub fn tabIcon(session: *const Session) []const u8 {
        if (!session.hasUnacknowledgedAttention()) return session.icon;
        return switch (session.agent_attention_status) {
            .waiting_approval, .failed => "alert",
            .completed => "check-circle",
            else => session.icon,
        };
    }

    pub fn closeLabel(session: *const Session, arena: std.mem.Allocator) []const u8 {
        return std.fmt.allocPrint(arena, "Close {s} {d}", .{ session.displayTitle(), session.id }) catch "Close tab";
    }

    pub fn tabGroupLabel(session: *const Session, arena: std.mem.Allocator) []const u8 {
        if (session.hasUnacknowledgedAttention()) {
            const attention = switch (session.agent_attention_status) {
                .waiting_approval => "needs approval",
                .completed => "review ready",
                .failed => "failed",
                else => "needs attention",
            };
            return std.fmt.allocPrint(arena, "{s} tab {d}, {s}", .{ session.displayTitle(), session.id, attention }) catch "Agent tab needs attention";
        }
        return std.fmt.allocPrint(arena, "{s} tab {d}", .{ session.displayTitle(), session.id }) catch "Session tab";
    }
};

fn utf8Prefix(value: []const u8, maximum: usize) []const u8 {
    var length = @min(value.len, maximum);
    while (length > 0 and !std.unicode.utf8ValidateSlice(value[0..length])) : (length -= 1) {}
    return value[0..length];
}

pub const TerminalSessionMetadataWire = struct {
    session_id: u16,
    revision: u64,
    title: ?[]const u8,
    cwd: ?[]const u8,
};

pub const TerminalMetadataResponseWire = struct {
    version: u16,
    sessions: []const TerminalSessionMetadataWire,
};

pub const DesktopWorkspaceWire = struct {
    version: u16,
    revision: u64,
    active_session_id: u8,
    next_session_id: u8,
    selected_agent_provider: []const u8,
    sessions: []const DesktopSessionWire,
};

pub const DesktopSessionWire = struct {
    id: u8,
    mode: []const u8,
    agent_provider: ?[]const u8 = null,
};

pub const PendingAgentPrompt = struct {
    storage: [max_agent_prompt_bytes]u8 = [_]u8{0} ** max_agent_prompt_bytes,
    len: usize = 0,

    pub fn set(pending: *PendingAgentPrompt, value: []const u8) void {
        const length = utf8BoundedLength(value, pending.storage.len);
        @memcpy(pending.storage[0..length], value[0..length]);
        pending.len = length;
    }

    pub fn text(pending: *const PendingAgentPrompt) []const u8 {
        return pending.storage[0..pending.len];
    }

    pub fn clear(pending: *PendingAgentPrompt) void {
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
    desktop_workspace_enabled: bool = false,
    desktop_workspace_restored: bool = false,
    desktop_workspace_revision: u64 = 0,
    desktop_workspace_persisted_revision: u64 = 0,
    desktop_workspace_in_flight_revision: u64 = 0,
    session_picker_open: bool = false,
    agent_provider_picker_open: bool = false,
    agent_provider_refresh_in_flight: bool = false,
    agent_attention_in_flight: bool = false,
    terminal_metadata_in_flight: bool = false,
    selected_agent_provider: AgentProvider = .codex,
    available_agent_providers: u8 = 0,
    authenticated_agent_providers: u8 = 0,
    session_auth_agent_providers: u8 = 0,
    login_required_agent_providers: u8 = 0,
    provider_missing_agent_providers: u8 = 0,
    provider_probe_failed_agent_providers: u8 = 0,
    contained_agent_providers: u8 = 0,
    provider_login: agent_provider.LoginGuide = .{},
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
    terminal_webview_mounted: bool = false,
    genui_webview_mounted: bool = false,
    agent_composer_buffer: canvas.TextBuffer(max_agent_prompt_bytes) = .{},
    agent_composer_drafts: [max_sessions]canvas.TextBuffer(max_agent_prompt_bytes) =
        [_]canvas.TextBuffer(max_agent_prompt_bytes){.{}} ** max_sessions,
    agent_composer_focus_requested: bool = false,
    agent_search_buffer: canvas.TextBuffer(max_agent_search_bytes) = .{},
    agent_search_open: bool = false,
    agent_pending_prompts: [max_sessions]PendingAgentPrompt = [_]PendingAgentPrompt{.{}} ** max_sessions,
    ui_font_registered: bool = false,
    agent_blocks: [max_agent_blocks]AgentBlockView = [_]AgentBlockView{.{}} ** max_agent_blocks,
    agent_block_count: usize = 0,
    agent_block_index_base: u64 = 0,
    agent_history_clipped: bool = false,
    agent_history_restored: bool = false,
    agent_plan: AgentBlockView = .{},
    agent_plan_visible: bool = false,
    agent_goal: AgentGoalView = .{},
    agent_goal_visible: bool = false,
    agent_goal_menu_open: bool = false,
    agent_goal_editing: bool = false,
    agent_goal_in_flight_session_id: u8 = 0,
    agent_projection_session_id: u8 = 0,
    agent_execution_contexts: [max_agent_execution_contexts]AgentExecutionContextView = [_]AgentExecutionContextView{.{}} ** max_agent_execution_contexts,
    agent_execution_context_count: usize = 0,
    agent_execution_context_session_id: u8 = 0,
    agent_execution_context_expanded: bool = false,
    agent_execution_context_summary_storage: [max_agent_context_summary_bytes]u8 = [_]u8{0} ** max_agent_context_summary_bytes,
    agent_execution_context_summary_len: usize = 0,
    agent_document_revision: u64 = 0,
    agent_stream_sequence: u64 = 0,
    agent_turn_status: AgentTurnStatus = .idle,
    agent_error_storage: [max_agent_error_bytes]u8 = [_]u8{0} ** max_agent_error_bytes,
    agent_error_len: usize = 0,
    agent_pending_operation_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    agent_pending_operation_len: usize = 0,
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
        "active_session_id",
        "next_session_id",
        "session_picker_open",
        "desktop_workspace_enabled",
        "desktop_workspace_restored",
        "desktop_workspace_revision",
        "desktop_workspace_persisted_revision",
        "desktop_workspace_in_flight_revision",
        "agentProviderUnavailable",
        "agent_attention_in_flight",
        "terminal_metadata_in_flight",
        "available_agent_providers",
        "authenticated_agent_providers",
        "session_auth_agent_providers",
        "login_required_agent_providers",
        "provider_missing_agent_providers",
        "provider_probe_failed_agent_providers",
        "contained_agent_providers",
        "provider_login",
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
        "agent_composer_drafts",
        "agent_composer_focus_requested",
        "agent_search_buffer",
        "agent_search_open",
        "agentSearchQuery",
        "agent_pending_prompts",
        "terminal_webview_mounted",
        "genui_webview_mounted",
        "ui_font_registered",
        "agent_blocks",
        "agent_block_count",
        "agent_block_index_base",
        "agent_history_restored",
        "agent_plan",
        "agent_plan_visible",
        "agent_goal",
        "agent_goal_visible",
        "agent_goal_menu_open",
        "agent_goal_editing",
        "agentGoalEditing",
        "agent_goal_in_flight_session_id",
        "agentGoalActionDisabled",
        "agentGoalEditDisabled",
        "agent_projection_session_id",
        "agent_execution_contexts",
        "agent_execution_context_count",
        "agent_execution_context_session_id",
        "agent_execution_context_summary_storage",
        "agent_execution_context_summary_len",
        "agent_document_revision",
        "agent_stream_sequence",
        "agent_turn_status",
        "agent_error_storage",
        "agent_error_len",
        "agent_pending_operation_storage",
        "agent_pending_operation_len",
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
        "openSessions",
        "inlineSessions",
        "hasSessionOverflow",
    };

    pub fn openSessions(model: *const Model) []const Session {
        return model.session_slots[0..model.session_count];
    }

    pub fn inlineSessions(model: *const Model) []const Session {
        if (!model.hasSessionOverflow()) return model.openSessions();
        for (model.openSessions()) |*session| {
            if (session.id == model.active_session_id) return session[0..1];
        }
        return model.session_slots[0..1];
    }

    pub fn hasSessionOverflow(model: *const Model) bool {
        return model.session_count > max_inline_session_tabs;
    }

    pub fn sessionLimitReached(model: *const Model) bool {
        return model.session_count >= max_sessions;
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
        if (!model.terminalReady()) return "zsh · hyperd disconnected";
        const cwd = model.activeSession().terminalCwd();
        return if (cwd.len > 0) cwd else "zsh · ordered Rust PTY plane";
    }

    pub fn terminalUrl(model: *const Model) []const u8 {
        return model.terminal_url_storage[0..model.terminal_url_len];
    }

    pub fn agentProviderUnavailable(model: *const Model) bool {
        return model.agent_base_url_len == 0 or model.available_agent_providers == 0;
    }

    pub fn agentProviderPickerUnavailable(model: *const Model) bool {
        return model.agent_base_url_len == 0 or model.sessionLimitReached();
    }

    pub fn agentProviderRefreshLabel(model: *const Model) []const u8 {
        return if (model.agent_provider_refresh_in_flight)
            "Checking providers…"
        else
            "Refresh providers";
    }

    pub fn codexLoginRequired(model: *const Model) bool {
        return model.agentProviderReadiness(.codex) == .login_required or
            model.agentProviderReadiness(.codex_acp) == .login_required;
    }

    pub fn claudeLoginRequired(model: *const Model) bool {
        return model.agentProviderReadiness(.claude_acp) == .login_required;
    }

    pub fn hasProviderLoginActions(model: *const Model) bool {
        return model.codexLoginRequired() or model.claudeLoginRequired();
    }

    pub fn hasProviderLoginHint(model: *const Model) bool {
        return model.provider_login.visible(model.active_session_id, model.isTerminal());
    }

    pub fn providerLoginLabel(model: *const Model) []const u8 {
        return model.provider_login.label();
    }
    pub fn providerLoginCommand(model: *const Model) []const u8 {
        return model.provider_login.command();
    }
    pub fn providerLoginStatus(model: *const Model) []const u8 {
        return model.provider_login.status();
    }
    pub fn providerLoginCopyInFlight(model: *const Model) bool {
        return model.provider_login.copy_state == .copying;
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
        return agent_provider.menuLabel(.codex, model.agentProviderReadiness(.codex));
    }
    pub fn codexAcpProviderMenuLabel(model: *const Model) []const u8 {
        return agent_provider.menuLabel(.codex_acp, model.agentProviderReadiness(.codex_acp));
    }
    pub fn claudeAcpProviderMenuLabel(model: *const Model) []const u8 {
        return agent_provider.menuLabel(.claude_acp, model.agentProviderReadiness(.claude_acp));
    }
    pub fn copilotAcpProviderMenuLabel(model: *const Model) []const u8 {
        return agent_provider.menuLabel(.copilot_acp, model.agentProviderReadiness(.copilot_acp));
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
        const visual_lines = utf8VisualLineCount(text, 96);
        const extra_lines = @min(visual_lines - 1, 4);
        return 66 + @as(f32, @floatFromInt(extra_lines)) * 18;
    }

    pub fn agentComposerText(model: *const Model) []const u8 {
        return model.agent_composer_buffer.text();
    }

    pub fn agentComposerAutofocus(model: *const Model) bool {
        return model.activeSession().mode == .agent and
            model.activeSession().agent_connection == .ready and
            !model.agent_search_open and
            (model.agent_composer_focus_requested or model.agent_goal_editing);
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

    pub fn hasAgentContextUsage(model: *const Model) bool {
        return model.activeSession().mode == .agent and model.activeSession().agent_capabilities.hasUsage();
    }

    pub fn agentContextUsage(model: *const Model) []const u8 {
        return model.activeSession().agent_capabilities.usageLabel();
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

    pub fn agentGoal(model: *const Model) ?*const AgentGoalView {
        return if (model.agent_goal_visible) &model.agent_goal else null;
    }

    pub fn agentGoalEditing(model: *const Model) bool {
        return model.agent_goal_editing;
    }

    pub fn agentGoalActionDisabled(model: *const Model) bool {
        return !model.agent_goal_visible or
            model.activeSession().agent_provider != .codex or
            model.agent_goal_in_flight_session_id != 0 or
            model.agentSubmitDisabled();
    }

    pub fn agentGoalEditDisabled(model: *const Model) bool {
        const current = std.mem.trim(u8, model.agent_composer_buffer.text(), " \t\r\n");
        return current.len > 0 and !std.mem.startsWith(u8, current, "/goal ");
    }

    pub fn hasAgentBlocks(model: *const Model) bool {
        return model.agent_block_count > 0;
    }

    pub fn hasAgentRestoredHistory(model: *const Model) bool {
        return model.activeSession().mode == .agent and model.agent_history_restored;
    }

    pub fn agentPermissionBusy(model: *const Model) bool {
        return model.agent_permission_in_flight_session_id != 0;
    }

    pub fn isLiveAgentApproval(model: *const Model, block: *const AgentBlockView) bool {
        if (!block.isApprovalPending() or model.agent_turn_status != .waiting_approval) {
            return false;
        }
        if (model.agent_pending_operation_len == 0) return true;
        return std.mem.eql(
            u8,
            block.operationId(),
            model.agent_pending_operation_storage[0..model.agent_pending_operation_len],
        );
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

    pub fn hasAgentExecutionContext(model: *const Model) bool {
        return model.activeSession().mode == .agent and
            model.agent_execution_context_session_id == model.active_session_id and
            model.agent_execution_context_count > 0;
    }

    pub fn hasAgentThreadToolbar(model: *const Model) bool {
        return model.agentSearchOpen() or model.hasAgentThreadActions();
    }

    pub fn hasAgentThreadActions(model: *const Model) bool {
        return model.hasAgentRestoredHistory() or
            model.hasAgentExecutionContext() or model.canOpenAgentEditor();
    }

    pub fn agentSearchOpen(model: *const Model) bool {
        return model.activeSession().mode == .agent and model.agent_search_open;
    }

    pub fn agentSearchText(model: *const Model) []const u8 {
        return model.agent_search_buffer.text();
    }

    pub fn agentSearchQuery(model: *const Model) []const u8 {
        return std.mem.trim(u8, model.agent_search_buffer.text(), " \t\r\n");
    }

    pub fn hasAgentSearchQuery(model: *const Model) bool {
        return model.agentSearchQuery().len > 0;
    }

    pub fn agentSearchResultCount(model: *const Model) usize {
        if (!model.agentSearchOpen() or !model.hasAgentSearchQuery()) {
            return model.agent_block_count;
        }
        var count: usize = 0;
        for (model.agent_blocks[0..model.agent_block_count]) |*block| {
            if (agent_block_view.matchesQuery(block, model.agentSearchQuery())) count += 1;
        }
        return count;
    }

    pub fn agentExecutionContextSummary(model: *const Model) []const u8 {
        return model.agent_execution_context_summary_storage[0..model.agent_execution_context_summary_len];
    }

    pub fn agentExecutionContexts(model: *const Model) []const AgentExecutionContextView {
        return model.agent_execution_contexts[0..model.agent_execution_context_count];
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

/// Derive desktop attention from the currently authenticated Rust projection.
/// Ordinary terminals, stale projections, and non-terminal Agent states never
/// produce a system notification.
pub fn agentAttention(model: *const Model) ?AgentAttention {
    var selected: ?AgentAttention = null;
    for (model.openSessions()) |session| {
        if (session.mode != .agent) continue;
        const status = if (session.id == model.active_session_id and
            model.agent_projection_session_id == session.id and
            attentionKind(model.agent_turn_status) != null)
            model.agent_turn_status
        else
            session.agent_attention_status;
        const kind = attentionKind(status) orelse continue;
        const candidate: AgentAttention = .{
            .kind = kind,
            .session_id = session.id,
            .provider = session.agent_provider,
            .document_revision = if (session.id == model.active_session_id and
                model.agent_projection_session_id == session.id)
                @max(session.agent_attention_revision, model.agent_document_revision)
            else
                session.agent_attention_revision,
            .stream_sequence = if (session.id == model.active_session_id and
                model.agent_projection_session_id == session.id)
                model.agent_stream_sequence
            else
                0,
        };
        if (selected == null or attentionPriority(candidate.kind) > attentionPriority(selected.?.kind) or
            (attentionPriority(candidate.kind) == attentionPriority(selected.?.kind) and
                candidate.session_id < selected.?.session_id))
        {
            selected = candidate;
        }
    }
    return selected;
}

fn attentionKind(status: AgentTurnStatus) ?AgentAttention.Kind {
    return switch (status) {
        .waiting_approval => .approval,
        .completed => .review_ready,
        .failed => .failed,
        else => null,
    };
}

fn attentionPriority(kind: AgentAttention.Kind) u8 {
    return switch (kind) {
        .approval => 3,
        .failed => 2,
        .review_ready => 1,
    };
}

fn providerBit(provider: AgentProvider) u8 {
    return @as(u8, 1) << @intFromEnum(provider);
}

fn utf8BoundedLength(value: []const u8, maximum: usize) usize {
    var end = @min(value.len, maximum);
    while (end > 0 and !std.unicode.utf8ValidateSlice(value[0..end])) end -= 1;
    return end;
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
