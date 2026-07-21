use super::*;

#[derive(Clone)]
pub(super) struct AgentTurnProjection {
    turn_id: String,
    user_block_id: BlockId,
    user_message_id: Option<String>,
    user_message_phase: u32,
    user_message_bytes: usize,
    agent_block_id: BlockId,
    agent_message_phase: u32,
    plan_block_id: BlockId,
    agent_message_bytes: usize,
    agent_message_interrupted: bool,
    plan_bytes: usize,
}

pub(super) fn run_turn(session: Arc<AgentSession>, daemon: DaemonState, prompt: String) {
    let turn_id = match session
        .client
        .start_turn(&session.thread_id, &prompt, START_TURN_TIMEOUT)
    {
        Ok(turn_id) => turn_id,
        Err(error) => {
            set_progress_failed(&session, &agent_error_summary(&error.to_string()));
            return;
        }
    };
    let cancellation_requested = if let Ok(mut progress) = session.progress.lock() {
        progress.turn_id = Some(turn_id.clone());
        progress.status == AgentStatus::Cancelling
    } else {
        let _ = session.client.close();
        return;
    };

    if cancellation_requested
        && session
            .client
            .cancel_turn(&session.thread_id, &turn_id)
            .is_err()
    {
        set_progress_failed(&session, "Agent turn cancellation could not be delivered");
        return;
    }

    continue_turn(
        session,
        daemon,
        AgentTurnProjection {
            turn_id,
            user_block_id: BlockId::new(),
            user_message_id: None,
            user_message_phase: 0,
            user_message_bytes: 0,
            agent_block_id: BlockId::new(),
            agent_message_phase: 0,
            plan_block_id: BlockId::new(),
            agent_message_bytes: 0,
            agent_message_interrupted: false,
            plan_bytes: 0,
        },
    );
}

pub(super) fn continue_turn(
    session: Arc<AgentSession>,
    daemon: DaemonState,
    mut projection: AgentTurnProjection,
) {
    let deadline = Instant::now() + COMPLETE_TURN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            set_progress_failed(&session, "Agent turn exceeded its five-minute bound");
            let _ = session.client.close();
            return;
        }
        let event = match session.client.next_event(remaining) {
            Ok(event) => event,
            Err(error) => {
                set_progress_failed(&session, &agent_error_summary(&error.to_string()));
                return;
            }
        };
        match event {
            AgentDriverEvent::UserMessageDelta {
                thread_id,
                turn_id: event_turn_id,
                message_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                if text.is_empty() {
                    continue;
                }
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if projection.user_message_bytes > 0 && message_id != projection.user_message_id {
                    projection.user_message_phase = projection.user_message_phase.saturating_add(1);
                    projection.user_block_id = BlockId::new();
                }
                projection.user_message_bytes =
                    match projection.user_message_bytes.checked_add(text.len()) {
                        Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                        _ => {
                            set_progress_failed(
                                &session,
                                "Agent user-message replay exceeded its 256 KiB bound",
                            );
                            let _ = session.client.close();
                            return;
                        }
                    };
                projection.user_message_id = message_id.clone();
                let external_message_id = message_id.or_else(|| {
                    Some(format!(
                        "{}-user-message-{}",
                        projection.turn_id, projection.user_message_phase
                    ))
                });
                if daemon
                    .append_message(
                        session.task_id,
                        projection.user_block_id,
                        MessageRole::User,
                        external_message_id,
                        text,
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "Agent user message could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::MessageDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                if text.is_empty() {
                    continue;
                }
                if projection.agent_message_interrupted && projection.agent_message_bytes > 0 {
                    projection.agent_message_phase =
                        projection.agent_message_phase.saturating_add(1);
                    projection.agent_block_id = BlockId::new();
                }
                projection.agent_message_bytes = match projection
                    .agent_message_bytes
                    .checked_add(text.len())
                {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => {
                        set_progress_failed(&session, "Agent response exceeded its 256 KiB bound");
                        let _ = session.client.close();
                        return;
                    }
                };
                if daemon
                    .append_message(
                        session.task_id,
                        projection.agent_block_id,
                        MessageRole::Agent,
                        Some(format!(
                            "{}-message-{}",
                            projection.turn_id, projection.agent_message_phase
                        )),
                        text,
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "Agent response could not be journaled");
                    let _ = session.client.close();
                    return;
                }
                projection.agent_message_interrupted = false;
            }
            AgentDriverEvent::PlanDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if text.is_empty() {
                    continue;
                }
                projection.plan_bytes = match projection.plan_bytes.checked_add(text.len()) {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => continue,
                };
                let _ = daemon.append_message(
                    session.task_id,
                    projection.plan_block_id,
                    MessageRole::Thought,
                    Some(projection.turn_id.clone()),
                    text,
                );
            }
            AgentDriverEvent::PlanUpdated {
                thread_id,
                turn_id: event_turn_id,
                entries,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if daemon
                    .update_agent_plan(session.task_id, projection.turn_id.clone(), entries)
                    .is_err()
                {
                    set_progress_failed(&session, "Agent plan could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::ToolCallUpdated {
                thread_id,
                turn_id: event_turn_id,
                call,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if daemon
                    .update_agent_tool_call(session.task_id, projection.turn_id.clone(), call)
                    .is_err()
                {
                    set_progress_failed(&session, "Agent tool call could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::ThoughtDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if text.is_empty() {
                    continue;
                }
                projection.plan_bytes = match projection.plan_bytes.checked_add(text.len()) {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => continue,
                };
                let _ = daemon.append_message(
                    session.task_id,
                    projection.plan_block_id,
                    MessageRole::Thought,
                    Some(projection.turn_id.clone()),
                    text,
                );
            }
            AgentDriverEvent::EffectProposed { proposal, .. } => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                let (kind, risk) = operation_kind_and_risk(proposal.kind);
                let operation = match daemon.propose_operation(
                    session.task_id,
                    kind,
                    OperationAction::Opaque {
                        kind: proposal.method.clone(),
                        payload_digest: proposal.payload_sha256.clone(),
                    },
                    proposal.summary.clone(),
                    risk,
                    proposal.required_capabilities.clone(),
                ) {
                    Ok(operation) => operation,
                    Err(_) => {
                        set_progress_failed(
                            &session,
                            "Agent effect proposal could not be journaled",
                        );
                        return;
                    }
                };
                let mut pending = match session.pending_effect.lock() {
                    Ok(pending) => pending,
                    Err(_) => {
                        set_progress_failed(
                            &session,
                            "Agent effect proposal could not be retained",
                        );
                        return;
                    }
                };
                if pending.is_some() {
                    set_progress_failed(&session, "Agent emitted overlapping effect proposals");
                    let _ = session.client.close();
                    return;
                }
                *pending = Some(PendingAgentEffect {
                    request_id: proposal.request_id,
                    payload_sha256: proposal.payload_sha256,
                    operation_id: operation.operation_id,
                    operation_revision: operation.revision,
                    projection,
                    host_request: None,
                });
                drop(pending);
                if let Ok(mut progress) = session.progress.lock()
                    && progress.status != AgentStatus::Cancelling
                {
                    progress.status = AgentStatus::WaitingApproval;
                }
                return;
            }
            AgentDriverEvent::HostRequest { request, .. }
                if request.thread_id == session.thread_id
                    && request.turn_id == projection.turn_id =>
            {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                let AgentHostOperation::TerminalCreate {
                    command,
                    args,
                    env,
                    cwd,
                    ..
                } = &request.operation
                else {
                    if !resolve_terminal_host_request(&session, request) {
                        return;
                    }
                    continue;
                };
                let terminal_count = match session.terminals.lock() {
                    Ok(terminals) => terminals.len(),
                    Err(_) => {
                        set_progress_failed(&session, "ACP terminal state could not be read");
                        return;
                    }
                };
                if terminal_count >= MAX_AGENT_TERMINALS {
                    if session
                        .client
                        .resolve_host_request(
                            &request.request_id,
                            AgentHostResponse::Error {
                                code: -32000,
                                message: "ACP terminal retention limit reached".into(),
                            },
                        )
                        .is_err()
                    {
                        set_progress_failed(
                            &session,
                            "ACP terminal rejection could not be returned",
                        );
                        let _ = session.client.close();
                        return;
                    }
                    continue;
                }
                let summary = std::iter::once(command.as_str())
                    .chain(args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                let terminal_command = TerminalCommand {
                    program: "/usr/bin/env".into(),
                    args: std::iter::once(command.clone())
                        .chain(args.iter().cloned())
                        .collect(),
                    cwd: Some(cwd.clone()),
                    env: env
                        .iter()
                        .map(|variable| (variable.name.clone(), variable.value.clone()))
                        .collect(),
                };
                let summary = format!("Agent terminal in Tier 2: {summary}");
                let operation = match daemon.propose_operation(
                    session.task_id,
                    OperationKind::Shell,
                    OperationAction::Shell {
                        command: terminal_command,
                    },
                    summary,
                    RiskClass::ExternalEffect,
                    vec!["shell".into(), "sandbox.isolated_task".into()],
                ) {
                    Ok(operation) => operation,
                    Err(_) => {
                        set_progress_failed(
                            &session,
                            "ACP terminal request could not be journaled",
                        );
                        return;
                    }
                };
                let mut pending = match session.pending_effect.lock() {
                    Ok(pending) => pending,
                    Err(_) => {
                        set_progress_failed(&session, "ACP terminal request could not be retained");
                        return;
                    }
                };
                if pending.is_some() {
                    set_progress_failed(&session, "Agent emitted overlapping host requests");
                    let _ = session.client.close();
                    return;
                }
                *pending = Some(PendingAgentEffect {
                    request_id: request.request_id.clone(),
                    payload_sha256: request.payload_sha256.clone(),
                    operation_id: operation.operation_id,
                    operation_revision: operation.revision,
                    projection,
                    host_request: Some(request),
                });
                drop(pending);
                if let Ok(mut progress) = session.progress.lock()
                    && progress.status != AgentStatus::Cancelling
                {
                    progress.status = AgentStatus::WaitingApproval;
                }
                return;
            }
            AgentDriverEvent::TurnCompleted {
                thread_id,
                turn_id: event_turn_id,
                status,
                ..
            } if thread_id == session.thread_id
                && event_turn_id
                    .as_deref()
                    .is_none_or(|value| value == projection.turn_id) =>
            {
                if status.as_deref() == Some("failed") {
                    set_progress_failed(&session, "Agent reported a failed turn");
                } else if let Ok(mut progress) = session.progress.lock() {
                    progress.status = AgentStatus::Completed;
                    progress.error = None;
                }
                return;
            }
            AgentDriverEvent::Exited { .. } => {
                set_progress_failed(&session, "Agent exited before the turn completed");
                return;
            }
            _ => {}
        }
    }
}

fn resolve_terminal_host_request(session: &AgentSession, request: AgentHostRequest) -> bool {
    let terminal_id = match &request.operation {
        AgentHostOperation::TerminalOutput { terminal_id }
        | AgentHostOperation::TerminalRelease { terminal_id }
        | AgentHostOperation::TerminalWaitForExit { terminal_id }
        | AgentHostOperation::TerminalKill { terminal_id } => terminal_id,
        AgentHostOperation::TerminalCreate { .. } => return false,
    };
    let response = match session.terminals.lock() {
        Ok(mut terminals) => match &request.operation {
            AgentHostOperation::TerminalOutput { .. } => terminals
                .get(terminal_id)
                .map(|record| AgentHostResponse::TerminalOutput {
                    output: record.output.clone(),
                    truncated: record.truncated,
                    exit_code: record.exit_code,
                    signal: record.signal.clone(),
                })
                .unwrap_or_else(|| unknown_terminal_response(terminal_id)),
            AgentHostOperation::TerminalRelease { .. } => {
                if terminals.remove(terminal_id).is_some() {
                    AgentHostResponse::TerminalReleased
                } else {
                    unknown_terminal_response(terminal_id)
                }
            }
            AgentHostOperation::TerminalWaitForExit { .. } => terminals
                .get(terminal_id)
                .map(|record| AgentHostResponse::TerminalExited {
                    exit_code: record.exit_code,
                    signal: record.signal.clone(),
                })
                .unwrap_or_else(|| unknown_terminal_response(terminal_id)),
            AgentHostOperation::TerminalKill { .. } => {
                if terminals.contains_key(terminal_id) {
                    AgentHostResponse::TerminalKilled
                } else {
                    unknown_terminal_response(terminal_id)
                }
            }
            AgentHostOperation::TerminalCreate { .. } => return false,
        },
        Err(_) => AgentHostResponse::Error {
            code: -32000,
            message: "ACP terminal state is unavailable".into(),
        },
    };
    if session
        .client
        .resolve_host_request(&request.request_id, response)
        .is_err()
    {
        set_progress_failed(session, "ACP terminal response could not be returned");
        let _ = session.client.close();
        return false;
    }
    true
}

fn unknown_terminal_response(terminal_id: &str) -> AgentHostResponse {
    AgentHostResponse::Error {
        code: -32001,
        message: format!("Unknown ACP terminal: {terminal_id}"),
    }
}

pub(super) fn execute_agent_terminal_create(
    session: Arc<AgentSession>,
    daemon: DaemonState,
    runner: LimaTaskRunner,
    request: AgentHostRequest,
    operation_id: OperationId,
    authorized_revision: u64,
    projection: AgentTurnProjection,
) {
    let AgentHostOperation::TerminalCreate {
        output_byte_limit, ..
    } = &request.operation
    else {
        set_progress_failed(
            &session,
            "ACP terminal worker received a mismatched request",
        );
        return;
    };
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let response = match daemon.dispatch_isolated_task(
        session.task_id,
        operation_id,
        authorized_revision,
        &runner,
        &cancelled,
    ) {
        Ok(receipt) => {
            let terminal_id = format!("hyper-term-{operation_id}");
            let (output, truncated) = retain_terminal_output(
                &receipt.stdout,
                &receipt.stderr,
                *output_byte_limit as usize,
            );
            let exit_code = receipt.exit_code.and_then(|code| u32::try_from(code).ok());
            let signal = match receipt.termination {
                IsolatedTaskTermination::Exited => None,
                IsolatedTaskTermination::Signaled => Some("signaled".into()),
                IsolatedTaskTermination::TimedOut => Some("timeout".into()),
                IsolatedTaskTermination::Cancelled => Some("cancelled".into()),
            };
            match session.terminals.lock() {
                Ok(mut terminals) => {
                    terminals.insert(
                        terminal_id.clone(),
                        AgentTerminalRecord {
                            _source_operation_id: operation_id,
                            output,
                            truncated,
                            exit_code,
                            signal,
                        },
                    );
                    AgentHostResponse::TerminalCreated { terminal_id }
                }
                Err(_) => AgentHostResponse::Error {
                    code: -32000,
                    message: "ACP terminal result could not be retained".into(),
                },
            }
        }
        Err(error) => AgentHostResponse::Error {
            code: -32000,
            message: bounded_error(&format!("Tier 2 terminal execution failed: {error}")),
        },
    };
    if session
        .client
        .resolve_host_request(&request.request_id, response)
        .is_err()
    {
        set_progress_failed(&session, "ACP terminal result could not be returned");
        let _ = session.client.close();
        return;
    }
    continue_turn(session, daemon, projection);
}

pub(super) fn retain_terminal_output(
    stdout: &str,
    stderr: &str,
    output_byte_limit: usize,
) -> (String, bool) {
    let mut output = String::with_capacity(stdout.len().saturating_add(stderr.len()));
    output.push_str(stdout);
    output.push_str(stderr);
    if output.len() <= output_byte_limit {
        return (output, false);
    }
    let mut start = output.len().saturating_sub(output_byte_limit);
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    (output[start..].to_owned(), true)
}

fn operation_kind_and_risk(kind: AgentEffectKind) -> (OperationKind, RiskClass) {
    match kind {
        AgentEffectKind::Shell => (
            OperationKind::Other("agent_shell".into()),
            RiskClass::ExternalEffect,
        ),
        AgentEffectKind::WorkspaceEdit => (OperationKind::FileEdit, RiskClass::WorkspaceWrite),
        AgentEffectKind::Tool => (OperationKind::AgentTool, RiskClass::ExternalEffect),
        AgentEffectKind::ComputerUse => (OperationKind::ComputerUse, RiskClass::ExternalEffect),
        AgentEffectKind::Opaque => (
            OperationKind::Other("agent_effect".into()),
            RiskClass::ExternalEffect,
        ),
    }
}

pub(super) fn projected_agent_status(
    progress_status: AgentStatus,
    pending_operation_id: Option<OperationId>,
) -> AgentStatus {
    if pending_operation_id.is_some()
        && matches!(
            progress_status,
            AgentStatus::Ready | AgentStatus::Running | AgentStatus::Completed
        )
    {
        AgentStatus::WaitingApproval
    } else {
        progress_status
    }
}

pub(super) fn set_progress_failed(session: &AgentSession, message: &str) {
    if let Ok(mut progress) = session.progress.lock() {
        progress.status = AgentStatus::Failed;
        progress.error = Some(bounded_error(message));
    }
}

pub(super) fn bounded_error(message: &str) -> String {
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

pub(super) fn agent_error_summary(message: &str) -> String {
    const CODEX_UPGRADE_MARKER: &str = "requires a newer version of Codex";
    if message.contains(CODEX_UPGRADE_MARKER) {
        let model = message
            .find("The '")
            .and_then(|start| {
                let value = &message[start + "The '".len()..];
                value.find("' model").map(|end| &value[..end])
            })
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 96
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
            });
        return match model {
            Some(model) => format!(
                "Model {model} requires a newer Codex CLI · choose another model or update Codex"
            ),
            None => {
                "Selected model requires a newer Codex CLI · choose another model or update Codex"
                    .to_owned()
            }
        };
    }
    bounded_error(message)
}
