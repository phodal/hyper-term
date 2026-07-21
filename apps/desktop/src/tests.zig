const std = @import("std");
const native_sdk = @import("native_sdk");
const main = @import("main.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const Markup = canvas.MarkupView(main.Model, main.Msg);

fn buildTree(arena: std.mem.Allocator, model: *const main.Model) !main.HyperTermUi.Tree {
    var ui = main.HyperTermUi.init(arena);
    return ui.finalize(main.rootView(&ui, model));
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

fn widgetCount(widget: canvas.Widget) usize {
    var count: usize = 1;
    for (widget.children) |child| count += widgetCount(child);
    return count;
}

fn pendingFetchIndexByKey(fx: *main.Effects, key: u64) ?usize {
    for (0..fx.pendingFetchCount()) |index| {
        if (fx.pendingFetchAt(index).?.key == key) return index;
    }
    return null;
}

test "default session is an ordinary terminal" {
    var model = main.initialModel();
    try testing.expectEqualStrings("hidden_inset", @tagName(main.shell_scene.windows[0].titlebar));
    try testing.expectEqual(main.SessionMode.terminal, model.activeSession().mode);
    try testing.expectEqual(@as(usize, 1), model.openSessions().len);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, main.terminal_view_anchor) != null);
    try testing.expect(!containsText(tree.root, "Native Block surface"));
}

test "desktop defers WebViews until native glass and mounts GenUI on demand" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const capsule_url = "http://127.0.0.1:55321/agent/workbench/?surface=capsule&token=abcdef0123456789abcdef0123456789";
    const allowed_origins = [_][]const u8{
        "zero://inline",
        "http://127.0.0.1:47437",
        "http://127.0.0.1:55321",
    };

    const app_state = try main.HyperTermApp.create(std.heap.page_allocator, .{
        .name = "hyper-term-deferred-webview-test",
        .scene = main.shell_scene,
        .canvas_label = main.canvas_label,
        .update_fx = main.update,
        .tokens_fn = main.hyperTermTokens,
        .view = main.rootView,
        .web_panes = main.desktopPanes,
    });
    defer app_state.destroy();
    app_state.model = main.initialModelWithTerminalUrl(terminal_url);

    var deferred = main.DeferredWebViewApp.init(app_state);
    const app = deferred.app();
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{
        .size = geometry.SizeF.init(main.window_width, main.window_height),
    });
    defer harness.destroy(testing.allocator);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &allowed_origins;

    try harness.start(app);
    var views_buffer: [4]native_sdk.ViewInfo = undefined;
    try testing.expectEqual(@as(usize, 1), harness.runtime.listViews(1, &views_buffer).len);

    try harness.runtime.dispatchPlatformEvent(app, .{ .gpu_surface_frame = .{
        .window_id = 1,
        .label = main.canvas_label,
        .size = geometry.SizeF.init(main.window_width, main.window_height),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
        .nonblank = true,
    } });
    try testing.expect(!app_state.model.terminal_webview_mounted);
    try testing.expect(!app_state.model.genui_webview_mounted);
    try testing.expectEqual(@as(usize, 1), harness.runtime.listViews(1, &views_buffer).len);

    const timer_event = harness.null_platform.fireTimer(main.deferred_webview_timer_id, 2_000_000).?;
    try harness.runtime.dispatchPlatformEvent(app, timer_event);
    try testing.expect(app_state.model.terminal_webview_mounted);
    try testing.expect(!app_state.model.genui_webview_mounted);
    try testing.expectEqual(@as(usize, 2), harness.runtime.listViews(1, &views_buffer).len);

    app_state.model.session_slots[0].mode = .capsule;
    @memcpy(app_state.model.genui_workbench_url_storage[0..capsule_url.len], capsule_url);
    app_state.model.genui_workbench_url_len = capsule_url.len;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expect(app_state.model.genui_webview_mounted);
    const mounted_views = harness.runtime.listViews(1, &views_buffer);
    try testing.expectEqual(@as(usize, 3), mounted_views.len);
    try testing.expectEqualStrings(main.terminal_view_label, mounted_views[1].label);
    try testing.expectEqualStrings(main.genui_view_label, mounted_views[2].label);
}

test "desktop attention is background-only semantic and deduplicated" {
    const notification_permissions = [_][]const u8{
        native_sdk.security.permission_notifications,
    };
    const app_state = try main.HyperTermApp.create(std.heap.page_allocator, .{
        .name = "hyper-term-attention-test",
        .scene = main.shell_scene,
        .canvas_label = main.canvas_label,
        .update_fx = main.update,
        .tokens_fn = main.hyperTermTokens,
        .view = main.rootView,
        .web_panes = main.desktopPanes,
    });
    defer app_state.destroy();

    var deferred = main.DeferredWebViewApp.init(app_state);
    const app = deferred.app();
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{
        .size = geometry.SizeF.init(main.window_width, main.window_height),
    });
    defer harness.destroy(testing.allocator);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.permissions = &notification_permissions;
    try harness.start(app);

    // A terminal never becomes an Agent notification source.
    app_state.model.agent_turn_status = .failed;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expect(main.agentAttention(&app_state.model) == null);
    try testing.expectEqual(@as(usize, 0), harness.null_platform.notificationCount());

    app_state.model.session_slots[0].mode = .agent;
    app_state.model.session_slots[0].agent_provider = .codex_acp;
    app_state.model.agent_projection_session_id = 1;
    app_state.model.agent_turn_status = .running;
    app_state.model.agent_document_revision = 4;
    app_state.model.agent_stream_sequence = 4;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try harness.runtime.dispatchPlatformEvent(app, .app_deactivated);
    try testing.expectEqual(@as(usize, 0), harness.null_platform.notificationCount());

    // The Rust-projected completion is announced once while backgrounded.
    app_state.model.agent_turn_status = .completed;
    app_state.model.agent_document_revision = 5;
    app_state.model.agent_stream_sequence = 5;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expectEqual(@as(usize, 1), harness.null_platform.notificationCount());
    try testing.expectEqualStrings("Agent finished", harness.null_platform.lastNotificationTitle());
    try testing.expectEqualStrings("Codex ACP", harness.null_platform.lastNotificationSubtitle());
    try testing.expectEqualStrings(
        "The result is ready to review in Hyper Term.",
        harness.null_platform.lastNotificationBody(),
    );
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expectEqual(@as(usize, 1), harness.null_platform.notificationCount());

    // A new semantic transition can notify even when the document revision
    // does not move (for example, a second turn that only returns prose).
    app_state.model.agent_turn_status = .running;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    app_state.model.agent_turn_status = .waiting_approval;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expectEqual(@as(usize, 2), harness.null_platform.notificationCount());
    try testing.expectEqualStrings("Agent needs approval", harness.null_platform.lastNotificationTitle());

    // Once foregrounded, the visible Agent surface acknowledges attention.
    try harness.runtime.dispatchPlatformEvent(app, .app_activated);
    app_state.model.agent_turn_status = .failed;
    app_state.model.agent_document_revision = 6;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expectEqual(@as(usize, 2), harness.null_platform.notificationCount());
}

test "Native typography uses the registered broad-coverage face when available" {
    try testing.expectEqualStrings(
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        main.preferredUiFontPath(null),
    );
    try testing.expectEqualStrings(
        "/tmp/HyperTerm-CJK.ttf",
        main.preferredUiFontPath("/tmp/HyperTerm-CJK.ttf"),
    );

    var model = main.initialModel();
    try testing.expectEqual(canvas.default_sans_font_id, main.hyperTermTokens(&model).typography.font_id);
    model.ui_font_registered = true;
    try testing.expectEqual(canvas.min_registered_font_id, main.hyperTermTokens(&model).typography.font_id);

    model.high_contrast = true;
    try testing.expectEqual(canvas.min_registered_font_id, main.hyperTermTokens(&model).typography.font_id);
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
    try testing.expectEqualStrings("Agent", findByLabel(tree.root, "New Agent tab").?.text);
    const terminal_tab = findByText(tree.root, .button, "zsh 1").?;
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
    try testing.expect(!containsText(tree.root, "Ask an Agent"));
    try testing.expect(findByLabel(tree.root, "Agent conversation") != null);
    try testing.expect(findByLabel(tree.root, main.terminal_view_anchor) == null);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) == null);
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
    const picker = findByLabel(tree.root, "Choose provider for a new Agent tab").?;
    main.update(&model, tree.msgForPointer(picker.id, .up).?, &fx);
    try testing.expect(model.agent_provider_picker_open);

    tree = try buildTree(arena, &model);
    const codex_acp = findAnyByText(tree.root, "Codex · ACP · authenticated").?;
    main.update(&model, tree.msgForPointer(codex_acp.id, .up).?, &fx);

    try testing.expect(!model.agent_provider_picker_open);
    try testing.expectEqual(main.SessionMode.agent, model.activeSession().mode);
    try testing.expectEqual(main.AgentProvider.codex_acp, model.activeSession().agent_provider);
    try testing.expectEqualStrings("Codex ACP", model.activeSession().title);
    try testing.expectEqual(@as(usize, 2), fx.pendingFetchCount());
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/providers?token=abcdef0123456789abcdef0123456789",
        fx.pendingFetchAt(0).?.url,
    );
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2&provider=codex-acp",
        fx.pendingFetchAt(1).?.url,
    );
}

test "Agent provider status disables unready adapters and enables Copilot ACP" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const status =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"authenticated","containment":"native_seatbelt"},
        \\  {"id":"codex-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"},
        \\  {"id":"claude-acp","protocol":"acp-v1","readiness":"probe_failed","containment":"native_seatbelt"},
        \\  {"id":"copilot-acp","protocol":"acp-v1","readiness":"available","containment":"native_seatbelt"}
        \\]
    ;
    var model = main.initialModelWithProviderStatus(terminal_url, agent_url, "", status);
    try testing.expect(model.agentProviderReady(.codex));
    try testing.expect(!model.agentProviderReady(.codex_acp));
    try testing.expect(model.agentProviderReady(.copilot_acp));
    try testing.expectEqual(model.available_agent_providers, model.contained_agent_providers);
    try testing.expectEqual(main.AgentProviderReadiness.available, model.agentProviderReadiness(.copilot_acp));

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
    const unavailable_codex = findAnyByText(tree.root, "Codex · ACP · sign in required").?;
    try testing.expect(tree.msgForPointer(unavailable_codex.id, .up) == null);

    const copilot = findAnyByText(tree.root, "Copilot · ACP · auth on session").?;
    main.update(&model, tree.msgForPointer(copilot.id, .up).?, &fx);
    try testing.expectEqual(main.AgentProvider.copilot_acp, model.activeSession().agent_provider);
    try testing.expectEqual(@as(usize, 2), fx.pendingFetchCount());
    try testing.expect(std.mem.endsWith(u8, fx.pendingFetchAt(1).?.url, "provider=copilot-acp"));
    try testing.expectEqualStrings("Agent connecting", model.agentStatus());
}

test "Agent provider picker refreshes authentication without restarting" {
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const signed_out =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"login_required","containment":"native_seatbelt"},
        \\  {"id":"codex-acp","protocol":"acp-v1","readiness":"login_required","containment":"native_seatbelt"}
        \\]
    ;
    var model = main.initialModelWithProviderStatus("", agent_url, "", signed_out);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .toggle_agent_provider_picker, &fx);
    try testing.expect(model.agent_provider_picker_open);
    try testing.expect(model.agent_provider_refresh_in_flight);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());
    const refresh = fx.pendingFetchAt(0).?;
    try testing.expectEqual(std.http.Method.POST, refresh.method);
    try testing.expectEqualStrings("{}", refresh.body);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/providers?token=abcdef0123456789abcdef0123456789",
        refresh.url,
    );

    const signed_in =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"authenticated","containment":"native_seatbelt"},
        \\  {"id":"codex-acp","protocol":"acp-v1","readiness":"authenticated","containment":"native_seatbelt"}
        \\]
    ;
    main.update(&model, .{ .agent_providers_refreshed = .{
        .key = main.agent_provider_refresh_effect_key,
        .status = 200,
        .body = signed_in,
    } }, &fx);
    try testing.expect(!model.agent_provider_refresh_in_flight);
    try testing.expect(model.agentProviderReady(.codex));
    try testing.expect(model.agentProviderReady(.codex_acp));

    main.update(&model, .refresh_agent_providers, &fx);
    main.update(&model, .{ .agent_providers_refreshed = .{
        .key = main.agent_provider_refresh_effect_key,
        .status = 502,
        .body = "{}",
    } }, &fx);
    try testing.expect(model.agentProviderReady(.codex));
    try testing.expect(model.agentProviderReady(.codex_acp));
}

test "malformed Agent provider status fails closed" {
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const duplicate =
        \\[
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"authenticated","containment":"native_seatbelt"},
        \\  {"id":"codex","protocol":"codex-app-server-v2","readiness":"authenticated","containment":"native_seatbelt"}
        \\]
    ;
    const model = main.initialModelWithProviderStatus("", agent_url, "codex", duplicate);
    try testing.expectEqual(@as(u8, 0), model.available_agent_providers);
    try testing.expectEqual(@as(u8, 0), model.contained_agent_providers);
    try testing.expect(model.agentProviderUnavailable());
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
    try testing.expectEqualStrings("{}", request.body);
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
    try testing.expectEqualStrings("Agent ready", model.agentStatus());
    try testing.expect(!model.hasAgentStatusNotice());
    try testing.expectEqual(@as(usize, 3), fx.pendingFetchCount());
    try testing.expectEqual(std.http.Method.GET, fx.pendingFetchAt(1).?.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session?token=abcdef0123456789abcdef0123456789&session_id=2",
        fx.pendingFetchAt(1).?.url,
    );
    try testing.expectEqual(std.http.Method.GET, fx.pendingFetchAt(2).?.method);
    try testing.expectEqual(native_sdk.FetchResponseMode.stream, fx.pendingFetchAt(2).?.response);
    try testing.expectEqual(native_sdk.max_effect_line_bytes_ceiling, fx.pendingFetchAt(2).?.max_line_bytes);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/stream?token=abcdef0123456789abcdef0123456789&session_id=2",
        fx.pendingFetchAt(2).?.url,
    );
}

test "Agent NDJSON stream applies message patches and state without polling" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    main.update(&model, .{ .agent_session_started = .{
        .key = main.agent_start_effect_key_base + 2,
        .status = 200,
        .body = "{\"session_id\":2,\"provider\":\"codex\",\"protocol\":\"codex-app-server-v2\",\"status\":\"ready\"}",
    } }, &fx);
    try testing.expectEqual(@as(u8, 2), model.agent_stream_session_id);

    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"ready","error":null,"document":{"revision":4,"blocks":[{"block_id":"00000000-0000-4000-8000-000000000041","kind":"message","payload":{"type":"message","role":"agent","text":"Stream"}}]}}
        ,
    } }, &fx);
    try testing.expectEqual(@as(u64, 4), model.agent_document_revision);

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"patch","status":"running","patch":{"stream_sequence":5,"base_revision":4,"target_revision":5,"operations":[{"type":"append_content","block_id":"00000000-0000-4000-8000-000000000041","expected_previous_revision":1,"block_revision":2,"text":"ing"}]}}
        ,
    } }, &fx);
    try testing.expectEqual(main.AgentTurnStatus.running, model.agent_turn_status);
    try testing.expectEqualStrings("Streaming", model.agentBlocks()[0].content());
    try testing.expectEqual(@as(u64, 5), model.agent_document_revision);
    try testing.expectEqual(@as(usize, 0), fx.pendingTimerCount());

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"state","status":"completed","error":null,"capabilities":{"config_options":[],"available_commands":[]}}
        ,
    } }, &fx);
    try testing.expectEqual(main.AgentTurnStatus.completed, model.agent_turn_status);
    try testing.expectEqualStrings("Streaming", model.agentBlocks()[0].content());
}

test "Agent stream state repairs patches missed while the stream connects" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_stream_session_id = 2;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body = "{\"session_id\":2,\"status\":\"running\",\"document\":{\"revision\":4,\"blocks\":[]}}",
    } }, &fx);
    const before = fx.pendingFetchCount();

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"state","status":"waiting_approval","document_revision":7,"capabilities":{"config_options":[],"available_commands":[]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(u64, 4), model.agent_document_revision);
    try testing.expectEqual(@as(u64, 7), model.agent_snapshot_resync_revision);
    try testing.expectEqual(before + 1, fx.pendingFetchCount());
    try testing.expectEqual(main.agent_snapshot_effect_key_base + 2, fx.pendingFetchAt(before).?.key);
}

test "Agent NDJSON stream appends a reasoning activity without rebuilding the transcript" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_stream_session_id = 2;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"running","document":{"revision":4,"blocks":[{"block_id":"00000000-0000-4000-8000-000000000042","kind":"message","payload":{"type":"message","role":"thought","text":"Inspecting"}}]}}
        ,
    } }, &fx);
    try testing.expect(model.agentBlocks()[0].isActivity());
    try testing.expectEqualStrings("Processed", model.agentBlocks()[0].activityTitle());
    const before = fx.pendingFetchCount();

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"patch","status":"running","patch":{"stream_sequence":5,"base_revision":4,"target_revision":5,"operations":[{"type":"append_content","block_id":"00000000-0000-4000-8000-000000000042","expected_previous_revision":1,"block_revision":2,"text":" workspace"}]}}
        ,
    } }, &fx);

    try testing.expectEqualStrings("Inspecting workspace", model.agentBlocks()[0].content());
    try testing.expectEqual(@as(u64, 5), model.agent_document_revision);
    try testing.expectEqual(before, fx.pendingFetchCount());
}

test "Agent patch revision gaps request one bounded snapshot resync" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_stream_session_id = 2;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body = "{\"session_id\":2,\"status\":\"ready\",\"document\":{\"revision\":7,\"blocks\":[]}}",
    } }, &fx);
    const before = fx.pendingFetchCount();

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"patch","status":"running","patch":{"stream_sequence":9,"base_revision":8,"target_revision":9,"operations":[]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(u64, 7), model.agent_document_revision);
    try testing.expectEqual(before + 1, fx.pendingFetchCount());
    try testing.expectEqual(main.agent_snapshot_effect_key_base + 2, fx.pendingFetchAt(before).?.key);
}

test "Agent patches observed during a snapshot force a follow-up resync" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_stream_session_id = 2;
    model.agent_document_revision = 4;
    model.agent_stream_sequence = 4;
    model.agent_snapshot_in_flight_session_id = 2;

    main.update(&model, .{ .agent_stream_line = .{
        .key = main.agent_stream_effect_key_base + 2,
        .line =
        \\{"type":"patch","status":"running","patch":{"stream_sequence":5,"base_revision":4,"target_revision":5,"operations":[]}}
        ,
    } }, &fx);
    try testing.expectEqual(@as(u64, 5), model.agent_snapshot_resync_revision);
    const before = fx.pendingFetchCount();

    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body = "{\"session_id\":2,\"status\":\"running\",\"document\":{\"revision\":4,\"blocks\":[]}}",
    } }, &fx);

    try testing.expectEqual(before + 2, fx.pendingFetchCount());
    try testing.expect(pendingFetchIndexByKey(&fx, main.agent_tier2_results_effect_key_base + 2) != null);
    try testing.expect(pendingFetchIndexByKey(&fx, main.agent_snapshot_effect_key_base + 2) != null);
    try testing.expectEqual(@as(u64, 5), model.agent_snapshot_resync_revision);
}

test "Agent start failures keep the tab inert and explain the gateway result" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    main.update(&model, .{ .agent_session_started = .{
        .key = main.agent_start_effect_key_base + 2,
        .status = 429,
        .body = "Agent session limit reached",
    } }, &fx);

    try testing.expectEqual(main.AgentConnection.failed, model.activeSession().agent_connection);
    try testing.expectEqual(main.AgentTurnStatus.failed, model.agent_turn_status);
    try testing.expect(model.agentSubmitDisabled());
    try testing.expect(model.hasAgentStatusNotice());
    try testing.expectEqualStrings(
        "Agent session limit reached · close a tab and retry",
        model.agentStatus(),
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
    try testing.expect(!model.agentComposerInputDisabled());
    try testing.expect(!model.agentSubmitDisabled());
    try testing.expectEqual(@as(f32, 66), model.agentComposerHeight());
    model.agent_composer_buffer.set("这是一个按视觉宽度计算高度的中文输入框，不应该因为 UTF-8 字节数而提前变高");
    try testing.expectEqual(@as(f32, 66), model.agentComposerHeight());
    model.agent_composer_buffer.set("这是一个按视觉宽度计算高度的中文输入框，不应该因为 UTF-8 字节数而提前变高，并且只有真正超过一行的视觉宽度后才需要扩展输入区域");
    try testing.expectEqual(@as(f32, 84), model.agentComposerHeight());
    model.agent_composer_buffer.set("One\nTwo\nThree");
    try testing.expect(model.agentComposerHeight() > 66);
    model.agent_turn_status = .running;
    try testing.expect(!model.agentComposerInputDisabled());
    try testing.expect(model.agentSubmitDisabled());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const running_tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(!findByLabel(running_tree.root, "Agent prompt").?.state.disabled);
    try testing.expect(findByLabel(running_tree.root, "Send prompt") == null);
    try testing.expect(!findByLabel(running_tree.root, "Stop Agent turn").?.state.disabled);
    try testing.expect(findAnyByText(
        findByLabel(running_tree.root, "Agent turn status").?,
        "Working",
    ) != null);
    model.agent_turn_status = .ready;
    model.agent_composer_buffer.set("Explain the PTY boundary");
    main.update(&model, .send_agent_prompt, &fx);

    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_turn_effect_key_base + 2).?).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/turn?token=abcdef0123456789abcdef0123456789&session_id=2",
        request.url,
    );
    try testing.expectEqualStrings("Explain the PTY boundary", request.body);
    try testing.expectEqualStrings("", model.agentComposerText());
    try testing.expectEqual(main.AgentTurnStatus.running, model.agent_turn_status);

    main.update(&model, .{ .agent_turn_started = .{
        .key = main.agent_turn_effect_key_base + 2,
        .status = 202,
        .body = "{\"session_id\":2,\"status\":\"running\"}",
    } }, &fx);
    try testing.expectEqual(@as(usize, 1), fx.pendingTimerCount());
    try testing.expectEqual(main.agent_poll_timer_key_base + 2, fx.pendingTimerAt(0).?.key);
}

test "Agent stop control posts cancellation and enters compact stopping state" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_turn_status = .running;
    main.update(&model, .cancel_agent_turn, &fx);

    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_cancel_effect_key_base + 2).?).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/cancel?token=abcdef0123456789abcdef0123456789&session_id=2",
        request.url,
    );
    try testing.expectEqualStrings("{}", request.body);
    try testing.expectEqual(main.AgentTurnStatus.cancelling, model.agent_turn_status);
    try testing.expectEqualStrings("Stopping…", model.agentComposerStatus());
    try testing.expect(model.agentCancelDisabled());

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Stop Agent turn").?.state.disabled);
    try testing.expect(findByLabel(tree.root, "Send prompt") == null);

    main.update(&model, .{ .agent_turn_cancelled = .{
        .key = main.agent_cancel_effect_key_base + 2,
        .status = 202,
        .body = "{\"session_id\":2,\"status\":\"cancelling\"}",
    } }, &fx);
    try testing.expectEqual(main.AgentTurnStatus.cancelling, model.agent_turn_status);
}

test "failed Agent turns restore the submitted prompt without replacing a newer draft" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_turn_status = .ready;
    model.agent_composer_buffer.set("Keep my failed prompt");
    main.update(&model, .send_agent_prompt, &fx);
    try testing.expectEqualStrings("", model.agentComposerText());

    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"failed","error":"Model gpt-test requires a newer Codex CLI · choose another model or update Codex","document":{"revision":1,"blocks":[]}}
        ,
    } }, &fx);
    try testing.expectEqualStrings("Keep my failed prompt", model.agentComposerText());
    try testing.expectEqualStrings(
        "Model gpt-test requires a newer Codex CLI · choose another model or update Codex",
        model.agentStatus(),
    );

    model.agent_turn_status = .ready;
    model.agent_composer_buffer.set("Submitted prompt");
    main.update(&model, .send_agent_prompt, &fx);
    model.agent_composer_buffer.set("Newer draft wins");
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"failed","error":"Agent failed","document":{"revision":2,"blocks":[]}}
        ,
    } }, &fx);
    try testing.expectEqualStrings("Newer draft wins", model.agentComposerText());
}

test "ACP composer renders provider capabilities and routes configuration through Rust" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithProviders(terminal_url, agent_url, "codex-acp");
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_codex_acp_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_turn_status = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"ready","error":null,"capabilities":{"config_options":[{"id":"model","name":"Model","description":null,"category":"model","kind":{"type":"select","current_value":"fast"},"choices":[{"value":"fast","name":"Fast","description":null,"group":null},{"value":"deep","name":"Deep","description":null,"group":null}]}],"available_commands":[{"name":"skills","description":"Configure skills","input_hint":null}]},"document":{"blocks":[]}}
        ,
    } }, &fx);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    try testing.expect(findAnyByText(tree.root, "Fast") != null);
    const command_trigger = findByLabel(tree.root, "Agent commands").?;
    main.update(&model, tree.msgForPointer(command_trigger.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    try testing.expect(findAnyByText(tree.root, "Skills · Configure skills") != null);

    main.update(&model, .dismiss_agent_command_picker, &fx);
    tree = try buildTree(arena, &model);

    const selector = findByLabel(tree.root, "Model").?;
    main.update(&model, tree.msgForPointer(selector.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    const deep = findAnyByText(tree.root, "Deep").?;
    main.update(&model, tree.msgForPointer(deep.id, .up).?, &fx);
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_config_effect_key_base + 2).?).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expect(std.mem.endsWith(u8, request.url, "/agent/session/config?token=abcdef0123456789abcdef0123456789&session_id=2"));
    try testing.expectEqualStrings(
        "{\"config_id\":\"model\",\"value\":{\"type\":\"id\",\"value\":\"deep\"}}",
        request.body,
    );

    main.update(&model, .{ .agent_config_updated = .{
        .key = main.agent_config_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"capabilities":{"config_options":[{"id":"model","name":"Model","description":null,"category":"model","kind":{"type":"select","current_value":"deep"},"choices":[{"value":"fast","name":"Fast","description":null,"group":null},{"value":"deep","name":"Deep","description":null,"group":null}]}],"available_commands":[{"name":"skills","description":"Configure skills","input_hint":null}]}}
        ,
    } }, &fx);
    try testing.expectEqualStrings("Deep", model.agentConfigOptions()[0].currentLabel());
    try testing.expectEqual(@as(f32, 76), model.agentConfigOptions()[0].compactWidth());

    tree = try buildTree(arena, &model);
    const updated_command_trigger = findByLabel(tree.root, "Agent commands").?;
    main.update(&model, tree.msgForPointer(updated_command_trigger.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    const skills = findAnyByText(tree.root, "Skills · Configure skills").?;
    main.update(&model, tree.msgForPointer(skills.id, .up).?, &fx);
    try testing.expectEqualStrings("/skills ", model.agentComposerText());
}

test "direct Codex capabilities insert skill mentions instead of fake slash commands" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_turn_status = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"status":"ready","error":null,"capabilities":{"config_options":[{"id":"model","name":"Model","description":"Next turn model","category":"model","kind":{"type":"select","current_value":"gpt-5.6-sol"},"choices":[{"value":"gpt-5.6-sol","name":"GPT-5.6 Sol","description":null,"group":null}]},{"id":"reasoning_effort","name":"Reasoning","description":"Next turn effort","category":"thought_level","kind":{"type":"select","current_value":"high"},"choices":[{"value":"high","name":"high","description":null,"group":null}]}],"available_commands":[{"name":"$native-sdk","description":"Build Native UI","input_hint":"Describe how this skill should help"}]},"document":{"blocks":[]}}
        ,
    } }, &fx);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findAnyByText(tree.root, "GPT-5.6 Sol") != null);
    try testing.expect(findAnyByText(tree.root, "high") != null);
    const command_trigger = findByLabel(tree.root, "Agent commands").?;
    main.update(&model, tree.msgForPointer(command_trigger.id, .up).?, &fx);
    tree = try buildTree(arena_state.allocator(), &model);
    const skill = findAnyByText(tree.root, "Skill · native-sdk · Build Native UI").?;
    main.update(&model, tree.msgForPointer(skill.id, .up).?, &fx);
    try testing.expectEqualStrings("$native-sdk ", model.agentComposerText());
}

test "direct Codex artifacts never expose the ACP editor panel" {
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000030","kind":"artifact","trust_class":"isolated_artifact","payload":{"type":"artifact","artifact":{"artifact_id":"44444444-4444-4444-8444-444444444444","source_revision":6,"entrypoint":"/App.tsx","content_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","compiler":{"name":"esbuild-wasm","version":"0.28.1"}}}}
        \\]}}
        ,
    } }, &fx);
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Agent conversation") != null);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) == null);
    try testing.expect(model.hasGenUiArtifact());
    try testing.expect(!model.hasEditableAgentArtifact());
    try testing.expect(!model.canOpenAgentEditor());
    try testing.expect(!model.hasAgentEditor());
    try testing.expect(findByLabel(tree.root, "Open ACP artifact editor") == null);

    var panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 0), main.desktopPanes(&model, &panes));
    model.terminal_webview_mounted = true;
    try testing.expectEqual(@as(usize, 1), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings("zero://inline", panes[0].url);
    try testing.expectEqualStrings(main.terminal_view_label, panes[0].label);
}

test "accepted ACP artifact stays single-pane until the user enters editing" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithProviders(
        terminal_url,
        agent_url,
        "codex,codex-acp",
    );
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_codex_acp_agent, &fx);
    try testing.expectEqual(main.AgentProvider.codex_acp, model.activeSession().agent_provider);
    try testing.expect(!model.hasGenUiArtifact());
    try testing.expect(!model.hasAgentEditor());

    var initial_panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 0), main.desktopPanes(&model, &initial_panes));
    model.terminal_webview_mounted = true;
    try testing.expectEqual(@as(usize, 1), main.desktopPanes(&model, &initial_panes));
    try testing.expectEqualStrings("zero://inline", initial_panes[0].url);
    try testing.expectEqualStrings(main.terminal_view_label, initial_panes[0].label);

    var initial_arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer initial_arena_state.deinit();
    const initial_tree = try buildTree(initial_arena_state.allocator(), &model);
    try testing.expect(findByLabel(initial_tree.root, "Agent conversation") != null);
    try testing.expect(findByLabel(initial_tree.root, main.genui_view_anchor) == null);

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
    try testing.expect(model.hasEditableAgentArtifact());
    try testing.expect(model.canOpenAgentEditor());
    try testing.expect(!model.hasAgentEditor());
    try testing.expectEqualStrings("55555555", model.genUiArtifactLabel());
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/workbench/?surface=artifact&artifact_id=55555555-5555-4555-8555-555555555555&session_id=2&token=abcdef0123456789abcdef0123456789",
        model.genUiWorkbenchUrl(),
    );
    try testing.expectEqual(@as(usize, 0), model.agentBlocks().len);

    var panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 1), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings(main.terminal_view_label, panes[0].label);
    try testing.expectEqualStrings("zero://inline", panes[0].url);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) == null);
    try testing.expect(findByLabel(tree.root, "Agent conversation") != null);
    const open_editor = findByLabel(tree.root, "Open ACP artifact editor").?;
    main.update(&model, tree.msgForPointer(open_editor.id, .up).?, &fx);

    try testing.expect(!model.canOpenAgentEditor());
    try testing.expect(model.hasAgentEditor());
    model.genui_webview_mounted = true;
    try testing.expectEqual(@as(usize, 2), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings(main.genui_view_anchor, panes[1].anchor.?);
    try testing.expectEqualStrings(model.genUiWorkbenchUrl(), panes[1].url);
    try testing.expectEqual(@as(u64, 7), panes[1].reload_token);

    tree = try buildTree(arena, &model);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) != null);
    try testing.expect(findByLabel(tree.root, "Open ACP artifact editor") == null);
    try testing.expect(containsText(tree.root, "Edit"));
    try testing.expect(containsText(tree.root, "draft"));
    try testing.expect(containsText(tree.root, "55555555"));
    const close_editor = findByLabel(tree.root, "Close ACP artifact editor").?;
    main.update(&model, tree.msgForPointer(close_editor.id, .up).?, &fx);
    try testing.expect(!model.hasAgentEditor());
    try testing.expect(model.canOpenAgentEditor());
    try testing.expectEqual(@as(usize, 2), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings("zero://inline", panes[1].url);

    tree = try buildTree(arena, &model);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) == null);
    try testing.expect(findByLabel(tree.root, "Open ACP artifact editor") != null);
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

test "ACP execution context stays compact until the user inspects it" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithProviders(terminal_url, agent_url, "codex-acp");
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_codex_acp_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"ready","error":null,"context":{"schema_version":1,"sequence":2,"event_id":"11111111-1111-4111-8111-111111111111","recorded_at_ms":1,"task_id":"22222222-2222-4222-8222-222222222222","run_id":null,"operation_id":null,"causation_id":"33333333-3333-4333-8333-333333333333","correlation_id":"33333333-3333-4333-8333-333333333333","payload":{"type":"agent_execution_context_recorded","context":{"provider_id":"codex-acp","protocol":"acp","thread_id":"thread-1","receipts":[{"schema_version":1,"context_id":"agent-provider","context_revision":1,"mode":"hermetic","context_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","environment_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","clear_inherited":true,"bindings":[{}],"credential_bindings":[{}]},{"schema_version":1,"context_id":"mcp:hyper_term","context_revision":1,"mode":"hermetic","context_digest":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","environment_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","clear_inherited":true,"bindings":[],"credential_bindings":[]}]}}},"document":{"revision":2,"blocks":[]}}
        ,
    } }, &fx);

    try testing.expect(model.hasAgentExecutionContext());
    try testing.expectEqualStrings("Hermetic · 2 contexts", model.agentExecutionContextSummary());
    try testing.expectEqual(@as(usize, 2), model.agentExecutionContexts().len);
    try testing.expectEqualStrings("agent-provider", model.agentExecutionContexts()[0].contextId());
    try testing.expectEqualStrings("aaaaaaaa", model.agentExecutionContexts()[0].digestPrefix());

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    const inspect = findByLabel(tree.root, "Inspect Agent execution context").?;
    try testing.expect(findByLabel(tree.root, "Agent execution context details") == null);
    main.update(&model, tree.msgForPointer(inspect.id, .up).?, &fx);

    tree = try buildTree(arena, &model);
    try testing.expect(findByLabel(tree.root, "Agent execution context details") != null);
    try testing.expect(containsText(tree.root, "mcp:hyper_term"));
    try testing.expect(containsText(tree.root, "1 environment bindings · 1 credential references"));
    try testing.expect(containsText(tree.root, "cccccccc"));
}

test "ACP execution context rejects uncorrelated evidence" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithProviders(terminal_url, agent_url, "codex-acp");
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_codex_acp_agent, &fx);
    model.session_slots[1].agent_connection = .ready;
    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"status":"ready","context":{"event_id":"11111111-1111-4111-8111-111111111111","causation_id":"22222222-2222-4222-8222-222222222222","correlation_id":"33333333-3333-4333-8333-333333333333","payload":{"type":"agent_execution_context_recorded","context":{"provider_id":"codex-acp","protocol":"acp","thread_id":"thread-1","receipts":[{"schema_version":1,"context_id":"agent-provider","context_revision":1,"mode":"hermetic","context_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","environment_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","clear_inherited":true}]}}},"document":{"revision":2,"blocks":[]}}
        ,
    } }, &fx);

    try testing.expect(!model.hasAgentExecutionContext());
    try testing.expectEqual(main.AgentTurnStatus.failed, model.agent_turn_status);
    try testing.expectEqualStrings("Agent execution context evidence was invalid", model.agentError());
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000005","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"11111111-1111-4111-8111-111111111111","operation_revision":3,"prompt":"Allow this exact operation once?","options":["allow_once","reject_once","cancelled"],"decision":null}}
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
    try testing.expect(containsText(tree.root, "Allow unavailable until Rust can enforce"));
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
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_permission_effect_key_base + 2).?).?;
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

test "Agent activity renders compact plans goals diffs terminals and hides low-signal tips" {
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
        \\{"status":"completed","error":null,"goal":{"objective":"Ship the compact Agent UI without losing terminal speed","status":"active","token_budget":50000,"tokens_used":1200,"time_used_seconds":90},"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000031","kind":"message","payload":{"type":"message","role":"agent","text":"Warning: Skill descriptions were shortened to fit the budget.\n\nHi! What are we working on today?"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000030","kind":"message","payload":{"type":"message","role":"agent","text":"Model metadata for gpt-5.6-sol is unavailable"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000035","kind":"message","payload":{"type":"message","role":"thought","text":"Inspecting the workspace before editing."}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000032","kind":"agent_tool_call","payload":{"type":"agent_tool_call","turn_id":"turn-1","call":{"tool_call_id":"edit-1","title":"Edit src/lib.rs","kind":"edit","status":"completed","locations":[{"path":"/workspace/src/lib.rs","line":7}],"content":[{"type":"diff","path":"/workspace/src/lib.rs","patch":"--- a/src/lib.rs\n+++ b/src/lib.rs\n-old\n+new\n","added_lines":1,"removed_lines":1},{"type":"terminal","terminal_id":"terminal-7"}],"raw_input":null,"raw_output":"{\"ok\":true}"}}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000034","kind":"agent_tool_call","payload":{"type":"agent_tool_call","turn_id":"turn-1","call":{"tool_call_id":"exec-1","title":"sed -n '1,240p' Cargo.toml && rg -n '^name' --glob Cargo.toml .","kind":"execute","status":"completed","locations":[],"content":[{"type":"terminal","terminal_id":"terminal-9"}],"raw_input":"{\"command\":\"sed Cargo.toml\"}","raw_output":null}}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000033","kind":"agent_plan","payload":{"type":"agent_plan","turn_id":"turn-1","entries":[{"content":"Inspect the workspace","priority":"high","status":"completed"},{"content":"Polish the notes","priority":"low","status":"pending"},{"content":"Verify the edit after reviewing the complete repository architecture","priority":"medium","status":"in_progress"}]}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 2), model.agentBlocks().len);
    try testing.expectEqualStrings("Hi! What are we working on today?", model.agentBlocks()[0].content());
    try testing.expect(model.agentBlocks()[1].isActivity());
    try testing.expectEqualStrings("Processed", model.agentBlocks()[1].activityTitle());
    try testing.expectEqualStrings("completed · 2 tools · 1 file · +1 −1", model.agentBlocks()[1].activityMeta());
    try testing.expectEqual(@as(usize, 1), model.agentBlocks()[1].diffFiles().len);
    try testing.expectEqualStrings("/workspace/src/lib.rs", model.agentBlocks()[1].diffFiles()[0].path());
    try testing.expectEqual(@as(u64, 1), model.agentBlocks()[1].diffFiles()[0].added_lines);
    try testing.expectEqual(@as(u64, 1), model.agentBlocks()[1].diffFiles()[0].removed_lines);
    try testing.expect(!model.agentBlocks()[1].expanded);
    const plan = model.agentPlan().?;
    try testing.expect(!plan.expanded);
    try testing.expectEqualStrings("Plan · Verify the edit after reviewing the comple…", plan.activityTitle());
    try testing.expectEqualStrings("1 / 3", plan.activityMeta());
    try testing.expectEqualStrings(
        "- [x] Inspect the workspace\n- [ ] Polish the notes\n- [ ] Verify the edit after reviewing the complete repository architecture\n",
        plan.content(),
    );
    const goal = model.agentGoal().?;
    try testing.expectEqualStrings(
        "Ship the compact Agent UI without losing terminal speed",
        goal.objective(),
    );
    try testing.expectEqualStrings("active · 1m · 1200 / 50000 tokens", goal.meta());
    try testing.expect(!goal.expanded);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    try testing.expect(!containsText(tree.root, "Skill descriptions were shortened"));
    try testing.expect(!containsText(tree.root, "Model metadata for"));
    try testing.expect(containsText(tree.root, "Hi! What are we working on today?"));
    try testing.expect(containsText(tree.root, "Processed"));
    try testing.expect(containsText(tree.root, "Plan · Verify the edit after reviewing the comple…"));
    try testing.expect(findByLabel(tree.root, "Agent context shelf") != null);
    try testing.expectEqual(@as(f32, 1), findByLabel(tree.root, "Agent turn plan").?.layout.grow);
    try testing.expectEqual(@as(f32, 1), findByLabel(tree.root, "Persistent Agent goal").?.layout.grow);
    try testing.expect(containsText(tree.root, "Goal · Ship the compact Agent UI without losing t…"));
    try testing.expect(containsText(tree.root, "active · 1m · 1200 / 50000 tokens"));
    try testing.expectEqualStrings("chevron-right", findByText(tree.root, .button, "Processed").?.icon);
    try testing.expectEqualStrings("chevron-right", findByText(tree.root, .button, "Plan · Verify the edit after reviewing the comple…").?.icon);
    try testing.expectEqualStrings("chevron-right", findByText(tree.root, .button, "Goal · Ship the compact Agent UI without losing t…").?.icon);
    const goal_actions = findByLabel(tree.root, "Goal actions").?;
    try testing.expect(!goal_actions.state.disabled);
    main.update(&model, tree.msgForPointer(goal_actions.id, .up).?, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByText(tree.root, .menu_item, "Edit goal") != null);
    try testing.expect(findByText(tree.root, .menu_item, "Pause goal") != null);
    try testing.expect(findByText(tree.root, .menu_item, "Clear goal") != null);

    const edit_goal = findByText(tree.root, .menu_item, "Edit goal").?;
    main.update(&model, tree.msgForPointer(edit_goal.id, .up).?, &fx);
    try testing.expectEqualStrings(
        "/goal Ship the compact Agent UI without losing terminal speed",
        model.agentComposerText(),
    );
    try testing.expect(model.agentGoalEditing());
    try testing.expect(!model.agent_goal_menu_open);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Agent prompt").?.autofocus);

    main.update(&model, .toggle_agent_goal_menu, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    const pause_goal = findByText(tree.root, .menu_item, "Pause goal").?;
    main.update(&model, tree.msgForPointer(pause_goal.id, .up).?, &fx);
    const goal_request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_goal_effect_key_base + 2).?).?;
    try testing.expectEqual(std.http.Method.POST, goal_request.method);
    try testing.expectEqualStrings(
        "http://127.0.0.1:55321/agent/session/turn?token=abcdef0123456789abcdef0123456789&session_id=2",
        goal_request.url,
    );
    try testing.expectEqualStrings("/goal pause", goal_request.body);
    try testing.expect(model.agentGoalActionDisabled());
    main.update(&model, .{ .agent_goal_updated = .{
        .key = main.agent_goal_effect_key_base + 2,
        .status = 202,
        .body = "{\"session_id\":2,\"status\":\"ready\"}",
    } }, &fx);
    try testing.expect(!model.agentGoalActionDisabled());
    try testing.expectEqual(@as(usize, 1), fx.pendingTimerCount());
    try testing.expectEqualStrings("/goal resume", main.AgentGoalAction.resume_goal.command());
    try testing.expectEqualStrings("/goal clear", main.AgentGoalAction.clear_goal.command());

    main.update(&model, .toggle_agent_goal, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expectEqualStrings("chevron-down", findByText(tree.root, .button, "Goal · Ship the compact Agent UI without losing t…").?.icon);
    try testing.expect(containsText(tree.root, "Ship the compact Agent UI without losing terminal speed"));

    main.update(&model, .{ .toggle_agent_block = plan.id }, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expectEqualStrings("chevron-down", findByText(tree.root, .button, "Plan · Verify the edit after reviewing the comple…").?.icon);

    main.update(&model, .{ .toggle_agent_block = model.agentBlocks()[1].id }, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expectEqualStrings("chevron-down", findByText(tree.root, .button, "Processed").?.icon);
    try testing.expect(containsText(tree.root, "Inspecting the workspace before editing."));
    try testing.expect(containsText(tree.root, "Edit src/lib.rs"));
    try testing.expect(containsText(tree.root, "Run shell command"));
    try testing.expect(containsText(tree.root, "/workspace/src/lib.rs"));
    try testing.expect(containsText(tree.root, "+new"));
    try testing.expect(containsText(tree.root, "terminal-7"));
    try testing.expect(findByLabel(tree.root, "Changed files") != null);
    try testing.expect(findByLabel(tree.root, "Changed file /workspace/src/lib.rs, plus 1, minus 1") != null);
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

test "Agent system notices remain one line until explicitly expanded" {
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices("", agent_url);
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000039","kind":"message","payload":{"type":"message","role":"system","text":"Provider restored a bounded session notice."}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 1), model.agentBlocks().len);
    try testing.expect(model.agentBlocks()[0].isSystemMessage());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Session notice"));
    try testing.expect(!containsText(tree.root, "Provider restored"));

    main.update(&model, .{ .toggle_agent_block = model.agentBlocks()[0].id }, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Provider restored a bounded session notice."));
}

test "empty ACP plans remain hidden as low-signal activity" {
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices("", agent_url);
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000036","kind":"agent_plan","payload":{"type":"agent_plan","entries":[]}}
        \\]}}
        ,
    } }, &fx);

    try testing.expect(model.agentPlan() == null);
    try testing.expectEqual(@as(usize, 0), model.agentBlocks().len);
}

test "ACP reasoning is one collapsed disclosure instead of transcript prose" {
    var model = main.initialModel();
    model.session_slots[0].mode = .agent;
    model.session_slots[0].title = "Agent";
    model.agent_turn_status = .completed;
    model.agent_block_count = 1;
    model.agent_blocks[0].id = 41;
    model.agent_blocks[0].kind = .message;
    model.agent_blocks[0].role = .thought;
    const thought = "Searching repository\n\nPlanning concise response\n\nConsolidating result";
    @memcpy(model.agent_blocks[0].content_storage[0..thought.len], thought);
    model.agent_blocks[0].content_len = thought.len;

    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var tree = try buildTree(arena_state.allocator(), &model);
    try testing.expectEqualStrings("chevron-right", findByText(tree.root, .button, "Processed").?.icon);

    main.update(&model, .{ .toggle_agent_block = 41 }, &fx);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expectEqualStrings("chevron-down", findByText(tree.root, .button, "Processed").?.icon);
}

test "Agent stream uses a tail-anchored variable timeline with stable block identity" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const snapshot =
        \\{"status":"running","error":null,"document":{"blocks":[
        \\  {"block_id":"00000000-0000-4000-8000-000000000041","kind":"message","payload":{"type":"message","role":"agent","text":"Streaming response"}}
        \\]}}
    ;
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
        .body = snapshot,
    } }, &fx);
    const first_id = model.agentBlocks()[0].id;
    const options = main.agentTimelineOptions(&model);
    try testing.expectEqual(@as(usize, 1), options.item_count);
    try testing.expectEqual(@as(u64, 0), options.index_base);
    try testing.expect(options.extent_estimate != null);
    try testing.expect(options.anchor == .trailing);
    try testing.expect(options.extent_estimate.?(options.extent_context, 0) > 0);

    model.agent_snapshot_in_flight_session_id = 2;
    main.update(&model, .{ .agent_snapshot_received = .{
        .key = main.agent_snapshot_effect_key_base + 2,
        .status = 200,
        .body = snapshot,
    } }, &fx);
    try testing.expectEqual(first_id, model.agentBlocks()[0].id);
}

test "desktop registers app markup as a hybrid hot reload fragment" {
    try testing.expectEqual(@as(usize, 1), main.hyper_term_fragments.len);
    try testing.expectEqualStrings("src/app.native", main.hyper_term_fragments[0].path);
    try testing.expectEqualStrings(main.app_markup, main.hyper_term_fragments[0].source);
}

test "Agent timeline mounts only a tail window at the full retained block bound" {
    var model = main.initialModel();
    model.session_slots[0].mode = .agent;
    model.session_slots[0].title = "Agent";
    model.agent_block_count = main.max_agent_blocks;
    model.agent_block_index_base = 4_096;
    for (model.agent_blocks[0..model.agent_block_count], 0..) |*block, index| {
        block.id = @intCast(index + 1);
        block.kind = .message;
        block.role = .agent;
        const text = std.fmt.bufPrint(&block.content_storage, "Message {d}", .{index}) catch unreachable;
        block.content_len = text.len;
    }

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    const timeline = findByLabel(tree.root, "Agent blocks").?;

    try testing.expectEqual(canvas.WidgetKind.scroll_view, timeline.kind);
    try testing.expect(timeline.layout.virtualized);
    try testing.expectEqual(main.max_agent_blocks, timeline.layout.virtual_item_count);
    try testing.expect(timeline.layout.virtual_first_index > 0);
    try testing.expect(timeline.children.len < main.max_agent_blocks / 2);
    try testing.expect(containsText(timeline, "Message 127"));
    try testing.expect(widgetCount(tree.root) < 220);
}

test "Agent conversation uses responsive reading and composer rails" {
    var model = main.initialModel();
    model.session_slots[0].mode = .agent;
    model.session_slots[0].title = "Agent";

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);

    const reading_rail = findByLabel(tree.root, "Agent reading rail").?;
    const composer_rail = findByLabel(tree.root, "Agent composer rail").?;
    try testing.expectEqual(@as(f32, 1), reading_rail.layout.grow);
    try testing.expectEqual(@as(f32, 0), composer_rail.layout.grow);
    try testing.expectEqual(@as(f32, 0), reading_rail.layout.min_size.width);
    try testing.expectEqual(@as(f32, 0), composer_rail.layout.min_size.width);
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000022","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"44444444-4444-4444-8444-444444444444","operation_revision":3,"prompt":"Allow this exact operation once?","options":["allow_once","reject_once","cancelled"],"decision":null}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 2), model.agentBlocks().len);
    try testing.expect(model.agentBlocks()[1].canAllowOnce());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Brokered read-only tool · receipt recorded"));
    try testing.expect(!containsText(tree.root, "Allow unavailable until Rust can enforce"));
    const allow = findByText(tree.root, .button, "Allow once").?;
    main.update(&model, tree.msgForPointer(allow.id, .up).?, &fx);
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_permission_effect_key_base + 2).?).?;
    try testing.expectEqualStrings(
        "{\"operation_id\":\"44444444-4444-4444-8444-444444444444\",\"expected_revision\":3,\"decision\":\"allow_once\"}",
        request.body,
    );
}

test "reviewed Tier 2 workspace edits expose a compact exact approval" {
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000031","block_revision":3,"kind":"operation","trust_class":"trusted_chrome","payload":{"type":"operation","operation_id":"55555555-5555-4555-8555-555555555555","kind":"file_edit","summary":"Apply 1 reviewed Tier 2 file: src/main.rs","risk":"workspace_write","state":"waiting_human"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000032","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"55555555-5555-4555-8555-555555555555","operation_revision":3,"prompt":"Allow this exact operation once?","options":["allow_once","reject_once","cancelled"],"decision":null}}
        \\]}}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 2), model.agentBlocks().len);
    const approval = &model.agentBlocks()[1];
    try testing.expect(approval.canAllowOnce());
    try testing.expect(approval.isWorkspaceReview());
    try testing.expectEqualStrings("Apply 1 reviewed Tier 2 file: src/main.rs", approval.content());
    const options = main.agentTimelineOptions(&model);
    try testing.expect(options.extent_estimate.?(options.extent_context, 1) < 180);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Rust-verified Diff · durable apply"));
    try testing.expect(!containsText(tree.root, "Proposal-only safety gate"));
    const allow = findByText(tree.root, .button, "Allow once").?;
    main.update(&model, tree.msgForPointer(allow.id, .up).?, &fx);
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_permission_effect_key_base + 2).?).?;
    try testing.expectEqualStrings(
        "{\"operation_id\":\"55555555-5555-4555-8555-555555555555\",\"expected_revision\":3,\"decision\":\"allow_once\"}",
        request.body,
    );
}

test "ACP Tier 2 terminal approvals expose the Rust-backed Allow once action" {
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000041","block_revision":3,"kind":"operation","trust_class":"trusted_chrome","payload":{"type":"operation","operation_id":"66666666-6666-4666-8666-666666666666","kind":"shell","summary":"Agent terminal in Tier 2: cargo test","risk":"external_effect","required_capabilities":["shell","sandbox.isolated_task"],"state":"waiting_human"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000042","block_revision":1,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"66666666-6666-4666-8666-666666666666","operation_revision":3,"prompt":"Allow this exact operation once?","options":["allow_once","reject_once","cancelled"],"decision":null}}
        \\]}}
        ,
    } }, &fx);

    const approval = &model.agentBlocks()[1];
    try testing.expect(approval.canAllowOnce());
    try testing.expect(approval.isTier2TerminalReview());
    try testing.expectEqualStrings("Isolated Tier 2 command · no ordinary PTY access", approval.approvalBoundaryLabel());

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Agent terminal in Tier 2: cargo test"));
    try testing.expect(containsText(tree.root, "Isolated Tier 2 command · no ordinary PTY access"));
    const allow = findByText(tree.root, .button, "Allow once").?;
    main.update(&model, tree.msgForPointer(allow.id, .up).?, &fx);
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_permission_effect_key_base + 2).?).?;
    try testing.expectEqualStrings(
        "{\"operation_id\":\"66666666-6666-4666-8666-666666666666\",\"expected_revision\":3,\"decision\":\"allow_once\"}",
        request.body,
    );
}

test "resolved Agent approval names Allow once explicitly" {
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
        \\  {"block_id":"00000000-0000-4000-8000-000000000051","block_revision":4,"kind":"operation","trust_class":"trusted_chrome","payload":{"type":"operation","operation_id":"77777777-7777-4777-8777-777777777777","kind":"mcp_tool","summary":"Read workspace status","risk":"read_only","required_capabilities":[],"state":"authorized"}},
        \\  {"block_id":"00000000-0000-4000-8000-000000000052","block_revision":2,"kind":"approval","trust_class":"trusted_chrome","payload":{"type":"approval","operation_id":"77777777-7777-4777-8777-777777777777","operation_revision":3,"prompt":"Allow this exact operation once?","options":["allow_once","reject_once","cancelled"],"decision":"allow_once"}}
        \\]}}
        ,
    } }, &fx);

    const approval = &model.agentBlocks()[1];
    try testing.expectEqualStrings("Allowed once", approval.approvalTitle());
    try testing.expectEqualStrings("allowed once", approval.decisionLabel());
    try testing.expect(!approval.canAllowOnce());
    const timeline_options = main.agentTimelineOptions(&model);
    const collapsed_extent = timeline_options.extent_estimate.?(timeline_options.extent_context, 1);
    try testing.expect(collapsed_extent <= 30);

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(!containsText(tree.root, "Decision: allowed once"));
    try testing.expect(findByText(tree.root, .button, "Allow once") == null);
    const disclosure = findByText(tree.root, .button, "Allowed once").?;
    try testing.expectEqualStrings("chevron-right", disclosure.icon);

    main.update(&model, tree.msgForPointer(disclosure.id, .up).?, &fx);
    try testing.expect(model.agentBlocks()[1].expanded);
    const expanded_extent = timeline_options.extent_estimate.?(timeline_options.extent_context, 1);
    try testing.expect(expanded_extent > collapsed_extent);
    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "Decision: allowed once"));
    try testing.expectEqualStrings("chevron-down", findByText(tree.root, .button, "Allowed once").?.icon);
}

test "Tier 2 results show a bounded Diff before creating workspace approval" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const source_operation_id = "66666666-6666-4666-8666-666666666666";
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
        .body = "{\"session_id\":2,\"status\":\"completed\",\"document\":{\"revision\":1,\"blocks\":[]}}",
    } }, &fx);
    try testing.expect(pendingFetchIndexByKey(&fx, main.agent_tier2_results_effect_key_base + 2) != null);

    main.update(&model, .{ .agent_tier2_results_received = .{
        .key = main.agent_tier2_results_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"results":[{"source_operation_id":"66666666-6666-4666-8666-666666666666","changed_bytes":20,"changed_files":[{"kind":"deleted","path":"README.md","bytes":0,"content_sha256":null},{"kind":"untracked","path":"data.bin","bytes":3,"content_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},{"kind":"modified","path":"src/main.rs","bytes":17,"content_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]}
        ,
    } }, &fx);

    try testing.expectEqual(@as(usize, 1), model.agentTier2Results().len);
    try testing.expectEqual(@as(usize, 1), model.agentTier2Results()[0].deletedFileCount());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Agent context shelf") != null);
    try testing.expect(containsText(tree.root, "Retained changes"));
    try testing.expect(containsText(tree.root, "3 files · 1 deleted · 20 bytes"));
    try testing.expect(containsText(tree.root, "README.md"));
    try testing.expect(containsText(tree.root, "delete"));
    const review_diff = findByText(tree.root, .button, "Review Diff").?;
    main.update(&model, tree.msgForPointer(review_diff.id, .up).?, &fx);
    const preview_request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_tier2_preview_effect_key_base + 2).?).?;
    try testing.expect(std.mem.endsWith(u8, preview_request.url, "/agent/session/tier2/preview?token=abcdef0123456789abcdef0123456789&session_id=2"));
    try testing.expectEqualStrings(
        "{\"source_operation_id\":\"66666666-6666-4666-8666-666666666666\"}",
        preview_request.body,
    );
    try testing.expect(!model.agentPermissionBusy());

    main.update(&model, .{ .agent_tier2_preview_received = .{
        .key = main.agent_tier2_preview_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"source_operation_id":"66666666-6666-4666-8666-666666666666","result_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","changes":[{"target_path":"README.md","deleted":true,"binary":false,"base_bytes":10,"proposed_bytes":0,"proposed_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","hunks":[{"id":"h0","base_start":1,"base_lines":1,"proposed_start":1,"proposed_lines":0,"patch":"@@ -1 +0,0 @@\n-remove me\n","truncated":false}],"truncated":false},{"target_path":"data.bin","deleted":false,"binary":true,"base_bytes":0,"proposed_bytes":3,"proposed_digest":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","hunks":[],"truncated":false},{"target_path":"src/main.rs","deleted":false,"binary":false,"base_bytes":4,"proposed_bytes":10,"proposed_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hunks":[{"id":"h1","base_start":1,"base_lines":1,"proposed_start":1,"proposed_lines":1,"patch":"@@ -1 +1 @@\n-old\n+generated\n","truncated":false}],"truncated":false}],"truncated":false}
        ,
    } }, &fx);

    arena_state.deinit();
    arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(containsText(tree.root, "-remove me"));
    try testing.expect(containsText(tree.root, "Binary file · 0 → 3 bytes · SHA-256 cccccccccccc… · no textual Diff"));
    try testing.expect(containsText(tree.root, "+generated"));
    try testing.expect(containsText(tree.root, "Preview only · no workspace permission created"));
    const request_approval = findByText(tree.root, .button, "Request apply approval").?;
    main.update(&model, tree.msgForPointer(request_approval.id, .up).?, &fx);
    const approval_request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_tier2_review_effect_key_base + 2).?).?;
    try testing.expect(std.mem.endsWith(u8, approval_request.url, "/agent/session/tier2/review?token=abcdef0123456789abcdef0123456789&session_id=2"));
    try testing.expectEqualStrings(
        "{\"source_operation_id\":\"66666666-6666-4666-8666-666666666666\"}",
        approval_request.body,
    );
    try testing.expectEqualStrings(source_operation_id, model.agentTier2Results()[0].sourceOperationId());
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

test "fallback Agent snapshots schedule one bounded stream reconnect" {
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
    try testing.expectEqual(@as(u64, 500), timer.interval_ms);
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
    try testing.expect(findByLabel(tree.root, "Close zsh 1") != null);
    try testing.expect(findByLabel(tree.root, "Close zsh 2") != null);
    const close_agent = findByLabel(tree.root, "Close Codex 3").?;
    main.update(&model, tree.msgForPointer(close_agent.id, .up).?, &fx);
    try testing.expectEqual(@as(usize, 2), model.openSessions().len);
    try testing.expectEqual(@as(u8, 2), model.active_session_id);
    try testing.expectEqual(@as(usize, 1), fx.pendingFetchCount());
    const close_request = fx.pendingFetchAt(0).?;
    try testing.expectEqual(std.http.Method.POST, close_request.method);
    try testing.expectEqualStrings("{}", close_request.body);
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
    const codex = main.command("hyper-term.new-codex-agent") orelse return error.TestUnexpectedResult;
    const codex_acp = main.command("hyper-term.new-codex-acp-agent") orelse return error.TestUnexpectedResult;
    const claude_acp = main.command("hyper-term.new-claude-acp-agent") orelse return error.TestUnexpectedResult;
    const copilot_acp = main.command("hyper-term.new-copilot-acp-agent") orelse return error.TestUnexpectedResult;

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
    try testing.expect(codex == .choose_codex_agent);
    try testing.expect(codex_acp == .choose_codex_acp_agent);
    try testing.expect(claude_acp == .choose_claude_acp_agent);
    try testing.expect(copilot_acp == .choose_copilot_acp_agent);
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

    var desktop_panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 0), main.desktopPanes(&model, &desktop_panes));
    model.terminal_webview_mounted = true;
    try testing.expectEqual(@as(usize, 1), main.desktopPanes(&model, &desktop_panes));
    try testing.expectEqualStrings(main.terminal_view_label, desktop_panes[0].label);
    try testing.expectEqualStrings(url ++ "&tab=1", desktop_panes[0].url);

    model = main.initialModel();
    try testing.expectEqual(@as(usize, 0), main.terminalPanes(&model, &panes));
}

test "Rust-verified Bug Capsule opens as a dedicated read-only Native tab" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    const capsule_url = "http://127.0.0.1:55321/agent/workbench/?surface=capsule&token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithDesktopServices(
        terminal_url,
        agent_url,
        "codex",
        "",
        capsule_url,
    );
    try testing.expectEqual(main.SessionMode.capsule, model.activeSession().mode);
    try testing.expect(model.isCapsule());
    try testing.expectEqualStrings("Capsule", model.activeSession().title);
    try testing.expectEqualStrings(capsule_url, model.genUiWorkbenchUrl());

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Offline Bug Capsule") != null);
    try testing.expect(findByLabel(tree.root, main.genui_view_anchor) != null);
    try testing.expect(findByLabel(tree.root, "Agent conversation") == null);
    try testing.expect(containsText(tree.root, "replay only"));
    try testing.expect(containsText(tree.root, "Rust verified"));

    var panes: [2]main.HyperTermApp.WebViewPane = undefined;
    try testing.expectEqual(@as(usize, 0), main.desktopPanes(&model, &panes));
    model.terminal_webview_mounted = true;
    model.genui_webview_mounted = true;
    try testing.expectEqual(@as(usize, 2), main.desktopPanes(&model, &panes));
    try testing.expectEqualStrings("zero://inline", panes[0].url);
    try testing.expectEqualStrings(main.genui_view_anchor, panes[1].anchor.?);
    try testing.expectEqualStrings(capsule_url, panes[1].url);

    model = main.initialModelWithDesktopServices(
        terminal_url,
        agent_url,
        "codex",
        "",
        "http://127.0.0.1:55321/agent/workbench/?surface=capsule&token=wrongwrongwrongwrongwrongwrongwrongwrong",
    );
    try testing.expectEqual(main.SessionMode.terminal, model.activeSession().mode);
    try testing.expect(!model.isCapsule());
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
    try testing.expect(main.trustedBugCapsuleUrl(
        "http://127.0.0.1:55321/agent/workbench/?surface=capsule&token=abcdef0123456789abcdef0123456789",
        "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789",
    ));
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
