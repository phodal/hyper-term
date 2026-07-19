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
pub const genui_view_anchor = "Agentic UI preview viewport";
pub const terminal_gateway_origin = "http://127.0.0.1:47437";
pub const max_sessions: usize = 8;
const terminal_url_capacity: usize = 256;
const agent_url_capacity: usize = 256;
const genui_url_capacity: usize = 512;
const max_gateway_token_bytes: usize = 128;
const terminal_close_url_capacity: usize = terminal_url_capacity + 64;
const agent_effect_url_capacity: usize = agent_url_capacity + 64;
const max_agent_blocks: usize = 16;
const max_agent_block_bytes: usize = 8 * 1024;
const max_agent_operation_id_bytes: usize = 36;
const max_agent_operation_kind_bytes: usize = 96;
const max_agent_error_bytes: usize = 512;
const max_agent_prompt_bytes: usize = 16 * 1024;
const terminal_close_effect_key_base: u64 = 0x4854_4300;
pub const agent_start_effect_key_base: u64 = 0x4854_4100;
const agent_close_effect_key_base: u64 = 0x4854_4200;
pub const agent_turn_effect_key_base: u64 = 0x4854_4400;
pub const agent_snapshot_effect_key_base: u64 = 0x4854_4500;
pub const agent_poll_timer_key_base: u64 = 0x4854_4600;
pub const agent_permission_effect_key_base: u64 = 0x4854_4700;
pub const window_width: f32 = 1180;
pub const window_height: f32 = 760;
pub const window_min_width: f32 = 840;
pub const window_min_height: f32 = 520;
pub const titlebar_natural_height: f32 = 48;

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
    .titlebar = .hidden_inset_tall,
    .views = &shell_views,
}};
pub const shell_scene: native_sdk.ShellConfig = .{ .windows = &shell_windows };

pub const SessionMode = enum {
    terminal,
    agent,
};

pub const AgentProvider = enum {
    codex,
    codex_acp,
    claude_acp,

    pub fn id(provider: AgentProvider) []const u8 {
        return switch (provider) {
            .codex => "codex",
            .codex_acp => "codex-acp",
            .claude_acp => "claude-acp",
        };
    }

    pub fn label(provider: AgentProvider) []const u8 {
        return switch (provider) {
            .codex => "Codex",
            .codex_acp => "Codex ACP",
            .claude_acp => "Claude ACP",
        };
    }
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
    completed,
    waiting_approval,
    failed,
};

pub const AgentMessageRole = enum { user, agent, system, thought };

pub const AgentBlockKind = enum { message, operation, approval };

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

    pub fn isOperation(block: *const AgentBlockView) bool {
        return block.kind == .operation;
    }

    pub fn isApproval(block: *const AgentBlockView) bool {
        return block.kind == .approval;
    }

    pub fn isApprovalPending(block: *const AgentBlockView) bool {
        return block.kind == .approval and block.decision == .none;
    }

    pub fn canAllowOnce(block: *const AgentBlockView) bool {
        return block.isApprovalPending() and
            block.risk == .read_only and
            std.mem.eql(u8, block.operationKindLabel(), "MCP tool");
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
    selected_agent_provider: AgentProvider = .codex,
    available_agent_providers: u8 = 0,
    terminal_base_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_base_url_len: usize = 0,
    terminal_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_url_len: usize = 0,
    agent_base_url_storage: [agent_url_capacity]u8 = [_]u8{0} ** agent_url_capacity,
    agent_base_url_len: usize = 0,
    genui_preview_url_storage: [genui_url_capacity]u8 = [_]u8{0} ** genui_url_capacity,
    genui_preview_url_len: usize = 0,
    genui_artifact_id_storage: [max_agent_operation_id_bytes]u8 = [_]u8{0} ** max_agent_operation_id_bytes,
    genui_artifact_id_len: usize = 0,
    genui_source_revision: u64 = 0,
    agent_composer_buffer: canvas.TextBuffer(max_agent_prompt_bytes) = .{},
    agent_blocks: [max_agent_blocks]AgentBlockView = [_]AgentBlockView{.{}} ** max_agent_blocks,
    agent_block_count: usize = 0,
    agent_history_clipped: bool = false,
    agent_projection_session_id: u8 = 0,
    agent_turn_status: AgentTurnStatus = .idle,
    agent_error_storage: [max_agent_error_bytes]u8 = [_]u8{0} ** max_agent_error_bytes,
    agent_error_len: usize = 0,
    agent_snapshot_in_flight_session_id: u8 = 0,
    agent_permission_in_flight_session_id: u8 = 0,

    /// Read by update, token, and derived-binding code rather than bound
    /// directly by the declarative view.
    pub const view_unbound = .{
        "system_scheme",
        "high_contrast",
        "reduce_motion",
        "session_slots",
        "session_count",
        "next_session_id",
        "available_agent_providers",
        "terminal_base_url_storage",
        "terminal_base_url_len",
        "terminal_url_storage",
        "terminal_url_len",
        "agent_base_url_storage",
        "agent_base_url_len",
        "genui_preview_url_storage",
        "genui_preview_url_len",
        "genui_artifact_id_storage",
        "genui_artifact_id_len",
        "genui_source_revision",
        "agent_composer_buffer",
        "agent_blocks",
        "agent_block_count",
        "agent_projection_session_id",
        "agent_turn_status",
        "agent_error_storage",
        "agent_error_len",
        "agent_snapshot_in_flight_session_id",
        "agent_permission_in_flight_session_id",
        "terminalReady",
        "terminalUrl",
        "genUiPreviewUrl",
        "agentError",
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

    pub fn terminalReady(model: *const Model) bool {
        return model.terminal_url_len > 0;
    }

    pub fn terminalDisconnected(model: *const Model) bool {
        return !model.terminalReady();
    }

    pub fn terminalConnectionLabel(model: *const Model) []const u8 {
        return if (model.terminalReady()) "connected" else "offline";
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

    pub fn codexProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.codex);
    }

    pub fn codexAcpProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.codex_acp);
    }

    pub fn claudeAcpProviderAvailable(model: *const Model) bool {
        return model.agentProviderAvailable(.claude_acp);
    }

    pub fn agentProviderAvailable(model: *const Model, provider: AgentProvider) bool {
        return model.available_agent_providers & providerBit(provider) != 0;
    }

    pub fn agentButtonLabel(model: *const Model) []const u8 {
        if (model.agentProviderUnavailable()) return "Agent unavailable";
        return switch (model.selected_agent_provider) {
            .codex => "Agent · Codex",
            .codex_acp => "Agent · Codex ACP",
            .claude_acp => "Agent · Claude ACP",
        };
    }

    pub fn activeAgentProviderLabel(model: *const Model) []const u8 {
        const session = model.activeSession();
        return if (session.mode == .agent) session.agent_provider.label() else model.selected_agent_provider.label();
    }

    pub fn agentConnectionLabel(model: *const Model) []const u8 {
        if (model.activeSession().agent_connection == .ready) {
            return switch (model.agent_turn_status) {
                .running => "working",
                .waiting_approval => "approval",
                .failed => "failed",
                else => "ready",
            };
        }
        return switch (model.activeSession().agent_connection) {
            .unavailable => "unavailable",
            .connecting => "connecting",
            .ready => "ready",
            .failed => "failed",
        };
    }

    pub fn agentStatus(model: *const Model) []const u8 {
        if (model.activeSession().agent_connection == .ready) {
            return switch (model.agent_turn_status) {
                .running => "Agent is responding · BlockDocument streaming",
                .waiting_approval => "Effect proposed · waiting for Rust permission flow",
                .failed => if (model.agent_error_len > 0) model.agentError() else "Agent turn failed",
                .completed => "Turn complete · history journaled locally",
                else => "Structured Agent ready · type a prompt",
            };
        }
        return switch (model.activeSession().agent_connection) {
            .unavailable => "Agent unavailable · no command executed",
            .connecting => "Agent connecting · no command executed",
            .ready => "Structured Agent ready · permission broker active",
            .failed => "Agent failed · no command executed",
        };
    }

    pub fn agentComposerText(model: *const Model) []const u8 {
        return model.agent_composer_buffer.text();
    }

    pub fn agentComposerDisabled(model: *const Model) bool {
        return model.activeSession().agent_connection != .ready or
            model.agent_base_url_len == 0 or
            model.agent_turn_status == .running or
            model.agent_turn_status == .waiting_approval;
    }

    pub fn agentBlocks(model: *const Model) []const AgentBlockView {
        return model.agent_blocks[0..model.agent_block_count];
    }

    pub fn hasAgentBlocks(model: *const Model) bool {
        return model.agent_block_count > 0;
    }

    pub fn agentPermissionBusy(model: *const Model) bool {
        return model.agent_permission_in_flight_session_id != 0;
    }

    pub fn agentError(model: *const Model) []const u8 {
        return model.agent_error_storage[0..model.agent_error_len];
    }

    pub fn hasGenUiArtifact(model: *const Model) bool {
        return model.activeSession().mode == .agent and model.genui_preview_url_len > 0;
    }

    pub fn genUiArtifactLabel(model: *const Model) []const u8 {
        return model.genui_artifact_id_storage[0..@min(model.genui_artifact_id_len, 8)];
    }

    pub fn genUiPreviewUrl(model: *const Model) []const u8 {
        return model.genui_preview_url_storage[0..model.genui_preview_url_len];
    }

    pub fn genUiStatus(model: *const Model) []const u8 {
        return if (model.hasGenUiArtifact()) "accepted · isolated" else "no artifact";
    }
};

pub const Msg = union(enum) {
    choose_terminal,
    choose_agent,
    toggle_agent_provider_picker,
    dismiss_agent_provider_picker,
    choose_codex_agent,
    choose_codex_acp_agent,
    choose_claude_acp_agent,
    select_session: u8,
    close_session: u8,
    close_active_session,
    terminal_session_closed: native_sdk.EffectResponse,
    agent_session_started: native_sdk.EffectResponse,
    agent_session_closed: native_sdk.EffectResponse,
    agent_composer_changed: canvas.TextInputEvent,
    send_agent_prompt,
    agent_turn_started: native_sdk.EffectResponse,
    agent_snapshot_received: native_sdk.EffectResponse,
    reject_agent_effect: []const u8,
    allow_agent_effect: []const u8,
    cancel_agent_effect: []const u8,
    agent_permission_decided: native_sdk.EffectResponse,
    agent_poll: native_sdk.EffectTimer,
    agent_split_resized: f32,
    system_appearance: struct {
        scheme: canvas.ColorScheme,
        high_contrast: bool,
        reduce_motion: bool,
    },
    chrome_changed: native_sdk.WindowChrome,

    /// Platform callbacks dispatch these messages; markup never does.
    pub const view_unbound = .{ "close_active_session", "terminal_session_closed", "agent_session_started", "agent_session_closed", "agent_turn_started", "agent_snapshot_received", "agent_permission_decided", "agent_poll", "system_appearance", "chrome_changed" };
};

const dev_markup_reload = builtin.mode == .Debug;
pub const HyperTermApp = native_sdk.UiAppWithFeatures(Model, Msg, .{ .runtime_markup = dev_markup_reload });
pub const Effects = HyperTermApp.Effects;

pub fn update(model: *Model, msg: Msg, fx: *Effects) void {
    switch (msg) {
        .choose_terminal => {
            _ = appendSession(model, .terminal);
        },
        .choose_agent => createAgentSession(model, model.selected_agent_provider, fx),
        .toggle_agent_provider_picker => model.agent_provider_picker_open = !model.agent_provider_picker_open,
        .dismiss_agent_provider_picker => model.agent_provider_picker_open = false,
        .choose_codex_agent => createAgentSession(model, .codex, fx),
        .choose_codex_acp_agent => createAgentSession(model, .codex_acp, fx),
        .choose_claude_acp_agent => createAgentSession(model, .claude_acp, fx),
        .select_session => |session_id| {
            const previous = model.active_session_id;
            selectSession(model, session_id);
            if (previous != model.active_session_id) {
                resetAgentProjection(model, model.active_session_id);
                requestActiveAgentSnapshot(model, fx);
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
        .agent_snapshot_received => |response| applyAgentSnapshotResponse(model, response, fx),
        .reject_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "reject_once", fx),
        .allow_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "allow_once", fx),
        .cancel_agent_effect => |operation_id| requestAgentPermission(model, operation_id, "cancelled", fx),
        .agent_permission_decided => |response| applyAgentPermissionResponse(model, response, fx),
        .agent_poll => |timer| {
            if (timer.outcome == .fired) requestActiveAgentSnapshot(model, fx);
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
        if (model.agent_base_url_len > 0 and model.agentProviderAvailable(provider)) {
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
    requestTerminalClose(model, session_id, fx);
    if (session.mode == .agent) {
        requestAgentClose(model, session_id, fx);
        fx.cancelTimer(agent_poll_timer_key_base + session_id);
        fx.cancel(agent_permission_effect_key_base + session_id);
        if (model.agent_permission_in_flight_session_id == session_id) {
            model.agent_permission_in_flight_session_id = 0;
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
    }
    model.session_count -= 1;
    model.session_slots[model.session_count] = .{};
    refreshTerminalUrl(model);
    if (was_active) {
        resetAgentProjection(model, model.active_session_id);
        requestActiveAgentSnapshot(model, fx);
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
        if (ready) requestAgentSnapshot(model, session_id, fx);
    }
}

fn requestAgentTurn(model: *Model, fx: *Effects) void {
    if (model.agentComposerDisabled()) return;
    const prompt = std.mem.trim(u8, model.agent_composer_buffer.text(), " \t\r\n");
    if (prompt.len == 0) return;
    const session_id = model.active_session_id;
    var storage: [agent_effect_url_capacity + 8]u8 = undefined;
    const request_url = writeAgentTurnUrl(model, session_id, storage[0..]) orelse return;
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

fn applyAgentTurnResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_turn_effect_key_base) orelse return;
    if (session_id != model.active_session_id) return;
    const accepted = response.outcome == .ok and response.status == 202 and !response.truncated;
    if (!accepted) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent turn could not be started");
        return;
    }
    requestAgentSnapshot(model, session_id, fx);
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
    requestAgentSnapshot(model, session_id, fx);
}

fn requestActiveAgentSnapshot(model: *Model, fx: *Effects) void {
    const session = model.activeSession();
    if (session.mode == .agent and session.agent_connection == .ready) {
        requestAgentSnapshot(model, session.id, fx);
    }
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
    status: []const u8,
    @"error": ?[]const u8 = null,
    document: struct {
        blocks: []const AgentBlockWire,
    },
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
    },
};

fn applyAgentSnapshotResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
    const session_id = effectSessionId(response.key, agent_snapshot_effect_key_base) orelse return;
    if (model.agent_snapshot_in_flight_session_id == session_id) {
        model.agent_snapshot_in_flight_session_id = 0;
    }
    if (session_id != model.active_session_id) {
        requestActiveAgentSnapshot(model, fx);
        return;
    }
    if (response.outcome != .ok or response.status != 200 or response.truncated) {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent history could not be refreshed");
        return;
    }
    const parsed = std.json.parseFromSlice(
        AgentSnapshotWire,
        std.heap.page_allocator,
        response.body,
        .{ .ignore_unknown_fields = true },
    ) catch {
        model.agent_turn_status = .failed;
        setAgentError(model, "Agent history response was invalid");
        return;
    };
    defer parsed.deinit();
    projectAgentBlocks(model, parsed.value.document.blocks);
    model.agent_turn_status = parseAgentTurnStatus(parsed.value.status);
    if (parsed.value.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
    if (model.agent_turn_status == .running) scheduleAgentPoll(session_id, fx);
}

fn projectAgentBlocks(model: *Model, blocks: []const AgentBlockWire) void {
    for (&model.agent_blocks) |*block| block.* = .{};
    model.agent_block_count = 0;
    clearGenUiArtifact(model);
    for (blocks) |block| {
        if (validGenUiArtifactBlock(block)) {
            projectGenUiArtifact(model, block);
        }
    }
    var block_total: usize = 0;
    for (blocks) |block| {
        if (renderableAgentBlock(block)) block_total += 1;
    }
    var skip = block_total -| max_agent_blocks;
    model.agent_history_clipped = skip > 0;
    for (blocks, 0..) |block, block_index| {
        if (!renderableAgentBlock(block)) continue;
        if (skip > 0) {
            skip -= 1;
            continue;
        }
        if (model.agent_block_count == max_agent_blocks) break;
        const view = &model.agent_blocks[model.agent_block_count];
        view.id = stableAgentBlockId(block.block_id, block_index);
        if (std.mem.eql(u8, block.kind, "message")) {
            view.kind = .message;
            view.role = parseAgentMessageRole(block.payload.role.?);
            copyAgentBlockContent(view, block.payload.text.?);
        } else if (std.mem.eql(u8, block.kind, "operation")) {
            projectOperationBlock(view, block);
        } else {
            projectApprovalBlock(view, block, blocks);
        }
        model.agent_block_count += 1;
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
    refreshGenUiPreviewUrl(model);
}

fn clearGenUiArtifact(model: *Model) void {
    model.genui_preview_url_len = 0;
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
            block.payload.role != null and block.payload.text != null;
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
    if (std.mem.eql(u8, value, "completed")) return .completed;
    if (std.mem.eql(u8, value, "waiting_approval")) return .waiting_approval;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    return .idle;
}

fn scheduleAgentPoll(session_id: u8, fx: *Effects) void {
    fx.startTimer(.{
        .key = agent_poll_timer_key_base + session_id,
        .interval_ms = 250,
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
    model.agent_history_clipped = false;
    model.agent_projection_session_id = session_id;
    model.agent_turn_status = .idle;
    model.agent_error_len = 0;
    model.agent_permission_in_flight_session_id = 0;
    clearGenUiArtifact(model);
}

fn setAgentError(model: *Model, message: []const u8) void {
    const length = utf8BoundedLength(message, model.agent_error_storage.len);
    @memcpy(model.agent_error_storage[0..length], message[0..length]);
    model.agent_error_len = length;
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
    return tokens;
}

pub const HyperTermUi = canvas.Ui(Msg);
pub const app_markup = @embedFile("app.native");
pub const CompiledHyperTermView = canvas.CompiledMarkupView(Model, Msg, app_markup);

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
    var model = initialModel();
    if (trustedTerminalUrl(terminal_url)) {
        @memcpy(model.terminal_base_url_storage[0..terminal_url.len], terminal_url);
        model.terminal_base_url_len = terminal_url.len;
        refreshTerminalUrl(&model);
    }
    if (trustedAgentUrl(agent_url)) {
        @memcpy(model.agent_base_url_storage[0..agent_url.len], agent_url);
        model.agent_base_url_len = agent_url.len;
        model.available_agent_providers = parseAgentProviders(providers);
        model.selected_agent_provider = firstAvailableAgentProvider(model.available_agent_providers) orelse .codex;
    }
    return model;
}

fn providerBit(provider: AgentProvider) u8 {
    return switch (provider) {
        .codex => 1,
        .codex_acp => 2,
        .claude_acp => 4,
    };
}

fn parseAgentProviders(value: []const u8) u8 {
    var providers: u8 = 0;
    var iterator = std.mem.splitScalar(u8, value, ',');
    while (iterator.next()) |provider| {
        if (std.mem.eql(u8, provider, "codex")) providers |= providerBit(.codex);
        if (std.mem.eql(u8, provider, "codex-acp")) providers |= providerBit(.codex_acp);
        if (std.mem.eql(u8, provider, "claude-acp")) providers |= providerBit(.claude_acp);
    }
    return providers;
}

fn firstAvailableAgentProvider(providers: u8) ?AgentProvider {
    inline for (.{ AgentProvider.codex, AgentProvider.codex_acp, AgentProvider.claude_acp }) |provider| {
        if (providers & providerBit(provider) != 0) return provider;
    }
    return null;
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
    if (!model.terminalReady() or out.len == 0) return 0;
    out[0] = .{
        .label = terminal_view_label,
        .anchor = terminal_view_anchor,
        .url = model.terminalUrl(),
    };
    return 1;
}

pub fn desktopPanes(model: *const Model, out: []HyperTermApp.WebViewPane) usize {
    var count = terminalPanes(model, out);
    if (count == out.len) return count;
    out[count] = if (model.hasGenUiArtifact()) .{
        .label = genui_view_label,
        .anchor = genui_view_anchor,
        .url = model.genUiPreviewUrl(),
        .reload_token = model.genui_source_revision,
    } else .{
        .label = genui_view_label,
        .frame = geometry.RectF.init(0, 0, 1, 1),
        .url = "zero://inline",
    };
    count += 1;
    return count;
}

fn refreshGenUiPreviewUrl(model: *Model) void {
    if (model.agent_base_url_len == 0 or model.genui_artifact_id_len == 0) {
        model.genui_preview_url_len = 0;
        return;
    }
    const base_url = model.agent_base_url_storage[0..model.agent_base_url_len];
    const marker = "/?token=";
    const marker_index = std.mem.indexOf(u8, base_url, marker) orelse {
        model.genui_preview_url_len = 0;
        return;
    };
    const origin = base_url[0..marker_index];
    const token = base_url[marker_index + marker.len ..];
    const artifact_id = model.genui_artifact_id_storage[0..model.genui_artifact_id_len];
    const url = std.fmt.bufPrint(
        model.genui_preview_url_storage[0..],
        "{s}/agent/artifact/{s}/preview?token={s}&session_id={d}#{s}",
        .{ origin, artifact_id, token, model.active_session_id, artifact_id },
    ) catch {
        model.genui_preview_url_len = 0;
        return;
    };
    model.genui_preview_url_len = url.len;
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
        .on_command = command,
        .on_key = onKey,
        .on_appearance = onAppearance,
        .on_chrome = onChrome,
        .view = CompiledHyperTermView.build,
        .web_panes = desktopPanes,
        .markup = if (dev_markup_reload)
            .{ .source = app_markup, .watch_path = "src/app.native", .io = init.io }
        else
            null,
    });
    defer app_state.destroy();
    app_state.model = initialModelWithProviders(terminal_url, agent_url, agent_providers);

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
