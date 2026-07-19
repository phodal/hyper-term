use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use hyper_term_protocol::{
    AcceptedGenUiArtifact, ArtifactId, GenUiArtifactCandidate, GenUiCompileDiagnostic,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

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
        let encoded = serde_json::to_vec(&candidate)?;
        if encoded.len() as u64 > MAX_STORED_ARTIFACT_BYTES {
            return Err(ArtifactStoreError::StoredArtifactTooLarge(encoded.len()));
        }
        let destination = self.path(metadata.artifact_id);
        let temporary = self.root.join(format!(".{}.tmp", metadata.artifact_id));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&encoded)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temporary, &destination)?;
            File::open(&self.root)?.sync_all()?;
            Ok::<(), ArtifactStoreError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(metadata)
    }

    pub(crate) fn read(
        &self,
        accepted: &AcceptedGenUiArtifact,
    ) -> Result<StoredGenUiArtifact, ArtifactStoreError> {
        let path = self.path(accepted.artifact_id);
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_STORED_ARTIFACT_BYTES {
            return Err(ArtifactStoreError::InvalidStoredArtifact);
        }
        let mut encoded = Vec::with_capacity(metadata.len() as usize);
        File::open(path)?
            .take(MAX_STORED_ARTIFACT_BYTES + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() as u64 > MAX_STORED_ARTIFACT_BYTES {
            return Err(ArtifactStoreError::InvalidStoredArtifact);
        }
        let candidate: GenUiArtifactCandidate = serde_json::from_slice(&encoded)?;
        validate_candidate(&candidate, false)?;
        if candidate.source_revision != accepted.source_revision
            || candidate.entrypoint != accepted.entrypoint
            || candidate.content_digest != accepted.content_digest
            || candidate.compiler != accepted.compiler
        {
            return Err(ArtifactStoreError::MetadataMismatch);
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
        let mut legacy = candidate("legacy");
        legacy.source_files.clear();
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
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let stored = store.read(&accepted).unwrap();
        assert!(stored.source_files.is_empty());
        assert_eq!(stored.bundle, "legacy");
    }
}
