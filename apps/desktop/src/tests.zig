const std = @import("std");
const native_sdk = @import("native_sdk");
const main = @import("main.zig");
const desktop_model = @import("desktop_model.zig");

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
    const terminal_views = harness.runtime.listViews(1, &views_buffer);
    try testing.expectEqual(@as(usize, 2), terminal_views.len);
    try testing.expect(terminal_views[1].focused);

    app_state.model.session_slots[0].mode = .capsule;
    @memcpy(app_state.model.genui_workbench_url_storage[0..capsule_url.len], capsule_url);
    app_state.model.genui_workbench_url_len = capsule_url.len;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expect(app_state.model.genui_webview_mounted);
    const mounted_views = harness.runtime.listViews(1, &views_buffer);
    try testing.expectEqual(@as(usize, 3), mounted_views.len);
    try testing.expectEqualStrings(main.terminal_view_label, mounted_views[1].label);
    try testing.expectEqualStrings(main.genui_view_label, mounted_views[2].label);
    try testing.expect(mounted_views[2].focused);

    app_state.model.session_slots[0].mode = .agent;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expect(!app_state.model.genui_webview_mounted);
    try testing.expectEqual(@as(usize, 2), harness.runtime.listViews(1, &views_buffer).len);
    try testing.expect(harness.runtime.listViews(1, &views_buffer)[0].focused);
}

test "desktop focus lease follows Terminal tabs and returns to Native Agent canvas" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const allowed_origins = [_][]const u8{ "zero://inline", "http://127.0.0.1:47437" };
    const app_state = try main.HyperTermApp.create(std.heap.page_allocator, .{
        .name = "hyper-term-focus-lease-test",
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
    try harness.runtime.dispatchPlatformEvent(app, .{ .gpu_surface_frame = .{
        .window_id = 1,
        .label = main.canvas_label,
        .size = geometry.SizeF.init(main.window_width, main.window_height),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
        .nonblank = true,
    } });
    const timer_event = harness.null_platform.fireTimer(main.deferred_webview_timer_id, 2_000_000).?;
    try harness.runtime.dispatchPlatformEvent(app, timer_event);

    var views_buffer: [4]native_sdk.ViewInfo = undefined;
    var views = harness.runtime.listViews(1, &views_buffer);
    try testing.expect(views[1].focused);
    try testing.expectEqualStrings(terminal_url ++ "&tab=1", views[1].url);

    main.update(&app_state.model, .choose_agent, &app_state.effects);
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    views = harness.runtime.listViews(1, &views_buffer);
    try testing.expect(views[0].focused);
    try testing.expect(!views[1].focused);
    try testing.expectEqualStrings(terminal_url ++ "&tab=1", views[1].url);
    try testing.expectEqual(@as(f32, 1), views[1].frame.width);
    try testing.expectEqual(@as(f32, 1), views[1].frame.height);

    main.update(&app_state.model, .{ .select_session = 1 }, &app_state.effects);
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    views = harness.runtime.listViews(1, &views_buffer);
    try testing.expect(views[1].focused);

    main.update(&app_state.model, .choose_terminal, &app_state.effects);
    const second_terminal_id = app_state.model.active_session_id;
    try app.event(&harness.runtime, .{ .lifecycle = .frame });
    try testing.expectEqual(second_terminal_id, deferred.focused_terminal_session_id);
    views = harness.runtime.listViews(1, &views_buffer);
    try testing.expect(views[1].focused);
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

test "attention feed keeps background Agent tabs observable" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    model.session_slots[1] = .{
        .id = 2,
        .mode = .agent,
        .title = "Codex ACP",
        .icon = "circle-dot",
        .agent_provider = .codex_acp,
        .agent_connection = .ready,
    };
    model.session_slots[2] = .{
        .id = 3,
        .mode = .agent,
        .title = "Claude ACP",
        .icon = "circle-dot",
        .agent_provider = .claude_acp,
        .agent_connection = .ready,
    };
    model.session_count = 3;
    model.next_session_id = 4;

    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;
    main.update(&model, .{ .agent_attention_received = .{
        .key = main.agent_attention_effect_key,
        .status = 200,
        .body =
        \\{"sessions":[{"session_id":2,"provider":"codex-acp","status":"completed","document_revision":7},{"session_id":3,"provider":"claude-acp","status":"waiting_approval","document_revision":11}]}
        ,
    } }, &fx);

    try testing.expectEqual(main.AgentTurnStatus.completed, model.session_slots[1].agent_attention_status);
    try testing.expectEqualStrings("check-circle", model.session_slots[1].tabIcon());
    try testing.expectEqual(main.AgentTurnStatus.waiting_approval, model.session_slots[2].agent_attention_status);
    try testing.expectEqualStrings("alert", model.session_slots[2].tabIcon());
    const attention = main.agentAttention(&model).?;
    try testing.expectEqual(@as(u8, 3), attention.session_id);
    try testing.expectEqual(main.AgentProvider.claude_acp, attention.provider);
    try testing.expectEqual(main.AgentAttention.Kind.approval, attention.kind);
    try testing.expectEqual(@as(usize, 1), fx.pendingTimerCount());
    try testing.expectEqual(main.agent_attention_poll_timer_key, fx.pendingTimerAt(0).?.key);

    main.update(&model, .{ .select_session = 3 }, &fx);
    try testing.expectEqualStrings("circle-dot", model.session_slots[2].tabIcon());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    try testing.expectEqualStrings(
        "Codex ACP tab 2, review ready",
        model.session_slots[1].tabGroupLabel(arena_state.allocator()),
    );
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
    try testing.expectEqualStrings("New Agent", findByLabel(tree.root, "New Agent tab").?.text);
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

test "Rust terminal metadata projects bounded title and cwd into Native tabs" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    var model = main.initialModel();
    main.initializeModelWithDesktopServices(&model, terminal_url, "", "", "", "");
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.initEffects(&model, &fx);
    const fetch = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.terminal_metadata_effect_key).?).?;
    try testing.expectEqualStrings(
        "http://127.0.0.1:47437/terminal/sessions/metadata?token=0123456789abcdef0123456789abcdef",
        fetch.url,
    );
    main.update(&model, .{ .terminal_metadata_received = .{
        .key = main.terminal_metadata_effect_key,
        .status = 200,
        .body =
        \\{"version":1,"sessions":[{"session_id":1,"revision":3,"title":"cargo test","cwd":"/Users/phodal/ai/hyper-term"}]}
        ,
    } }, &fx);

    try testing.expectEqualStrings("cargo test", model.session_slots[0].displayTitle());
    try testing.expectEqualStrings("/Users/phodal/ai/hyper-term", model.terminalStatus());
    try testing.expectEqual(main.terminal_metadata_poll_timer_key, fx.pendingTimerAt(0).?.key);
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByText(tree.root, .button, "cargo test 1") != null);
    try testing.expect(findByLabel(tree.root, "Close cargo test 1") != null);

    const long_title = "phodal@Phodal-Studio:/Users/phodal/ai/hyper-term";
    @memcpy(model.session_slots[0].terminal_title_storage[0..long_title.len], long_title);
    model.session_slots[0].terminal_title_len = long_title.len;
    try testing.expectEqualStrings("hyper-term", model.session_slots[0].displayTitle());
}

test "Native terminal metadata rejects cross-mode and partial malformed updates" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    var model = main.initialModel();
    main.initializeModelWithDesktopServices(&model, terminal_url, "", "", "", "");
    model.session_slots[1] = .{ .id = 2, .mode = .agent, .title = "Codex", .icon = "circle-dot" };
    model.session_count = 2;
    model.next_session_id = 3;
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .{ .terminal_metadata_received = .{
        .key = main.terminal_metadata_effect_key,
        .status = 200,
        .body =
        \\{"version":1,"sessions":[{"session_id":1,"revision":1,"title":"must not apply","cwd":"/tmp"},{"session_id":2,"revision":1,"title":"spoofed Agent","cwd":"/tmp"}]}
        ,
    } }, &fx);
    try testing.expectEqualStrings("zsh", model.session_slots[0].displayTitle());

    main.update(&model, .{ .terminal_metadata_received = .{
        .key = main.terminal_metadata_effect_key,
        .status = 200,
        .body =
        \\{"version":1,"sessions":[{"session_id":1,"revision":2,"title":"bad\\u0007title","cwd":"relative"}]}
        ,
    } }, &fx);
    try testing.expectEqualStrings("zsh", model.session_slots[0].displayTitle());
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
    try testing.expect(model.agentComposerAutofocus());
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

    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findByLabel(tree.root, "Agent prompt").?.autofocus);
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
    try testing.expect(model.agentComposerAutofocus());

    main.update(&model, .{ .agent_turn_started = .{
        .key = main.agent_turn_effect_key_base + 2,
        .status = 202,
        .body = "{\"session_id\":2,\"status\":\"running\"}",
    } }, &fx);
    try testing.expectEqual(@as(usize, 1), fx.pendingTimerCount());
    try testing.expectEqual(main.agent_poll_timer_key_base + 2, fx.pendingTimerAt(0).?.key);
}

test "Agent composer preserves IME composition and restores focus after search" {
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
    try testing.expect(model.agentComposerAutofocus());

    main.update(&model, .{ .agent_composer_changed = .{ .set_composition = .{
        .text = "中文",
        .cursor = 6,
    } } }, &fx);
    try testing.expectEqualStrings("中文", model.agentComposerText());
    try testing.expectEqualDeep(
        @as(?canvas.TextRange, canvas.TextRange.init(0, 6)),
        model.agent_composer_buffer.composition,
    );
    try testing.expect(!model.agentComposerAutofocus());

    main.update(&model, .{ .agent_composer_changed = .commit_composition }, &fx);
    try testing.expectEqualStrings("中文", model.agentComposerText());
    try testing.expect(model.agent_composer_buffer.composition == null);

    main.update(&model, .open_agent_search, &fx);
    try testing.expect(model.agentSearchOpen());
    try testing.expect(!model.agentComposerAutofocus());

    main.update(&model, .close_agent_search, &fx);
    try testing.expect(!model.agentSearchOpen());
    try testing.expect(model.agentComposerAutofocus());
}

test "Agent tabs keep independent composer drafts" {
    const terminal_url = "http://127.0.0.1:47437/?token=0123456789abcdef0123456789abcdef";
    const agent_url = "http://127.0.0.1:55321/?token=abcdef0123456789abcdef0123456789";
    var model = main.initialModelWithServices(terminal_url, agent_url);
    var fx = main.Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;

    main.update(&model, .choose_agent, &fx);
    model.agent_composer_buffer.set("First Agent draft");

    main.update(&model, .choose_agent, &fx);
    try testing.expectEqual(@as(u8, 3), model.active_session_id);
    try testing.expectEqualStrings("", model.agentComposerText());
    model.agent_composer_buffer.set("Second Agent draft");

    main.update(&model, .{ .select_session = 2 }, &fx);
    try testing.expectEqualStrings("First Agent draft", model.agentComposerText());

    main.update(&model, .{ .select_session = 3 }, &fx);
    try testing.expectEqualStrings("Second Agent draft", model.agentComposerText());

    main.update(&model, .{ .close_session = 2 }, &fx);
    try testing.expectEqual(@as(u8, 3), model.active_session_id);
    try testing.expectEqualStrings("Second Agent draft", model.agentComposerText());

    main.update(&model, .choose_agent, &fx);
    try testing.expectEqualStrings("", model.agentComposerText());
    model.agent_composer_buffer.set("Third Agent draft");
    main.update(&model, .{ .select_session = 3 }, &fx);
    main.update(&model, .{ .close_session = 3 }, &fx);
    try testing.expectEqual(@as(u8, 4), model.active_session_id);
    try testing.expectEqualStrings("Third Agent draft", model.agentComposerText());
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
        \\{"status":"ready","error":null,"capabilities":{"session_info":{"title":"Refactor auth"},"usage":{"used":8192,"size":32768},"config_options":[{"id":"acp.session_mode","name":"Mode","description":null,"category":"mode","kind":{"type":"select","current_value":"ask"},"choices":[{"value":"ask","name":"Ask","description":null,"group":null},{"value":"code","name":"Code","description":null,"group":null}]}],"available_commands":[{"name":"skills","description":"Configure skills","input_hint":null}]},"document":{"blocks":[]}}
        ,
    } }, &fx);
    try testing.expectEqualStrings("Refactor auth", model.activeSession().displayTitle());
    try testing.expectEqualStrings("25% context", model.agentContextUsage());
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var tree = try buildTree(arena, &model);
    try testing.expect(findAnyByText(tree.root, "Ask") != null);
    try testing.expect(findByLabel(tree.root, "Agent context usage") != null);
    const command_trigger = findByLabel(tree.root, "Agent commands").?;
    main.update(&model, tree.msgForPointer(command_trigger.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    try testing.expect(findAnyByText(tree.root, "Skills · Configure skills") != null);
    main.update(&model, .dismiss_agent_command_picker, &fx);
    tree = try buildTree(arena, &model);
    const selector = findByLabel(tree.root, "Mode").?;
    main.update(&model, tree.msgForPointer(selector.id, .up).?, &fx);
    tree = try buildTree(arena, &model);
    const code = findAnyByText(tree.root, "Code").?;
    main.update(&model, tree.msgForPointer(code.id, .up).?, &fx);
    const request = fx.pendingFetchAt(pendingFetchIndexByKey(&fx, main.agent_config_effect_key_base + 2).?).?;
    try testing.expectEqual(std.http.Method.POST, request.method);
    try testing.expect(std.mem.endsWith(u8, request.url, "/agent/session/config?token=abcdef0123456789abcdef0123456789&session_id=2"));
    try testing.expectEqualStrings(
        "{\"config_id\":\"acp.session_mode\",\"value\":{\"type\":\"id\",\"value\":\"code\"}}",
        request.body,
    );

    main.update(&model, .{ .agent_config_updated = .{
        .key = main.agent_config_effect_key_base + 2,
        .status = 200,
        .body =
        \\{"session_id":2,"capabilities":{"session_info":{"title":"Implement auth"},"usage":{"used":16384,"size":32768},"config_options":[{"id":"acp.session_mode","name":"Mode","description":null,"category":"mode","kind":{"type":"select","current_value":"code"},"choices":[{"value":"ask","name":"Ask","description":null,"group":null},{"value":"code","name":"Code","description":null,"group":null}]}],"available_commands":[{"name":"skills","description":"Configure skills","input_hint":null}]}}
        ,
    } }, &fx);
    try testing.expectEqualStrings("Code", model.agentConfigOptions()[0].currentLabel());
    try testing.expectEqual(@as(f32, 76), model.agentConfigOptions()[0].compactWidth());
    try testing.expectEqualStrings("Implement auth", model.activeSession().displayTitle());
    try testing.expectEqualStrings("50% context", model.agentContextUsage());
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
    try testing.expectEqualStrings(terminal_url ++ "&tab=1", panes[0].url);
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
    try testing.expectEqualStrings(terminal_url ++ "&tab=1", initial_panes[0].url);
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
        \\{"status":"completed","error":null,"capabilities":{"session_info":{"title":"Artifact review"},"usage":{"used":8192,"size":32768}},"document":{"blocks":[
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
    try testing.expectEqualStrings(terminal_url ++ "&tab=1", panes[0].url);

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
    try testing.expectApproxEqAbs(
        desktop_model.agent_split_default,
        model.agent_split,
        0.0001,
    );
    main.update(&model, .{ .agent_split_resized = 0.1 }, &fx);
    try testing.expectApproxEqAbs(
        desktop_model.agent_split_min,
        model.agent_split,
        0.0001,
    );
    main.update(&model, .{ .agent_split_resized = 0.9 }, &fx);
    try testing.expectApproxEqAbs(
        desktop_model.agent_split_max,
        model.agent_split,
        0.0001,
    );
    main.update(
        &model,
        .{ .agent_split_resized = desktop_model.agent_split_default },
        &fx,
    );
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
    try testing.expectEqual(@as(usize, 1), main.desktopPanes(&model, &panes));

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
