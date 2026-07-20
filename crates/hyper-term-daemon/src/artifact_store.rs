use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use hyper_term_protocol::{
    AcceptedGenUiArtifact, ArtifactId, GenUiArtifactCandidate, GenUiCompileDiagnostic,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const LEGACY_STORED_ARTIFACT_SCHEMA_VERSION: u16 = 1;
const STORED_ARTIFACT_SCHEMA_VERSION: u16 = 2;
const MAX_BUNDLE_BYTES: usize = 768 * 1024;
const MAX_CSS_BYTES: usize = 256 * 1024;
const MAX_SOURCE_MAP_BYTES: usize = 768 * 1024;
const MAX_SOURCE_FILES: usize = 100;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_STORED_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 256;

pub(crate) struct ArtifactStore {
    root: PathBuf,
}

#[derive(Clone)]
pub(crate) struct StoredGenUiArtifact {
    pub metadata: AcceptedGenUiArtifact,
    pub source_files: BTreeMap<String, String>,
    pub bundle: String,
    pub css: String,
    pub source_map: String,
}

#[derive(Deserialize, Serialize)]
struct StoredArtifactEnvelope {
    schema_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted_source_digest: Option<String>,
    candidate: GenUiArtifactCandidate,
}

impl ArtifactStore {
    pub(crate) fn open(state_directory: &Path) -> Result<Self, ArtifactStoreError> {
        let root = state_directory.join("artifacts");
        create_private_directory(&root)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    pub(crate) fn persist(
        &self,
        candidate: GenUiArtifactCandidate,
    ) -> Result<AcceptedGenUiArtifact, ArtifactStoreError> {
        validate_candidate(&candidate, true)?;
        let metadata = AcceptedGenUiArtifact {
            artifact_id: ArtifactId::new(),
            source_revision: candidate.source_revision,
            entrypoint: candidate.entrypoint.clone(),
            content_digest: candidate.content_digest.clone(),
            compiler: candidate.compiler.clone(),
        };
        let envelope = stored_envelope(candidate)?;
        let encoded = encode_stored_artifact(&envelope)?;
        let destination = self.path(metadata.artifact_id);
        write_new_artifact(&self.root, &destination, &encoded)?;
        Ok(metadata)
    }

    pub(crate) fn read(
        &self,
        accepted: &AcceptedGenUiArtifact,
    ) -> Result<StoredGenUiArtifact, ArtifactStoreError> {
        let path = self.path(accepted.artifact_id);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_STORED_ARTIFACT_BYTES
        {
            return Err(ArtifactStoreError::InvalidStoredArtifact);
        }
        let mut encoded = Vec::with_capacity(metadata.len() as usize);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        options
            .open(&path)?
            .take(MAX_STORED_ARTIFACT_BYTES + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() as u64 > MAX_STORED_ARTIFACT_BYTES {
            return Err(ArtifactStoreError::InvalidStoredArtifact);
        }
        let (candidate, migration_required) = decode_stored_artifact(&encoded)?;
        validate_candidate(&candidate, false)?;
        if candidate.source_revision != accepted.source_revision
            || candidate.entrypoint != accepted.entrypoint
            || candidate.content_digest != accepted.content_digest
            || candidate.compiler != accepted.compiler
        {
            return Err(ArtifactStoreError::MetadataMismatch);
        }
        if migration_required {
            let envelope = stored_envelope(candidate.clone())?;
            let migrated = encode_stored_artifact(&envelope)?;
            atomic_replace_artifact(&self.root, &path, &migrated)?;
        }
        Ok(StoredGenUiArtifact {
            metadata: accepted.clone(),
            source_files: candidate.source_files,
            bundle: candidate.bundle,
            css: candidate.css,
            source_map: candidate.source_map,
        })
    }

    fn path(&self, artifact_id: ArtifactId) -> PathBuf {
        self.root.join(format!("{artifact_id}.json"))
    }
}

fn stored_envelope(
    candidate: GenUiArtifactCandidate,
) -> Result<StoredArtifactEnvelope, ArtifactStoreError> {
    let accepted_source_digest = if candidate.source_files.is_empty() {
        None
    } else {
        Some(accepted_source_digest(&candidate)?)
    };
    Ok(StoredArtifactEnvelope {
        schema_version: STORED_ARTIFACT_SCHEMA_VERSION,
        accepted_source_digest,
        candidate,
    })
}

fn encode_stored_artifact(
    envelope: &StoredArtifactEnvelope,
) -> Result<Vec<u8>, ArtifactStoreError> {
    let encoded = serde_json::to_vec(envelope)?;
    if encoded.len() as u64 > MAX_STORED_ARTIFACT_BYTES {
        return Err(ArtifactStoreError::StoredArtifactTooLarge(encoded.len()));
    }
    Ok(encoded)
}

fn decode_stored_artifact(
    encoded: &[u8],
) -> Result<(GenUiArtifactCandidate, bool), ArtifactStoreError> {
    let value: Value = serde_json::from_slice(encoded)?;
    let schema_version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|version| u16::try_from(version).ok())
        .ok_or(ArtifactStoreError::InvalidStoredArtifact)?;
    match schema_version {
        LEGACY_STORED_ARTIFACT_SCHEMA_VERSION if value.get("candidate").is_none() => {
            let candidate = serde_json::from_value(value)?;
            Ok((candidate, true))
        }
        STORED_ARTIFACT_SCHEMA_VERSION => {
            let envelope: StoredArtifactEnvelope = serde_json::from_value(value)?;
            if envelope.candidate.schema_version != LEGACY_STORED_ARTIFACT_SCHEMA_VERSION {
                return Err(ArtifactStoreError::InvalidMetadata);
            }
            let expected = if envelope.candidate.source_files.is_empty() {
                None
            } else {
                Some(accepted_source_digest(&envelope.candidate)?)
            };
            if envelope.accepted_source_digest != expected {
                return Err(ArtifactStoreError::AcceptedSourceDigestMismatch);
            }
            Ok((envelope.candidate, false))
        }
        version => Err(ArtifactStoreError::UnsupportedStoredSchema(version)),
    }
}

fn accepted_source_digest(
    candidate: &GenUiArtifactCandidate,
) -> Result<String, ArtifactStoreError> {
    let encoded = serde_json::to_vec(&(
        "hyper-term.accepted-source",
        LEGACY_STORED_ARTIFACT_SCHEMA_VERSION,
        candidate.source_revision,
        &candidate.entrypoint,
        &candidate.source_files,
    ))?;
    Ok(hex_digest(Sha256::digest(encoded)))
}

fn write_new_artifact(
    root: &Path,
    destination: &Path,
    encoded: &[u8],
) -> Result<(), ArtifactStoreError> {
    let temporary = root.join(format!(".artifact-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(encoded)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, destination)?;
        File::open(root)?.sync_all()?;
        Ok::<(), ArtifactStoreError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn atomic_replace_artifact(
    root: &Path,
    destination: &Path,
    encoded: &[u8],
) -> Result<(), ArtifactStoreError> {
    write_new_artifact(root, destination, encoded)
}

fn validate_candidate(
    candidate: &GenUiArtifactCandidate,
    require_source: bool,
) -> Result<(), ArtifactStoreError> {
    if candidate.schema_version != 1 || candidate.source_revision == 0 {
        return Err(ArtifactStoreError::InvalidMetadata);
    }
    if !valid_virtual_entrypoint(&candidate.entrypoint) {
        return Err(ArtifactStoreError::InvalidEntrypoint);
    }
    validate_source_files(candidate, require_source)?;
    if candidate.compiler.name != "esbuild-wasm"
        || candidate.compiler.version.is_empty()
        || candidate.compiler.version.len() > 64
    {
        return Err(ArtifactStoreError::InvalidCompiler);
    }
    if candidate.bundle.len() > MAX_BUNDLE_BYTES
        || candidate.css.len() > MAX_CSS_BYTES
        || candidate.source_map.len() > MAX_SOURCE_MAP_BYTES
    {
        return Err(ArtifactStoreError::ArtifactOutputTooLarge);
    }
    validate_diagnostics(&candidate.diagnostics)?;
    let mut digest = Sha256::new();
    digest.update(candidate.bundle.as_bytes());
    digest.update(candidate.css.as_bytes());
    let actual = hex_digest(digest.finalize());
    if candidate.content_digest != actual {
        return Err(ArtifactStoreError::DigestMismatch {
            expected: candidate.content_digest.clone(),
            actual,
        });
    }
    Ok(())
}

fn validate_source_files(
    candidate: &GenUiArtifactCandidate,
    require_source: bool,
) -> Result<(), ArtifactStoreError> {
    if candidate.source_files.is_empty() {
        return if require_source {
            Err(ArtifactStoreError::InvalidSourceSnapshot)
        } else {
            Ok(())
        };
    }
    if candidate.source_files.len() > MAX_SOURCE_FILES
        || !candidate.source_files.contains_key(&candidate.entrypoint)
    {
        return Err(ArtifactStoreError::InvalidSourceSnapshot);
    }
    let mut bytes = 0usize;
    for (path, source) in &candidate.source_files {
        if !valid_virtual_source_path(path) {
            return Err(ArtifactStoreError::InvalidSourceSnapshot);
        }
        bytes = bytes.saturating_add(source.len());
    }
    if bytes > MAX_SOURCE_BYTES {
        return Err(ArtifactStoreError::InvalidSourceSnapshot);
    }
    Ok(())
}

fn valid_virtual_entrypoint(value: &str) -> bool {
    valid_virtual_source_path(value)
        && [".tsx", ".ts", ".jsx", ".js"]
            .iter()
            .any(|extension| value.ends_with(extension))
}

fn valid_virtual_source_path(value: &str) -> bool {
    value.starts_with('/') && !value.contains("..") && !value.contains('\\') && value.len() <= 4096
}

fn validate_diagnostics(diagnostics: &[GenUiCompileDiagnostic]) -> Result<(), ArtifactStoreError> {
    if diagnostics.len() > MAX_DIAGNOSTICS
        || diagnostics.iter().any(|diagnostic| {
            diagnostic.severity != "warning"
                || diagnostic.text.len() > 16 * 1024
                || diagnostic
                    .file
                    .as_ref()
                    .is_some_and(|path| path.len() > 4096)
        })
    {
        return Err(ArtifactStoreError::InvalidDiagnostics);
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Error)]
pub(crate) enum ArtifactStoreError {
    #[error("artifact store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("artifact candidate metadata is invalid")]
    InvalidMetadata,
    #[error("artifact entrypoint is not a bounded virtual module")]
    InvalidEntrypoint,
    #[error("artifact source snapshot is missing or invalid")]
    InvalidSourceSnapshot,
    #[error("artifact compiler identity is invalid")]
    InvalidCompiler,
    #[error("artifact output exceeds the accepted size budget")]
    ArtifactOutputTooLarge,
    #[error("artifact diagnostics are invalid or include errors")]
    InvalidDiagnostics,
    #[error("artifact digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("serialized artifact contains {0} bytes and exceeds its store budget")]
    StoredArtifactTooLarge(usize),
    #[error("stored artifact is not a bounded regular file")]
    InvalidStoredArtifact,
    #[error("stored artifact schema {0} is not supported")]
    UnsupportedStoredSchema(u16),
    #[error("stored artifact accepted-source digest does not match its source tree")]
    AcceptedSourceDigestMismatch,
    #[error("stored artifact metadata no longer matches the accepted journal event")]
    MetadataMismatch,
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{GenUiArtifactCandidate, GenUiCompilerIdentity};

    use super::*;

    fn candidate(bundle: &str) -> GenUiArtifactCandidate {
        let mut digest = Sha256::new();
        digest.update(bundle.as_bytes());
        GenUiArtifactCandidate {
            schema_version: 1,
            source_revision: 7,
            entrypoint: "/App.tsx".into(),
            source_files: BTreeMap::from([(
                "/App.tsx".into(),
                "export default () => null;".into(),
            )]),
            bundle: bundle.into(),
            css: String::new(),
            source_map: "{}".into(),
            content_digest: hex_digest(digest.finalize()),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "0.28.1".into(),
            },
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn accepted_artifact_round_trips_through_private_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temporary.path()).unwrap();
        let accepted = store.persist(candidate("bundle")).unwrap();
        let stored = store.read(&accepted).unwrap();
        assert_eq!(stored.metadata, accepted);
        assert_eq!(
            stored.source_files.get("/App.tsx").map(String::as_str),
            Some("export default () => null;")
        );
        assert_eq!(stored.bundle, "bundle");
        assert_eq!(stored.source_map, "{}");
        let encoded = fs::read(store.path(accepted.artifact_id)).unwrap();
        let envelope: StoredArtifactEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(envelope.schema_version, STORED_ARTIFACT_SCHEMA_VERSION);
        assert_eq!(
            envelope.accepted_source_digest,
            Some(accepted_source_digest(&envelope.candidate).unwrap())
        );
        let mode = fs::metadata(store.path(accepted.artifact_id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn invalid_candidate_never_replaces_an_artifact_file() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temporary.path()).unwrap();
        let accepted = store.persist(candidate("last good")).unwrap();
        let mut invalid = candidate("broken");
        invalid.content_digest = "0".repeat(64);
        assert!(matches!(
            store.persist(invalid),
            Err(ArtifactStoreError::DigestMismatch { .. })
        ));
        assert_eq!(store.read(&accepted).unwrap().bundle, "last good");
    }

    #[test]
    fn new_acceptance_requires_source_but_legacy_artifacts_remain_readable() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temporary.path()).unwrap();
        let legacy: GenUiArtifactCandidate =
            serde_json::from_slice(include_bytes!("../testdata/artifact_candidate_v1.json"))
                .unwrap();
        assert!(matches!(
            store.persist(legacy.clone()),
            Err(ArtifactStoreError::InvalidSourceSnapshot)
        ));

        let accepted = AcceptedGenUiArtifact {
            artifact_id: ArtifactId::new(),
            source_revision: legacy.source_revision,
            entrypoint: legacy.entrypoint.clone(),
            content_digest: legacy.content_digest.clone(),
            compiler: legacy.compiler.clone(),
        };
        fs::write(
            store.path(accepted.artifact_id),
            include_bytes!("../testdata/artifact_candidate_v1.json"),
        )
        .unwrap();
        let stored = store.read(&accepted).unwrap();
        assert!(stored.source_files.is_empty());
        assert_eq!(stored.bundle, "legacy");
        let encoded = fs::read(store.path(accepted.artifact_id)).unwrap();
        let envelope: StoredArtifactEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(envelope.schema_version, STORED_ARTIFACT_SCHEMA_VERSION);
        assert_eq!(envelope.accepted_source_digest, None);
        assert!(envelope.candidate.source_files.is_empty());
    }

    #[test]
    fn v2_source_identity_and_future_schema_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temporary.path()).unwrap();
        let accepted = store.persist(candidate("identity")).unwrap();
        let path = store.path(accepted.artifact_id);
        let encoded = fs::read(&path).unwrap();
        let mut envelope: StoredArtifactEnvelope = serde_json::from_slice(&encoded).unwrap();
        envelope.candidate.source_files.insert(
            "/App.tsx".into(),
            "export default () => 'substituted';".into(),
        );
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(matches!(
            store.read(&accepted),
            Err(ArtifactStoreError::AcceptedSourceDigestMismatch)
        ));

        envelope.schema_version = STORED_ARTIFACT_SCHEMA_VERSION + 1;
        envelope.accepted_source_digest =
            Some(accepted_source_digest(&envelope.candidate).unwrap());
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(matches!(
            store.read(&accepted),
            Err(ArtifactStoreError::UnsupportedStoredSchema(version))
                if version == STORED_ARTIFACT_SCHEMA_VERSION + 1
        ));
    }
}
