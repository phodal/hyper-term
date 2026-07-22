const std = @import("std");
const native_sdk = @import("native_sdk");
const main = @import("main.zig");

const canvas = native_sdk.canvas;
const testing = std.testing;

fn buildTree(arena: std.mem.Allocator, model: *const main.Model) !main.HyperTermUi.Tree {
    var ui = main.HyperTermUi.init(arena);
    return ui.finalize(main.rootView(&ui, model));
}

fn findByLabel(widget: canvas.Widget, value: []const u8) ?canvas.Widget {
    if (std.mem.eql(u8, widget.semantics.label, value)) return widget;
    for (widget.children) |child| {
        if (findByLabel(child, value)) |found| return found;
    }
    return null;
}

fn findAnyByText(widget: canvas.Widget, value: []const u8) ?canvas.Widget {
    if (std.mem.eql(u8, widget.text, value)) return widget;
    for (widget.children) |child| {
        if (findAnyByText(child, value)) |found| return found;
    }
    return null;
}

fn containsText(widget: canvas.Widget, value: []const u8) bool {
    if (std.mem.indexOf(u8, widget.text, value) != null) return true;
    for (widget.children) |child| {
        if (containsText(child, value)) return true;
    }
    return false;
}

test "provider sign-in opens an ordinary Terminal and copies without executing" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const signed_out =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"login_required","containment":"native_seatbelt"},
        \\  {"id":"codex-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"},
        \\  {"id":"claude-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"}
        \\]
    ;
    var model = main.initialModelWithProviderStatus(terminal_url, agent_url, "", signed_out);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var tree = try buildTree(arena, &model);
    const picker = findByLabel(tree.root, "Choose provider for a new Agent tab").?;
    main.update(&model, tree.msgForPointer(picker.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    const sign_in = findAnyByText(tree.root, "Sign in to Codex in Terminal").?;
    main.update(&model, tree.msgForPointer(sign_in.id, .up).?, &fx);

    try testing.expectEqual(main.SessionMode.terminal, model.activeSession().mode);
    try testing.expectEqual(@as(usize, 2), model.openSessions().len);
    try testing.expectEqual(@as(usize, 1), fx.pendingClipboardCount());
    try testing.expectEqualStrings("codex login", fx.pendingClipboardAt(0).?.text);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());

    main.update(&model, .{ .provider_login_command_copied = .{
        .key = main.provider_login_clipboard_effect_key,
        .op = .write,
        .outcome = .ok,
    } }, &fx);
    tree = try buildTree(arena, &model);
    try testing.expect(findByLabel(tree.root, "Provider sign-in guide") != null);
    try testing.expect(containsText(tree.root, "codex login"));
    try testing.expect(containsText(tree.root, "paste with Command-V"));

    const signed_in =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"authenticated","containment":"native_seatbelt"},
        \\  {"id":"codex-acp","protocol":"acp-v1","readiness":"authenticated","containment":"native_seatbelt"},
        \\  {"id":"claude-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"}
        \\]
    ;
    main.update(&model, .{ .agent_providers_refreshed = .{
        .key = main.agent_provider_refresh_effect_key,
        .status = 200,
        .body = signed_in,
    } }, &fx);
    try testing.expect(!model.hasProviderLoginHint());
}

test "Claude sign-in keeps its command explicit when clipboard fails" {
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const signed_out =
        \\[{"id":"claude-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"}]
    ;
    var model = main.initialModelWithProviderStatus("", agent_url, "", signed_out);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .open_claude_login_terminal, &fx);
    try testing.expectEqualStrings("claude auth login", fx.pendingClipboardAt(0).?.text);
    main.update(&model, .{ .provider_login_command_copied = .{
        .key = main.provider_login_clipboard_effect_key,
        .op = .write,
        .outcome = .failed,
    } }, &fx);
    try testing.expectEqualStrings("Copy failed · type the shown command manually", model.providerLoginStatus());
}
