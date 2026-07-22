//! Pure projection from Rust-authenticated Agent wire data into bounded Native UI state.
//!
//! This module parses and projects BlockDocument snapshots, stream metadata,
//! Goals, execution contexts, diffs, and pending composer state. It owns no
//! process, filesystem, PTY, network, timer, or WebView effect authority.

const std = @import("std");
const agent_capabilities = @import("agent_capabilities.zig");
const agent_block_view = @import("agent_block_view.zig");
const agent_wire = @import("agent_wire.zig");
const desktop_model = @import("desktop_model.zig");

const AgentBlockWire = agent_wire.Block;
const AgentCapabilitiesWire = agent_wire.Capabilities;
const AgentExecutionContextEventWire = agent_wire.ExecutionContextEvent;
const AgentGoalWire = agent_wire.Goal;
const AgentPlanEntryWire = agent_wire.PlanEntry;
const AgentSnapshotWire = agent_wire.Snapshot;
const AgentToolCallWire = agent_wire.ToolCall;

const AgentBlockView = desktop_model.AgentBlockView;
const AgentConnection = desktop_model.AgentConnection;
const AgentDecision = desktop_model.AgentDecision;
const AgentExecutionMode = desktop_model.AgentExecutionMode;
const AgentGoalStatus = desktop_model.AgentGoalStatus;
const AgentMessageRole = desktop_model.AgentMessageRole;
const AgentOperationState = desktop_model.AgentOperationState;
const AgentRisk = desktop_model.AgentRisk;
const AgentToolStatus = desktop_model.AgentToolStatus;
const AgentTurnStatus = desktop_model.AgentTurnStatus;
const Model = desktop_model.Model;
const PendingAgentPrompt = desktop_model.PendingAgentPrompt;

const agent_context_digest_bytes = desktop_model.agent_context_digest_bytes;
const max_agent_activity_meta_bytes = agent_block_view.max_activity_meta_bytes;
const max_agent_activity_title_bytes = agent_block_view.max_activity_title_bytes;
const max_agent_blocks = desktop_model.max_agent_blocks;
const max_agent_capability_id_bytes = desktop_model.max_agent_capability_id_bytes;
const max_agent_context_id_bytes = desktop_model.max_agent_context_id_bytes;
const max_agent_execution_contexts = desktop_model.max_agent_execution_contexts;
const max_agent_goal_step_columns = desktop_model.max_agent_goal_step_columns;
const max_agent_operation_id_bytes = desktop_model.max_agent_operation_id_bytes;

pub fn findAgentAppendTarget(model: *Model, block_id: []const u8) ?*AgentBlockView {
    const projected_id = stableAgentBlockId(block_id, 0);
    for (model.agent_blocks[0..model.agent_block_count]) |*block| {
        if (block.id != projected_id) continue;
        if (block.kind == .message or
            (block.kind == .tool_call and block.has_reasoning and block.activity_count == 0)) return block;
    }
    return null;
}

pub fn appendAgentBlockContent(view: *AgentBlockView, value: []const u8) void {
    const remaining = view.content_storage.len - view.content_len;
    const length = utf8BoundedLength(value, remaining);
    @memcpy(view.content_storage[view.content_len..][0..length], value[0..length]);
    view.content_len += length;
    view.truncated = view.truncated or length < value.len;
}

pub fn applyAgentSnapshotPayload(model: *Model, session_id: u8, body: []const u8) bool {
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

pub fn projectPendingAgentOperation(model: *Model, operation_id: ?[]const u8) void {
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

pub fn projectAgentCapabilities(model: *Model, capabilities: AgentCapabilitiesWire) void {
    agent_capabilities.project(model, &model.session_slots[activeSessionIndex(model)].agent_capabilities, capabilities);
}

fn activeSessionIndex(model: *const Model) usize {
    for (model.openSessions(), 0..) |session, index| {
        if (session.id == model.active_session_id) return index;
    }
    return 0;
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

pub fn projectAgentGoal(model: *Model, wire: ?AgentGoalWire) void {
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

pub fn utf8DisplayColumnPrefixLength(value: []const u8, maximum_columns: usize) usize {
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

pub fn validOperationId(value: []const u8) bool {
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

pub fn parseAgentOperationState(value: []const u8) AgentOperationState {
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

pub fn parseAgentTurnStatus(value: []const u8) AgentTurnStatus {
    return parseAgentTurnStatusStrict(value) orelse .idle;
}

pub fn parseAgentTurnStatusStrict(value: []const u8) ?AgentTurnStatus {
    if (std.mem.eql(u8, value, "ready")) return .ready;
    if (std.mem.eql(u8, value, "running")) return .running;
    if (std.mem.eql(u8, value, "cancelling")) return .cancelling;
    if (std.mem.eql(u8, value, "completed")) return .completed;
    if (std.mem.eql(u8, value, "waiting_approval")) return .waiting_approval;
    if (std.mem.eql(u8, value, "failed")) return .failed;
    return null;
}

pub fn resetAgentProjection(model: *Model, session_id: u8) void {
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

pub fn setAgentError(model: *Model, message: []const u8) void {
    const length = utf8BoundedLength(message, model.agent_error_storage.len);
    @memcpy(model.agent_error_storage[0..length], message[0..length]);
    model.agent_error_len = length;
}

pub fn pendingAgentPrompt(model: *Model, session_id: u8) ?*PendingAgentPrompt {
    for (model.session_slots[0..model.session_count], 0..) |session, index| {
        if (session.id == session_id) return &model.agent_pending_prompts[index];
    }
    return null;
}

pub fn reconcilePendingAgentPrompt(model: *Model, session_id: u8) void {
    switch (model.agent_turn_status) {
        .failed => restorePendingAgentPrompt(model, session_id),
        .completed => if (pendingAgentPrompt(model, session_id)) |pending| pending.clear(),
        else => {},
    }
}

pub fn restorePendingAgentPrompt(model: *Model, session_id: u8) void {
    const pending = pendingAgentPrompt(model, session_id) orelse return;
    defer pending.clear();
    if (session_id != model.active_session_id or pending.len == 0) return;
    if (model.agent_composer_buffer.text().len == 0) {
        model.agent_composer_buffer.set(pending.text());
    }
}

pub fn utf8BoundedLength(value: []const u8, maximum: usize) usize {
    var end = @min(value.len, maximum);
    while (end > 0 and !std.unicode.utf8ValidateSlice(value[0..end])) end -= 1;
    return end;
}

pub fn setAgentConnection(model: *Model, session_id: u8, connection: AgentConnection) void {
    for (model.session_slots[0..model.session_count]) |*session| {
        if (session.id == session_id and session.mode == .agent) {
            session.agent_connection = connection;
            return;
        }
    }
}
