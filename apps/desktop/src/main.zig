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
pub const terminal_gateway_origin = "http://127.0.0.1:47437";
pub const max_sessions: usize = 8;
const terminal_url_capacity: usize = 256;
const agent_url_capacity: usize = 256;
const max_gateway_token_bytes: usize = 128;
const terminal_close_url_capacity: usize = terminal_url_capacity + 64;
const agent_effect_url_capacity: usize = agent_url_capacity + 64;
const max_agent_messages: usize = 12;
const max_agent_message_bytes: usize = 8 * 1024;
const max_agent_error_bytes: usize = 512;
const max_agent_prompt_bytes: usize = 16 * 1024;
const terminal_close_effect_key_base: u64 = 0x4854_4300;
pub const agent_start_effect_key_base: u64 = 0x4854_4100;
const agent_close_effect_key_base: u64 = 0x4854_4200;
pub const agent_turn_effect_key_base: u64 = 0x4854_4400;
pub const agent_snapshot_effect_key_base: u64 = 0x4854_4500;
pub const agent_poll_timer_key_base: u64 = 0x4854_4600;
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

pub const AgentMessageView = struct {
    id: u16 = 0,
    role: AgentMessageRole = .agent,
    text_storage: [max_agent_message_bytes]u8 = [_]u8{0} ** max_agent_message_bytes,
    text_len: usize = 0,
    truncated: bool = false,

    pub fn text(message: *const AgentMessageView) []const u8 {
        return message.text_storage[0..message.text_len];
    }

    pub fn roleLabel(message: *const AgentMessageView) []const u8 {
        return switch (message.role) {
            .user => "You",
            .agent => "Agent",
            .system => "System",
            .thought => "Plan",
        };
    }

    pub fn isUser(message: *const AgentMessageView) bool {
        return message.role == .user;
    }
};

pub const Session = struct {
    id: u8 = 0,
    mode: SessionMode = .terminal,
    title: []const u8 = "zsh",
    icon: []const u8 = "terminal",
    accessibility_label: []const u8 = "Terminal session",
    close_label: []const u8 = "Close Terminal session",
    agent_connection: AgentConnection = .unavailable,
};

pub const Model = struct {
    new_session_open: bool = false,
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
    terminal_base_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_base_url_len: usize = 0,
    terminal_url_storage: [terminal_url_capacity]u8 = [_]u8{0} ** terminal_url_capacity,
    terminal_url_len: usize = 0,
    agent_base_url_storage: [agent_url_capacity]u8 = [_]u8{0} ** agent_url_capacity,
    agent_base_url_len: usize = 0,
    agent_composer_buffer: canvas.TextBuffer(max_agent_prompt_bytes) = .{},
    agent_messages: [max_agent_messages]AgentMessageView = [_]AgentMessageView{.{}} ** max_agent_messages,
    agent_message_count: usize = 0,
    agent_history_clipped: bool = false,
    agent_projection_session_id: u8 = 0,
    agent_turn_status: AgentTurnStatus = .idle,
    agent_error_storage: [max_agent_error_bytes]u8 = [_]u8{0} ** max_agent_error_bytes,
    agent_error_len: usize = 0,
    agent_snapshot_in_flight_session_id: u8 = 0,

    /// Read by update, token, and derived-binding code rather than bound
    /// directly by the declarative view.
    pub const view_unbound = .{
        "system_scheme",
        "high_contrast",
        "reduce_motion",
        "session_slots",
        "session_count",
        "next_session_id",
        "terminal_base_url_storage",
        "terminal_base_url_len",
        "terminal_url_storage",
        "terminal_url_len",
        "agent_base_url_storage",
        "agent_base_url_len",
        "agent_composer_buffer",
        "agent_messages",
        "agent_message_count",
        "agent_projection_session_id",
        "agent_turn_status",
        "agent_error_storage",
        "agent_error_len",
        "agent_snapshot_in_flight_session_id",
        "terminalReady",
        "terminalUrl",
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
                .running => "Codex is responding · BlockDocument streaming",
                .waiting_approval => "Effect proposed · waiting for Rust permission flow",
                .failed => if (model.agent_error_len > 0) model.agentError() else "Agent turn failed",
                .completed => "Turn complete · history journaled locally",
                else => "Codex app-server ready · type a prompt",
            };
        }
        return switch (model.activeSession().agent_connection) {
            .unavailable => "Codex unavailable · no command executed",
            .connecting => "Codex app-server connecting · no command executed",
            .ready => "Codex app-server ready · permission broker active",
            .failed => "Codex app-server failed · no command executed",
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

    pub fn agentMessages(model: *const Model) []const AgentMessageView {
        return model.agent_messages[0..model.agent_message_count];
    }

    pub fn hasAgentMessages(model: *const Model) bool {
        return model.agent_message_count > 0;
    }

    pub fn agentError(model: *const Model) []const u8 {
        return model.agent_error_storage[0..model.agent_error_len];
    }
};

pub const Msg = union(enum) {
    new_session,
    dismiss_new_session,
    choose_terminal,
    choose_agent,
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
    agent_poll: native_sdk.EffectTimer,
    agent_split_resized: f32,
    system_appearance: struct {
        scheme: canvas.ColorScheme,
        high_contrast: bool,
        reduce_motion: bool,
    },
    chrome_changed: native_sdk.WindowChrome,

    /// Platform callbacks dispatch these messages; markup never does.
    pub const view_unbound = .{ "close_active_session", "terminal_session_closed", "agent_session_started", "agent_session_closed", "agent_turn_started", "agent_snapshot_received", "agent_poll", "system_appearance", "chrome_changed" };
};

const dev_markup_reload = builtin.mode == .Debug;
pub const HyperTermApp = native_sdk.UiAppWithFeatures(Model, Msg, .{ .runtime_markup = dev_markup_reload });
pub const Effects = HyperTermApp.Effects;

pub fn update(model: *Model, msg: Msg, fx: *Effects) void {
    switch (msg) {
        .new_session => model.new_session_open = true,
        .dismiss_new_session => model.new_session_open = false,
        .choose_terminal => {
            _ = appendSession(model, .terminal);
            model.new_session_open = false;
        },
        .choose_agent => {
            if (appendSession(model, .agent)) |session_id| {
                requestAgentStart(model, session_id, fx);
            }
            model.new_session_open = false;
        },
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
        .title = if (mode == .terminal) "zsh" else "Agent",
        .icon = if (mode == .terminal) "terminal" else "circle-dot",
        .accessibility_label = if (mode == .terminal) "Terminal session" else "Agent session",
        .close_label = if (mode == .terminal) "Close Terminal session" else "Close Agent session",
    };
    model.session_count += 1;
    model.active_session_id = session_id;
    model.next_session_id +%= 1;
    if (model.next_session_id == 0) model.next_session_id = 1;
    refreshTerminalUrl(model);
    return session_id;
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
    const request_url = writeAgentSessionUrl(model, session_id, storage[0..]) orelse return;
    setAgentConnection(model, session_id, .connecting);
    fx.fetch(.{
        .key = agent_start_effect_key_base + session_id,
        .method = .POST,
        .url = request_url,
        .timeout_ms = 12_000,
        .on_response = Effects.responseMsg(.agent_session_started),
    });
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
    kind: []const u8,
    payload: struct {
        @"type": []const u8,
        role: ?[]const u8 = null,
        text: ?[]const u8 = null,
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
    projectAgentMessages(model, parsed.value.document.blocks);
    model.agent_turn_status = parseAgentTurnStatus(parsed.value.status);
    if (parsed.value.@"error") |message| setAgentError(model, message) else model.agent_error_len = 0;
    if (model.agent_turn_status == .running) scheduleAgentPoll(session_id, fx);
}

fn projectAgentMessages(model: *Model, blocks: []const AgentBlockWire) void {
    for (&model.agent_messages) |*message| message.* = .{};
    model.agent_message_count = 0;
    var message_total: usize = 0;
    for (blocks) |block| {
        if (std.mem.eql(u8, block.kind, "message") and
            std.mem.eql(u8, block.payload.@"type", "message") and
            block.payload.role != null and block.payload.text != null)
        {
            message_total += 1;
        }
    }
    var skip = message_total -| max_agent_messages;
    model.agent_history_clipped = skip > 0;
    for (blocks) |block| {
        if (!std.mem.eql(u8, block.kind, "message") or
            !std.mem.eql(u8, block.payload.@"type", "message")) continue;
        const role_text = block.payload.role orelse continue;
        const text_value = block.payload.text orelse continue;
        if (skip > 0) {
            skip -= 1;
            continue;
        }
        if (model.agent_message_count == max_agent_messages) break;
        const slot = &model.agent_messages[model.agent_message_count];
        slot.id = @intCast(model.agent_message_count + 1);
        slot.role = parseAgentMessageRole(role_text);
        const text_len = utf8BoundedLength(text_value, slot.text_storage.len);
        @memcpy(slot.text_storage[0..text_len], text_value[0..text_len]);
        slot.text_len = text_len;
        slot.truncated = text_len < text_value.len;
        model.agent_message_count += 1;
    }
    model.agent_projection_session_id = model.active_session_id;
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
    for (&model.agent_messages) |*message| message.* = .{};
    model.agent_message_count = 0;
    model.agent_history_clipped = false;
    model.agent_projection_session_id = session_id;
    model.agent_turn_status = .idle;
    model.agent_error_len = 0;
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
    if (std.mem.eql(u8, name, "hyper-term.new-session")) return .new_session;
    if (std.mem.eql(u8, name, "hyper-term.new-terminal")) return .choose_terminal;
    if (std.mem.eql(u8, name, "hyper-term.new-agent")) return .choose_agent;
    if (std.mem.eql(u8, name, "hyper-term.close-session")) return .close_active_session;
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
        return canvas.DesignTokens.theme(.{
            .color_scheme = model.system_scheme,
            .contrast = contrast,
            .density = .compact,
            .reduce_motion = model.reduce_motion,
        });
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
    var model = initialModel();
    if (trustedTerminalUrl(terminal_url)) {
        @memcpy(model.terminal_base_url_storage[0..terminal_url.len], terminal_url);
        model.terminal_base_url_len = terminal_url.len;
        refreshTerminalUrl(&model);
    }
    if (trustedAgentUrl(agent_url)) {
        @memcpy(model.agent_base_url_storage[0..agent_url.len], agent_url);
        model.agent_base_url_len = agent_url.len;
    }
    return model;
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
    const app_state = try HyperTermApp.create(std.heap.page_allocator, .{
        .name = "hyper-term",
        .scene = shell_scene,
        .canvas_label = canvas_label,
        .update_fx = update,
        .tokens_fn = hyperTermTokens,
        .on_command = command,
        .on_appearance = onAppearance,
        .on_chrome = onChrome,
        .view = CompiledHyperTermView.build,
        .web_panes = terminalPanes,
        .markup = if (dev_markup_reload)
            .{ .source = app_markup, .watch_path = "src/app.native", .io = init.io }
        else
            null,
    });
    defer app_state.destroy();
    app_state.model = initialModelWithServices(terminal_url, agent_url);

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
            .navigation = .{ .allowed_origins = &.{ "zero://app", "zero://inline", terminal_gateway_origin } },
        },
    }, init);
}

test {
    _ = @import("tests.zig");
}
