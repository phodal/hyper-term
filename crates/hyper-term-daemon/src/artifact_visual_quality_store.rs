use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hyper_term_protocol::{
    AcceptedGenUiArtifact, ArtifactId, GENUI_VISUAL_QUALITY_CHECKER_VERSION,
    GENUI_VISUAL_QUALITY_SCHEMA_VERSION, GenUiAdvisoryVisualStatus, GenUiObjectiveVisualStatus,
    GenUiVisualCaptureEvidence, GenUiVisualCaptureObservation, GenUiVisualFindingCategory,
    GenUiVisualFindingSeverity, GenUiVisualIssueSample, GenUiVisualQualityFinding,
    GenUiVisualQualityReport, GenUiVisualQualitySubmission, GenUiVisualReviewState, TaskId,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const MAX_REPORT_BYTES: u64 = 256 * 1024;
const MAX_CAPTURE_SAMPLES: usize = 24;
const MAX_SEMANTIC_PATH_BYTES: usize = 256;
const MAX_LAYOUT_EXTENT: u32 = 1_000_000;
const MAX_ELEMENT_COUNT: u32 = 100_000;
const BLOCKING_LAYOUT_SHIFT_MILLI: u32 = 100;

#[derive(Clone, Copy, Debug, Serialize)]
struct ExpectedCapture {
    capture_id: &'static str,
    width: u32,
    height: u32,
}

const CAPTURE_MATRIX: [ExpectedCapture; 3] = [
    ExpectedCapture {
        capture_id: "narrow-light-default",
        width: 390,
        height: 844,
    },
    ExpectedCapture {
        capture_id: "tablet-light-default",
        width: 768,
        height: 1_024,
    },
    ExpectedCapture {
        capture_id: "desktop-light-default",
        width: 1_280,
        height: 800,
    },
];

pub(crate) struct ArtifactVisualQualityStore {
    root: PathBuf,
}

impl ArtifactVisualQualityStore {
    pub(crate) fn open(state_directory: &Path) -> Result<Self, VisualQualityStoreError> {
        let root = state_directory.join("artifact-visual-quality");
        create_private_directory(&root)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    pub(crate) fn load(
        &self,
        task_id: TaskId,
        artifact: &AcceptedGenUiArtifact,
        preview_runtime_digest: &str,
    ) -> Result<Option<GenUiVisualQualityReport>, VisualQualityStoreError> {
        validate_sha256(preview_runtime_digest)?;
        let path = self.report_path(task_id, artifact.artifact_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_REPORT_BYTES
        {
            return Err(VisualQualityStoreError::InvalidReport);
        }
        let mut encoded = Vec::with_capacity(metadata.len() as usize);
        OpenOptions::new()
            .read(true)
            .open(&path)?
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() as u64 > MAX_REPORT_BYTES {
            return Err(VisualQualityStoreError::TooLarge);
        }
        let report: GenUiVisualQualityReport = serde_json::from_slice(&encoded)?;
        validate_report_context(&report, artifact, preview_runtime_digest)?;
        let expected_digest = report_digest(&report)?;
        if report.report_digest != expected_digest {
            return Err(VisualQualityStoreError::DigestMismatch);
        }
        Ok(Some(report))
    }

    pub(crate) fn submit(
        &self,
        task_id: TaskId,
        artifact: &AcceptedGenUiArtifact,
        preview_runtime_digest: &str,
        submission: GenUiVisualQualitySubmission,
    ) -> Result<GenUiVisualQualityReport, VisualQualityStoreError> {
        validate_sha256(preview_runtime_digest)?;
        validate_submission(artifact, &submission)?;
        let capture_manifest_digest = digest_json(&serde_json::json!({
            "schema_version": GENUI_VISUAL_QUALITY_SCHEMA_VERSION,
            "checker_version": GENUI_VISUAL_QUALITY_CHECKER_VERSION,
            "artifact_digest": artifact.content_digest,
            "preview_runtime_digest": preview_runtime_digest,
            "captures": CAPTURE_MATRIX,
        }))?;
        let captures = submission
            .captures
            .into_iter()
            .map(|observation| {
                let observation_digest = digest_json(&observation)?;
                Ok(GenUiVisualCaptureEvidence {
                    observation,
                    observation_digest,
                    pixel_digest: None,
                })
            })
            .collect::<Result<Vec<_>, VisualQualityStoreError>>()?;
        let findings = derive_findings(&captures);
        let objective_status = if findings
            .iter()
            .any(|finding| finding.severity == GenUiVisualFindingSeverity::Blocking)
        {
            GenUiObjectiveVisualStatus::Failed
        } else {
            GenUiObjectiveVisualStatus::Passed
        };
        // Version 1 has deterministic layout observations but no trusted host
        // pixel capture or scenario emulation. It must never self-promote to
        // ReviewReady, even when the generated artifact reports clean metrics.
        let advisory_status = GenUiAdvisoryVisualStatus::NotRun;
        let review_state = if objective_status == GenUiObjectiveVisualStatus::Failed {
            GenUiVisualReviewState::NeedsRevision
        } else {
            GenUiVisualReviewState::NeedsReview
        };
        let mut report = GenUiVisualQualityReport {
            schema_version: GENUI_VISUAL_QUALITY_SCHEMA_VERSION,
            artifact_id: artifact.artifact_id,
            source_revision: artifact.source_revision,
            artifact_digest: artifact.content_digest.clone(),
            preview_runtime_digest: preview_runtime_digest.to_owned(),
            capture_manifest_digest,
            checker_version: GENUI_VISUAL_QUALITY_CHECKER_VERSION.into(),
            captures,
            findings,
            objective_status,
            advisory_status,
            review_state,
            report_digest: String::new(),
        };
        report.report_digest = report_digest(&report)?;
        self.persist(task_id, &report)?;
        Ok(report)
    }

    fn persist(
        &self,
        task_id: TaskId,
        report: &GenUiVisualQualityReport,
    ) -> Result<(), VisualQualityStoreError> {
        let path = self.report_path(task_id, report.artifact_id)?;
        let encoded = serde_json::to_vec_pretty(report)?;
        if encoded.len() as u64 > MAX_REPORT_BYTES {
            return Err(VisualQualityStoreError::TooLarge);
        }
        let parent = path.parent().ok_or(VisualQualityStoreError::InvalidPath)?;
        create_private_directory(parent)?;
        let temporary = parent.join(format!(".visual-quality-{}.tmp", Uuid::new_v4()));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Ok::<_, std::io::Error>(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        Ok(())
    }

    fn report_path(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
    ) -> Result<PathBuf, VisualQualityStoreError> {
        let task_root = self.root.join(task_id.to_string());
        if task_root.exists() {
            validate_private_directory(&task_root)?;
        }
        Ok(task_root.join(format!("{artifact_id}.json")))
    }
}

fn validate_submission(
    artifact: &AcceptedGenUiArtifact,
    submission: &GenUiVisualQualitySubmission,
) -> Result<(), VisualQualityStoreError> {
    if submission.schema_version != GENUI_VISUAL_QUALITY_SCHEMA_VERSION
        || submission.source_revision != artifact.source_revision
        || submission.artifact_digest != artifact.content_digest
        || submission.captures.len() != CAPTURE_MATRIX.len()
    {
        return Err(VisualQualityStoreError::ContextMismatch);
    }
    validate_sha256(&submission.artifact_digest)?;
    for (capture, expected) in submission.captures.iter().zip(CAPTURE_MATRIX) {
        validate_capture(capture, expected)?;
    }
    Ok(())
}

fn validate_capture(
    capture: &GenUiVisualCaptureObservation,
    expected: ExpectedCapture,
) -> Result<(), VisualQualityStoreError> {
    let counts = [
        capture.element_count,
        capture.interactive_count,
        capture.clipped_count,
        capture.undersized_target_count,
        capture.low_contrast_count,
        capture.hidden_primary_action_count,
        capture.console_error_count,
        capture.resource_failure_count,
    ];
    if capture.capture_id != expected.capture_id
        || capture.viewport.width != expected.width
        || capture.viewport.height != expected.height
        || capture.color_scheme != "light"
        || capture.locale != "en"
        || capture.scenario != "default"
        || capture.reduced_motion
        || capture.document_width == 0
        || capture.document_height == 0
        || capture.document_width > MAX_LAYOUT_EXTENT
        || capture.document_height > MAX_LAYOUT_EXTENT
        || counts.iter().any(|count| *count > MAX_ELEMENT_COUNT)
        || capture.interactive_count > capture.element_count
        || capture.clipped_count > capture.element_count
        || capture.low_contrast_count > capture.element_count
        || capture.undersized_target_count > capture.interactive_count
        || capture.hidden_primary_action_count > capture.interactive_count
        || capture.layout_shift_milli > 10_000
        || capture.samples.len() > MAX_CAPTURE_SAMPLES
    {
        return Err(VisualQualityStoreError::InvalidObservation);
    }
    validate_sha256(&capture.semantic_digest)?;
    for sample in &capture.samples {
        if sample.category == GenUiVisualFindingCategory::CoverageGap
            || sample.semantic_path.is_empty()
            || sample.semantic_path.len() > MAX_SEMANTIC_PATH_BYTES
            || sample.semantic_path.chars().any(char::is_control)
            || sample.rect.as_ref().is_some_and(|rect| {
                rect.width > MAX_LAYOUT_EXTENT || rect.height > MAX_LAYOUT_EXTENT
            })
        {
            return Err(VisualQualityStoreError::InvalidObservation);
        }
    }
    Ok(())
}

fn derive_findings(captures: &[GenUiVisualCaptureEvidence]) -> Vec<GenUiVisualQualityFinding> {
    let mut findings = Vec::new();
    for capture in captures {
        let observation = &capture.observation;
        if observation.element_count == 0 {
            push_finding(
                &mut findings,
                GenUiVisualFindingCategory::EmptyRender,
                &observation.capture_id,
                "The accepted artifact rendered no visible elements.".into(),
                None,
            );
        }
        if observation.document_width > observation.viewport.width.saturating_add(1) {
            push_finding(
                &mut findings,
                GenUiVisualFindingCategory::ViewportOverflow,
                &observation.capture_id,
                format!(
                    "Document width {} px exceeds the {} px viewport.",
                    observation.document_width, observation.viewport.width
                ),
                sample_for(observation, GenUiVisualFindingCategory::ViewportOverflow),
            );
        }
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::ClippedContent,
            observation.clipped_count,
            "clipped visible element(s)",
        );
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::UndersizedTarget,
            observation.undersized_target_count,
            "interaction target(s) below the 24 px hard minimum",
        );
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::LowContrast,
            observation.low_contrast_count,
            "text element(s) below the deterministic contrast threshold",
        );
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::HiddenPrimaryAction,
            observation.hidden_primary_action_count,
            "declared primary action(s) hidden or outside the viewport",
        );
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::ConsoleError,
            observation.console_error_count,
            "console error(s) during capture",
        );
        add_count_finding(
            &mut findings,
            observation,
            GenUiVisualFindingCategory::ResourceFailure,
            observation.resource_failure_count,
            "resource failure(s) during capture",
        );
        if observation.layout_shift_milli >= BLOCKING_LAYOUT_SHIFT_MILLI {
            push_finding(
                &mut findings,
                GenUiVisualFindingCategory::LayoutInstability,
                &observation.capture_id,
                format!(
                    "Layout shift score {} exceeded the 100 milli threshold.",
                    observation.layout_shift_milli
                ),
                sample_for(observation, GenUiVisualFindingCategory::LayoutInstability),
            );
        }
    }
    for (id, explanation) in [
        (
            "host-pixel-capture",
            "Host pixel captures are not available from the current sandboxed WebView path.",
        ),
        (
            "dark-theme",
            "Dark color scheme has not been captured by checker version 1.",
        ),
        (
            "reduced-motion",
            "Reduced-motion behavior has not been captured by checker version 1.",
        ),
        (
            "cjk-long-content",
            "CJK, long-label, and long-content fixtures have not been captured.",
        ),
        (
            "state-focus-matrix",
            "Empty, loading, error, disabled, and keyboard-focus states need declared scenarios.",
        ),
    ] {
        findings.push(GenUiVisualQualityFinding {
            finding_id: format!("coverage:{id}"),
            category: GenUiVisualFindingCategory::CoverageGap,
            severity: GenUiVisualFindingSeverity::Warning,
            capture_id: None,
            explanation: explanation.into(),
            sample: None,
        });
    }
    findings
}

fn add_count_finding(
    findings: &mut Vec<GenUiVisualQualityFinding>,
    observation: &GenUiVisualCaptureObservation,
    category: GenUiVisualFindingCategory,
    count: u32,
    label: &str,
) {
    if count == 0 {
        return;
    }
    push_finding(
        findings,
        category,
        &observation.capture_id,
        format!("Detected {count} {label}."),
        sample_for(observation, category),
    );
}

fn push_finding(
    findings: &mut Vec<GenUiVisualQualityFinding>,
    category: GenUiVisualFindingCategory,
    capture_id: &str,
    explanation: String,
    sample: Option<GenUiVisualIssueSample>,
) {
    findings.push(GenUiVisualQualityFinding {
        finding_id: format!("{}:{category:?}", capture_id).to_ascii_lowercase(),
        category,
        severity: GenUiVisualFindingSeverity::Blocking,
        capture_id: Some(capture_id.into()),
        explanation,
        sample,
    });
}

fn sample_for(
    observation: &GenUiVisualCaptureObservation,
    category: GenUiVisualFindingCategory,
) -> Option<GenUiVisualIssueSample> {
    observation
        .samples
        .iter()
        .find(|sample| sample.category == category)
        .cloned()
}

fn validate_report_context(
    report: &GenUiVisualQualityReport,
    artifact: &AcceptedGenUiArtifact,
    preview_runtime_digest: &str,
) -> Result<(), VisualQualityStoreError> {
    if report.schema_version != GENUI_VISUAL_QUALITY_SCHEMA_VERSION
        || report.artifact_id != artifact.artifact_id
        || report.source_revision != artifact.source_revision
        || report.artifact_digest != artifact.content_digest
        || report.preview_runtime_digest != preview_runtime_digest
        || report.checker_version != GENUI_VISUAL_QUALITY_CHECKER_VERSION
        || report.captures.len() != CAPTURE_MATRIX.len()
    {
        return Err(VisualQualityStoreError::ContextMismatch);
    }
    Ok(())
}

fn report_digest(report: &GenUiVisualQualityReport) -> Result<String, VisualQualityStoreError> {
    let mut unsigned = report.clone();
    unsigned.report_digest.clear();
    digest_json(&unsigned)
}

fn digest_json(value: &impl Serialize) -> Result<String, VisualQualityStoreError> {
    let encoded = serde_json::to_vec(value)?;
    Ok(Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_sha256(value: &str) -> Result<(), VisualQualityStoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(VisualQualityStoreError::InvalidDigest);
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), VisualQualityStoreError> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    validate_private_directory(path)
}

fn validate_private_directory(path: &Path) -> Result<(), VisualQualityStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(VisualQualityStoreError::InvalidPath);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum VisualQualityStoreError {
    #[error("visual quality context does not match the accepted artifact")]
    ContextMismatch,
    #[error("visual quality observation is invalid")]
    InvalidObservation,
    #[error("visual quality digest is invalid")]
    InvalidDigest,
    #[error("visual quality report is invalid")]
    InvalidReport,
    #[error("visual quality report digest does not match")]
    DigestMismatch,
    #[error("visual quality path is invalid")]
    InvalidPath,
    #[error("visual quality report is too large")]
    TooLarge,
    #[error("visual quality I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("visual quality JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_term_protocol::{GenUiCompilerIdentity, GenUiVisualRect, GenUiVisualViewport};
    use tempfile::tempdir;

    fn artifact(revision: u64) -> AcceptedGenUiArtifact {
        AcceptedGenUiArtifact {
            artifact_id: ArtifactId::new(),
            source_revision: revision,
            entrypoint: "/App.tsx".into(),
            content_digest: "a".repeat(64),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "1".into(),
            },
        }
    }

    fn capture(expected: ExpectedCapture) -> GenUiVisualCaptureObservation {
        GenUiVisualCaptureObservation {
            capture_id: expected.capture_id.into(),
            viewport: GenUiVisualViewport {
                width: expected.width,
                height: expected.height,
            },
            color_scheme: "light".into(),
            locale: "en".into(),
            scenario: "default".into(),
            reduced_motion: false,
            document_width: expected.width,
            document_height: expected.height,
            element_count: 12,
            interactive_count: 2,
            clipped_count: 0,
            undersized_target_count: 0,
            low_contrast_count: 0,
            hidden_primary_action_count: 0,
            console_error_count: 0,
            resource_failure_count: 0,
            layout_shift_milli: 0,
            semantic_digest: "b".repeat(64),
            samples: Vec::new(),
        }
    }

    fn submission(artifact: &AcceptedGenUiArtifact) -> GenUiVisualQualitySubmission {
        GenUiVisualQualitySubmission {
            schema_version: GENUI_VISUAL_QUALITY_SCHEMA_VERSION,
            source_revision: artifact.source_revision,
            artifact_digest: artifact.content_digest.clone(),
            captures: CAPTURE_MATRIX.into_iter().map(capture).collect(),
        }
    }

    #[test]
    fn clean_layout_remains_needs_review_until_host_captures_exist() {
        let temporary = tempdir().unwrap();
        let store = ArtifactVisualQualityStore::open(temporary.path()).unwrap();
        let artifact = artifact(4);
        let task_id = TaskId::new();
        let report = store
            .submit(task_id, &artifact, &"c".repeat(64), submission(&artifact))
            .unwrap();
        assert_eq!(report.objective_status, GenUiObjectiveVisualStatus::Passed);
        assert_eq!(report.review_state, GenUiVisualReviewState::NeedsReview);
        assert!(
            report
                .captures
                .iter()
                .all(|capture| capture.pixel_digest.is_none())
        );
        assert!(report.findings.iter().any(|finding| {
            finding.category == GenUiVisualFindingCategory::CoverageGap
                && finding.finding_id == "coverage:host-pixel-capture"
        }));
        assert_eq!(
            store
                .load(task_id, &artifact, &"c".repeat(64))
                .unwrap()
                .unwrap(),
            report
        );
    }

    #[test]
    fn objective_failure_is_derived_and_revision_bound() {
        let temporary = tempdir().unwrap();
        let store = ArtifactVisualQualityStore::open(temporary.path()).unwrap();
        let artifact = artifact(7);
        let mut input = submission(&artifact);
        input.captures[0].document_width = 480;
        input.captures[0].clipped_count = 1;
        input.captures[0].samples.push(GenUiVisualIssueSample {
            category: GenUiVisualFindingCategory::ClippedContent,
            semantic_path: "main/button[0]".into(),
            rect: Some(GenUiVisualRect {
                x: 360,
                y: 20,
                width: 120,
                height: 32,
            }),
        });
        let report = store
            .submit(TaskId::new(), &artifact, &"d".repeat(64), input)
            .unwrap();
        assert_eq!(report.objective_status, GenUiObjectiveVisualStatus::Failed);
        assert_eq!(report.review_state, GenUiVisualReviewState::NeedsRevision);
        assert!(
            report.findings.iter().any(|finding| {
                finding.category == GenUiVisualFindingCategory::ViewportOverflow
            })
        );

        let mut stale = submission(&artifact);
        stale.source_revision = artifact.source_revision + 1;
        assert!(matches!(
            store.submit(TaskId::new(), &artifact, &"d".repeat(64), stale),
            Err(VisualQualityStoreError::ContextMismatch)
        ));
    }

    #[test]
    fn empty_render_is_valid_evidence_and_blocks_review() {
        let temporary = tempdir().unwrap();
        let store = ArtifactVisualQualityStore::open(temporary.path()).unwrap();
        let artifact = artifact(9);
        let mut input = submission(&artifact);
        for capture in &mut input.captures {
            capture.element_count = 0;
            capture.interactive_count = 0;
        }

        let report = store
            .submit(TaskId::new(), &artifact, &"e".repeat(64), input)
            .unwrap();
        assert_eq!(report.objective_status, GenUiObjectiveVisualStatus::Failed);
        assert_eq!(report.review_state, GenUiVisualReviewState::NeedsRevision);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.category == GenUiVisualFindingCategory::EmptyRender)
        );
    }
}
