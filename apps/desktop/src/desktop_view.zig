const std = @import("std");
const native_sdk = @import("native_sdk");
const agent_block_view = @import("agent_block_view.zig");

const canvas = native_sdk.canvas;

pub const Options = struct {
    max_sessions: usize,
    max_inline_session_tabs: usize,
    max_agent_blocks: usize,
    max_agent_goal_step_columns: usize,
};

pub fn DesktopView(
    comptime Model: type,
    comptime Msg: type,
    comptime CompiledHyperTermView: type,
    comptime displayColumnPrefixLength: fn ([]const u8, usize) usize,
    comptime config: Options,
) type {
    return struct {
        const Ui = canvas.Ui(Msg);
        const AgentMarkdown = native_sdk.markdown.Markdown(Msg);

        const agent_timeline_id = "agent-blocks";
        const agent_timeline_estimated_width: usize = 84;
        const agent_timeline_line_height: f32 = 19;
        const agent_timeline_viewport_fallback: f32 = 480;

        fn agentSearchFiltering(model: *const Model) bool {
            return model.agentSearchOpen() and model.hasAgentSearchQuery();
        }

        fn agentTimelineBlockIndex(model: *const Model, list_index: u64) ?usize {
            if (!agentSearchFiltering(model)) {
                if (list_index < model.agent_block_index_base) return null;
                const physical = list_index - model.agent_block_index_base;
                if (physical >= model.agent_block_count) return null;
                return @intCast(physical);
            }
            const query = model.agentSearchQuery();
            var match_index: u64 = 0;
            for (model.agent_blocks[0..model.agent_block_count], 0..) |*block, physical| {
                if (!agent_block_view.matchesQuery(block, query)) continue;
                if (match_index == list_index) return physical;
                match_index += 1;
            }
            return null;
        }

        fn agentBlockExtentEstimate(context: ?*const anyopaque, logical_index: u64) f32 {
            const pointer = context orelse return 36;
            const model: *const Model = @ptrCast(@alignCast(pointer));
            const physical = agentTimelineBlockIndex(model, logical_index) orelse return 36;
            const block = &model.agent_blocks[physical];
            const lines = @max(@as(usize, 1), (block.content_len + agent_timeline_estimated_width - 1) / agent_timeline_estimated_width);
            const text_extent = @as(f32, @floatFromInt(@min(lines, 96))) * agent_timeline_line_height;
            const diff_extent = if (block.expanded and block.diff_file_count > 0)
                26 + @as(f32, @floatFromInt(block.diff_file_count)) * 24 +
                    @as(f32, @floatFromInt(@intFromBool(block.diff_files_truncated))) * 20
            else
                0;
            return switch (block.kind) {
                .message => if (block.role == .system and !block.expanded)
                    28
                else if (block.role == .user)
                    24 + text_extent
                else
                    10 + text_extent,
                .tool_call, .plan => if (block.expanded) 42 + diff_extent + text_extent else 30,
                .operation => 36 + @min(text_extent, agent_timeline_line_height),
                .approval => if (block.isApprovalPending())
                    118 + @min(text_extent, agent_timeline_line_height * 5)
                else if (block.expanded)
                    58 + @min(text_extent, agent_timeline_line_height * 5)
                else
                    30,
            };
        }

        pub fn agentTimelineOptions(model: *const Model) Ui.VirtualListOptions {
            const filtering = agentSearchFiltering(model);
            return .{
                .id = agent_timeline_id,
                .item_count = model.agentSearchResultCount(),
                .index_base = if (filtering) 0 else model.agent_block_index_base,
                .item_extent = 36,
                .extent_estimate = agentBlockExtentEstimate,
                .extent_context = model,
                .gap = 2,
                .overscan = 4,
                .grow = 1,
                .viewport_fallback = agent_timeline_viewport_fallback,
                .anchor = if (filtering) .leading else .trailing,
                .semantics = .{ .label = "Agent blocks" },
            };
        }

        fn agentTimeline(ui: *Ui, model: *const Model) Ui.Node {
            const options = agentTimelineOptions(model);
            if (options.item_count == 0 and agentSearchFiltering(model)) {
                return ui.column(.{
                    .grow = 1,
                    .cross = .center,
                    .main = .center,
                    .semantics = .{ .label = "No Agent history results" },
                }, .{
                    ui.text(.{ .style_tokens = .{ .foreground = .text_muted } }, "No matching Agent activity"),
                });
            }
            const window = ui.virtualWindow(options);
            const rows = ui.arena.alloc(Ui.Node, window.itemCount()) catch {
                ui.failed = true;
                return ui.column(.{ .grow = 1 }, .{});
            };
            for (rows, 0..) |*row, offset| {
                const list_index: u64 = @intCast(window.start_index + offset);
                const physical = agentTimelineBlockIndex(model, list_index + options.index_base) orelse {
                    ui.failed = true;
                    return ui.column(.{ .grow = 1 }, .{});
                };
                var node = agentBlockNode(ui, model, &model.agent_blocks[physical]);
                node.key = .{ .int = model.agent_blocks[physical].id };
                row.* = node;
            }
            const timeline = ui.virtualList(options, window, .{rows});
            const transcript = if (!model.agent_history_clipped)
                timeline
            else
                ui.column(.{ .grow = 1 }, .{
                    ui.text(.{ .padding = 6, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("Older activity is compacted · showing the latest {d} blocks", .{config.max_agent_blocks})),
                    timeline,
                });
            const content = if (model.agent_tier2_result_count == 0 and
                !model.agent_plan_visible and !model.agent_goal_visible)
                transcript
            else
                ui.column(.{ .grow = 1 }, .{
                    transcript,
                    agentContextShelfNode(ui, model),
                });
            if (model.hasAgentEditor()) return content;
            return ui.column(.{
                .grow = 1,
                .padding = 10,
                .semantics = .{ .label = "Agent reading rail" },
            }, .{content});
        }

        fn agentContextShelfNode(ui: *Ui, model: *const Model) Ui.Node {
            return ui.column(.{
                .gap = 3,
                .padding = 4,
                .semantics = .{ .label = "Agent context shelf" },
            }, .{
                if (model.agent_tier2_result_count > 0)
                    agentTier2ResultsNode(ui, model)
                else
                    ui.el(.stack, .{}, .{}),
                if (model.agent_plan_visible)
                    agentPlanNode(ui, &model.agent_plan)
                else
                    ui.el(.stack, .{}, .{}),
                if (model.agent_goal_visible)
                    agentGoalNode(ui, model)
                else
                    ui.el(.stack, .{}, .{}),
            });
        }

        fn agentTier2ResultsNode(ui: *Ui, model: *const Model) Ui.Node {
            const results = model.agentTier2Results();
            const nodes = ui.arena.alloc(Ui.Node, results.len) catch {
                ui.failed = true;
                return ui.column(.{}, .{});
            };
            for (results, nodes) |*result, *node| {
                node.* = agentTier2ResultNode(ui, model, result);
                node.key = .{ .str = result.sourceOperationId() };
            }
            return ui.column(.{ .gap = 3, .semantics = .{ .label = "Tier 2 review results" } }, .{nodes});
        }

        fn agentTier2ResultNode(
            ui: *Ui,
            model: *const Model,
            result: anytype,
        ) Ui.Node {
            const preview_selected = std.mem.eql(
                u8,
                result.sourceOperationId(),
                model.agent_tier2_preview_source_storage[0..model.agent_tier2_preview_source_len],
            );
            const preview_ready = preview_selected and model.agent_tier2_preview_ready;
            const busy = model.agent_tier2_action_in_flight_session_id != 0;
            const first_file = if (result.file_count > 0) result.files[0].path() else "No accepted text files";
            const deleted_files = result.deletedFileCount();
            const first_file_deleted = result.file_count > 0 and std.mem.eql(u8, result.files[0].kind(), "deleted");
            const more_files = if (result.file_count > 1)
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("+{d} more", .{result.file_count - 1}))
            else
                ui.el(.stack, .{}, .{});
            const preview = if (!preview_ready)
                ui.el(.stack, .{}, .{})
            else
                ui.column(.{ .gap = 5 }, .{
                    ui.scroll(.{
                        .height = 180,
                        .semantics = .{ .label = "Rust-verified Tier 2 Diff" },
                        .style_tokens = .{ .background = .surface_subtle, .radius = .md },
                    }, if (model.agent_tier2_diff_len == 0)
                        ui.text(.{ .padding = 7, .style_tokens = .{ .foreground = .text_muted } }, "No textual Diff was produced.")
                    else
                        ui.paragraph(.{ .padding = 7, .wrap = true }, &.{.{
                            .text = model.agentTier2Diff(),
                            .monospace = true,
                        }})),
                    if (model.agent_tier2_diff_truncated)
                        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Diff preview clipped to the bounded desktop budget.")
                    else
                        ui.el(.stack, .{}, .{}),
                    if (!result.has_acceptance)
                        ui.row(.{ .gap = 6, .cross = .center }, .{
                            ui.text(.{ .grow = 1, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, "Preview only · no workspace permission created"),
                            ui.button(.{
                                .size = .sm,
                                .variant = .primary,
                                .disabled = busy,
                                .on_press = Msg{ .request_agent_tier2_review = result.sourceOperationId() },
                            }, "Request apply approval"),
                        })
                    else
                        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "WorkspaceWrite approval is waiting in the transcript."),
                });
            return ui.el(.card, .{
                // Cards default to a roomy 24 px inset. This shelf is a compact
                // status disclosure; the inner column owns its deliberate spacing.
                .padding = 1,
                .style_tokens = .{ .border_color = if (result.has_acceptance) .warning else .border },
            }, .{
                ui.column(.{ .gap = 5, .padding = 6 }, .{
                    ui.row(.{ .gap = 6, .cross = .center }, .{
                        ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .info } }, "edit"),
                        ui.text(.{}, "Retained changes"),
                        ui.text(.{ .grow = 1, .size = .sm }, first_file),
                        if (first_file_deleted)
                            ui.el(.badge, .{ .variant = .secondary, .text = "delete" }, .{})
                        else
                            ui.el(.stack, .{}, .{}),
                        more_files,
                        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, if (deleted_files == 0)
                            ui.fmt("{d} files · {d} bytes", .{ result.file_count, result.changed_bytes })
                        else
                            ui.fmt("{d} files · {d} deleted · {d} bytes", .{ result.file_count, deleted_files, result.changed_bytes })),
                        ui.el(.badge, .{ .variant = .secondary, .text = if (result.has_acceptance) "approval pending" else "not applied" }, .{}),
                        if (!result.has_acceptance)
                            ui.button(.{
                                .size = .sm,
                                .variant = .ghost,
                                .disabled = busy,
                                .on_press = Msg{ .discard_agent_tier2_result = result.sourceOperationId() },
                            }, "Discard")
                        else
                            ui.el(.stack, .{}, .{}),
                        ui.button(.{
                            .size = .sm,
                            .variant = .outline,
                            .disabled = busy,
                            .on_press = Msg{ .preview_agent_tier2_result = result.sourceOperationId() },
                        }, if (preview_ready) "Hide Diff" else if (preview_selected) "Loading Diff" else "Review Diff"),
                    }),
                    preview,
                }),
            });
        }

        fn agentBlockNode(ui: *Ui, model: *const Model, block: anytype) Ui.Node {
            return switch (block.kind) {
                .message => agentMessageNode(ui, model, block),
                .tool_call, .plan => agentActivityNode(ui, block),
                .operation => agentOperationNode(ui, block),
                .approval => agentApprovalNode(ui, model, block),
            };
        }

        fn agentMessageNode(ui: *Ui, model: *const Model, block: anytype) Ui.Node {
            const clipped = if (block.truncated)
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, if (block.isUserMessage()) "Block clipped to 8 KiB in this view." else "Response clipped to 8 KiB in this view.")
            else
                ui.el(.stack, .{}, .{});
            if (block.isUserMessage()) {
                return ui.row(.{ .padding = 1 }, .{
                    ui.spacer(1),
                    ui.el(.bubble, .{}, .{
                        ui.column(.{ .gap = 3, .padding = 6 }, .{
                            ui.text(.{ .wrap = true }, block.content()),
                            clipped,
                        }),
                    }),
                });
            }
            if (block.isSystemMessage()) {
                return ui.column(.{ .grow = 1 }, .{
                    ui.row(.{ .gap = 4, .padding = 1, .cross = .center }, .{
                        ui.button(.{
                            .size = .sm,
                            .variant = .ghost,
                            .icon = if (block.expanded) "chevron-down" else "chevron-right",
                            .on_press = Msg{ .toggle_agent_block = block.id },
                        }, "Session notice"),
                    }),
                    if (block.expanded)
                        ui.column(.{ .gap = 4, .padding = 6 }, .{
                            AgentMarkdown.view(ui, block.content(), .{}),
                            clipped,
                        })
                    else
                        ui.el(.stack, .{}, .{}),
                });
            }
            if (block.isThoughtMessage()) {
                const label = switch (model.agent_turn_status) {
                    .running, .waiting_approval => "Reasoning",
                    else => "Processed",
                };
                return ui.column(.{ .grow = 1 }, .{
                    ui.row(.{ .gap = 4, .padding = 2, .cross = .center }, .{
                        ui.button(.{
                            .size = .sm,
                            .variant = .ghost,
                            .icon = if (block.expanded) "chevron-down" else "chevron-right",
                            .on_press = Msg{ .toggle_agent_block = block.id },
                        }, label),
                    }),
                    if (block.expanded)
                        ui.column(.{ .gap = 4, .padding = 7 }, .{
                            AgentMarkdown.view(ui, block.content(), .{}),
                            clipped,
                        })
                    else
                        ui.el(.stack, .{}, .{}),
                });
            }
            return ui.column(.{ .gap = 3, .padding = 1 }, .{
                ui.column(.{ .padding = 1 }, .{AgentMarkdown.view(ui, block.content(), .{})}),
                clipped,
            });
        }

        fn agentPlanNode(ui: *Ui, block: anytype) Ui.Node {
            return ui.column(.{ .grow = 1, .semantics = .{ .label = "Agent turn plan" } }, .{
                ui.row(.{
                    .grow = 1,
                    .gap = 5,
                    .padding = 3,
                    .cross = .center,
                    .style_tokens = .{ .background = .surface_subtle, .radius = .lg },
                }, .{
                    ui.icon(.{ .width = 12, .height = 12, .style_tokens = .{ .foreground = .accent } }, "circle-dot"),
                    ui.button(.{
                        .grow = 1,
                        .size = .sm,
                        .variant = .ghost,
                        .icon = if (block.expanded) "chevron-down" else "chevron-right",
                        .on_press = Msg{ .toggle_agent_block = block.id },
                    }, block.activityTitle()),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.activityMeta()),
                }),
                if (block.expanded)
                    ui.column(.{ .gap = 4, .padding = 6 }, .{
                        AgentMarkdown.view(ui, block.content(), .{}),
                    })
                else
                    ui.el(.stack, .{}, .{}),
            });
        }

        fn agentGoalNode(ui: *Ui, model: *const Model) Ui.Node {
            const goal = &model.agent_goal;
            const objective = goal.objective();
            const title_length = displayColumnPrefixLength(objective, config.max_agent_goal_step_columns);
            const title = if (title_length < objective.len)
                ui.fmt("Goal · {s}…", .{objective[0..title_length]})
            else
                ui.fmt("Goal · {s}", .{objective});
            return ui.column(.{ .grow = 1, .semantics = .{ .label = "Persistent Agent goal" } }, .{
                ui.row(.{
                    .grow = 1,
                    .gap = 5,
                    .padding = 3,
                    .cross = .center,
                    .style_tokens = .{ .background = .surface_subtle, .radius = .lg },
                }, .{
                    ui.icon(.{ .width = 12, .height = 12, .style_tokens = .{ .foreground = .accent } }, "circle-dot"),
                    ui.button(.{
                        .grow = 1,
                        .size = .sm,
                        .variant = .ghost,
                        .icon = if (goal.expanded) "chevron-down" else "chevron-right",
                        .on_press = .toggle_agent_goal,
                    }, title),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, goal.meta()),
                    agentGoalActionsNode(ui, model),
                }),
                if (goal.expanded)
                    ui.text(.{ .padding = 6, .wrap = true }, objective)
                else
                    ui.el(.stack, .{}, .{}),
            });
        }

        fn agentGoalActionsNode(ui: *Ui, model: *const Model) Ui.Node {
            var children: [2]Ui.Node = undefined;
            children[0] = ui.button(.{
                .size = .icon,
                .variant = .ghost,
                .icon = "menu",
                .disabled = model.agentGoalActionDisabled(),
                .on_press = .toggle_agent_goal_menu,
                .semantics = .{ .label = "Goal actions" },
            }, "");
            var child_count: usize = 1;
            if (model.agent_goal_menu_open) {
                var items: [3]Ui.Node = undefined;
                var item_count: usize = 0;
                items[item_count] = agentGoalMenuItem(ui, "Edit goal", .edit_agent_goal, .default, model.agentGoalEditDisabled());
                item_count += 1;
                if (model.agent_goal.status == .paused) {
                    items[item_count] = agentGoalMenuItem(ui, "Resume goal", .{ .apply_agent_goal_action = .resume_goal }, .default, false);
                } else {
                    items[item_count] = agentGoalMenuItem(ui, "Pause goal", .{ .apply_agent_goal_action = .pause_goal }, .default, false);
                }
                item_count += 1;
                items[item_count] = agentGoalMenuItem(ui, "Clear goal", .{ .apply_agent_goal_action = .clear_goal }, .destructive, false);
                item_count += 1;
                children[child_count] = ui.el(.dropdown_menu, .{
                    .width = 170,
                    .gap = 2,
                    .anchor = .above,
                    .anchor_alignment = .end,
                    .on_dismiss = .dismiss_agent_goal_menu,
                    .semantics = .{ .label = "Goal actions" },
                }, items[0..item_count]);
                child_count += 1;
            }
            return ui.stack(.{}, children[0..child_count]);
        }

        fn agentGoalMenuItem(
            ui: *Ui,
            label: []const u8,
            msg: Msg,
            variant: canvas.WidgetVariant,
            disabled: bool,
        ) Ui.Node {
            var item = ui.el(.menu_item, .{ .variant = variant, .disabled = disabled, .on_press = msg }, .{});
            item.widget.text = label;
            return item;
        }

        fn agentActivityNode(ui: *Ui, block: anytype) Ui.Node {
            return agentActivityNodeWithWidth(ui, block, 0);
        }

        fn agentDiffFilesNode(ui: *Ui, block: anytype) Ui.Node {
            const files = block.diffFiles();
            if (files.len == 0) return ui.el(.stack, .{}, .{});
            const rows = ui.arena.alloc(Ui.Node, files.len) catch {
                ui.failed = true;
                return ui.column(.{}, .{});
            };
            for (files, rows) |*file, *row| {
                row.* = ui.row(.{
                    .gap = 6,
                    .padding = 4,
                    .cross = .center,
                    .style_tokens = .{ .background = .surface_subtle, .radius = .sm },
                    .semantics = .{ .label = ui.fmt("Changed file {s}, plus {d}, minus {d}", .{ file.path(), file.added_lines, file.removed_lines }) },
                }, .{
                    ui.icon(.{ .width = 12, .height = 12, .style_tokens = .{ .foreground = .info } }, "file-text"),
                    ui.paragraph(.{ .grow = 1 }, &.{.{ .text = file.path(), .monospace = true }}),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .success } }, ui.fmt("+{d}", .{file.added_lines})),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .destructive } }, ui.fmt("-{d}", .{file.removed_lines})),
                });
                row.key = .{ .str = file.path() };
            }
            return ui.column(.{ .gap = 3, .semantics = .{ .label = "Changed files" } }, .{
                ui.row(.{ .gap = 5, .cross = .center }, .{
                    ui.text(.{ .grow = 1, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, "Changed files"),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{d} shown", .{files.len})),
                }),
                ui.column(.{ .gap = 3 }, .{rows}),
                if (block.diff_files_truncated)
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Additional changed files are hidden by the bounded Native view.")
                else
                    ui.el(.stack, .{}, .{}),
            });
        }

        fn agentActivityNodeWithWidth(ui: *Ui, block: anytype, width: f32) Ui.Node {
            return ui.column(.{
                .width = width,
                .grow = if (width == 0) 1 else 0,
            }, .{
                ui.row(.{ .gap = 5, .padding = 2, .cross = .center }, .{
                    ui.button(.{
                        .size = .sm,
                        .variant = .ghost,
                        .icon = if (block.expanded) "chevron-down" else "chevron-right",
                        .on_press = Msg{ .toggle_agent_block = block.id },
                    }, block.activityTitle()),
                    ui.spacer(1),
                    ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.activityMeta()),
                }),
                if (block.expanded)
                    ui.column(.{ .gap = 5, .padding = 7 }, .{
                        agentDiffFilesNode(ui, block),
                        if (block.hasActivityDetails()) AgentMarkdown.view(ui, block.content(), .{}) else ui.el(.stack, .{}, .{}),
                        if (block.truncated)
                            ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Tool details clipped to 8 KiB in this view.")
                        else
                            ui.el(.stack, .{}, .{}),
                    })
                else
                    ui.el(.stack, .{}, .{}),
            });
        }

        fn agentOperationNode(ui: *Ui, block: anytype) Ui.Node {
            return ui.row(.{ .gap = 7, .padding = 5, .cross = .center }, .{
                ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .info } }, "wrench"),
                ui.text(.{ .grow = 1, .wrap = true }, block.content()),
                ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{s} · {s}", .{ block.operationKindLabel(), block.riskLabel() })),
                ui.el(.badge, .{ .text = block.stateLabel(), .variant = .secondary }, .{}),
            });
        }

        fn agentApprovalNode(ui: *Ui, model: *const Model, block: anytype) Ui.Node {
            const live_approval = model.isLiveAgentApproval(block);
            if (!block.isApprovalPending() or !live_approval) {
                const title = if (block.isApprovalPending()) "Archived approval" else block.approvalTitle();
                const decision = if (block.isApprovalPending())
                    "Previous Agent runtime ended before a decision"
                else
                    ui.fmt("Decision: {s}", .{block.decisionLabel()});
                return ui.column(.{ .grow = 1 }, .{
                    ui.row(.{ .grow = 1, .gap = 5, .padding = 2, .cross = .center }, .{
                        ui.icon(.{ .width = 12, .height = 12, .style_tokens = .{ .foreground = .warning } }, "circle-dot"),
                        ui.button(.{
                            .size = .sm,
                            .variant = .ghost,
                            .icon = if (block.expanded) "chevron-down" else "chevron-right",
                            .on_press = Msg{ .toggle_agent_block = block.id },
                        }, title),
                        ui.spacer(1),
                        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{s} · {s}", .{ block.operationKindLabel(), block.riskLabel() })),
                    }),
                    if (block.expanded)
                        ui.column(.{
                            .gap = 4,
                            .padding = 7,
                            .style_tokens = .{ .background = .surface_subtle, .radius = .md },
                        }, .{
                            ui.text(.{ .wrap = true }, block.content()),
                            ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, decision),
                        })
                    else
                        ui.el(.stack, .{}, .{}),
                });
            }
            const decision = if (block.canAllowOnce())
                ui.row(.{ .gap = 6, .cross = .center }, .{
                    ui.text(.{ .grow = 1, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.approvalBoundaryLabel()),
                    ui.button(.{ .size = .sm, .variant = .outline, .on_press = Msg{ .cancel_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Cancel"),
                    ui.button(.{ .size = .sm, .variant = .destructive, .on_press = Msg{ .reject_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Reject"),
                    ui.button(.{ .size = .sm, .variant = .primary, .on_press = Msg{ .allow_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Allow once"),
                })
            else
                ui.row(.{ .gap = 6, .cross = .center }, .{
                    ui.text(.{ .grow = 1, .wrap = true, .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, block.approvalBoundaryLabel()),
                    ui.button(.{ .size = .sm, .variant = .outline, .on_press = Msg{ .cancel_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Cancel"),
                    ui.button(.{ .size = .sm, .variant = .destructive, .on_press = Msg{ .reject_agent_effect = block.operationId() }, .disabled = model.agentPermissionBusy() }, "Reject"),
                });
            return ui.el(.card, .{
                .global_key = canvas.uiKey(block.id),
                .style_tokens = .{ .border_color = .warning },
            }, .{
                ui.column(.{ .gap = 6, .padding = 8 }, .{
                    ui.row(.{ .gap = 6, .cross = .center }, .{
                        ui.icon(.{ .width = 13, .height = 13, .style_tokens = .{ .foreground = .warning } }, "alert"),
                        ui.text(.{ .grow = 1 }, block.approvalTitle()),
                        ui.text(.{ .size = .sm, .style_tokens = .{ .foreground = .text_muted } }, ui.fmt("{s} · {s}", .{ block.operationKindLabel(), block.riskLabel() })),
                    }),
                    ui.text(.{ .wrap = true }, block.content()),
                    if (block.approval_detail_valid)
                        ui.text(.{
                            .wrap = true,
                            .size = .sm,
                            .style_tokens = .{ .foreground = .text_muted },
                        }, block.approvalDetail())
                    else
                        ui.text(.{ .wrap = true, .size = .sm, .style_tokens = .{ .foreground = .warning } }, "Rust could not produce complete review detail; Allow is disabled."),
                    decision,
                }),
            });
        }

        fn sessionTab(ui: *Ui, model: *const Model, session: anytype) Ui.Node {
            const selected = session.id == model.active_session_id;
            return ui.el(.button_group, .{
                .global_key = canvas.uiKey(session.id),
                .gap = 0,
                .semantics = .{ .label = session.tabGroupLabel(ui.arena) },
            }, .{
                ui.button(.{
                    .size = .sm,
                    .variant = .ghost,
                    .icon = session.tabIcon(),
                    .selected = selected,
                    .on_press = Msg{ .select_session = session.id },
                    .context_menu = &.{.{
                        .label = "Close Tab",
                        .msg = Msg{ .close_session = session.id },
                    }},
                    .semantics = .{ .label = session.tabGroupLabel(ui.arena) },
                }, ui.fmt("{s} {d}", .{ session.tabTitle(), session.id })),
                ui.button(.{
                    .size = .sm,
                    .variant = .ghost,
                    .icon = "x",
                    .selected = selected,
                    .on_press = Msg{ .close_session = session.id },
                    .semantics = .{ .label = session.closeLabel(ui.arena) },
                }, ""),
            });
        }

        fn allTabsPicker(ui: *Ui, model: *const Model) Ui.Node {
            var children: [2]Ui.Node = undefined;
            children[0] = ui.button(.{
                .size = .sm,
                .variant = .ghost,
                .icon = "menu",
                .selected = model.session_picker_open,
                .on_press = .toggle_session_picker,
                .semantics = .{ .label = "Show all open tabs" },
            }, "All Tabs");
            var child_count: usize = 1;
            if (model.session_picker_open) {
                var items: [config.max_sessions]Ui.Node = undefined;
                for (model.openSessions(), 0..) |*session, index| {
                    var item = ui.el(.menu_item, .{
                        .global_key = canvas.uiKey(session.id),
                        .icon = session.tabIcon(),
                        .selected = session.id == model.active_session_id,
                        .on_press = Msg{ .select_session = session.id },
                        .semantics = .{ .label = session.tabGroupLabel(ui.arena) },
                    }, .{});
                    item.widget.text = ui.fmt("{s} {d}", .{ session.tabTitle(), session.id });
                    items[index] = item;
                }
                children[child_count] = ui.el(.dropdown_menu, .{
                    .width = 300,
                    .gap = 2,
                    .anchor = .below,
                    .anchor_alignment = .start,
                    .anchor_offset = 8,
                    .on_dismiss = .dismiss_session_picker,
                    .semantics = .{ .label = "All open tabs" },
                }, items[0..model.session_count]);
                child_count += 1;
            }
            return ui.stack(.{}, children[0..child_count]);
        }

        fn sessionTabs(ui: *Ui, model: *const Model) Ui.Node {
            var children: [config.max_inline_session_tabs + 1]Ui.Node = undefined;
            var child_count: usize = 0;
            for (model.inlineSessions()) |*session| {
                children[child_count] = sessionTab(ui, model, session);
                child_count += 1;
            }
            if (model.hasSessionOverflow()) {
                children[child_count] = allTabsPicker(ui, model);
                child_count += 1;
            }
            return ui.row(.{
                .gap = 4,
                .grow = 1,
                .cross = .center,
                .semantics = .{ .label = "Open sessions" },
            }, children[0..child_count]);
        }

        fn replaceNodeByLabel(ui: *Ui, source: Ui.Node, label: []const u8, replacement: Ui.Node) Ui.Node {
            if (std.mem.eql(u8, source.widget.semantics.label, label)) return replacement;
            if (source.nodes.len == 0) return source;
            var result = source;
            const children = ui.arena.alloc(Ui.Node, source.nodes.len) catch {
                ui.failed = true;
                return source;
            };
            for (source.nodes, children) |child, *output| {
                output.* = replaceNodeByLabel(ui, child, label, replacement);
            }
            result.nodes = children;
            return result;
        }

        /// Stable Zig composition seam for the product shell. Today the complete
        /// shell is one compiled Native markup document; builder-owned surfaces such
        /// as the windowed Agent transcript can replace individual branches here
        /// without moving the rest of the design system out of `.native` fragments.
        pub fn rootView(ui: *Ui, model: *const Model) Ui.Node {
            const shell = CompiledHyperTermView.build(ui, model);
            const with_tabs = replaceNodeByLabel(ui, shell, "Open sessions", sessionTabs(ui, model));
            if (model.isTerminal()) return with_tabs;
            return replaceNodeByLabel(ui, with_tabs, "Agent blocks", agentTimeline(ui, model));
        }
    };
}
