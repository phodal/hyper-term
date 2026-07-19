use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ArtifactId;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiCompilerIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiCompileDiagnostic {
    pub severity: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
}

/// A bounded compiler result that is still outside the Rust authority state.
///
/// The daemon revalidates this value before persisting it and recording an
/// `ArtifactAccepted` event. Renderers must never treat a candidate as an
/// accepted artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiArtifactCandidate {
    pub schema_version: u16,
    pub source_revision: u64,
    pub entrypoint: String,
    /// The exact bounded virtual source tree that produced this candidate.
    ///
    /// Older stored artifacts did not retain source. The default keeps those
    /// files readable during migration, while new acceptance requires a
    /// non-empty snapshot before the candidate can enter authority state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_files: BTreeMap<String, String>,
    pub bundle: String,
    pub css: String,
    pub source_map: String,
    pub content_digest: String,
    pub compiler: GenUiCompilerIdentity,
    #[serde(default)]
    pub diagnostics: Vec<GenUiCompileDiagnostic>,
}

/// Journal-safe metadata for a GenUI artifact accepted by the Rust authority.
///
/// Executable content stays in the daemon's private artifact store instead of
/// entering the event journal or BlockDocument.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptedGenUiArtifact {
    pub artifact_id: ArtifactId,
    pub source_revision: u64,
    pub entrypoint: String,
    pub content_digest: String,
    pub compiler: GenUiCompilerIdentity,
}

/// Semantic runtime evidence emitted by an isolated GenUI preview.
///
/// The preview cannot write this value directly. The trusted Workbench forwards
/// it to the Rust gateway, which revalidates, redacts, sequences, digests, and
/// persists the event before it becomes Time Travel evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenUiRuntimeTraceKind {
    Action,
    Checkpoint,
    Console,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenUiRuntimeTraceInput {
    pub schema_version: u16,
    pub stream_id: Uuid,
    pub client_sequence: u64,
    pub kind: GenUiRuntimeTraceKind,
    pub name: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenUiRuntimeTraceEvent {
    pub schema_version: u16,
    pub event_sequence: u64,
    pub artifact_id: ArtifactId,
    pub source_revision: u64,
    pub stream_id: Uuid,
    pub client_sequence: u64,
    pub kind: GenUiRuntimeTraceKind,
    pub name: String,
    pub payload: serde_json::Value,
    pub payload_digest: String,
    pub redacted: bool,
    pub recorded_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenUiRuntimeTraceAppendRequest {
    pub source_revision: u64,
    pub events: Vec<GenUiRuntimeTraceInput>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenUiRuntimeTraceProjection {
    pub artifact_id: ArtifactId,
    pub source_revision: u64,
    pub events: Vec<GenUiRuntimeTraceEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_trace_contract_uses_explicit_semantic_kinds() {
        let input: GenUiRuntimeTraceInput = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "stream_id": "11111111-1111-4111-8111-111111111111",
            "client_sequence": 1,
            "kind": "checkpoint",
            "name": "counter.changed",
            "payload": {"count": 2}
        }))
        .unwrap();
        assert_eq!(input.kind, GenUiRuntimeTraceKind::Checkpoint);
        assert_eq!(input.payload["count"], 2);
        assert!(
            serde_json::to_string(&input)
                .unwrap()
                .contains("\"kind\":\"checkpoint\"")
        );
    }
}
