//! Bounded Agent transport and response router for the Native desktop shell.
//!
//! The router may schedule Native SDK HTTP, stream, clipboard-independent, and
//! timer effects against the authenticated loopback Rust gateway. It never
//! spawns providers, executes commands, reads files, or grants permissions;
//! those authorities remain in Rust.

const std = @import("std");
const native_sdk = @import("native_sdk");
const agent_projection = @import("agent_projection.zig");
const agent_provider = @import("agent_provider.zig");
const agent_start_policy = @import("agent_start_policy.zig");
const agent_wire = @import("agent_wire.zig");
const desktop_model = @import("desktop_model.zig");

const canvas = native_sdk.canvas;

const AgentAttentionResponseWire = agent_wire.AttentionResponse;
const AgentCapabilitiesResponseWire = agent_wire.CapabilitiesResponse;
const AgentPatchWire = agent_wire.Patch;
const AgentStreamFrameWire = agent_wire.StreamFrame;
const AgentTier2PreviewWire = agent_wire.Tier2Preview;
const AgentTier2ResultWire = agent_wire.Tier2Result;
const AgentTier2ResultsWire = agent_wire.Tier2Results;

const AgentConfigChoiceView = desktop_model.AgentConfigChoiceView;
const AgentConfigOptionView = desktop_model.AgentConfigOptionView;
const AgentGoalAction = desktop_model.AgentGoalAction;
const AgentTier2ResultView = desktop_model.AgentTier2ResultView;
const Model = desktop_model.Model;
const Session = desktop_model.Session;

const applyAgentSnapshotPayload = agent_projection.applyAgentSnapshotPayload;
const appendAgentBlockContent = agent_projection.appendAgentBlockContent;
const findAgentAppendTarget = agent_projection.findAgentAppendTarget;
const parseAgentOperationState = agent_projection.parseAgentOperationState;
const parseAgentTurnStatus = agent_projection.parseAgentTurnStatus;
const parseAgentTurnStatusStrict = agent_projection.parseAgentTurnStatusStrict;
const pendingAgentPrompt = agent_projection.pendingAgentPrompt;
const projectAgentCapabilities = agent_projection.projectAgentCapabilities;
const projectAgentGoal = agent_projection.projectAgentGoal;
const projectPendingAgentOperation = agent_projection.projectPendingAgentOperation;
const reconcilePendingAgentPrompt = agent_projection.reconcilePendingAgentPrompt;
const resetAgentProjection = agent_projection.resetAgentProjection;
const restorePendingAgentPrompt = agent_projection.restorePendingAgentPrompt;
const setAgentConnection = agent_projection.setAgentConnection;
const setAgentError = agent_projection.setAgentError;
const utf8BoundedLength = agent_projection.utf8BoundedLength;
const validOperationId = agent_projection.validOperationId;
const parseAgentProvider = agent_provider.parse;

const agent_effect_url_capacity = desktop_model.agent_url_capacity + 64;
const max_agent_attention_status_bytes: usize = 4 * 1024;
const max_agent_operation_id_bytes = desktop_model.max_agent_operation_id_bytes;
const max_agent_prompt_bytes = desktop_model.max_agent_prompt_bytes;
const max_agent_tier2_files = desktop_model.max_agent_tier2_files;
const max_agent_tier2_path_bytes = desktop_model.max_agent_tier2_path_bytes;
const max_agent_tier2_results = desktop_model.max_agent_tier2_results;
const max_sessions = desktop_model.max_sessions;

pub const agent_start_effect_key_base: u64 = 0x4854_4100;
pub const agent_close_effect_key_base: u64 = 0x4854_4200;
pub const agent_turn_effect_key_base: u64 = 0x4854_4400;
pub const agent_snapshot_effect_key_base: u64 = 0x4854_4500;
pub const agent_poll_timer_key_base: u64 = 0x4854_4600;
pub const agent_permission_effect_key_base: u64 = 0x4854_4700;
pub const agent_config_effect_key_base: u64 = 0x4854_4800;
pub const agent_stream_effect_key_base: u64 = 0x4854_4900;
pub const agent_tier2_results_effect_key_base: u64 = 0x4854_4a00;
pub const agent_tier2_preview_effect_key_base: u64 = 0x4854_4b00;
pub const agent_tier2_review_effect_key_base: u64 = 0x4854_4c00;
pub const agent_tier2_discard_effect_key_base: u64 = 0x4854_4d00;
pub const agent_cancel_effect_key_base: u64 = 0x4854_4f00;
pub const agent_goal_effect_key_base: u64 = 0x4854_5000;
pub const agent_attention_effect_key: u64 = 0x4854_5200;
pub const agent_attention_poll_timer_key: u64 = 0x4854_5300;

pub fn Router(comptime Effects: type) type {
    return struct {
        pub fn hasAgentSessions(model: *const Model) bool {
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

        pub fn requestAgentAttention(model: *Model, fx: *Effects) void {
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

        pub fn applyAgentAttentionResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn findSession(model: *Model, session_id: u8) ?*Session {
            for (model.session_slots[0..model.session_count]) |*session| {
                if (session.id == session_id) return session;
            }
            return null;
        }

        pub fn acknowledgeSessionAttention(model: *Model, session_id: u8) void {
            const session = findSession(model, session_id) orelse return;
            session.acknowledged_attention_status = session.agent_attention_status;
            session.acknowledged_attention_revision = session.agent_attention_revision;
        }

        pub fn requestAgentStart(model: *Model, session_id: u8, fx: *Effects) void {
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

        pub fn requestAgentClose(model: *const Model, session_id: u8, fx: *Effects) void {
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

        pub fn requestAgentTier2Preview(model: *Model, operation_id: []const u8, fx: *Effects) void {
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

        pub fn requestAgentTier2Review(model: *Model, operation_id: []const u8, fx: *Effects) void {
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

        pub fn requestAgentTier2Discard(model: *Model, operation_id: []const u8, fx: *Effects) void {
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

        pub fn applyAgentStartResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn requestAgentTurn(model: *Model, fx: *Effects) void {
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

        pub fn editAgentGoal(model: *Model) void {
            if (model.agentGoalActionDisabled()) return;
            const current = std.mem.trim(u8, model.agent_composer_buffer.text(), " \t\r\n");
            if (current.len > 0 and !std.mem.startsWith(u8, current, "/goal ")) return;
            var storage: [max_agent_prompt_bytes]u8 = undefined;
            const goal_command = std.fmt.bufPrint(&storage, "/goal {s}", .{model.agent_goal.objective()}) catch return;
            model.agent_composer_buffer.set(goal_command);
            model.agent_goal_editing = true;
            model.agent_goal_menu_open = false;
        }

        pub fn requestAgentGoalAction(model: *Model, action: AgentGoalAction, fx: *Effects) void {
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

        pub fn applyAgentGoalResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn applyAgentTurnResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn requestAgentCancel(model: *Model, fx: *Effects) void {
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

        pub fn applyAgentCancelResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn requestAgentPermission(model: *Model, operation_id: []const u8, decision: []const u8, fx: *Effects) void {
            if (model.agent_permission_in_flight_session_id != 0 or
                !validOperationId(operation_id)) return;
            const block = for (model.agentBlocks()) |*candidate| {
                if (candidate.isApprovalPending() and
                    std.mem.eql(u8, candidate.operationId(), operation_id)) break candidate;
            } else return;
            if (block.operation_revision == 0 or
                (std.mem.eql(u8, decision, "allow_once") and !block.approval_detail_valid)) return;
            const session = model.activeSession();
            if (session.mode != .agent or session.agent_connection != .ready) return;
            var url_storage: [agent_effect_url_capacity + 16]u8 = undefined;
            const request_url = writeAgentPermissionUrl(model, session.id, url_storage[0..]) orelse return;
            var body_storage: [256]u8 = undefined;
            const body = if (block.approval_detail_bound)
                std.fmt.bufPrint(
                    body_storage[0..],
                    "{{\"operation_id\":\"{s}\",\"expected_revision\":{d},\"approval_detail_digest\":\"{s}\",\"decision\":\"{s}\"}}",
                    .{ block.operationId(), block.operation_revision, block.approvalDetailDigest(), decision },
                ) catch return
            else
                std.fmt.bufPrint(
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

        pub fn applyAgentPermissionResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn toggleAgentConfigPicker(model: *Model, index: u8) void {
            model.agent_command_picker_open = false;
            for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
                option.picker_open = option.index == index and !option.picker_open;
            }
        }

        pub fn closeAgentConfigPickers(model: *Model) void {
            for (model.agent_config_options[0..model.agent_config_option_count]) |*option| {
                option.picker_open = false;
            }
        }

        pub fn requestAgentConfig(model: *Model, action_id: u16, fx: *Effects) void {
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

        pub fn applyAgentConfigResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn insertAgentCommand(model: *Model, index: u8) void {
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

        pub fn requestActiveAgentStream(model: *Model, fx: *Effects) void {
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

        pub fn cancelAgentStream(model: *Model, session_id: u8, fx: *Effects) void {
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

        pub fn applyAgentTier2ResultsResponse(model: *Model, response: native_sdk.EffectResponse) void {
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

        pub fn applyAgentTier2PreviewResponse(model: *Model, response: native_sdk.EffectResponse) void {
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

        pub fn applyAgentTier2ReviewResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn applyAgentTier2DiscardResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn applyAgentSnapshotResponse(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn applyAgentStreamLine(model: *Model, line: native_sdk.EffectLine, fx: *Effects) void {
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

        pub fn applyAgentStreamClosed(model: *Model, response: native_sdk.EffectResponse, fx: *Effects) void {
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

        pub fn requestAgentComposerFocus(model: *Model) void {
            model.agent_composer_focus_requested =
                model.activeSession().mode == .agent and
                model.activeSession().agent_connection == .ready and
                !model.agent_search_open;
        }
    };
}
