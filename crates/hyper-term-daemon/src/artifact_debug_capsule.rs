use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use hyper_term_protocol::{
    GenUiBugCapsule, GenUiBugCapsuleEditorState, GenUiBugCapsuleEnvironment, GenUiBugCapsuleFile,
    GenUiBugCapsuleInclusion, GenUiBugCapsuleInventoryEntry, GenUiBugCapsuleOutputs,
    GenUiRuntimeTraceEvent, GenUiRuntimeTraceKind, GenUiRuntimeTraceProjection,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use thiserror::Error;

use crate::artifact_editor_store::{ArtifactEditorCheckpoint, ArtifactEditorView};
use crate::artifact_runtime_trace_store::{RuntimeTraceStoreError, replay_projection_digest};
use crate::artifact_store::StoredGenUiArtifact;

const LEGACY_BUG_CAPSULE_SCHEMA_VERSION: u16 = 1;
const BUG_CAPSULE_SCHEMA_VERSION: u16 = 2;
const MAX_RUNTIME_EVENT_BYTES: usize = 384 * 1024;
const MAX_BUG_CAPSULE_BYTES: usize = 512 * 1024;
const EXCLUDED_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Error)]
pub enum BugCapsuleError {
    #[error("bug capsule exceeds its bounded export size")]
    TooLarge,
    #[error("bug capsule serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bug capsule runtime projection failed: {0}")]
    Runtime(String),
    #[error("bug capsule I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("bug capsule contract is invalid")]
    Invalid,
}

impl From<RuntimeTraceStoreError> for BugCapsuleError {
    fn from(error: RuntimeTraceStoreError) -> Self {
        Self::Runtime(error.to_string())
    }
}

pub fn load_bug_capsule(path: &Path) -> Result<GenUiBugCapsule, BugCapsuleError> {
    if !path.is_absolute() {
        return Err(BugCapsuleError::Invalid);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_BUG_CAPSULE_BYTES as u64 {
        return Err(BugCapsuleError::Invalid);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.file_type().is_file()
        || opened_metadata.len() > MAX_BUG_CAPSULE_BYTES as u64
    {
        return Err(BugCapsuleError::Invalid);
    }
    let mut encoded = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(MAX_BUG_CAPSULE_BYTES as u64 + 1)
        .read_to_end(&mut encoded)?;
    if encoded.len() > MAX_BUG_CAPSULE_BYTES {
        return Err(BugCapsuleError::TooLarge);
    }
    let mut capsule: GenUiBugCapsule = serde_json::from_slice(&encoded)?;
    match capsule.schema_version {
        LEGACY_BUG_CAPSULE_SCHEMA_VERSION => {
            validate_bug_capsule_version(&capsule, LEGACY_BUG_CAPSULE_SCHEMA_VERSION)?;
            capsule.schema_version = BUG_CAPSULE_SCHEMA_VERSION;
            capsule.accepted_source_digest = accepted_source_digest(&capsule)?;
            capsule.capsule_digest = None;
            capsule.capsule_digest = Some(unsigned_digest(&capsule)?);
            validate_bug_capsule(&capsule)?;
        }
        BUG_CAPSULE_SCHEMA_VERSION => validate_bug_capsule(&capsule)?,
        _ => return Err(BugCapsuleError::Invalid),
    }
    Ok(capsule)
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
        accepted_source_digest: String::new(),
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
    capsule.accepted_source_digest = accepted_source_digest(&capsule)?;
    capsule.capsule_digest = Some(unsigned_digest(&capsule)?);
    if serde_json::to_vec(&capsule)?.len() > MAX_BUG_CAPSULE_BYTES {
        return Err(BugCapsuleError::TooLarge);
    }
    Ok(capsule)
}

pub(crate) fn verify_bug_capsule(capsule: &GenUiBugCapsule) -> Result<bool, BugCapsuleError> {
    Ok(capsule.capsule_digest.as_deref() == Some(unsigned_digest(capsule)?.as_str()))
}

fn validate_bug_capsule(capsule: &GenUiBugCapsule) -> Result<(), BugCapsuleError> {
    validate_bug_capsule_version(capsule, BUG_CAPSULE_SCHEMA_VERSION)
}

fn validate_bug_capsule_version(
    capsule: &GenUiBugCapsule,
    expected_schema_version: u16,
) -> Result<(), BugCapsuleError> {
    let valid_source_identity = match expected_schema_version {
        LEGACY_BUG_CAPSULE_SCHEMA_VERSION => capsule.accepted_source_digest.is_empty(),
        BUG_CAPSULE_SCHEMA_VERSION => {
            is_sha256(&capsule.accepted_source_digest)
                && accepted_source_digest(capsule)? == capsule.accepted_source_digest
        }
        _ => false,
    };
    if capsule.schema_version != expected_schema_version
        || capsule.mode != "replay_only"
        || capsule.artifact.source_revision == 0
        || capsule.runtime.artifact_id != capsule.artifact.artifact_id
        || capsule.runtime.source_revision != capsule.artifact.source_revision
        || capsule.editor.base_source_revision != capsule.artifact.source_revision
        || !is_sha256(&capsule.artifact.content_digest)
        || !is_sha256(&capsule.runtime.projection_digest)
        || !is_sha256(&capsule.editor.state_digest)
        || !is_sha256(&capsule.outputs.bundle_digest)
        || !is_sha256(&capsule.outputs.css_digest)
        || !is_sha256(&capsule.outputs.source_map_digest)
        || !capsule.capsule_digest.as_deref().is_some_and(is_sha256)
        || !valid_source_identity
        || capsule.accepted_source.is_empty()
        || capsule.accepted_source.len() > 100
        || capsule.editor.files.is_empty()
        || capsule.editor.files.len() > 100
        || capsule.runtime.events.len() > 256
        || capsule.inventory.is_empty()
        || capsule.inventory.len() > 32
        || capsule.reproduction.is_empty()
        || capsule.reproduction.len() > 16
        || capsule.reproduction.iter().any(|step| step.len() > 4096)
    {
        return Err(BugCapsuleError::Invalid);
    }
    let mut accepted_paths = HashSet::new();
    if capsule.accepted_source.iter().any(|file| {
        !valid_capsule_file(file.path.as_str(), &file.content_digest)
            || file.modified
            || !accepted_paths.insert(file.path.as_str())
    }) {
        return Err(BugCapsuleError::Invalid);
    }
    let mut editor_paths = HashSet::new();
    if capsule.editor.files.iter().any(|file| {
        !valid_capsule_file(file.path.as_str(), &file.content_digest)
            || !editor_paths.insert(file.path.as_str())
    }) || accepted_paths != editor_paths
        || !editor_paths.contains(capsule.artifact.entrypoint.as_str())
        || !editor_paths.contains(capsule.editor.active_path.as_str())
    {
        return Err(BugCapsuleError::Invalid);
    }
    if capsule
        .runtime
        .events
        .iter()
        .enumerate()
        .any(|(index, event)| {
            event.schema_version != 1
                || event.event_sequence == 0
                || event.artifact_id != capsule.artifact.artifact_id
                || event.source_revision != capsule.artifact.source_revision
                || event.client_sequence == 0
                || !is_sha256(&event.payload_digest)
                || (index > 0
                    && event.event_sequence
                        != capsule.runtime.events[index - 1]
                            .event_sequence
                            .saturating_add(1))
        })
    {
        return Err(BugCapsuleError::Invalid);
    }
    if replay_projection_digest(capsule.runtime.source_revision, &capsule.runtime.events)?
        != capsule.runtime.projection_digest
    {
        return Err(BugCapsuleError::Invalid);
    }
    let required_exclusions = [
        "terminal_output",
        "provider_prompts",
        "mcp_payloads",
        "computer_use_and_screenshots",
        "environment_variables",
    ];
    let mut inventory = HashSet::new();
    if capsule.inventory.iter().any(|entry| {
        entry.category.is_empty()
            || entry.category.len() > 128
            || entry.reason.is_empty()
            || entry.reason.len() > 4096
            || !inventory.insert(entry.category.as_str())
    }) || required_exclusions.iter().any(|category| {
        !capsule.inventory.iter().any(|entry| {
            entry.category == *category && entry.inclusion == GenUiBugCapsuleInclusion::Excluded
        })
    }) || !verify_bug_capsule(capsule)?
    {
        return Err(BugCapsuleError::Invalid);
    }
    Ok(())
}

fn valid_capsule_file(path: &str, digest: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains("..")
        && !path.contains('\\')
        && is_sha256(digest)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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

#[derive(Serialize)]
struct AcceptedSourceIdentity<'a> {
    schema_version: u16,
    source_revision: u64,
    entrypoint: &'a str,
    files: &'a [GenUiBugCapsuleFile],
}

fn accepted_source_digest(capsule: &GenUiBugCapsule) -> Result<String, serde_json::Error> {
    let identity = AcceptedSourceIdentity {
        schema_version: 1,
        source_revision: capsule.artifact.source_revision,
        entrypoint: &capsule.artifact.entrypoint,
        files: &capsule.accepted_source,
    };
    Ok(sha256_bytes(&serde_json::to_vec(&identity)?))
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
        assert!(capsule.editor.files[0].modified);
        assert_eq!(capsule.schema_version, BUG_CAPSULE_SCHEMA_VERSION);
        assert_eq!(capsule.accepted_source_digest.len(), 64);
        assert_eq!(
            capsule.accepted_source_digest,
            accepted_source_digest(&capsule).unwrap()
        );
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

    #[test]
    fn pretty_saved_capsule_reopens_but_tampering_fails_closed() {
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
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("fixture.bug-capsule.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&capsule).unwrap()).unwrap();
        let loaded = load_bug_capsule(&path).unwrap();
        assert_eq!(loaded.capsule_digest, capsule.capsule_digest);

        let mut forged_projection = capsule.clone();
        forged_projection.runtime.projection_digest = "e".repeat(64);
        forged_projection.capsule_digest = Some(unsigned_digest(&forged_projection).unwrap());
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&forged_projection).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));

        let mut tampered = capsule;
        tampered.environment.os = "tampered".into();
        std::fs::write(&path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));
    }

    #[test]
    fn schema_v1_capsule_migrates_in_memory_after_legacy_digest_verification() {
        let original = include_bytes!("../testdata/bug_capsule_v1.json");
        let legacy: GenUiBugCapsule = serde_json::from_slice(original).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("legacy.bug-capsule.json");
        std::fs::write(&path, original).unwrap();

        let migrated = load_bug_capsule(&path).unwrap();
        assert_eq!(migrated.schema_version, BUG_CAPSULE_SCHEMA_VERSION);
        assert_eq!(
            migrated.accepted_source_digest,
            accepted_source_digest(&migrated).unwrap()
        );
        assert!(verify_bug_capsule(&migrated).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let mut tampered_legacy = legacy;
        tampered_legacy.environment.os = "tampered".into();
        std::fs::write(&path, serde_json::to_vec_pretty(&tampered_legacy).unwrap()).unwrap();
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));
    }

    #[test]
    fn schema_v2_rejects_source_substitution_and_future_versions() {
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
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("fixture.bug-capsule.json");

        let mut substituted = capsule.clone();
        substituted.accepted_source[0].content_digest = "e".repeat(64);
        substituted.capsule_digest = None;
        substituted.capsule_digest = Some(unsigned_digest(&substituted).unwrap());
        std::fs::write(&path, serde_json::to_vec_pretty(&substituted).unwrap()).unwrap();
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));

        let mut future = capsule;
        future.schema_version = BUG_CAPSULE_SCHEMA_VERSION + 1;
        future.capsule_digest = None;
        future.capsule_digest = Some(unsigned_digest(&future).unwrap());
        std::fs::write(&path, serde_json::to_vec_pretty(&future).unwrap()).unwrap();
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn capsule_loader_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target.json");
        let link = temporary.path().join("link.json");
        std::fs::write(&target, "{}").unwrap();
        symlink(&target, &link).unwrap();
        assert!(matches!(
            load_bug_capsule(&link),
            Err(BugCapsuleError::Invalid)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn capsule_loader_rejects_non_regular_files_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("capsule.pipe");
        let encoded_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `encoded_path` is a live, NUL-terminated path and the mode is
        // bounded to the temporary directory used by this test.
        assert_eq!(unsafe { libc::mkfifo(encoded_path.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            load_bug_capsule(&path),
            Err(BugCapsuleError::Invalid)
        ));
    }

    #[test]
    fn imported_projection_digest_uses_the_stable_replay_identity() {
        let event = GenUiRuntimeTraceEvent {
            schema_version: 1,
            event_sequence: 1,
            artifact_id: ArtifactId::from_uuid(
                Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap(),
            ),
            source_revision: 7,
            stream_id: Uuid::parse_str("77777777-7777-4777-8777-777777777777").unwrap(),
            client_sequence: 1,
            kind: GenUiRuntimeTraceKind::Checkpoint,
            name: "evidence.panel".into(),
            payload: serde_json::json!({"expanded": true}),
            payload_digest: "3".repeat(64),
            redacted: false,
            recorded_at_ms: 1_753_000_000_001,
        };

        assert_eq!(
            replay_projection_digest(7, &[event]).unwrap(),
            "12fb95ac9b2b5adc0fb43f64f8f10f8861aff850083d1ff3bebee7df568cd38c"
        );
    }
}
