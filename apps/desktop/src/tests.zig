const std = @import("std");
const native_sdk = @import("native_sdk");
const main = @import("main.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const Markup = canvas.MarkupView(main.Model, main.Msg);

fn buildTree(arena: std.mem.Allocator, model: *const main.Model) !main.HyperTermUi.Tree {
    var ui = main.HyperTermUi.init(arena);
    return ui.finalize(main.CompiledHyperTermView.build(&ui, model));
}

fn interpretTree(arena: std.mem.Allocator, model: *const main.Model) !main.HyperTermUi.Tree {
    var view = try Markup.init(arena, main.app_markup);
    var ui = main.HyperTermUi.init(arena);
    return ui.finalize(try view.build(&ui, model));
}

fn findByText(widget: canvas.Widget, kind: canvas.WidgetKind, value: []const u8) ?canvas.Widget {
    if (widget.kind == kind and std.mem.eql(u8, widget.text, value)) return widget;
    for (widget.children) |child| {
        if (findByText(child, kind, value)) |found| return found;
    }
    return null;
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

test "default session is an ordinary terminal" {
    var model = main.initialModel();
    try testing.expectEqual(main.SessionMode.terminal, model.activeSession().mode);
    try testing.expectEqual(@as(usize, 1), model.openSessions().len);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, main.terminal_view_anchor) != null);
    try testing.expect(!containsText(tree.root, "Native Block surface"));
}

test "session bar exposes direct Terminal and Agent creation" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    var model = main.initialModel();
    var tree = try buildTree(arena, &model);
    const terminal_tab = findByText(tree.root, .segmented_control, "zsh 1").?;
    const close_from_menu = tree.msgForContextMenu(terminal_tab.id, 0).?;
    switch (close_from_menu) {
        .close_session => |session_id| try testing.expectEqual(@as(u8, 1), session_id),
        else => return error.TestUnexpectedResult,
    }

    const terminal_item = findByLabel(tree.root, "New Terminal tab").?;
    main.update(&model, tree.msgForPointer(terminal_item.id, .up).?, &fx);
    try testing.expectEqual(main.SessionMode.terminal, model.activeSession().mode);
    try testing.expectEqual(@as(usize, 2), model.openSessions().len);

    tree = try buildTree(arena, &model);
    const agent_item = findByLabel(tree.root, "New Agent tab").?;
    main.update(&model, tree.msgForPointer(agent_item.id, .up).?, &fx);
    try testing.expectEqual(main.SessionMode.agent, model.activeSession().mode);
    try testing.expectEqual(@as(usize, 3), model.openSessions().len);

    tree = try buildTree(arena, &model);
    try testing.expect(containsText(tree.root, "Ask an Agent"));
    try testing.expect(findByLabel(tree.root, main.terminal_view_anchor) != null);
}

test "Agent provider picker creates a provider-bound ACP tab" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithProviders(
        terminal_url,
        agent_url,
        "codex,codex-acp,claude-acp",
    );
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    const picker = findByLabel(tree.root, "Choose Agent provider").?;
    main.update(&model, tree.msgForPointer(picker.id, .up).?, &fx);
    try testing.expect(model.agent_provider_picker_open);

    tree = try buildTree(arena, &model);
    const codex_acp = findAnyByText(tree.root, "Codex ACP").?;
    main.update(&model, tree.msgForPointer(codex_acp.id, .up).?, &fx);

    try testing.expect(!model.agent_provider_picker_open);
    try testing.expectEqual(main.SessionMode.agent, model.activeSession().mode);
    try testing.expectEqual(main.AgentProvider.codex_acp, model.activeSession().agent_provider);
    try testing.expectEqualStrings("Codex ACP", model.activeSession().title);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2&provider=codex-acp",
        fx.pendingFetchAt(0).?.url,
    );
}

test "Agent tabs start the brokered Codex runtime and render readiness" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    try testing.expectEqual(main.AgentConnection.connecting, model.activeSession().agent_connection);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());
    const request = fx.pendingFetchAt(0).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2&provider=codex",
        request.url,
    );

    main.update(&model, .{ .agent_session_started = .{
        .key = main.agent_start_effect_key_base + 2,
        .status = 200,
        .body = "{\"session_id\":2,\"provider\":\"codex\",\"protocol\":\"codex-app-server-v2\",\"status\":\"ready\"}",
    } }, &fx);
    try testing.expectEqual(main.AgentConnection.ready, model.activeSession().agent_connection);
    try testing.expectEqualStrings("Structured Agent ready · type a prompt", model.agentStatus());
    try testing.expectEqual(@as(usize, 2), fx.pendingFetchCount());
    try testing.expectEqual(std.http.Method.GET, fx.pendingFetchAt(1).?.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2",
        fx.pendingFetchAt(1).?.url,
    );
}

test "Agent composer posts a bounded prompt to the active Codex turn" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_turn_status = .ready;
    model.agent_composer_buffer.set("Explain the PTY boundary");
    main.update(&model, .send_agent_prompt, &fx);

    try testing.expectEqual(@as(usize, 2), fx.pendingFetchCount());
    const request = fx.pendingFetchAt(1).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/turn?token=abcdef0123456789abcdef0123456789&session_id=2",
        request.url,
    );
    try testing.expectEqualStrings("Explain the PTY boundary", request.body);
    try testing.expectEqualStrings("", model.agentComposerText());
    try testing.expectEqual(main.AgentTurnStatus.running, model.agent_turn_status);
}

test "accepted artifact blocks drive an isolated Agent preview pane" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"completed","error":null,"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000031","kind":"artifact","trust_class":"isolated_artifact","payload":{"type":"artifact","artifact":{"artifact_id":"55555555-5555-4555-8555-555555555555","source_revision":7,"entrypoint":"/App.tsx","content_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","compiler":{"name":"esbuild-wasm","version":"0.28.1"}}}}
        \\]}}
        ,
    } }, &fx);

    try testing.expect(model.hasGenUiArtifact());
    try testing.expectEqualStrings("55555555", model.genUiArtifactLabel());
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/artifact/55555555-5555-4555-8555-555555555555/preview?token=abcdef0123456789abcdef0123456789&session_id=2#55555555-5555-4555-8555-555555555555",
        model.genUiPreviewUrl(),
    );
    try testing.expectEqual(@as(usize, 0), model.agentBlocks().len);

    var panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 2), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings(main.genui_view_label, panes[1].label);
    try testing.expectEqualStrings(main.genui_view_anchor, panes[1].anchor.?);
    try testing.expectEqualStrings(model.genUiPreviewUrl(), panes[1].url);
    try testing.expectEqual(@as(u64, 7), panes[1].reload_token);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) != null);
    try testing.expect(containsText(tree.root, "accepted · isolated"));
    try testing.expect(containsText(tree.root, "55555555"));
    const tokens = main.hyperTermTokens(&model);
    const sweep = canvas.LayoutAuditSweepOptions{
        .tokens = tokens,
        .min_size = geometry.SizeF.init(main.window_min_width, main.window_min_height),
        .default_size = geometry.SizeF.init(main.window_width, main.window_height),
    };
    try canvas.expectLayoutAuditSweepClean(testing.allocator, tree.root, sweep);
    try canvas.expectA11yAuditSweepClean(testing.allocator, tree.root, .{
        .tokens = tokens,
        .min_size = sweep.min_size,
        .default_size = sweep.default_size,
    });
}

test "Agent snapshot renders trusted operation and approval blocks" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"completed","error":null,"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000001","kind":"task","payload":{"type":"task","title":"Agent"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000002","kind":"message","payload":{"type":"message","role":"user","text":"What changed?"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000003","kind":"message","payload":{"type":"message","role":"agent","text":"The Agent tab now streams **BlockDocument** messages."}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000004","block_revision":3,"kind":"operation","trust_class":"trusted_chrome","payload":{"type":"operation","operation_id":"11111111-1111-4111-8111-111111111111","kind":{"other":"codex_shell"},"summary":"touch forbidden","risk":"external_effect","state":"waiting_human"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000005","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"11111111-1111-4111-8111-111111111111","operation_revision":3,"prompt":"Allow this exact operation once?","decision":null}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(main.AgentTurnStatus.completed, model.agent_turn_status);
    try testing.expectEqual(@as(usize, 4), model.agentBlocks().len);
    try testing.expectEqualStrings("What changed?", model.agentBlocks()[0].content());
    try testing.expectEqualStrings("The Agent tab now streams **BlockDocument** messages.", model.agentBlocks()[1].content());
    try testing.expect(model.agentBlocks()[2].isOperation());
    try testing.expectEqualStrings("Codex shell request", model.agentBlocks()[2].operationKindLabel());
    try testing.expect(model.agentBlocks()[3].isApprovalPending());
    try testing.expectEqual(@as(u64, 3), model.agentBlocks()[3].operation_revision);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "What changed?"));
    try testing.expect(containsText(tree.root, "BlockDocument"));
    try testing.expect(containsText(tree.root, "touch forbidden"));
    try testing.expect(containsText(tree.root, "Allow is unavailable until the Rust sandbox"));
    try testing.expect(findByLabel(tree.root, "Agent prompt composer") != null);
    try testing.expect(findByLabel(tree.root, "Send prompt") != null);
    const tokens = main.hyperTermTokens(&model);
    const sweep = canvas.LayoutAuditSweepOptions{
        .tokens = tokens,
        .min_size = geometry.SizeF.init(main.window_min_width, main.window_min_height),
        .default_size = geometry.SizeF.init(main.window_width, main.window_height),
    };
    try canvas.expectLayoutAuditSweepClean(testing.allocator, tree.root, sweep);
    try canvas.expectA11yAuditSweepClean(testing.allocator, tree.root, .{
        .tokens = tokens,
        .min_size = sweep.min_size,
        .default_size = sweep.default_size,
    });

    const reject = findByText(tree.root, .button, "Reject").?;
    main.update(&model, tree.msgForPointer(reject.id, .up).?, &fx);
    try testing.expect(model.agentPermissionBusy());
    try testing.expectEqual(@as(usize, 2), fx.pendingFetchCount());
    const request = fx.pendingFetchAt(1).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/permission?token=abcdef0123456789abcdef0123456789&session_id=2",
        request.url,
    );
    try testing.expectEqualStrings(
        "{\"operation_id\":\"11111111-1111-4111-8111-111111111111\",\"expected_revision\":3,\"decision\":\"reject_once\"}",
        request.body,
    );

    main.update(&model, .{ .agent_permission_decided = .{
        .key = main.agent_permission_effect_key_base + 2,
        .status = 202,
        .body = "{\"session_id\":2,\"status\":\"running\"}",
    } }, &fx);
    try testing.expect(!model.agentPermissionBusy());
    try testing.expectEqual(main.AgentTurnStatus.running, model.agent_turn_status);
}

test "read-only MCP approvals expose an exact Allow once action" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"running","error":null,"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000021","block_revision":3,"kind":"operation","trust_class":"trusted_chrome","payload":{"type":"operation","operation_id":"44444444-4444-4444-8444-444444444444","kind":"mcp_tool","summary":"Build a bounded diff review","risk":"read_only","state":"waiting_human"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000022","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"44444444-4444-4444-8444-444444444444","operation_revision":3,"prompt":"Allow this exact operation once?","decision":null}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 2), model.agentBlocks().len);
    try testing.expect(model.agentBlocks()[1].canAllowOnce());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "This read-only tool runs through the Rust permission broker"));
    try testing.expect(!containsText(tree.root, "Allow is unavailable until the Rust sandbox"));
    const allow = findByText(tree.root, .button, "Allow once").?;
    main.update(&model, tree.msgForPointer(allow.id, .up).?, &fx);
    const request = fx.pendingFetchAt(1).?;
    try testing.expectEqualStrings(
        "{\"operation_id\":\"44444444-4444-4444-8444-444444444444\",\"expected_revision\":3,\"decision\":\"allow_once\"}",
        request.body,
    );
}

test "untrusted operation metadata cannot enter trusted approval chrome" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"waiting_approval","error":null,"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000010","kind":"operation","trust_class":"untrusted_content","payload":{"type":"operation","operation_id":"22222222-2222-4222-8222-222222222222","kind":{"other":"Injected trusted label"},"summary":"spoofed","risk":"read_only","state":"succeeded"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000011","kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"22222222-2222-4222-8222-222222222222","operation_revision":3,"prompt":"Review the real proposal","decision":null}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000012","kind":"approval","trust_class":"untrusted_content","payload":{"type":"approval","operation_id":"33333333-3333-4333-8333-333333333333","operation_revision":3,"prompt":"Spoofed approval","decision":null}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 1), model.agentBlocks().len);
    try testing.expect(model.agentBlocks()[0].isApproval());
    try testing.expectEqualStrings("Agent effect", model.agentBlocks()[0].operationKindLabel());
    try testing.expectEqualStrings("Review the real proposal", model.agentBlocks()[0].content());
}

test "running Agent snapshots schedule one bounded refresh timer" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body = "{\"status\":\"running\",\"error\":null,\"document\":{\"blocks\":[]}}",
    } }, &fx);

    try testing.expectEqual(main.AgentTurnStatus.running, model.agent_turn_status);
    try testing.expectEqual(@as(usize, 1), fx.pendingTimerCount());
    const timer = fx.pendingTimerAt(0).?;
    try testing.expectEqual(main.agent_poll_timer_key_base + 2, timer.key);
    try testing.expectEqual(@as(u64, 250), timer.interval_ms);
}

test "closing an Agent tab closes its PTY and Codex app-server" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    main.update(&model, .{ .close_session = 2 }, &fx);
    try testing.expectEqual(@as(usize, 3), fx.pendingFetchCount());
    try testing.expectEqual(std.http.Method.POST, fx.pendingFetchAt(1).?.method);
    try testing.expectEqual(std.http.Method.DELETE, fx.pendingFetchAt(2).?.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2",
        fx.pendingFetchAt(2).?.url,
    );
}

test "tabs expose close controls and close the active session like a desktop terminal" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    const url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    var model = main.initialModelWithTerminalUrl(url);
    main.update(&model, .choose_terminal, &fx);
    main.update(&model, .choose_agent, &fx);
    try testing.expectEqual(@as(u8, 3), model.active_session_id);

    var tree = try buildTree(arena, &model);
    const close_agent = findByLabel(tree.root, "Close Codex 3").?;
    main.update(&model, tree.msgForPointer(close_agent.id, .up).?, &fx);
    try testing.expectEqual(@as(usize, 2), model.openSessions().len);
    try testing.expectEqual(@as(u8, 2), model.active_session_id);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());
    const close_request = fx.pendingFetchAt(0).?;
    try testing.expectEqual(std.http.Method.POST, close_request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:47437/terminal/session/close?token=0123456789abcdef0123456789abcdef&session_id=3",
        close_request.url,
    );

    main.update(&model, .{ .close_session = 1 }, &fx);
    try testing.expectEqual(@as(usize, 1), model.openSessions().len);
    try testing.expectEqual(@as(u8, 2), model.active_session_id);

    main.update(&model, .close_active_session, &fx);
    try testing.expectEqual(@as(u32, 1), fx.windowActionState().close_count);
    try testing.expectEqualStrings("main", fx.windowActionState().lastLabel());
}

test "closing an inactive tab preserves the active session" {
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    var model = main.initialModel();
    main.update(&model, .choose_terminal, &fx);
    main.update(&model, .choose_agent, &fx);
    main.update(&model, .{ .select_session = 2 }, &fx);
    main.update(&model, .{ .close_session = 1 }, &fx);

    try testing.expectEqual(@as(usize, 2), model.openSessions().len);
    try testing.expectEqual(@as(u8, 2), model.active_session_id);
    try testing.expectEqual(@as(u8, 2), model.openSessions()[0].id);
    try testing.expectEqual(@as(u8, 3), model.openSessions()[1].id);
}

test "native menu commands map to explicit tab lifecycles" {
    const terminal = main.command("hyper-term.new-terminal") orelse return error.TestUnexpectedResult;
    const agent = main.command("hyper-term.new-agent") orelse return error.TestUnexpectedResult;
    const close = main.command("hyper-term.close-session") orelse return error.TestUnexpectedResult;

    switch (terminal) {
        .choose_terminal => {},
        else => return error.TestUnexpectedResult,
    }
    switch (agent) {
        .choose_agent => {},
        else => return error.TestUnexpectedResult,
    }
    switch (close) {
        .close_active_session => {},
        else => return error.TestUnexpectedResult,
    }
    try testing.expect(main.command("hyper-term.new-session") == null);
}

test "macOS canvas shortcuts preserve terminal and Agent tab lifecycles" {
    const command = canvas.WidgetKeyboardModifiers{ .super = true };
    const shifted_command = canvas.WidgetKeyboardModifiers{ .shift = true, .super = true };

    const terminal = main.onKey(.{ .phase = .key_down, .key = "t", .modifiers = command }) orelse
        return error.TestUnexpectedResult;
    const agent = main.onKey(.{ .phase = .key_down, .key = "n", .modifiers = shifted_command }) orelse
        return error.TestUnexpectedResult;
    const close = main.onKey(.{ .phase = .key_down, .key = "w", .modifiers = command }) orelse
        return error.TestUnexpectedResult;

    try testing.expect(terminal == .choose_terminal);
    try testing.expect(agent == .choose_agent);
    try testing.expect(close == .close_active_session);
    try testing.expect(main.onKey(.{ .phase = .key_down, .key = "w", .modifiers = .{ .control = true } }) == null);
    try testing.expect(main.onKey(.{ .phase = .key_up, .key = "w", .modifiers = command }) == null);
}

test "compiled and hot-reload markup produce the same root" {
    var model = main.initialModel();
    model.session_slots[0].mode = .agent;
    model.session_slots[0].title = "Agent";

    var compiled_arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer compiled_arena.deinit();
    var interpreted_arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer interpreted_arena.deinit();

    const compiled = try buildTree(compiled_arena.allocator(), &model);
    const interpreted = try interpretTree(interpreted_arena.allocator(), &model);
    try testing.expectEqual(compiled.root.id, interpreted.root.id);
    try testing.expectEqual(compiled.root.children.len, interpreted.root.children.len);
}

test "layout and accessibility sweeps stay clean in both modes" {
    inline for ([_]main.SessionMode{ .terminal, .agent }) |mode| {
        var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
        defer arena_state.deinit();

        var model = main.initialModel();
        model.session_slots[0].mode = mode;
        model.session_slots[0].title = if (mode == .terminal) "zsh" else "Agent";
        const tree = try buildTree(arena_state.allocator(), &model);
        const tokens = main.hyperTermTokens(&model);
        const sweep = canvas.LayoutAuditSweepOptions{
            .tokens = tokens,
            .min_size = geometry.SizeF.init(main.window_min_width, main.window_min_height),
            .default_size = geometry.SizeF.init(main.window_width, main.window_height),
        };
        try canvas.expectLayoutAuditSweepClean(testing.allocator, tree.root, sweep);
        try canvas.expectA11yAuditSweepClean(testing.allocator, tree.root, .{
            .tokens = tokens,
            .min_size = sweep.min_size,
            .default_size = sweep.default_size,
        });
    }
}

test "high contrast defers to the Native SDK accessible register" {
    var model = main.initialModel();
    model.high_contrast = true;
    const actual = main.hyperTermTokens(&model);
    const expected = canvas.DesignTokens.theme(.{
        .color_scheme = .dark,
        .contrast = .high,
        .density = .compact,
    });
    try testing.expectEqualDeep(expected.colors, actual.colors);
    try testing.expect(actual.controls.tabs_indicator == .underline);
}

test "terminal web pane accepts only the authenticated fixed loopback shape" {
    try testing.expect(!main.trustedTerminalUrl("https://example.com/?token=0123456789abcdef0123456789abcdef"));
    try testing.expect(!main.trustedTerminalUrl("http://127.0.0.1:47437/?token=short"));
    try testing.expect(!main.trustedTerminalUrl("http://127.0.0.1:47437.evil/?token=0123456789abcdef0123456789abcdef"));

    const url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    var model = main.initialModelWithTerminalUrl(url);
    try testing.expect(model.terminalReady());
    var panes: [1]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 1), main.terminalPanes(&model, &panes));
    try testing.expectEqualStrings(main.terminal_view_label, panes[0].label);
    try testing.expectEqualStrings(main.terminal_view_anchor, panes[0].anchor.?);
    try testing.expectEqualStrings(url ++ "&tab=1", panes[0].url);

    model = main.initialModel();
    try testing.expectEqual(@as(usize, 0), main.terminalPanes(&model, &panes));
}

test "Agent gateway accepts only an authenticated dynamic loopback shape" {
    try testing.expect(!main.trustedAgentUrl("https://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789"));
    try testing.expect(!main.trustedAgentUrl("http://127.0.0.1:0/?token=abcdef0123456789abcdef0123456789"));
    try testing.expect(!main.trustedAgentUrl("http://127.0.0.1:55321.evil/?token=abcdef0123456789abcdef0123456789"));
    try testing.expect(!main.trustedAgentUrl("http://127.0.0.1:55321/?token=short"));
    try testing.expect(main.trustedAgentUrl("http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789"));
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321",
        main.trustedAgentOrigin("http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789").?,
    );
    try testing.expect(main.trustedAgentOrigin("http://127.0.0.1:55321.evil/?token=abcdef0123456789abcdef0123456789") == null);
}

test "new terminal tabs switch reconnect namespaces without exceeding the bound" {
    const url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    var model = main.initialModelWithTerminalUrl(url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_terminal, &fx);
    try testing.expectEqual(@as(usize, 2), model.openSessions().len);
    try testing.expectEqual(@as(u8, 2), model.active_session_id);
    try testing.expectEqualStrings(url ++ "&tab=2", model.terminalUrl());

    main.update(&model, .{ .select_session = 1 }, &fx);
    try testing.expectEqual(@as(u8, 1), model.active_session_id);
    try testing.expectEqualStrings(url ++ "&tab=1", model.terminalUrl());

    for (0..main.max_sessions + 2) |_| main.update(&model, .choose_terminal, &fx);
    try testing.expectEqual(main.max_sessions, model.openSessions().len);
}
