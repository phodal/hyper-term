const std = @import("std");

pub const Provider = enum {
    codex,
    codex_acp,
    claude_acp,
    copilot_acp,

    pub fn id(provider: Provider) []const u8 {
        return switch (provider) {
            .codex => "codex",
            .codex_acp => "codex-acp",
            .claude_acp => "claude-acp",
            .copilot_acp => "copilot-acp",
        };
    }

    pub fn label(provider: Provider) []const u8 {
        return switch (provider) {
            .codex => "Codex",
            .codex_acp => "Codex ACP",
            .claude_acp => "Claude ACP",
            .copilot_acp => "Copilot ACP",
        };
    }

    pub fn protocol(provider: Provider) []const u8 {
        return if (provider == .codex) "codex-app-server-v2" else "acp-v1";
    }

    pub fn loginCommand(provider: Provider) ?[]const u8 {
        return switch (provider) {
            .codex, .codex_acp => "codex login",
            .claude_acp => "claude auth login",
            .copilot_acp => null,
        };
    }

    pub fn loginLabel(provider: Provider) []const u8 {
        return switch (provider) {
            .codex, .codex_acp => "Sign in to Codex",
            .claude_acp => "Sign in to Claude",
            .copilot_acp => "Copilot authenticates when its session starts",
        };
    }
};

pub const Readiness = enum {
    unavailable,
    authenticated,
    available,
    login_required,
    provider_missing,
    probe_failed,
};

pub const LoginCopyState = enum { idle, copying, copied, failed };

pub const LoginGuide = struct {
    provider: ?Provider = null,
    session_id: u8 = 0,
    copy_state: LoginCopyState = .idle,

    pub fn open(guide: *LoginGuide, provider: Provider, session_id: u8) void {
        guide.* = .{ .provider = provider, .session_id = session_id, .copy_state = .copying };
    }

    pub fn clear(guide: *LoginGuide) void {
        guide.* = .{};
    }

    pub fn visible(guide: LoginGuide, active_session_id: u8, is_terminal: bool) bool {
        return guide.provider != null and guide.session_id == active_session_id and is_terminal;
    }

    pub fn command(guide: LoginGuide) []const u8 {
        return if (guide.provider) |provider| provider.loginCommand() orelse "" else "";
    }

    pub fn label(guide: LoginGuide) []const u8 {
        return if (guide.provider) |provider| provider.loginLabel() else "Provider sign-in";
    }

    pub fn status(guide: LoginGuide) []const u8 {
        return switch (guide.copy_state) {
            .idle => "Review the command before running it",
            .copying => "Copying command…",
            .copied => "Copied · paste with Command-V, review, then press Return",
            .failed => "Copy failed · type the shown command manually",
        };
    }
};

pub fn parse(id: []const u8) ?Provider {
    inline for (.{ Provider.codex, Provider.codex_acp, Provider.claude_acp, Provider.copilot_acp }) |provider| {
        if (std.mem.eql(u8, id, provider.id())) return provider;
    }
    return null;
}

pub fn menuLabel(provider: Provider, readiness: Readiness) []const u8 {
    return switch (provider) {
        .codex => switch (readiness) {
            .authenticated => "Codex · App Server · authenticated",
            .login_required => "Codex · App Server · sign in required",
            .probe_failed => "Codex · App Server · readiness failed",
            else => "Codex · App Server · unavailable",
        },
        .codex_acp => switch (readiness) {
            .authenticated => "Codex · ACP · authenticated",
            .login_required => "Codex · ACP · sign in required",
            .probe_failed => "Codex · ACP · readiness failed",
            else => "Codex · ACP · unavailable",
        },
        .claude_acp => switch (readiness) {
            .authenticated => "Claude · ACP · authenticated",
            .login_required => "Claude · ACP · sign in required",
            .probe_failed => "Claude · ACP · readiness failed",
            else => "Claude · ACP · unavailable",
        },
        .copilot_acp => switch (readiness) {
            .available => "Copilot · ACP · auth on session",
            .probe_failed => "Copilot · ACP · readiness failed",
            else => "Copilot · ACP · unavailable",
        },
    };
}

test "provider authentication commands are fixed user-terminal guidance" {
    try std.testing.expectEqualStrings("codex login", Provider.codex.loginCommand().?);
    try std.testing.expectEqualStrings("codex login", Provider.codex_acp.loginCommand().?);
    try std.testing.expectEqualStrings("claude auth login", Provider.claude_acp.loginCommand().?);
    try std.testing.expect(Provider.copilot_acp.loginCommand() == null);
    try std.testing.expectEqual(Provider.codex_acp, parse("codex-acp").?);
    try std.testing.expect(parse("codex-acp --unsafe") == null);
}

test "provider labels preserve protocol and readiness" {
    try std.testing.expectEqualStrings(
        "Codex · ACP · sign in required",
        menuLabel(.codex_acp, .login_required),
    );
    try std.testing.expectEqualStrings(
        "Copilot · ACP · auth on session",
        menuLabel(.copilot_acp, .available),
    );
}

test "login guide is scoped to one ordinary terminal session" {
    var guide = LoginGuide{};
    guide.open(.claude_acp, 3);
    try std.testing.expect(!guide.visible(2, true));
    try std.testing.expect(!guide.visible(3, false));
    try std.testing.expect(guide.visible(3, true));
    try std.testing.expectEqualStrings("claude auth login", guide.command());
    guide.clear();
    try std.testing.expect(!guide.visible(3, true));
}
