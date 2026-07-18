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
