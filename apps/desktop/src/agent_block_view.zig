//! Bounded presentation model for Rust-authenticated Agent document blocks.
//!
//! This module contains no effects, protocol decoding, command execution, or
//! WebView authority. `main.zig` projects trusted gateway state into these
//! fixed-capacity values and the Native view renders their derived labels.

const std = @import("std");

pub const max_block_bytes: usize = 8 * 1024;
pub const max_operation_id_bytes: usize = 36;
pub const max_operation_kind_bytes: usize = 96;
pub const max_activity_title_bytes: usize = 512;
pub const max_activity_meta_bytes: usize = 512;
pub const max_approval_detail_bytes: usize = 512;
pub const approval_detail_digest_bytes: usize = 64;
pub const max_diff_files: usize = 8;
pub const max_diff_path_bytes: usize = 256;

pub const MessageRole = enum { user, agent, system, thought };

pub const BlockKind = enum { message, operation, approval, tool_call, plan };

pub const ToolStatus = enum { pending, in_progress, completed, failed };

pub const Risk = enum {
    read_only,
    workspace_write,
    external_effect,
    destructive,
    unknown,
};

pub const OperationState = enum {
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

pub const Decision = enum { none, allow_once, reject_once, cancelled, other };

pub const DiffFileView = struct {
    path_storage: [max_diff_path_bytes]u8 = [_]u8{0} ** max_diff_path_bytes,
    path_len: usize = 0,
    added_lines: u64 = 0,
    removed_lines: u64 = 0,

    pub fn path(file: *const DiffFileView) []const u8 {
        return file.path_storage[0..file.path_len];
    }
};

pub const BlockView = struct {
    id: u64 = 0,
    kind: BlockKind = .message,
    role: MessageRole = .agent,
    content_storage: [max_block_bytes]u8 = [_]u8{0} ** max_block_bytes,
    content_len: usize = 0,
    truncated: bool = false,
    operation_id_storage: [max_operation_id_bytes]u8 = [_]u8{0} ** max_operation_id_bytes,
    operation_id_len: usize = 0,
    operation_kind_storage: [max_operation_kind_bytes]u8 = [_]u8{0} ** max_operation_kind_bytes,
    operation_kind_len: usize = 0,
    title_storage: [max_activity_title_bytes]u8 = [_]u8{0} ** max_activity_title_bytes,
    title_len: usize = 0,
    meta_storage: [max_activity_meta_bytes]u8 = [_]u8{0} ** max_activity_meta_bytes,
    meta_len: usize = 0,
    expanded: bool = false,
    tool_status: ToolStatus = .pending,
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
    diff_files: [max_diff_files]DiffFileView = [_]DiffFileView{.{}} ** max_diff_files,
    diff_file_count: usize = 0,
    diff_files_truncated: bool = false,
    operation_revision: u64 = 0,
    approval_detail_storage: [max_approval_detail_bytes]u8 = [_]u8{0} ** max_approval_detail_bytes,
    approval_detail_len: usize = 0,
    approval_detail_digest_storage: [approval_detail_digest_bytes]u8 = [_]u8{0} ** approval_detail_digest_bytes,
    approval_detail_bound: bool = false,
    approval_detail_valid: bool = false,
    allow_once_available: bool = false,
    tier2_isolated: bool = false,
    risk: Risk = .unknown,
    state: OperationState = .proposed,
    decision: Decision = .none,

    pub fn content(block: *const BlockView) []const u8 {
        return block.content_storage[0..block.content_len];
    }

    pub fn roleLabel(block: *const BlockView) []const u8 {
        return switch (block.role) {
            .user => "You",
            .agent => "Agent",
            .system => "System",
            .thought => "Plan",
        };
    }

    pub fn isMessage(block: *const BlockView) bool {
        return block.kind == .message;
    }

    pub fn isUserMessage(block: *const BlockView) bool {
        return block.kind == .message and block.role == .user;
    }

    pub fn isThoughtMessage(block: *const BlockView) bool {
        return block.kind == .message and block.role == .thought;
    }

    pub fn isSystemMessage(block: *const BlockView) bool {
        return block.kind == .message and block.role == .system;
    }

    pub fn isOperation(block: *const BlockView) bool {
        return block.kind == .operation;
    }

    pub fn isApproval(block: *const BlockView) bool {
        return block.kind == .approval;
    }

    pub fn isActivity(block: *const BlockView) bool {
        return block.kind == .tool_call or block.kind == .plan;
    }

    pub fn activityTitle(block: *const BlockView) []const u8 {
        return block.title_storage[0..block.title_len];
    }

    pub fn activityMeta(block: *const BlockView) []const u8 {
        return block.meta_storage[0..block.meta_len];
    }

    pub fn hasActivityDetails(block: *const BlockView) bool {
        return block.content_len > 0;
    }

    pub fn diffFiles(block: *const BlockView) []const DiffFileView {
        return block.diff_files[0..block.diff_file_count];
    }

    pub fn isApprovalPending(block: *const BlockView) bool {
        return block.kind == .approval and block.decision == .none;
    }

    pub fn canAllowOnce(block: *const BlockView) bool {
        return block.isApprovalPending() and block.allow_once_available and block.approval_detail_valid and
            (block.isBrokeredMcpReview() or block.isWorkspaceReview() or block.isTier2TerminalReview());
    }

    pub fn isBrokeredMcpReview(block: *const BlockView) bool {
        return block.risk == .read_only and
            std.mem.eql(u8, block.operationKindLabel(), "MCP tool");
    }

    pub fn isWorkspaceReview(block: *const BlockView) bool {
        return block.risk == .workspace_write and
            std.mem.eql(u8, block.operationKindLabel(), "Workspace edit");
    }

    pub fn isTier2TerminalReview(block: *const BlockView) bool {
        return block.tier2_isolated and block.risk == .external_effect and
            std.mem.eql(u8, block.operationKindLabel(), "Shell command");
    }

    pub fn approvalBoundaryLabel(block: *const BlockView) []const u8 {
        if (block.isWorkspaceReview()) return "Rust-verified Diff · durable apply";
        if (block.isBrokeredMcpReview()) return "Brokered read-only tool · receipt recorded";
        if (block.isTier2TerminalReview()) return "Isolated Tier 2 command · no ordinary PTY access";
        return "Allow unavailable until Rust can enforce this effect.";
    }

    pub fn approvalDetail(block: *const BlockView) []const u8 {
        return block.approval_detail_storage[0..block.approval_detail_len];
    }

    pub fn approvalDetailDigest(block: *const BlockView) []const u8 {
        if (!block.approval_detail_bound) return "";
        return &block.approval_detail_digest_storage;
    }

    pub fn operationId(block: *const BlockView) []const u8 {
        return block.operation_id_storage[0..block.operation_id_len];
    }

    pub fn operationKindLabel(block: *const BlockView) []const u8 {
        return block.operation_kind_storage[0..block.operation_kind_len];
    }

    pub fn riskLabel(block: *const BlockView) []const u8 {
        return switch (block.risk) {
            .read_only => "read only",
            .workspace_write => "workspace write",
            .external_effect => "external effect",
            .destructive => "destructive",
            .unknown => "unknown risk",
        };
    }

    pub fn stateLabel(block: *const BlockView) []const u8 {
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

    pub fn approvalTitle(block: *const BlockView) []const u8 {
        return switch (block.decision) {
            .none => "Approval required",
            .allow_once => "Allowed once",
            .reject_once => "Request rejected",
            .cancelled => "Request cancelled",
            .other => "Approval resolved",
        };
    }

    pub fn decisionLabel(block: *const BlockView) []const u8 {
        return switch (block.decision) {
            .none => "pending",
            .allow_once => "allowed once",
            .reject_once => "rejected once",
            .cancelled => "cancelled",
            .other => "resolved",
        };
    }
};

fn containsAsciiInsensitive(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var start: usize = 0;
    while (start + needle.len <= haystack.len) : (start += 1) {
        var offset: usize = 0;
        while (offset < needle.len and
            std.ascii.toLower(haystack[start + offset]) == std.ascii.toLower(needle[offset])) : (offset += 1)
        {}
        if (offset == needle.len) return true;
    }
    return false;
}

pub fn matchesQuery(block: *const BlockView, query: []const u8) bool {
    if (containsAsciiInsensitive(block.content(), query)) return true;
    switch (block.kind) {
        .message => if (containsAsciiInsensitive(block.roleLabel(), query)) return true,
        .tool_call, .plan => if (containsAsciiInsensitive(block.activityTitle(), query) or
            containsAsciiInsensitive(block.activityMeta(), query)) return true,
        .operation => if (containsAsciiInsensitive(block.operationKindLabel(), query) or
            containsAsciiInsensitive(block.operationId(), query) or
            containsAsciiInsensitive(block.riskLabel(), query) or
            containsAsciiInsensitive(block.stateLabel(), query)) return true,
        .approval => if (containsAsciiInsensitive(block.approvalTitle(), query) or
            containsAsciiInsensitive(block.operationKindLabel(), query) or
            containsAsciiInsensitive(block.operationId(), query) or
            containsAsciiInsensitive(block.riskLabel(), query) or
            containsAsciiInsensitive(block.stateLabel(), query)) return true,
    }
    for (block.diffFiles()) |*file| {
        if (containsAsciiInsensitive(file.path(), query)) return true;
    }
    return false;
}

test "only Rust-enforceable approvals expose Allow once" {
    var workspace: BlockView = .{
        .kind = .approval,
        .risk = .workspace_write,
        .allow_once_available = true,
        .approval_detail_valid = true,
    };
    const label = "Workspace edit";
    @memcpy(workspace.operation_kind_storage[0..label.len], label);
    workspace.operation_kind_len = label.len;
    try std.testing.expect(workspace.canAllowOnce());

    var external: BlockView = .{
        .kind = .approval,
        .risk = .external_effect,
        .allow_once_available = true,
    };
    const opaque_label = "Opaque effect";
    @memcpy(external.operation_kind_storage[0..opaque_label.len], opaque_label);
    external.operation_kind_len = opaque_label.len;
    try std.testing.expect(!external.canAllowOnce());
}
