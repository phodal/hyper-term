use super::*;

pub(super) async fn agent_provider_statuses(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    let config = runtime.config.clone();
    match tokio::task::spawn_blocking(move || probe_agent_provider_statuses(&config)).await {
        Ok(statuses) => json_response(StatusCode::OK, &statuses),
        Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider readiness could not be refreshed",
        ),
    }
}

pub(super) async fn agent_attention(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    match runtime.attention() {
        Ok(sessions) => json_response(StatusCode::OK, &AgentAttentionResponse { sessions }),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent attention projection failed",
        ),
    }
}

pub(super) async fn workbench_index(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if query.session_id.is_some() {
        let session_id = match authorize(&runtime, &query) {
            Ok(session_id) => session_id,
            Err(response) => return *response,
        };
        if runtime.session(session_id).is_err() {
            return status_response(StatusCode::NOT_FOUND, "Agent session does not exist");
        }
    } else {
        if let Err(response) = authorize_gateway_token(&runtime, &query) {
            return *response;
        }
        if runtime.config.debug_capsule.is_none() {
            return status_response(StatusCode::NOT_FOUND, "Offline Bug Capsule is unavailable");
        }
    }
    serve_workbench_asset(&runtime, Path::new("index.html")).await
}

pub(super) async fn workbench_asset(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(path): RoutePath<String>,
) -> Response {
    if path.contains('%') {
        return status_response(StatusCode::BAD_REQUEST, "Workbench asset path is invalid");
    }
    let relative = Path::new(&path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return status_response(StatusCode::BAD_REQUEST, "Workbench asset path is invalid");
    }
    serve_workbench_asset(&runtime, relative).await
}

pub(super) async fn serve_workbench_asset(
    runtime: &AgentGatewayRuntime,
    relative: &Path,
) -> Response {
    let Some(root) = runtime.workbench_assets.as_ref() else {
        return status_response(StatusCode::NOT_FOUND, "Workbench is unavailable");
    };
    let candidate = root.join(relative);
    let Ok(candidate) = candidate.canonicalize() else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    let Ok(metadata) = std::fs::metadata(&candidate) else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    if !candidate.starts_with(root.as_ref())
        || !metadata.is_file()
        || metadata.len() > MAX_WORKBENCH_ASSET_BYTES
    {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    }
    let Ok(bytes) = tokio::fs::read(&candidate).await else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    let content_type = match candidate.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    let csp = if relative == Path::new("genui/preview.html") {
        WORKBENCH_PREVIEW_CSP
    } else {
        WORKBENCH_CSP
    };
    secure_response_with_csp(StatusCode::OK, content_type, Body::from(bytes), csp)
}

pub(super) async fn start_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let provider = query.provider.unwrap_or_else(|| "codex".into());
    if !valid_provider_id(&provider) {
        return status_response(StatusCode::BAD_REQUEST, "Agent provider id is invalid");
    }
    let result =
        tokio::task::spawn_blocking(move || runtime.start_agent(session_id, &provider)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(error)) => agent_start_error_response(error),
        Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider failed to initialize",
        ),
    }
}

pub(super) fn agent_start_error_response(error: StartError) -> Response {
    match error {
        StartError::Unavailable => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Requested Agent provider is unavailable",
        ),
        StartError::ProviderMismatch => status_response(
            StatusCode::CONFLICT,
            "Agent session already uses a different provider",
        ),
        StartError::Capacity => {
            status_response(StatusCode::TOO_MANY_REQUESTS, "Agent session limit reached")
        }
        StartError::Lock | StartError::Driver => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider failed to initialize",
        ),
    }
}

pub(super) async fn close_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let task_id = match runtime.close_session(session_id, true) {
        Ok(task_id) => task_id,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session history could not be forgotten safely",
            );
        }
    };
    if let Some(task_id) = task_id {
        runtime.local_mcp.close_task(task_id).await;
    }
    secure_response(
        StatusCode::NO_CONTENT,
        "text/plain; charset=utf-8",
        Body::empty(),
    )
}

pub(super) async fn local_mcp_status(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let registered = match runtime.local_mcp.registered_servers() {
        Ok(registered) => registered,
        Err(error) => return local_mcp_error_response(error),
    };
    let active = match runtime
        .local_mcp
        .active_server_receipts(session.task_id)
        .await
    {
        Ok(active) => active,
        Err(error) => return local_mcp_error_response(error),
    };
    json_response(
        StatusCode::OK,
        &AgentLocalMcpStatusResponse { registered, active },
    )
}

pub(super) async fn propose_local_mcp_launch(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let request = match serde_json::from_slice::<AgentLocalMcpLaunchRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Local MCP launch is invalid"),
    };
    match runtime
        .local_mcp
        .propose_launch(session.task_id, &request.server_id)
    {
        Ok(operation) => json_response(
            StatusCode::ACCEPTED,
            &local_mcp_operation_response(operation, None, None, None),
        ),
        Err(error) => local_mcp_error_response(error),
    }
}

pub(super) async fn propose_local_mcp_call(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let request = match serde_json::from_slice::<AgentLocalMcpCallRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Local MCP call is invalid"),
    };
    match runtime.local_mcp.propose_tool_call(
        session.task_id,
        &request.server_id,
        request.tool_name,
        request.arguments,
    ) {
        Ok(operation) => json_response(
            StatusCode::ACCEPTED,
            &local_mcp_operation_response(operation, None, None, None),
        ),
        Err(error) => local_mcp_error_response(error),
    }
}

pub(super) async fn snapshot_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    match runtime.snapshot(session_id) {
        Ok(snapshot) => json_response(StatusCode::OK, &snapshot),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent session snapshot failed",
        ),
    }
}

pub(super) fn encode_agent_stream_line(
    value: &impl Serialize,
) -> Result<Vec<u8>, AgentStreamError> {
    let mut line = serde_json::to_vec(value).map_err(|_| AgentStreamError::Encode)?;
    if line.len() + 1 > MAX_AGENT_STREAM_LINE_BYTES {
        return Err(AgentStreamError::TooLarge);
    }
    line.push(b'\n');
    Ok(line)
}

pub(super) async fn stream_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(SessionError::NotFound) => {
            return status_response(StatusCode::NOT_FOUND, "Agent session does not exist");
        }
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not start",
            );
        }
    };
    let task_id = session.task_id;
    let block_patches = match runtime.config.daemon.subscribe_block_patches() {
        Ok(receiver) => receiver,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not subscribe",
            );
        }
    };
    let initial_state = match runtime.stream_state(session_id) {
        Ok(state) => state,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not start",
            );
        }
    };
    let initial = match encode_agent_stream_line(&AgentStreamFrame::State {
        state: &initial_state,
    }) {
        Ok(line) => line,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session state exceeds the stream frame bound",
            );
        }
    };
    let (patch_sender, patch_receiver) = mpsc::channel(AGENT_STREAM_PATCH_QUEUE);
    let _ = std::thread::Builder::new()
        .name(format!("hyper-term-agent-stream-{session_id}"))
        .spawn(move || {
            while !patch_sender.is_closed() {
                match block_patches.recv_timeout(Duration::from_millis(100)) {
                    Ok((candidate_task_id, patch)) => {
                        if candidate_task_id == task_id
                            && patch_sender.blocking_send(patch).is_err()
                        {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    let mut refresh = tokio::time::interval(AGENT_STREAM_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let state = AgentEventStreamState {
        runtime,
        session_id,
        patches: patch_receiver,
        previous_state: initial.clone(),
        first_state: Some(initial),
        refresh,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if let Some(first) = state.first_state.take() {
            return Some((Ok::<Bytes, Infallible>(Bytes::from(first)), state));
        }
        loop {
            tokio::select! {
                patch = state.patches.recv() => {
                    let mut patch = patch?;
                    let mut patch_gap = false;
                    let cadence = tokio::time::sleep(AGENT_STREAM_FRAME_CADENCE);
                    tokio::pin!(cadence);
                    loop {
                        tokio::select! {
                            _ = &mut cadence => break,
                            next = state.patches.recv() => {
                                let Some(next) = next else { break };
                                if next.base_revision != patch.target_revision {
                                    patch.stream_sequence = next.stream_sequence;
                                    patch.target_revision = next.target_revision;
                                    patch_gap = true;
                                    break;
                                }
                                patch.stream_sequence = next.stream_sequence;
                                patch.target_revision = next.target_revision;
                                patch.operations.extend(next.operations);
                            }
                        }
                    }
                    let status = state.runtime.stream_status(state.session_id).ok()?;
                    let frame = if patch_gap {
                        AgentStreamFrame::Resync {
                            status,
                            target_revision: patch.target_revision,
                            reason: "patch_sequence_gap",
                        }
                    } else {
                        AgentStreamFrame::Patch {
                            status,
                            patch: &patch,
                        }
                    };
                    let line = match encode_agent_stream_line(&frame) {
                        Ok(line) => line,
                        Err(AgentStreamError::TooLarge) => encode_agent_stream_line(
                            &AgentStreamFrame::Resync {
                                status,
                                target_revision: patch.target_revision,
                                reason: "patch_frame_too_large",
                            },
                        ).ok()?,
                        Err(AgentStreamError::Encode) => return None,
                    };
                    return Some((Ok(Bytes::from(line)), state));
                }
                _ = state.refresh.tick() => {
                    let current = state.runtime.stream_state(state.session_id).ok()?;
                    let line = encode_agent_stream_line(&AgentStreamFrame::State {
                        state: &current,
                    }).ok()?;
                    if line == state.previous_state {
                        continue;
                    }
                    state.previous_state = line.clone();
                    return Some((Ok(Bytes::from(line)), state));
                }
            }
        }
    });
    secure_response(
        StatusCode::OK,
        "application/x-ndjson; charset=utf-8",
        Body::from_stream(stream),
    )
}

pub(super) async fn preview_artifact(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.preview_document(session_id, artifact_id) {
        Ok(document) => secure_response_with_csp(
            StatusCode::OK,
            "text/html; charset=utf-8",
            Body::from(document),
            PREVIEW_CSP,
        ),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact preview is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact preview could not be rendered",
        ),
    }
}

pub(super) async fn artifact_source_map(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_source_map(session_id, artifact_id) {
        Ok(source_map) => secure_response(
            StatusCode::OK,
            "application/json; charset=utf-8",
            Body::from(source_map),
        ),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source map is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact source map could not be read",
        ),
    }
}

pub(super) async fn artifact_source(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_source(session_id, artifact_id) {
        Ok(source) => json_response(StatusCode::OK, &source),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact source could not be read",
        ),
    }
}

pub(super) async fn artifact_editor_state(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result =
        tokio::task::spawn_blocking(move || runtime.artifact_editor_state(session_id, artifact_id))
            .await;
    artifact_editor_response(result)
}

pub(super) async fn save_artifact_editor_state(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<ArtifactEditorCheckpointRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Artifact editor checkpoint is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.save_artifact_editor_state(session_id, artifact_id, request)
    })
    .await;
    artifact_editor_response(result)
}

pub(super) fn artifact_editor_response(
    result: Result<Result<ArtifactEditorCheckpoint, ArtifactEditorError>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok(checkpoint)) => json_response(StatusCode::OK, &checkpoint),
        Ok(Err(
            ArtifactEditorError::SessionUnavailable | ArtifactEditorError::ArtifactUnavailable,
        )) => status_response(
            StatusCode::NOT_FOUND,
            "Artifact editor state is unavailable",
        ),
        Ok(Err(ArtifactEditorError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Artifact editor checkpoint no longer matches the current revision",
        ),
        Ok(Err(ArtifactEditorError::InvalidRequest)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Artifact editor checkpoint violates the bounded fixed-path state",
        ),
        Ok(Err(ArtifactEditorError::Lock | ArtifactEditorError::Store)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Artifact editor checkpoint could not be persisted",
            )
        }
    }
}

pub(super) async fn artifact_runtime_trace(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.artifact_runtime_trace(session_id, artifact_id)
    })
    .await;
    artifact_runtime_trace_response(result)
}

pub(super) async fn append_artifact_runtime_trace(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<GenUiRuntimeTraceAppendRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Runtime trace batch is invalid");
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.append_artifact_runtime_trace(session_id, artifact_id, request)
    })
    .await;
    artifact_runtime_trace_response(result)
}

pub(super) fn artifact_runtime_trace_response(
    result: Result<Result<GenUiRuntimeTraceProjection, RuntimeTraceError>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok(projection)) => json_response(StatusCode::OK, &projection),
        Ok(Err(RuntimeTraceError::SessionUnavailable | RuntimeTraceError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Runtime trace is unavailable")
        }
        Ok(Err(RuntimeTraceError::StaleRevision | RuntimeTraceError::Sequence)) => status_response(
            StatusCode::CONFLICT,
            "Runtime trace no longer matches the current Artifact stream",
        ),
        Ok(Err(RuntimeTraceError::InvalidRequest)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Runtime trace violates the bounded redacted event contract",
        ),
        Ok(Err(RuntimeTraceError::Lock | RuntimeTraceError::Store)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Runtime trace could not be persisted",
        ),
    }
}

pub(super) async fn artifact_debug_capsule(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.artifact_debug_capsule(session_id, artifact_id)
    })
    .await;
    match result {
        Ok(Ok(capsule)) => json_response(StatusCode::OK, &capsule),
        Ok(Err(
            BugCapsuleRequestError::SessionUnavailable
            | BugCapsuleRequestError::ArtifactUnavailable,
        )) => status_response(StatusCode::NOT_FOUND, "Bug capsule is unavailable"),
        Ok(Err(BugCapsuleRequestError::Lock | BugCapsuleRequestError::Store)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Bug capsule could not be prepared",
            )
        }
    }
}

pub(super) async fn offline_debug_capsule(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    match runtime.config.debug_capsule.as_ref() {
        Some(capsule) => json_response(StatusCode::OK, capsule),
        None => status_response(StatusCode::NOT_FOUND, "Offline Bug Capsule is unavailable"),
    }
}

pub(super) async fn artifact_history(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_history(session_id, artifact_id) {
        Ok(history) => json_response(StatusCode::OK, &history),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact history is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact history could not be read",
        ),
    }
}

pub(super) async fn artifact_history_source(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath((artifact_id, revision_id)): RoutePath<(String, String)>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let Some(artifact_id) = parse_artifact_id(&artifact_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid");
    };
    let Some(revision_id) = parse_artifact_id(&revision_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Revision id is invalid");
    };
    match runtime.artifact_history_source(session_id, artifact_id, revision_id) {
        Ok(source) => json_response(StatusCode::OK, &source),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => status_response(
            StatusCode::NOT_FOUND,
            "Artifact revision source is unavailable",
        ),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact revision source could not be read",
        ),
    }
}

pub(super) async fn artifact_editor_lsp(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<EditorLspRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid");
        }
    };
    if request.validate().is_err() {
        return status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid");
    }
    match tokio::task::spawn_blocking(move || {
        runtime.editor_lsp_query(session_id, artifact_id, request)
    })
    .await
    {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(EditorRequestError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(EditorRequestError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(EditorRequestError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Editor source revision is no longer current",
        ),
        Ok(Err(EditorRequestError::InvalidRequest)) => {
            status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid")
        }
        Ok(Err(EditorRequestError::RuntimeUnavailable)) => {
            status_response(StatusCode::SERVICE_UNAVAILABLE, "Editor LSP is unavailable")
        }
        Ok(Err(EditorRequestError::Driver)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Editor LSP request could not be completed",
        ),
    }
}

pub(super) async fn propose_artifact_draft(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentArtifactDraftRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Artifact draft is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_artifact_draft(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(ArtifactDraftError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(ArtifactDraftError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(ArtifactDraftError::StaleRevision | ArtifactDraftError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Artifact draft no longer matches the current revision",
        ),
        Ok(Err(ArtifactDraftError::InvalidRequest)) => {
            status_response(StatusCode::BAD_REQUEST, "Artifact draft is invalid")
        }
        Ok(Err(ArtifactDraftError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Artifact draft has no changes",
        ),
        Ok(Err(ArtifactDraftError::RuntimeUnavailable)) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Rust-supervised Deno artifact publishing is unavailable",
        ),
        Ok(Err(ArtifactDraftError::Daemon | ArtifactDraftError::Lock)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact draft could not enter the permission broker",
        ),
    }
}

pub(super) async fn artifact_draft_status(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentArtifactDraftStatusQuery>,
) -> Response {
    let session_id = match authorize(
        &runtime,
        &AgentSessionQuery {
            token: query.token,
            session_id: query.session_id,
            provider: None,
        },
    ) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let Some(operation_id) = query.operation_id else {
        return status_response(StatusCode::BAD_REQUEST, "Draft operation id is invalid");
    };
    match runtime.artifact_draft_status(session_id, artifact_id, operation_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(ArtifactDraftError::SessionUnavailable | ArtifactDraftError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact draft is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact draft status could not be read",
        ),
    }
}

pub(super) async fn propose_workspace_apply(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentWorkspaceApplyRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Workspace apply request is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_workspace_apply(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(WorkspaceProposalError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(WorkspaceProposalError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(WorkspaceProposalError::StaleRevision | WorkspaceProposalError::Busy)) => {
            status_response(
                StatusCode::CONFLICT,
                "Workspace apply no longer matches the current revision",
            )
        }
        Ok(Err(WorkspaceProposalError::RecoveryRequired)) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Workspace apply is blocked until an interrupted transaction is recovered",
        ),
        Ok(Err(WorkspaceProposalError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Workspace target already matches the artifact source",
        ),
        Ok(Err(WorkspaceProposalError::InvalidRequest | WorkspaceProposalError::UnsafeTarget)) => {
            status_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Workspace target is not a bounded regular file path",
            )
        }
        Ok(Err(WorkspaceProposalError::Daemon | WorkspaceProposalError::Lock)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Workspace apply could not enter the permission broker",
            )
        }
    }
}

pub(super) async fn preview_workspace_apply(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentWorkspaceApplyRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Workspace preview request is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.preview_workspace_apply(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(WorkspaceProposalError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(WorkspaceProposalError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(WorkspaceProposalError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Workspace preview no longer matches the current revision",
        ),
        Ok(Err(WorkspaceProposalError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Workspace targets already match the artifact source",
        ),
        Ok(Err(WorkspaceProposalError::InvalidRequest | WorkspaceProposalError::UnsafeTarget)) => {
            status_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Workspace targets are not bounded regular file paths",
            )
        }
        Ok(Err(
            WorkspaceProposalError::Busy
            | WorkspaceProposalError::RecoveryRequired
            | WorkspaceProposalError::Daemon
            | WorkspaceProposalError::Lock,
        ))
        | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Workspace preview could not be prepared",
        ),
    }
}

pub(super) async fn workspace_apply_status(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentWorkspaceApplyStatusQuery>,
) -> Response {
    let session_id = match authorize(
        &runtime,
        &AgentSessionQuery {
            token: query.token,
            session_id: query.session_id,
            provider: None,
        },
    ) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let Some(operation_id) = query.operation_id else {
        return status_response(
            StatusCode::BAD_REQUEST,
            "Workspace apply operation id is invalid",
        );
    };
    match runtime.workspace_apply_status(session_id, artifact_id, operation_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(
            WorkspaceProposalError::SessionUnavailable
            | WorkspaceProposalError::ArtifactUnavailable,
        ) => status_response(StatusCode::NOT_FOUND, "Workspace apply is unavailable"),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Workspace apply status could not be read",
        ),
    }
}

pub(super) async fn start_turn(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let prompt = match String::from_utf8(body.to_vec()) {
        Ok(prompt) if !prompt.trim().is_empty() => prompt,
        _ => return status_response(StatusCode::BAD_REQUEST, "Agent prompt is invalid"),
    };
    let is_goal_command = prompt.trim() == "/goal" || prompt.trim().starts_with("/goal ");
    let result = if is_goal_command {
        tokio::task::spawn_blocking(move || runtime.apply_goal_command(session_id, &prompt))
            .await
            .map_err(|_| SessionError::Thread)
            .and_then(|result| result)
    } else {
        runtime.submit_turn(session_id, prompt)
    };
    match result {
        Ok(response) => json_response(StatusCode::ACCEPTED, &response),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(SessionError::Busy) => status_response(
            StatusCode::CONFLICT,
            "Agent session already has an active turn",
        ),
        Err(SessionError::PromptTooLarge) => status_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Agent prompt exceeds its bound",
        ),
        Err(SessionError::Unsupported | SessionError::InvalidConfig) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Agent provider does not support this goal command",
        ),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent turn could not be started",
        ),
    }
}

pub(super) async fn cancel_turn(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let result = tokio::task::spawn_blocking(move || runtime.cancel_turn(session_id)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::NoActiveTurn)) => {
            status_response(StatusCode::CONFLICT, "Agent session has no active turn")
        }
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent turn cancellation could not be delivered safely",
        ),
    }
}

pub(super) async fn tier2_results(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    match runtime.tier2_results(session_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 review results could not be read",
        ),
    }
}

pub(super) async fn preview_tier2_result(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.preview_tier2_result(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 Diff could not be prepared safely",
        ),
    }
}

pub(super) async fn propose_tier2_review(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_tier2_review(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Tier 2 result already has a pending review",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 review could not enter the permission broker",
        ),
    }
}

pub(super) async fn discard_tier2_result(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.discard_tier2_result(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(())) => secure_response(
            StatusCode::NO_CONTENT,
            "text/plain; charset=utf-8",
            Body::empty(),
        ),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Reject the pending Tier 2 review before discarding its result",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 result could not be discarded",
        ),
    }
}

pub(super) async fn set_session_config(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentConfigRequest>(&body) {
        Ok(request) if !request.config_id.is_empty() && request.config_id.len() <= 128 => request,
        _ => return status_response(StatusCode::BAD_REQUEST, "Agent configuration is invalid"),
    };
    let result =
        tokio::task::spawn_blocking(move || runtime.set_session_config(session_id, request)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Agent configuration cannot change during an active turn",
        ),
        Ok(Err(SessionError::Unsupported)) => status_response(
            StatusCode::CONFLICT,
            "Agent provider does not expose session configuration",
        ),
        Ok(Err(SessionError::InvalidConfig)) => status_response(
            StatusCode::BAD_REQUEST,
            "Agent configuration value is invalid",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent configuration could not be updated",
        ),
    }
}

pub(super) fn authorize(
    runtime: &AgentGatewayRuntime,
    query: &AgentSessionQuery,
) -> Result<u16, Box<Response>> {
    authorize_gateway_token(runtime, query)?;
    let Some(session_id @ 1..=999) = query.session_id else {
        return Err(Box::new(status_response(
            StatusCode::BAD_REQUEST,
            "agent session id is invalid",
        )));
    };
    Ok(session_id)
}

pub(super) fn authorize_gateway_token(
    runtime: &AgentGatewayRuntime,
    query: &AgentSessionQuery,
) -> Result<(), Box<Response>> {
    if !constant_time_eq(
        query.token.as_deref().unwrap_or_default().as_bytes(),
        runtime.config.token.as_bytes(),
    ) {
        return Err(Box::new(status_response(
            StatusCode::UNAUTHORIZED,
            "agent gateway token is invalid",
        )));
    }
    Ok(())
}
