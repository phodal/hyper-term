use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ArtifactId;
use uuid::Uuid;

pub const MAX_GENUI_SOURCE_FILES: usize = 1_000;
pub const MAX_GENUI_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_GENUI_VIRTUAL_PATH_BYTES: usize = 4 * 1024;

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
    EffectReceipt,
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
    /// Digest of the deterministic replay inputs in this projection.
    ///
    /// Wall-clock fields and observational console/error events are excluded,
    /// so the same accepted event range produces the same digest after a
    /// daemon restart. Rendered pixels and DOM layout are deliberately not
    /// canonical state.
    pub projection_digest: String,
    pub events: Vec<GenUiRuntimeTraceEvent>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenUiBugCapsuleInclusion {
    Included,
    DigestOnly,
    Excluded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiBugCapsuleInventoryEntry {
    pub category: String,
    pub inclusion: GenUiBugCapsuleInclusion,
    pub item_count: u64,
    pub byte_count: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiBugCapsuleFile {
    pub path: String,
    pub byte_count: u64,
    pub content_digest: String,
    pub modified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiBugCapsuleOutputs {
    pub bundle_bytes: u64,
    pub bundle_digest: String,
    pub css_bytes: u64,
    pub css_digest: String,
    pub source_map_bytes: u64,
    pub source_map_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiBugCapsuleEditorState {
    pub base_source_revision: u64,
    pub revision: u64,
    pub state_digest: String,
    pub active_path: String,
    pub view: String,
    pub files: Vec<GenUiBugCapsuleFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenUiBugCapsuleEnvironment {
    pub hyper_term_version: String,
    pub os: String,
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deno_runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deno_executable_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_script_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_wasm_digest: Option<String>,
}

/// A bounded, default-redacted, replay-only diagnostic package.
///
/// Rust creates this contract from already-authoritative stores. Web clients
/// may preview and save the exact response, but cannot add workspace data.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenUiBugCapsule {
    pub schema_version: u16,
    pub mode: String,
    pub artifact: AcceptedGenUiArtifact,
    pub accepted_source: Vec<GenUiBugCapsuleFile>,
    /// Digest of the accepted source identity: source revision, entrypoint,
    /// and the ordered digest-only file inventory.
    ///
    /// Schema v1 capsules did not include this field. Rust verifies the v1
    /// capsule before deriving it during an in-memory import migration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub accepted_source_digest: String,
    pub outputs: GenUiBugCapsuleOutputs,
    pub editor: GenUiBugCapsuleEditorState,
    pub runtime: GenUiRuntimeTraceProjection,
    pub runtime_truncated: bool,
    pub omitted_runtime_events: u64,
    pub environment: GenUiBugCapsuleEnvironment,
    pub inventory: Vec<GenUiBugCapsuleInventoryEntry>,
    pub reproduction: Vec<String>,
    /// Digest of the serialized capsule with this field omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule_digest: Option<String>,
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

        let receipt: GenUiRuntimeTraceInput = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "stream_id": "11111111-1111-4111-8111-111111111111",
            "client_sequence": 2,
            "kind": "effect_receipt",
            "name": "weather.lookup",
            "payload": {
                "input": {"city": "Shanghai"},
                "outcome": "succeeded",
                "output": {"temperature": 31}
            }
        }))
        .unwrap();
        assert_eq!(receipt.kind, GenUiRuntimeTraceKind::EffectReceipt);
    }
}
