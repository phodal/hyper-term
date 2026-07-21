//! Bounded JSON contracts projected by the Rust Agent gateway.
//!
//! These types describe untrusted wire data only. They never execute commands,
//! read files, or own UI state; `main.zig` validates and projects them into the
//! Native SDK model.

const std = @import("std");

pub const AttentionSession = struct {
    session_id: u16,
    provider: []const u8,
    status: []const u8,
    document_revision: u64,
};

pub const AttentionResponse = struct {
    sessions: []const AttentionSession,
};

pub const ExecutionContextReceipt = struct {
    schema_version: u16,
    context_id: []const u8,
    context_revision: u64,
    mode: []const u8,
    context_digest: []const u8,
    environment_digest: []const u8,
    clear_inherited: bool,
    bindings: []const std.json.Value = &.{},
    credential_bindings: []const std.json.Value = &.{},
};

pub const ExecutionContextEvent = struct {
    event_id: []const u8,
    causation_id: ?[]const u8 = null,
    correlation_id: ?[]const u8 = null,
    payload: struct {
        type: []const u8,
        context: struct {
            provider_id: []const u8,
            protocol: []const u8,
            thread_id: []const u8,
            receipts: []const ExecutionContextReceipt,
        },
    },
};

pub const ConfigChoice = struct {
    value: []const u8,
    name: []const u8,
};

pub const ConfigOption = struct {
    id: []const u8,
    name: []const u8,
    kind: std.json.Value,
    choices: []const ConfigChoice = &.{},
};

pub const Command = struct {
    name: []const u8,
    description: ?[]const u8 = null,
};

pub const Capabilities = struct {
    config_options: []const ConfigOption = &.{},
    available_commands: []const Command = &.{},
};

pub const PatchOperation = struct {
    type: []const u8,
    block_id: ?[]const u8 = null,
    text: ?[]const u8 = null,
};

pub const Patch = struct {
    stream_sequence: u64,
    base_revision: u64,
    target_revision: u64,
    operations: []const PatchOperation,
};

pub const Goal = struct {
    objective: []const u8,
    status: []const u8,
    token_budget: ?i64 = null,
    tokens_used: i64 = 0,
    time_used_seconds: i64 = 0,
};

pub const StreamFrame = struct {
    type: []const u8,
    status: ?[]const u8 = null,
    @"error": ?[]const u8 = null,
    history_restored: ?bool = null,
    pending_operation_id: ?[]const u8 = null,
    document_revision: ?u64 = null,
    capabilities: Capabilities = .{},
    goal: ?Goal = null,
    patch: ?Patch = null,
    target_revision: ?u64 = null,
    reason: ?[]const u8 = null,
};

pub const ToolContent = struct {
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

pub const ToolLocation = struct {
    path: []const u8,
    line: ?u32 = null,
};

pub const ToolCall = struct {
    tool_call_id: []const u8,
    title: []const u8,
    kind: []const u8,
    status: []const u8,
    content: []const ToolContent,
    locations: []const ToolLocation,
    raw_input: ?[]const u8 = null,
    raw_output: ?[]const u8 = null,
};

pub const PlanEntry = struct {
    content: []const u8,
    priority: []const u8,
    status: []const u8,
};

pub const Block = struct {
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
        required_capabilities: ?[]const []const u8 = null,
        prompt: ?[]const u8 = null,
        options: ?[]const []const u8 = null,
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
        call: ?ToolCall = null,
        entries: ?[]const PlanEntry = null,
    },
};

pub const Snapshot = struct {
    session_id: ?u8 = null,
    status: []const u8,
    @"error": ?[]const u8 = null,
    history_restored: bool = false,
    pending_operation_id: ?[]const u8 = null,
    capabilities: Capabilities = .{},
    goal: ?Goal = null,
    context: ?ExecutionContextEvent = null,
    document: struct {
        revision: u64 = 0,
        blocks: []const Block,
    },
};

pub const CapabilitiesResponse = struct {
    session_id: u8,
    capabilities: Capabilities,
};

pub const Tier2File = struct {
    kind: []const u8,
    path: []const u8,
    bytes: u64,
};

pub const Tier2Acceptance = struct {
    operation_id: []const u8,
    operation_revision: u64,
    state: []const u8,
};

pub const Tier2Result = struct {
    source_operation_id: []const u8,
    changed_bytes: u64,
    changed_files: []const Tier2File,
    acceptance: ?Tier2Acceptance = null,
};

pub const Tier2Results = struct {
    results: []const Tier2Result,
};

pub const Tier2PreviewHunk = struct {
    patch: []const u8,
    truncated: bool = false,
};

pub const Tier2PreviewChange = struct {
    target_path: []const u8,
    deleted: bool = false,
    binary: bool = false,
    base_bytes: u64 = 0,
    proposed_bytes: u64 = 0,
    proposed_digest: []const u8 = "",
    hunks: []const Tier2PreviewHunk,
    truncated: bool = false,
};

pub const Tier2Preview = struct {
    source_operation_id: []const u8,
    changes: []const Tier2PreviewChange,
    truncated: bool = false,
};

pub const ProviderStatus = struct {
    id: []const u8,
    protocol: []const u8,
    readiness: []const u8,
    containment: []const u8,
};

test "snapshot and streaming patch contracts parse the Rust projection" {
    const snapshot_source =
        \\{
        \\  "session_id": 2,
        \\  "status": "ready",
        \\  "document": {
        \\    "revision": 4,
        \\    "blocks": [{
        \\      "block_id": "message-1",
        \\      "block_revision": 1,
        \\      "kind": "message",
        \\      "payload": {"type": "message", "role": "agent", "text": "hello"}
        \\    }]
        \\  }
        \\}
    ;
    const snapshot = try std.json.parseFromSlice(
        Snapshot,
        std.testing.allocator,
        snapshot_source,
        .{ .ignore_unknown_fields = true },
    );
    defer snapshot.deinit();
    try std.testing.expectEqual(@as(u64, 4), snapshot.value.document.revision);
    try std.testing.expectEqualStrings("hello", snapshot.value.document.blocks[0].payload.text.?);

    const frame_source =
        \\{
        \\  "type": "patch",
        \\  "document_revision": 5,
        \\  "patch": {
        \\    "stream_sequence": 9,
        \\    "base_revision": 4,
        \\    "target_revision": 5,
        \\    "operations": [{"type": "append", "block_id": "message-1", "text": " world"}]
        \\  }
        \\}
    ;
    const frame = try std.json.parseFromSlice(
        StreamFrame,
        std.testing.allocator,
        frame_source,
        .{ .ignore_unknown_fields = true },
    );
    defer frame.deinit();
    try std.testing.expectEqual(@as(u64, 9), frame.value.patch.?.stream_sequence);
    try std.testing.expectEqualStrings(" world", frame.value.patch.?.operations[0].text.?);
}
