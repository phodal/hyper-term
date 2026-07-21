//! Bounded Native projection for provider-owned Agent session capabilities.
//!
//! Rust remains authoritative for protocol state. This module only converts
//! authenticated gateway JSON into fixed-size presentation state.

const std = @import("std");
const agent_wire = @import("agent_wire.zig");

pub const max_session_title_bytes: usize = 256;
const max_usage_label_bytes: usize = 32;

pub const SessionState = struct {
    title_storage: [max_session_title_bytes]u8 = [_]u8{0} ** max_session_title_bytes,
    title_len: usize = 0,
    usage_label_storage: [max_usage_label_bytes]u8 = [_]u8{0} ** max_usage_label_bytes,
    usage_label_len: usize = 0,
    usage_used: u64 = 0,
    usage_size: u64 = 0,

    pub fn title(state: *const SessionState) []const u8 {
        return state.title_storage[0..state.title_len];
    }

    pub fn hasUsage(state: *const SessionState) bool {
        return state.usage_size > 0;
    }

    pub fn usageLabel(state: *const SessionState) []const u8 {
        return state.usage_label_storage[0..state.usage_label_len];
    }

    fn project(state: *SessionState, info: agent_wire.SessionInfo, usage: ?agent_wire.Usage) void {
        state.title_len = 0;
        if (info.title) |value| {
            if (validTitle(value)) copyText(&state.title_storage, &state.title_len, value);
        }
        state.usage_used = 0;
        state.usage_size = 0;
        state.usage_label_len = 0;
        const value = usage orelse return;
        if (value.size == 0 or value.used > value.size) return;
        state.usage_used = value.used;
        state.usage_size = value.size;
        const percent: u64 = @intCast((@as(u128, value.used) * 100 + value.size / 2) / value.size);
        const label = std.fmt.bufPrint(&state.usage_label_storage, "{d}% context", .{percent}) catch return;
        state.usage_label_len = label.len;
    }
};

pub fn project(model: anytype, session: *SessionState, capabilities: agent_wire.Capabilities) void {
    session.project(capabilities.session_info, capabilities.usage);
    for (&model.agent_config_options) |*option| option.* = .{};
    for (&model.agent_commands) |*entry| entry.* = .{};
    model.agent_config_option_count = 0;
    model.agent_command_count = 0;
    var next_action_id: u16 = 1;
    for (capabilities.config_options) |wire| {
        if (model.agent_config_option_count == model.agent_config_options.len) break;
        const option = &model.agent_config_options[model.agent_config_option_count];
        if (wire.id.len == 0 or wire.id.len > option.id_storage.len or
            wire.name.len == 0 or wire.name.len > option.name_storage.len) continue;
        const kind = switch (wire.kind) {
            .object => |object| object,
            else => continue,
        };
        const type_name = switch (kind.get("type") orelse continue) {
            .string => |value| value,
            else => continue,
        };
        option.index = @intCast(model.agent_config_option_count);
        copyText(&option.id_storage, &option.id_len, wire.id);
        copyText(&option.name_storage, &option.name_len, wire.name);
        if (std.mem.eql(u8, type_name, "select")) {
            const current = switch (kind.get("current_value") orelse continue) {
                .string => |value| value,
                else => continue,
            };
            for (wire.choices) |choice| {
                if (option.choice_count == option.choices.len) break;
                const choice_view = &option.choices[option.choice_count];
                if (choice.value.len == 0 or choice.value.len > choice_view.value_storage.len or
                    choice.name.len == 0 or choice.name.len > choice_view.name_storage.len) continue;
                choice_view.action_id = next_action_id;
                next_action_id +%= 1;
                copyText(&choice_view.value_storage, &choice_view.value_len, choice.value);
                copyText(&choice_view.name_storage, &choice_view.name_len, choice.name);
                choice_view.selected = std.mem.eql(u8, choice.value, current);
                if (choice_view.selected) copyText(&option.current_storage, &option.current_len, choice.name);
                option.choice_count += 1;
            }
            if (option.choice_count == 0) continue;
            if (option.current_len == 0) copyText(&option.current_storage, &option.current_len, current);
        } else if (std.mem.eql(u8, type_name, "boolean")) {
            const current = switch (kind.get("current_value") orelse continue) {
                .bool => |value| value,
                else => continue,
            };
            option.is_boolean = true;
            const values = [_]struct { value: []const u8, name: []const u8, selected: bool }{
                .{ .value = "true", .name = "On", .selected = current },
                .{ .value = "false", .name = "Off", .selected = !current },
            };
            for (values) |value| {
                const choice_view = &option.choices[option.choice_count];
                choice_view.action_id = next_action_id;
                next_action_id +%= 1;
                copyText(&choice_view.value_storage, &choice_view.value_len, value.value);
                copyText(&choice_view.name_storage, &choice_view.name_len, value.name);
                choice_view.selected = value.selected;
                if (value.selected) copyText(&option.current_storage, &option.current_len, value.name);
                option.choice_count += 1;
            }
        } else continue;
        model.agent_config_option_count += 1;
    }
    for (capabilities.available_commands) |wire| {
        if (model.agent_command_count == model.agent_commands.len) break;
        const entry = &model.agent_commands[model.agent_command_count];
        if (wire.name.len == 0 or wire.name.len > entry.name_storage.len) continue;
        entry.index = @intCast(model.agent_command_count);
        copyText(&entry.name_storage, &entry.name_len, wire.name);
        if (std.mem.startsWith(u8, wire.name, "$")) {
            copyText(&entry.label_storage, &entry.label_len, "Skill · ");
            appendText(&entry.label_storage, &entry.label_len, wire.name[1..]);
        } else if (std.mem.eql(u8, wire.name, "skills")) {
            copyText(&entry.label_storage, &entry.label_len, "Skills");
        } else {
            copyText(&entry.label_storage, &entry.label_len, "/");
            appendText(&entry.label_storage, &entry.label_len, wire.name);
        }
        if (wire.description) |description| {
            appendText(&entry.label_storage, &entry.label_len, " · ");
            appendText(&entry.label_storage, &entry.label_len, description);
        }
        model.agent_command_count += 1;
    }
}

fn validTitle(value: []const u8) bool {
    if (value.len == 0 or value.len > max_session_title_bytes or !std.unicode.utf8ValidateSlice(value)) return false;
    for (value) |byte| if (std.ascii.isControl(byte)) return false;
    return true;
}

fn copyText(destination: []u8, length: *usize, value: []const u8) void {
    const bounded_length = utf8BoundedLength(value, destination.len);
    @memcpy(destination[0..bounded_length], value[0..bounded_length]);
    length.* = bounded_length;
}

fn appendText(destination: []u8, length: *usize, value: []const u8) void {
    if (length.* >= destination.len) return;
    const bounded_length = utf8BoundedLength(value, destination.len - length.*);
    @memcpy(destination[length.*..][0..bounded_length], value[0..bounded_length]);
    length.* += bounded_length;
}

fn utf8BoundedLength(value: []const u8, maximum: usize) usize {
    var length = @min(value.len, maximum);
    while (length > 0 and !std.unicode.utf8ValidateSlice(value[0..length])) : (length -= 1) {}
    return length;
}

test "session metadata and context usage stay bounded" {
    var state = SessionState{};
    state.project(.{ .title = "Refactor auth" }, .{ .used = 53_000, .size = 200_000 });
    try std.testing.expectEqualStrings("Refactor auth", state.title());
    try std.testing.expectEqualStrings("27% context", state.usageLabel());

    state.project(.{ .title = "unsafe\nlabel" }, .{ .used = 2, .size = 1 });
    try std.testing.expectEqualStrings("", state.title());
    try std.testing.expect(!state.hasUsage());
}
