use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ArtifactId;

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
