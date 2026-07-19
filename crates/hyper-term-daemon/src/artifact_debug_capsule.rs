use hyper_term_protocol::{
    GenUiBugCapsule, GenUiBugCapsuleEditorState, GenUiBugCapsuleEnvironment, GenUiBugCapsuleFile,
    GenUiBugCapsuleInclusion, GenUiBugCapsuleInventoryEntry, GenUiBugCapsuleOutputs,
    GenUiRuntimeTraceEvent, GenUiRuntimeTraceKind, GenUiRuntimeTraceProjection,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::artifact_editor_store::{ArtifactEditorCheckpoint, ArtifactEditorView};
use crate::artifact_runtime_trace_store::{RuntimeTraceStoreError, replay_projection_digest};
use crate::artifact_store::StoredGenUiArtifact;

const BUG_CAPSULE_SCHEMA_VERSION: u16 = 1;
const MAX_RUNTIME_EVENT_BYTES: usize = 384 * 1024;
const MAX_BUG_CAPSULE_BYTES: usize = 512 * 1024;
const EXCLUDED_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Error)]
pub(crate) enum BugCapsuleError {
    #[error("bug capsule exceeds its bounded export size")]
    TooLarge,
    #[error("bug capsule serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bug capsule runtime projection failed: {0}")]
    Runtime(#[from] RuntimeTraceStoreError),
}

pub(crate) fn build_bug_capsule(
    artifact: &StoredGenUiArtifact,
    editor: &ArtifactEditorCheckpoint,
    runtime: &GenUiRuntimeTraceProjection,
    environment: GenUiBugCapsuleEnvironment,
) -> Result<GenUiBugCapsule, BugCapsuleError> {
    let accepted_source = artifact
        .source_files
        .iter()
        .map(|(path, source)| GenUiBugCapsuleFile {
            path: path.clone(),
            byte_count: source.len() as u64,
            content_digest: sha256_bytes(source.as_bytes()),
            modified: false,
        })
        .collect::<Vec<_>>();
    let editor_files = editor
        .files
        .iter()
        .map(|(path, source)| GenUiBugCapsuleFile {
            path: path.clone(),
            byte_count: source.len() as u64,
            content_digest: sha256_bytes(source.as_bytes()),
            modified: artifact.source_files.get(path) != Some(source),
        })
        .collect::<Vec<_>>();
    let outputs = GenUiBugCapsuleOutputs {
        bundle_bytes: artifact.bundle.len() as u64,
        bundle_digest: sha256_bytes(artifact.bundle.as_bytes()),
        css_bytes: artifact.css.len() as u64,
        css_digest: sha256_bytes(artifact.css.as_bytes()),
        source_map_bytes: artifact.source_map.len() as u64,
        source_map_digest: sha256_bytes(artifact.source_map.as_bytes()),
    };
    let (events, runtime_bytes) = bounded_runtime_events(&runtime.events)?;
    let omitted_runtime_events = runtime.events.len().saturating_sub(events.len()) as u64;
    let runtime = GenUiRuntimeTraceProjection {
        artifact_id: runtime.artifact_id,
        source_revision: runtime.source_revision,
        projection_digest: replay_projection_digest(runtime.source_revision, &events)?,
        events,
    };
    let accepted_source_bytes = accepted_source.iter().map(|file| file.byte_count).sum();
    let editor_source_bytes = editor_files.iter().map(|file| file.byte_count).sum();
    let output_bytes = outputs.bundle_bytes + outputs.css_bytes + outputs.source_map_bytes;
    let inventory = vec![
        inventory(
            "artifact_metadata",
            GenUiBugCapsuleInclusion::Included,
            1,
            0,
            "Accepted Rust authority metadata required to identify the replay target",
        ),
        inventory(
            "accepted_source_content",
            GenUiBugCapsuleInclusion::DigestOnly,
            accepted_source.len() as u64,
            accepted_source_bytes,
            "Virtual paths, byte counts, and SHA-256 only; source text is excluded by default",
        ),
        inventory(
            "editor_source_content",
            GenUiBugCapsuleInclusion::DigestOnly,
            editor_files.len() as u64,
            editor_source_bytes,
            "Checkpoint metadata and SHA-256 only; unpublished source text is excluded",
        ),
        inventory(
            "compiler_outputs",
            GenUiBugCapsuleInclusion::DigestOnly,
            3,
            output_bytes,
            "Bundle, CSS, and source map content are excluded; sizes and SHA-256 are included",
        ),
        inventory(
            "semantic_runtime_events",
            GenUiBugCapsuleInclusion::Included,
            runtime.events.len() as u64,
            runtime_bytes as u64,
            "Bounded Rust-redacted replay inputs; console and error payloads use excluded placeholders",
        ),
        inventory(
            "terminal_output",
            GenUiBugCapsuleInclusion::Excluded,
            0,
            0,
            "Terminal output is untrusted and excluded by default",
        ),
        inventory(
            "provider_prompts",
            GenUiBugCapsuleInclusion::Excluded,
            0,
            0,
            "Provider prompts and responses are excluded by default",
        ),
        inventory(
            "mcp_payloads",
            GenUiBugCapsuleInclusion::Excluded,
            0,
            0,
            "MCP request and response payloads are excluded by default",
        ),
        inventory(
            "computer_use_and_screenshots",
            GenUiBugCapsuleInclusion::Excluded,
            0,
            0,
            "Computer Use frames and screenshots are excluded by default",
        ),
        inventory(
            "environment_variables",
            GenUiBugCapsuleInclusion::Excluded,
            0,
            0,
            "Environment values and credentials are never exported",
        ),
    ];
    let mut capsule = GenUiBugCapsule {
        schema_version: BUG_CAPSULE_SCHEMA_VERSION,
        mode: "replay_only".into(),
        artifact: artifact.metadata.clone(),
        accepted_source,
        outputs,
        editor: GenUiBugCapsuleEditorState {
            base_source_revision: editor.base_source_revision,
            revision: editor.revision,
            state_digest: editor.state_digest.clone(),
            active_path: editor.active_path.clone(),
            view: match editor.view {
                ArtifactEditorView::Code => "code",
                ArtifactEditorView::Diff => "diff",
                ArtifactEditorView::Trace => "trace",
            }
            .into(),
            files: editor_files,
        },
        runtime,
        runtime_truncated: omitted_runtime_events > 0,
        omitted_runtime_events,
        environment,
        inventory,
        reproduction: vec![
            "Open this JSON in a Hyper Term offline Bug Capsule viewer.".into(),
            "Verify capsule_digest before loading replay data.".into(),
            "Replay semantic events only; do not execute Shell, ACP, MCP, or Computer Use effects."
                .into(),
            "Match digest-only source and compiler outputs on the originating machine if deeper inspection is required."
                .into(),
        ],
        capsule_digest: None,
    };
    capsule.capsule_digest = Some(unsigned_digest(&capsule)?);
    if serde_json::to_vec(&capsule)?.len() > MAX_BUG_CAPSULE_BYTES {
        return Err(BugCapsuleError::TooLarge);
    }
    Ok(capsule)
}

#[cfg(test)]
pub(crate) fn verify_bug_capsule(capsule: &GenUiBugCapsule) -> Result<bool, BugCapsuleError> {
    Ok(capsule.capsule_digest.as_deref() == Some(unsigned_digest(capsule)?.as_str()))
}

fn bounded_runtime_events(
    events: &[GenUiRuntimeTraceEvent],
) -> Result<(Vec<GenUiRuntimeTraceEvent>, usize), BugCapsuleError> {
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    for event in events {
        let event = if matches!(
            event.kind,
            GenUiRuntimeTraceKind::Console | GenUiRuntimeTraceKind::Error
        ) {
            GenUiRuntimeTraceEvent {
                name: "observation.excluded".into(),
                payload: Value::Null,
                payload_digest: EXCLUDED_DIGEST.into(),
                redacted: true,
                recorded_at_ms: 0,
                ..event.clone()
            }
        } else {
            event.clone()
        };
        let event_bytes = serde_json::to_vec(&event)?.len();
        if bytes.saturating_add(event_bytes) > MAX_RUNTIME_EVENT_BYTES {
            break;
        }
        bytes += event_bytes;
        selected.push(event);
    }
    Ok((selected, bytes))
}

fn inventory(
    category: &str,
    inclusion: GenUiBugCapsuleInclusion,
    item_count: u64,
    byte_count: u64,
    reason: &str,
) -> GenUiBugCapsuleInventoryEntry {
    GenUiBugCapsuleInventoryEntry {
        category: category.into(),
        inclusion,
        item_count,
        byte_count,
        reason: reason.into(),
    }
}

fn unsigned_digest(capsule: &GenUiBugCapsule) -> Result<String, serde_json::Error> {
    let mut unsigned = capsule.clone();
    unsigned.capsule_digest = None;
    Ok(sha256_bytes(&serde_json::to_vec(&unsigned)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hyper_term_protocol::{
        AcceptedGenUiArtifact, ArtifactId, GenUiCompilerIdentity, GenUiRuntimeTraceEvent,
    };
    use uuid::Uuid;

    use super::*;
    use crate::artifact_editor_store::{ArtifactEditorCheckpoint, ArtifactEditorView};

    fn fixture() -> (
        StoredGenUiArtifact,
        ArtifactEditorCheckpoint,
        GenUiRuntimeTraceProjection,
    ) {
        let artifact_id = ArtifactId::new();
        let artifact = StoredGenUiArtifact {
            metadata: AcceptedGenUiArtifact {
                artifact_id,
                source_revision: 7,
                entrypoint: "/App.tsx".into(),
                content_digest: "a".repeat(64),
                compiler: GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([(
                "/App.tsx".into(),
                "const sourceSecret = 'must-not-export';".into(),
            )]),
            bundle: "bundle body must not export".into(),
            css: ".secret { color: red }".into(),
            source_map: "source map body must not export".into(),
        };
        let editor = ArtifactEditorCheckpoint {
            schema_version: 1,
            artifact_id,
            base_source_revision: 7,
            revision: 2,
            state_digest: "b".repeat(64),
            entrypoint: "/App.tsx".into(),
            files: BTreeMap::from([(
                "/App.tsx".into(),
                "const editorSecret = 'must-not-export';".into(),
            )]),
            active_path: "/App.tsx".into(),
            view: ArtifactEditorView::Trace,
            selections: BTreeMap::new(),
        };
        let runtime = GenUiRuntimeTraceProjection {
            artifact_id,
            source_revision: 7,
            projection_digest: "c".repeat(64),
            events: vec![GenUiRuntimeTraceEvent {
                schema_version: 1,
                event_sequence: 1,
                artifact_id,
                source_revision: 7,
                stream_id: Uuid::new_v4(),
                client_sequence: 1,
                kind: GenUiRuntimeTraceKind::Console,
                name: "console.log".into(),
                payload: serde_json::json!({"message": "console secret"}),
                payload_digest: "d".repeat(64),
                redacted: false,
                recorded_at_ms: 42,
            }],
        };
        (artifact, editor, runtime)
    }

    #[test]
    fn capsule_is_digest_only_for_sources_and_excludes_observations() {
        let (artifact, editor, runtime) = fixture();
        let capsule = build_bug_capsule(
            &artifact,
            &editor,
            &runtime,
            GenUiBugCapsuleEnvironment {
                hyper_term_version: "0.1.0".into(),
                os: "macos".into(),
                architecture: "aarch64".into(),
                deno_runtime_version: None,
                deno_executable_digest: None,
                compiler_script_digest: None,
                compiler_wasm_digest: None,
            },
        )
        .unwrap();
        let encoded = serde_json::to_string(&capsule).unwrap();
        assert!(!encoded.contains("must-not-export"));
        assert!(!encoded.contains("console secret"));
        assert_eq!(capsule.runtime.events[0].name, "observation.excluded");
        assert_eq!(capsule.editor.files[0].modified, true);
        assert!(verify_bug_capsule(&capsule).unwrap());

        let mut tampered = capsule;
        tampered.mode = "execute_live".into();
        assert!(!verify_bug_capsule(&tampered).unwrap());
    }

    #[test]
    fn capsule_truncates_runtime_at_a_contiguous_replay_prefix() {
        let (artifact, editor, mut runtime) = fixture();
        let stream_id = Uuid::new_v4();
        runtime.events = (1..=256)
            .map(|sequence| GenUiRuntimeTraceEvent {
                schema_version: 1,
                event_sequence: sequence,
                artifact_id: artifact.metadata.artifact_id,
                source_revision: 7,
                stream_id,
                client_sequence: sequence,
                kind: GenUiRuntimeTraceKind::Checkpoint,
                name: "large.checkpoint".into(),
                payload: serde_json::json!({"value": "x".repeat(2048)}),
                payload_digest: sha256_bytes(sequence.to_string().as_bytes()),
                redacted: false,
                recorded_at_ms: 42,
            })
            .collect();
        let capsule = build_bug_capsule(
            &artifact,
            &editor,
            &runtime,
            GenUiBugCapsuleEnvironment {
                hyper_term_version: "0.1.0".into(),
                os: "macos".into(),
                architecture: "aarch64".into(),
                deno_runtime_version: None,
                deno_executable_digest: None,
                compiler_script_digest: None,
                compiler_wasm_digest: None,
            },
        )
        .unwrap();

        assert!(capsule.runtime_truncated);
        assert!(capsule.omitted_runtime_events > 0);
        assert_eq!(capsule.runtime.events[0].event_sequence, 1);
        assert_eq!(
            capsule.runtime.events.last().unwrap().event_sequence,
            capsule.runtime.events.len() as u64
        );
        assert!(serde_json::to_vec(&capsule).unwrap().len() <= MAX_BUG_CAPSULE_BYTES);
        assert!(verify_bug_capsule(&capsule).unwrap());
    }
}
