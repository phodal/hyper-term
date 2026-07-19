const std = @import("std");
const canvas = @import("canvas");

pub const Options = struct {
    width: f32,
    height: f32,
    background: ?canvas.Color = null,
    title: []const u8 = "Native SDK interface",
    description: []const u8 = "Rendered from a Native SDK display list.",
};

pub fn writeSvg(
    allocator: std.mem.Allocator,
    writer: *std.Io.Writer,
    display_list: canvas.DisplayList,
    options: Options,
) !void {
    const render_commands = try allocator.alloc(canvas.RenderCommand, display_list.commands.len);
    defer allocator.free(render_commands);
    const plan = try display_list.renderPlan(render_commands);

    try writer.print(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{d}\" height=\"{d}\" viewBox=\"0 0 {d} {d}\" role=\"img\" aria-labelledby=\"native-svg-title native-svg-description\" shape-rendering=\"geometricPrecision\">\n",
        .{ options.width, options.height, options.width, options.height },
    );
    try writer.writeAll("  <title id=\"native-svg-title\">");
    try writeXmlText(writer, options.title);
    try writer.writeAll("</title>\n  <desc id=\"native-svg-description\">");
    try writeXmlText(writer, options.description);
    try writer.writeAll("</desc>\n  <!-- Generated from a Native SDK display list. Do not edit by hand. -->\n");
    try writeDefs(writer, plan.commands);
    if (options.background) |background| {
        try writer.print("  <rect width=\"{d}\" height=\"{d}\" ", .{ options.width, options.height });
        try writeColorAttribute(writer, "fill", background);
        try writer.writeAll("/>\n");
    }

    for (plan.commands, 0..) |command, index| {
        try writeRenderCommand(allocator, writer, command, index);
    }
    try writer.writeAll("</svg>\n");
}

fn writeDefs(writer: *std.Io.Writer, commands: []const canvas.RenderCommand) !void {
    try writer.writeAll("  <defs>\n");
    for (commands, 0..) |command, index| {
        if (command.clip) |clip| {
            const rect = clip.normalized();
            try writer.print(
                "    <clipPath id=\"clip-{d}\" clipPathUnits=\"userSpaceOnUse\"><rect x=\"{d}\" y=\"{d}\" width=\"{d}\" height=\"{d}\"/></clipPath>\n",
                .{ index, rect.x, rect.y, rect.width, rect.height },
            );
        }
        if (commandGradient(command.command)) |gradient| {
            try writer.print(
                "    <linearGradient id=\"gradient-{d}\" gradientUnits=\"userSpaceOnUse\" x1=\"{d}\" y1=\"{d}\" x2=\"{d}\" y2=\"{d}\">\n",
                .{ index, gradient.start.x, gradient.start.y, gradient.end.x, gradient.end.y },
            );
            for (gradient.stops) |stop| {
                try writer.print("      <stop offset=\"{d}\" ", .{std.math.clamp(stop.offset, 0, 1)});
                try writeColorAttribute(writer, "stop-color", stop.color);
                try writer.writeAll("/>\n");
            }
            try writer.writeAll("    </linearGradient>\n");
        }
        switch (command.command) {
            .shadow => |shadow| {
                const bounds = command.bounds.normalized();
                try writer.print(
                    "    <filter id=\"shadow-{d}\" filterUnits=\"userSpaceOnUse\" x=\"{d}\" y=\"{d}\" width=\"{d}\" height=\"{d}\" color-interpolation-filters=\"sRGB\"><feGaussianBlur stdDeviation=\"{d}\"/></filter>\n",
                    .{ index, bounds.x, bounds.y, bounds.width, bounds.height, @max(0, shadow.blur * 0.5) },
                );
            },
            else => {},
        }
    }
    try writer.writeAll("  </defs>\n");
}

fn writeRenderCommand(
    allocator: std.mem.Allocator,
    writer: *std.Io.Writer,
    render: canvas.RenderCommand,
    index: usize,
) !void {
    try writer.writeAll("  <g");
    if (render.clip != null) try writer.print(" clip-path=\"url(#clip-{d})\"", .{index});
    if (render.opacity < 1) try writer.print(" opacity=\"{d}\"", .{std.math.clamp(render.opacity, 0, 1)});
    try writer.writeAll(">");

    const transformed = !isIdentity(render.transform);
    if (transformed) {
        try writer.print(
            "<g transform=\"matrix({d} {d} {d} {d} {d} {d})\">",
            .{ render.transform.a, render.transform.b, render.transform.c, render.transform.d, render.transform.tx, render.transform.ty },
        );
    }

    switch (render.command) {
        .fill_rect => |value| {
            const rect = value.rect.normalized();
            try writer.print("<rect x=\"{d}\" y=\"{d}\" width=\"{d}\" height=\"{d}\" ", .{ rect.x, rect.y, rect.width, rect.height });
            try writePaintAttribute(writer, "fill", value.fill, index);
            try writer.writeAll("/>");
        },
        .stroke_rect => |value| {
            try writer.writeAll("<path d=\"");
            try writeRoundedRectPath(writer, value.rect, value.radius);
            try writer.writeAll("\" fill=\"none\" ");
            try writeStrokeAttributes(writer, value.stroke, index, .butt);
            try writer.writeAll("/>");
        },
        .fill_rounded_rect => |value| {
            try writer.writeAll("<path d=\"");
            try writeRoundedRectPath(writer, value.rect, value.radius);
            try writer.writeAll("\" ");
            try writePaintAttribute(writer, "fill", value.fill, index);
            try writer.writeAll("/>");
        },
        .draw_line => |value| {
            try writer.print(
                "<line x1=\"{d}\" y1=\"{d}\" x2=\"{d}\" y2=\"{d}\" ",
                .{ value.from.x, value.from.y, value.to.x, value.to.y },
            );
            try writeStrokeAttributes(writer, value.stroke, index, .butt);
            try writer.writeAll("/>");
        },
        .fill_path => |value| {
            try writer.writeAll("<path d=\"");
            try writePathData(writer, value.elements);
            try writer.writeAll("\" ");
            try writePaintAttribute(writer, "fill", value.fill, index);
            try writer.writeAll("/>");
        },
        .stroke_path => |value| {
            try writer.writeAll("<path d=\"");
            try writePathData(writer, value.elements);
            try writer.writeAll("\" fill=\"none\" ");
            try writeStrokeAttributes(writer, value.stroke, index, value.cap);
            try writer.writeAll("/>");
        },
        .draw_text => |value| try writeText(allocator, writer, value),
        .draw_image => |value| {
            const rect = value.dst.normalized();
            try writer.print(
                "<rect x=\"{d}\" y=\"{d}\" width=\"{d}\" height=\"{d}\" fill=\"none\" data-native-image-id=\"{d}\"/>",
                .{ rect.x, rect.y, rect.width, rect.height, value.image_id },
            );
        },
        .shadow => |value| {
            var rect = value.rect.normalized();
            const spread = @max(0, @abs(value.spread));
            rect.x += value.offset.dx - spread;
            rect.y += value.offset.dy - spread;
            rect.width += spread * 2;
            rect.height += spread * 2;
            try writer.writeAll("<path d=\"");
            try writeRoundedRectPath(writer, rect, value.radius);
            try writer.print("\" filter=\"url(#shadow-{d})\" ", .{index});
            try writeColorAttribute(writer, "fill", value.color);
            try writer.writeAll("/>");
        },
        .blur => |value| {
            const rect = value.rect.normalized();
            try writer.print(
                "<rect x=\"{d}\" y=\"{d}\" width=\"{d}\" height=\"{d}\" fill=\"none\" data-native-blur=\"{d}\"/>",
                .{ rect.x, rect.y, rect.width, rect.height, value.radius },
            );
        },
        .push_clip, .pop_clip, .push_opacity, .pop_opacity, .transform => unreachable,
    }

    if (transformed) try writer.writeAll("</g>");
    try writer.writeAll("</g>\n");
}

fn writeText(allocator: std.mem.Allocator, writer: *std.Io.Writer, value: canvas.DrawText) !void {
    const line_capacity = @max(@as(usize, 1), value.text.len + value.glyphs.len + 1);
    const lines = try allocator.alloc(canvas.TextLine, line_capacity);
    defer allocator.free(lines);

    const layout = if (value.text_layout) |options|
        try canvas.layoutTextRun(value, options, lines)
    else blk: {
        const line_height = value.size * 1.25;
        lines[0] = .{
            .text_start = 0,
            .text_len = value.text.len,
            .glyph_start = 0,
            .glyph_len = value.glyphs.len,
            .bounds = .init(value.origin.x, value.origin.y - value.size, canvas.estimateTextWidthForFont(value.font_id, value.text, value.size), line_height),
            .baseline = value.origin.y,
        };
        break :blk canvas.TextLayout{ .lines = lines[0..1], .bounds = lines[0].bounds };
    };

    try writer.writeAll("<path d=\"");
    for (layout.lines) |line| try writeTextLine(writer, value, line);
    try writer.writeAll("\" ");
    try writeColorAttribute(writer, "fill", value.color);
    try writer.writeAll(" fill-rule=\"nonzero\"/>");
}

fn writeTextLine(writer: *std.Io.Writer, value: canvas.DrawText, line: canvas.TextLine) !void {
    if (line.glyph_len > 0 and line.glyph_start < value.glyphs.len) {
        const glyph_end = @min(value.glyphs.len, line.glyph_start + line.paintedGlyphLen());
        const first_x = value.glyphs[line.glyph_start].x;
        for (value.glyphs[line.glyph_start..glyph_end]) |glyph| {
            const start = @min(value.text.len, glyph.text_start);
            const end = @min(value.text.len, start + glyph.text_len);
            const bytes = value.text[start..end];
            const advance = if (glyph.advance > 0) glyph.advance else canvas.estimateTextAdvanceForBytes(value.font_id, bytes, value.size);
            try writeGlyph(writer, value.font_id, value.size, bytes, line.bounds.x + glyph.x - first_x, line.baseline + glyph.y, advance);
        }
    } else {
        const end = @min(value.text.len, line.text_start + line.paintedTextLen());
        var offset = @min(line.text_start, end);
        var x = line.bounds.x;
        while (offset < end) {
            const byte_len = @min(canvas.utf8SequenceLength(value.text[offset]), end - offset);
            const next = offset + byte_len;
            const bytes = value.text[offset..next];
            const advance = canvas.estimateTextAdvanceForBytes(value.font_id, bytes, value.size);
            try writeGlyph(writer, value.font_id, value.size, bytes, x, line.baseline, advance);
            offset = next;
            x += advance;
        }
    }

    if (line.hasEllipsis()) {
        try writeGlyph(
            writer,
            value.font_id,
            value.size,
            canvas.text_ellipsis,
            line.bounds.x + line.bounds.width - line.ellipsis_advance,
            line.baseline,
            line.ellipsis_advance,
        );
    }
}

const glyph_path_capacity = @max(
    canvas.font_ttf.max_glyph_points + 3 * canvas.font_ttf.max_glyph_contours,
    canvas.font_ttf.max_composite_points + 3 * canvas.font_ttf.max_composite_contours,
);

fn writeGlyph(
    writer: *std.Io.Writer,
    font_id: canvas.FontId,
    size: f32,
    bytes: []const u8,
    pen_x: f32,
    baseline: f32,
    cell_advance: f32,
) !void {
    const face = if (font_id == canvas.default_mono_font_id)
        &canvas.font_ttf.geist_mono
    else
        &canvas.font_ttf.geist_regular;
    const codepoint: ?u21 = if (bytes.len > 0) std.unicode.utf8Decode(bytes) catch null else null;
    const glyph = if (codepoint) |value| face.glyphIndex(value) else 0;
    if (glyph == 0) {
        try writeRectPath(writer, pen_x, baseline - size, cell_advance, size);
        return;
    }

    const natural_advance = size * (face.advance(glyph) / face.units_per_em);
    const inset = @max(0, (cell_advance - natural_advance) * 0.5);
    const scale = size / face.units_per_em;
    const transform = canvas.Affine{ .a = scale, .d = -scale, .tx = pen_x + inset, .ty = baseline };
    var path = canvas.vector.PathBuilder(glyph_path_capacity){};
    face.glyphOutline(glyph, transform, &path) catch {
        try writeRectPath(writer, pen_x, baseline - size, cell_advance, size);
        return;
    };
    try writePathData(writer, path.slice());
}

fn writePathData(writer: *std.Io.Writer, elements: []const canvas.PathElement) !void {
    for (elements) |element| switch (element.verb) {
        .move_to => try writer.print("M{d} {d}", .{ element.points[0].x, element.points[0].y }),
        .line_to => try writer.print("L{d} {d}", .{ element.points[0].x, element.points[0].y }),
        .quad_to => try writer.print("Q{d} {d} {d} {d}", .{ element.points[0].x, element.points[0].y, element.points[1].x, element.points[1].y }),
        .cubic_to => try writer.print("C{d} {d} {d} {d} {d} {d}", .{ element.points[0].x, element.points[0].y, element.points[1].x, element.points[1].y, element.points[2].x, element.points[2].y }),
        .close => try writer.writeByte('Z'),
    };
}

fn writeRoundedRectPath(writer: *std.Io.Writer, rect_value: anytype, radius: canvas.Radius) !void {
    const rect = rect_value.normalized();
    const max_radius = @max(0, @min(rect.width, rect.height) * 0.5);
    const tl = std.math.clamp(radius.top_left, 0, max_radius);
    const tr = std.math.clamp(radius.top_right, 0, max_radius);
    const br = std.math.clamp(radius.bottom_right, 0, max_radius);
    const bl = std.math.clamp(radius.bottom_left, 0, max_radius);
    const right = rect.x + rect.width;
    const bottom = rect.y + rect.height;
    try writer.print("M{d} {d}H{d}", .{ rect.x + tl, rect.y, right - tr });
    if (tr > 0) try writer.print("A{d} {d} 0 0 1 {d} {d}", .{ tr, tr, right, rect.y + tr });
    try writer.print("V{d}", .{bottom - br});
    if (br > 0) try writer.print("A{d} {d} 0 0 1 {d} {d}", .{ br, br, right - br, bottom });
    try writer.print("H{d}", .{rect.x + bl});
    if (bl > 0) try writer.print("A{d} {d} 0 0 1 {d} {d}", .{ bl, bl, rect.x, bottom - bl });
    try writer.print("V{d}", .{rect.y + tl});
    if (tl > 0) try writer.print("A{d} {d} 0 0 1 {d} {d}", .{ tl, tl, rect.x + tl, rect.y });
    try writer.writeByte('Z');
}

fn writeRectPath(writer: *std.Io.Writer, x: f32, y: f32, width: f32, height: f32) !void {
    try writer.print("M{d} {d}H{d}V{d}H{d}Z", .{ x, y, x + width, y + height, x });
}

fn writeStrokeAttributes(writer: *std.Io.Writer, stroke: canvas.Stroke, gradient_id: usize, cap: canvas.LineCap) !void {
    try writePaintAttribute(writer, "stroke", stroke.fill, gradient_id);
    try writer.print("stroke-width=\"{d}\" stroke-linecap=\"{s}\" ", .{ @max(0, stroke.width), @tagName(cap) });
}

fn writePaintAttribute(writer: *std.Io.Writer, comptime name: []const u8, fill: canvas.Fill, gradient_id: usize) !void {
    switch (fill) {
        .color => |color| try writeColorAttribute(writer, name, color),
        .linear_gradient => try writer.print("{s}=\"url(#gradient-{d})\" ", .{ name, gradient_id }),
    }
}

fn writeColorAttribute(writer: *std.Io.Writer, comptime name: []const u8, color: canvas.Color) !void {
    try writer.print(
        "{s}=\"rgb({d},{d},{d})\" {s}-opacity=\"{d}\" ",
        .{ name, colorChannel(color.r), colorChannel(color.g), colorChannel(color.b), name, std.math.clamp(color.a, 0, 1) },
    );
}

fn colorChannel(value: f32) u8 {
    return @intFromFloat(@round(std.math.clamp(value, 0, 1) * 255));
}

fn commandGradient(command: canvas.CanvasCommand) ?canvas.LinearGradient {
    const fill: ?canvas.Fill = switch (command) {
        .fill_rect => |value| value.fill,
        .stroke_rect => |value| value.stroke.fill,
        .fill_rounded_rect => |value| value.fill,
        .draw_line => |value| value.stroke.fill,
        .fill_path => |value| value.fill,
        .stroke_path => |value| value.stroke.fill,
        else => null,
    };
    if (fill) |value| return switch (value) {
        .color => null,
        .linear_gradient => |gradient| gradient,
    };
    return null;
}

fn isIdentity(value: canvas.Affine) bool {
    return value.a == 1 and value.b == 0 and value.c == 0 and value.d == 1 and value.tx == 0 and value.ty == 0;
}

fn writeXmlText(writer: *std.Io.Writer, value: []const u8) !void {
    for (value) |byte| switch (byte) {
        '&' => try writer.writeAll("&amp;"),
        '<' => try writer.writeAll("&lt;"),
        '>' => try writer.writeAll("&gt;"),
        else => try writer.writeByte(byte),
    };
}

test "renders Native SDK geometry, clipping, opacity, gradients, and Geist outlines" {
    const testing = std.testing;
    const stops = [_]canvas.GradientStop{
        .{ .offset = 0, .color = canvas.Color.rgb8(13, 15, 11) },
        .{ .offset = 1, .color = canvas.Color.rgb8(215, 255, 114) },
    };
    var commands: [12]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try builder.fillRect(.{
        .rect = .init(0, 0, 320, 180),
        .fill = .{ .linear_gradient = .{ .start = .init(0, 0), .end = .init(320, 180), .stops = &stops } },
    });
    try builder.pushClip(.{ .rect = .init(8, 8, 304, 164), .radius = .all(12) });
    try builder.pushOpacity(0.8);
    try builder.drawText(.{
        .font_id = canvas.default_mono_font_id,
        .size = 18,
        .origin = .init(20, 48),
        .color = canvas.Color.rgb8(230, 233, 221),
        .text = "Hyper <Term>",
    });
    try builder.popOpacity();
    try builder.popClip();

    var output_buffer: [64 * 1024]u8 = undefined;
    var writer = std.Io.Writer.fixed(&output_buffer);
    try writeSvg(testing.allocator, &writer, builder.displayList(), .{
        .width = 320,
        .height = 180,
        .title = "Hyper & Term",
    });
    const svg = writer.buffered();
    try testing.expect(std.mem.startsWith(u8, svg, "<svg xmlns=\"http://www.w3.org/2000/svg\""));
    try testing.expect(std.mem.indexOf(u8, svg, "Hyper &amp; Term") != null);
    try testing.expect(std.mem.indexOf(u8, svg, "linearGradient id=\"gradient-0\"") != null);
    try testing.expect(std.mem.indexOf(u8, svg, "clip-path=\"url(#clip-1)\"") != null);
    try testing.expect(std.mem.indexOf(u8, svg, "opacity=\"0.8\"") != null);
    try testing.expect(std.mem.indexOf(u8, svg, "<path d=\"M") != null);
    try testing.expect(std.mem.indexOf(u8, svg, "<text") == null);
}
