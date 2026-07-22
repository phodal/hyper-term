use super::*;

pub(super) async fn decide_permission(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentPermissionRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Permission decision is invalid");
        }
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    match runtime.config.daemon.approval_detail(request.operation_id) {
        Ok(approval)
            if approval.detail.operation_revision == request.expected_revision
                && match request.approval_detail_digest.as_ref() {
                    Some(digest) => approval.detail_digest == *digest,
                    None => !matches!(
                        request.decision,
                        PermissionDecision::AllowOnce | PermissionDecision::AllowAlways
                    ),
                } => {}
        Ok(_) | Err(_) => {
            return status_response(
                StatusCode::CONFLICT,
                "Permission decision no longer matches the reviewed approval detail",
            );
        }
    }
    match runtime.local_mcp.has_pending_launch(request.operation_id) {
        Ok(true) => {
            let receipt = match runtime
                .local_mcp
                .resolve_launch(
                    session.task_id,
                    request.operation_id,
                    request.expected_revision,
                    request.decision,
                )
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => return local_mcp_error_response(error),
            };
            let operation = match runtime.config.daemon.operation(request.operation_id) {
                Ok(operation) => operation,
                Err(_) => {
                    return status_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Local MCP launch state is unavailable",
                    );
                }
            };
            return json_response(
                StatusCode::ACCEPTED,
                &local_mcp_operation_response(operation, receipt, None, None),
            );
        }
        Ok(false) => {}
        Err(error) => return local_mcp_error_response(error),
    }
    match runtime.local_mcp.has_pending_call(request.operation_id) {
        Ok(true) => {
            let execution = match runtime
                .local_mcp
                .resolve_tool_call(
                    session.task_id,
                    request.operation_id,
                    request.expected_revision,
                    request.decision,
                )
                .await
            {
                Ok(execution) => execution,
                Err(error) => return local_mcp_error_response(error),
            };
            let operation = match runtime.config.daemon.operation(request.operation_id) {
                Ok(operation) => operation,
                Err(_) => {
                    return status_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Local MCP call state is unavailable",
                    );
                }
            };
            let (receipt, result) = execution.map_or((None, None), |execution| {
                let result = serde_json::to_value(execution.result).ok();
                (Some(execution.receipt), result)
            });
            return json_response(
                StatusCode::ACCEPTED,
                &local_mcp_operation_response(operation, None, receipt, result),
            );
        }
        Ok(false) => {}
        Err(error) => return local_mcp_error_response(error),
    }
    let result =
        tokio::task::spawn_blocking(move || runtime.decide_effect(session_id, request)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::UnsafeApproval)) => status_response(
            StatusCode::FORBIDDEN,
            "Allow is unavailable until the Rust sandbox can enforce the exact effect",
        ),
        Ok(Err(SessionError::NoPendingEffect | SessionError::StalePermission)) => status_response(
            StatusCode::CONFLICT,
            "Permission decision no longer matches the pending effect",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Permission decision could not be delivered safely",
        ),
    }
}
