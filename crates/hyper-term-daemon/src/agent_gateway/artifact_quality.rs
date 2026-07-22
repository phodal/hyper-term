//! Accepted-artifact render payload and host-owned visual quality routes.
//!
//! Keeping this vertical slice outside `agent_gateway.rs` prevents the legacy
//! gateway hotspot from growing while the 2000-line module split proceeds.

use axum::body::Bytes;
use axum::extract::{Path as RoutePath, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use hyper_term_protocol::{ArtifactId, GenUiVisualQualityReport, GenUiVisualQualitySubmission};
use serde::Serialize;

use super::{
    AgentGatewayRuntime, AgentSessionQuery, StructuredAgentProtocol, authorize, json_response,
    parse_artifact_id, status_response,
};
use crate::artifact_visual_quality_store::VisualQualityStoreError;

#[derive(Serialize)]
struct AgentArtifactRenderPayloadResponse {
    artifact_id: ArtifactId,
    source_revision: u64,
    content_digest: String,
    bundle: String,
    css: String,
    source_map: String,
}

pub(super) async fn artifact_render_payload(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let Some(artifact_id) = parse_artifact_id(&artifact_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid");
    };
    match runtime.artifact_render_payload(session_id, artifact_id) {
        Ok(payload) => json_response(StatusCode::OK, &payload),
        Err(VisualQualityRequestError::SessionUnavailable) => status_response(
            StatusCode::NOT_FOUND,
            "Artifact render payload is unavailable",
        ),
        Err(VisualQualityRequestError::AcpRequired) => status_response(
            StatusCode::FORBIDDEN,
            "Artifact render payload is available only for ACP Agent artifacts",
        ),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact render payload could not be loaded",
        ),
    }
}

pub(super) async fn artifact_visual_quality(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let Some(artifact_id) = parse_artifact_id(&artifact_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid");
    };
    visual_quality_response(runtime.artifact_visual_quality(session_id, artifact_id))
}

pub(super) async fn submit_artifact_visual_quality(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let Some(artifact_id) = parse_artifact_id(&artifact_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid");
    };
    let submission = match serde_json::from_slice::<GenUiVisualQualitySubmission>(&body) {
        Ok(submission) => submission,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Visual quality observations are invalid",
            );
        }
    };
    visual_quality_response(runtime.submit_artifact_visual_quality(
        session_id,
        artifact_id,
        submission,
    ))
}

fn visual_quality_response(
    result: Result<GenUiVisualQualityReport, VisualQualityRequestError>,
) -> Response {
    match result {
        Ok(report) => json_response(StatusCode::OK, &report),
        Err(
            VisualQualityRequestError::SessionUnavailable
            | VisualQualityRequestError::ArtifactUnavailable
            | VisualQualityRequestError::ReportUnavailable,
        ) => status_response(
            StatusCode::NOT_FOUND,
            "Visual quality report is unavailable",
        ),
        Err(VisualQualityRequestError::AcpRequired) => status_response(
            StatusCode::FORBIDDEN,
            "Visual quality evidence is available only for ACP Agent artifacts",
        ),
        Err(VisualQualityRequestError::RuntimeUnavailable) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Packaged preview runtime identity is unavailable",
        ),
        Err(VisualQualityRequestError::InvalidObservation) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Visual quality observations violate the fixed capture contract",
        ),
        Err(VisualQualityRequestError::Lock | VisualQualityRequestError::Store) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Visual quality report could not be persisted",
        ),
    }
}

impl AgentGatewayRuntime {
    fn artifact_render_payload(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<AgentArtifactRenderPayloadResponse, VisualQualityRequestError> {
        let session = self.visual_quality_session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| VisualQualityRequestError::ArtifactUnavailable)?;
        Ok(AgentArtifactRenderPayloadResponse {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            content_digest: artifact.metadata.content_digest,
            bundle: artifact.bundle,
            css: artifact.css,
            source_map: artifact.source_map,
        })
    }

    fn artifact_visual_quality(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<GenUiVisualQualityReport, VisualQualityRequestError> {
        let session = self.visual_quality_session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| VisualQualityRequestError::ArtifactUnavailable)?;
        let runtime_digest = self
            .preview_runtime_digest
            .as_deref()
            .ok_or(VisualQualityRequestError::RuntimeUnavailable)?;
        let _guard = self
            .artifact_visual_quality_lock
            .lock()
            .map_err(|_| VisualQualityRequestError::Lock)?;
        self.artifact_visual_quality_store
            .load(session.task_id, &artifact.metadata, runtime_digest)
            .map_err(map_store_error)?
            .ok_or(VisualQualityRequestError::ReportUnavailable)
    }

    fn submit_artifact_visual_quality(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        submission: GenUiVisualQualitySubmission,
    ) -> Result<GenUiVisualQualityReport, VisualQualityRequestError> {
        let session = self.visual_quality_session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| VisualQualityRequestError::ArtifactUnavailable)?;
        let runtime_digest = self
            .preview_runtime_digest
            .as_deref()
            .ok_or(VisualQualityRequestError::RuntimeUnavailable)?;
        let _guard = self
            .artifact_visual_quality_lock
            .lock()
            .map_err(|_| VisualQualityRequestError::Lock)?;
        self.artifact_visual_quality_store
            .submit(
                session.task_id,
                &artifact.metadata,
                runtime_digest,
                submission,
            )
            .map_err(map_store_error)
    }

    fn visual_quality_session(
        &self,
        session_id: u16,
    ) -> Result<std::sync::Arc<super::AgentSession>, VisualQualityRequestError> {
        let session = self
            .session(session_id)
            .map_err(|_| VisualQualityRequestError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(VisualQualityRequestError::AcpRequired);
        }
        Ok(session)
    }
}

fn map_store_error(error: VisualQualityStoreError) -> VisualQualityRequestError {
    match error {
        VisualQualityStoreError::ContextMismatch
        | VisualQualityStoreError::InvalidObservation
        | VisualQualityStoreError::InvalidDigest
        | VisualQualityStoreError::InvalidReport
        | VisualQualityStoreError::DigestMismatch
        | VisualQualityStoreError::InvalidPath
        | VisualQualityStoreError::TooLarge => VisualQualityRequestError::InvalidObservation,
        VisualQualityStoreError::Io(_) | VisualQualityStoreError::Json(_) => {
            VisualQualityRequestError::Store
        }
    }
}

enum VisualQualityRequestError {
    SessionUnavailable,
    ArtifactUnavailable,
    ReportUnavailable,
    AcpRequired,
    RuntimeUnavailable,
    InvalidObservation,
    Lock,
    Store,
}
