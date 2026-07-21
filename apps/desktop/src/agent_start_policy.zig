const std = @import("std");

pub const default_timeout_ms: u32 = 15_000;
pub const slow_acp_timeout_ms: u32 = 35_000;

pub fn timeoutMs(provider_id: []const u8) u32 {
    if (std.mem.eql(u8, provider_id, "claude-acp") or
        std.mem.eql(u8, provider_id, "copilot-acp"))
    {
        return slow_acp_timeout_ms;
    }
    return default_timeout_ms;
}

test "ACP adapters receive startup budgets that match adapter behavior" {
    try std.testing.expectEqual(slow_acp_timeout_ms, timeoutMs("claude-acp"));
    try std.testing.expectEqual(slow_acp_timeout_ms, timeoutMs("copilot-acp"));
    try std.testing.expectEqual(default_timeout_ms, timeoutMs("codex"));
    try std.testing.expectEqual(default_timeout_ms, timeoutMs("codex-acp"));
}
