const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const native_dep = b.dependency("native_sdk", .{});

    const sdk = nativeSdkModules(b, native_dep, target, optimize);

    const options = b.addOptions();
    options.addOption([]const u8, "platform", "null");
    options.addOption([]const u8, "trace", "off");
    options.addOption([]const u8, "web_engine", "system");
    options.addOption(bool, "debug_overlay", false);
    options.addOption(bool, "automation", false);
    options.addOption(bool, "js_bridge", false);
    options.addOption(bool, "web_layer", true);

    const manifest_mod = b.createModule(.{
        .root_source_file = b.path("../../apps/desktop/app.zon"),
    });
    const runner_mod = b.createModule(.{
        .root_source_file = native_dep.path("src/app_runner/root.zig"),
        .target = target,
        .optimize = optimize,
    });
    runner_mod.addImport("native_sdk", sdk.root);
    runner_mod.addImport("build_options", options.createModule());
    runner_mod.addImport("app_manifest_zon", manifest_mod);

    const app_mod = b.createModule(.{
        .root_source_file = b.path("../../apps/desktop/src/main.zig"),
        .target = target,
        .optimize = optimize,
    });
    app_mod.addImport("native_sdk", sdk.root);
    app_mod.addImport("runner", runner_mod);

    const generator_mod = b.createModule(.{
        .root_source_file = b.path("src/generate.zig"),
        .target = target,
        .optimize = optimize,
    });
    generator_mod.addImport("canvas", sdk.canvas);
    generator_mod.addImport("hyper_term_app", app_mod);

    const tests = b.addTest(.{ .root_module = generator_mod });
    const test_step = b.step("test", "Compile the Hyper Term Native SVG adapter");
    test_step.dependOn(&b.addRunArtifact(tests).step);

    const generator = b.addExecutable(.{
        .name = "render-hyper-term-readme",
        .root_module = generator_mod,
    });
    const run = b.addRunArtifact(generator);
    run.setCwd(b.path("../.."));
    run.addArg("docs/assets/hyper-term-ui.svg");

    const render_step = b.step("render-readme", "Render the current Hyper Term Native UI to README SVG");
    render_step.dependOn(&run.step);

    const check_run = b.addRunArtifact(generator);
    check_run.setCwd(b.path("../.."));
    check_run.addArgs(&.{ "--check", "docs/assets/hyper-term-ui.svg" });
    const check_step = b.step("check-readme", "Verify the tracked README SVG matches the current Native UI");
    check_step.dependOn(&check_run.step);
}

const SdkModules = struct {
    root: *std.Build.Module,
    canvas: *std.Build.Module,
};

fn nativeSdkModules(
    b: *std.Build,
    dep: *std.Build.Dependency,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
) SdkModules {
    const geometry = externalModule(b, dep, target, optimize, "src/primitives/geometry/root.zig");
    const assets = externalModule(b, dep, target, optimize, "src/primitives/assets/root.zig");
    const app_dirs = externalModule(b, dep, target, optimize, "src/primitives/app_dirs/root.zig");
    const trace = externalModule(b, dep, target, optimize, "src/primitives/trace/root.zig");
    const app_manifest = externalModule(b, dep, target, optimize, "src/primitives/app_manifest/root.zig");
    const diagnostics = externalModule(b, dep, target, optimize, "src/primitives/diagnostics/root.zig");
    const platform_info = externalModule(b, dep, target, optimize, "src/primitives/platform_info/root.zig");
    const json = externalModule(b, dep, target, optimize, "src/primitives/json/root.zig");
    const canvas = externalModule(b, dep, target, optimize, "src/primitives/canvas/root.zig");
    canvas.addImport("geometry", geometry);
    canvas.addImport("json", json);
    if (target.result.os.tag == .macos) {
        canvas.linkFramework("CoreFoundation", .{});
        canvas.linkFramework("CoreGraphics", .{});
        canvas.linkFramework("CoreText", .{});
        canvas.linkSystemLibrary("c", .{});
    }

    const root = externalModule(b, dep, target, optimize, "src/root.zig");
    root.addImport("geometry", geometry);
    root.addImport("assets", assets);
    root.addImport("app_dirs", app_dirs);
    root.addImport("trace", trace);
    root.addImport("app_manifest", app_manifest);
    root.addImport("diagnostics", diagnostics);
    root.addImport("platform_info", platform_info);
    root.addImport("json", json);
    root.addImport("canvas", canvas);
    return .{ .root = root, .canvas = canvas };
}

fn externalModule(
    b: *std.Build,
    dep: *std.Build.Dependency,
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
    path: []const u8,
) *std.Build.Module {
    return b.createModule(.{
        .root_source_file = dep.path(path),
        .target = target,
        .optimize = optimize,
    });
}
