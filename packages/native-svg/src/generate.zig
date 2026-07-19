const std = @import("std");
const canvas = @import("canvas");
const app = @import("hyper_term_app");

const max_widgets = 2048;
const max_commands = 8192;

pub fn main(init: std.process.Init) !void {
    const args = try init.minimal.args.toSlice(init.arena.allocator());
    const check_only = args.len == 3 and std.mem.eql(u8, args[1], "--check");
    if (args.len != 2 and !check_only) return error.ExpectedOutputPath;
    const output_path = args[args.len - 1];

    var model = app.initialModel();
    var ui = app.HyperTermUi.init(init.arena.allocator());
    const tree = try ui.finalize(app.CompiledHyperTermView.build(&ui, &model));
    const tokens = app.hyperTermTokens(&model);

    var layout_nodes: [max_widgets]canvas.WidgetLayoutNode = undefined;
    const layout = try canvas.layoutWidgetTreeWithTokens(
        tree.root,
        .init(0, 0, app.window_width, app.window_height),
        tokens,
        &layout_nodes,
    );

    var commands: [max_commands]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try canvas.emitWidgetLayout(&builder, layout, tokens);

    var output = try std.Io.Writer.Allocating.initCapacity(init.gpa, 128 * 1024);
    defer output.deinit();
    try canvas.writeSvg(
        init.gpa,
        &output.writer,
        .{
            .display_list = builder.displayList(),
            .size = .{ .width = app.window_width, .height = app.window_height },
        },
        .{
            .background = tokens.colors.background,
            .title = "Hyper Term",
            .description = "Generated from apps/desktop/src/app.native through the Native SDK widget layout and display list.",
        },
    );

    const cwd = std.Io.Dir.cwd();
    if (check_only) {
        const existing = try cwd.readFileAlloc(init.io, output_path, init.gpa, .limited(1024 * 1024));
        defer init.gpa.free(existing);
        if (!std.mem.eql(u8, existing, output.written())) {
            std.debug.print("{s} is stale; run `deno task render:readme`\n", .{output_path});
            return error.ReadmeSvgOutOfDate;
        }
        return;
    }

    if (std.fs.path.dirname(output_path)) |directory| try cwd.createDirPath(init.io, directory);
    try cwd.writeFile(init.io, .{ .sub_path = output_path, .data = output.written() });
}
