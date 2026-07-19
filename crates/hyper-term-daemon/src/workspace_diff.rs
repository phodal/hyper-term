use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use similar::{Algorithm, ChangeTag, DiffOp, DiffTag, TextDiff};
use thiserror::Error;

pub(crate) const MAX_WORKSPACE_HUNKS_PER_FILE: usize = 256;
const DIFF_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceDiffHunk {
    pub id: String,
    pub base_start: usize,
    pub base_lines: usize,
    pub proposed_start: usize,
    pub proposed_lines: usize,
    pub patch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceDiffReview {
    pub base_digest: String,
    pub artifact_digest: String,
    pub review_digest: String,
    pub hunks: Vec<WorkspaceDiffHunk>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum WorkspaceDiffError {
    #[error("workspace hunk selection is invalid or stale")]
    InvalidSelection,
    #[error("workspace hunk selection produced no changes")]
    NoChanges,
}

pub(crate) fn review_workspace_diff(
    target_path: &str,
    before: &str,
    artifact_after: &str,
) -> WorkspaceDiffReview {
    let analysis = analyze(target_path, before, artifact_after);
    WorkspaceDiffReview {
        base_digest: sha256_bytes(before.as_bytes()),
        artifact_digest: sha256_bytes(artifact_after.as_bytes()),
        review_digest: review_digest(target_path, before, artifact_after, &analysis.hunks),
        hunks: analysis.hunks,
    }
}

pub(crate) fn select_workspace_hunks(
    target_path: &str,
    before: &str,
    artifact_after: &str,
    expected_review_digest: &str,
    selected_hunk_ids: &[String],
) -> Result<String, WorkspaceDiffError> {
    if selected_hunk_ids.is_empty() {
        return Err(WorkspaceDiffError::NoChanges);
    }
    let analysis = analyze(target_path, before, artifact_after);
    if review_digest(target_path, before, artifact_after, &analysis.hunks) != expected_review_digest
    {
        return Err(WorkspaceDiffError::InvalidSelection);
    }
    let selected = selected_hunk_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if selected.len() != selected_hunk_ids.len()
        || selected
            .iter()
            .any(|id| !analysis.hunks.iter().any(|hunk| hunk.id == *id))
    {
        return Err(WorkspaceDiffError::InvalidSelection);
    }

    let mut op_hunks = BTreeMap::new();
    for (hunk, ops) in analysis.hunks.iter().zip(&analysis.groups) {
        for op in ops.iter().filter(|op| op.tag() != DiffTag::Equal) {
            op_hunks.insert(op_key(op), hunk.id.as_str());
        }
    }
    let mut result = String::with_capacity(artifact_after.len().max(before.len()));
    for op in analysis.diff.ops() {
        if op.tag() == DiffTag::Equal {
            for change in analysis.diff.iter_changes(op) {
                result.push_str(change.value());
            }
            continue;
        }
        let Some(hunk_id) = op_hunks.get(&op_key(op)) else {
            return Err(WorkspaceDiffError::InvalidSelection);
        };
        let apply = selected.contains(*hunk_id);
        for change in analysis.diff.iter_changes(op) {
            if (apply && change.tag() != ChangeTag::Delete)
                || (!apply && change.tag() != ChangeTag::Insert)
            {
                result.push_str(change.value());
            }
        }
    }
    if result == before {
        return Err(WorkspaceDiffError::NoChanges);
    }
    Ok(result)
}

struct WorkspaceDiffAnalysis<'old, 'new> {
    diff: TextDiff<'old, 'new, str>,
    groups: Vec<Vec<DiffOp>>,
    hunks: Vec<WorkspaceDiffHunk>,
}

fn analyze<'old, 'new>(
    target_path: &str,
    before: &'old str,
    artifact_after: &'new str,
) -> WorkspaceDiffAnalysis<'old, 'new> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .timeout(DIFF_TIMEOUT)
        .diff_lines(before, artifact_after);
    let mut groups = diff.grouped_ops(3);
    if groups.len() > MAX_WORKSPACE_HUNKS_PER_FILE {
        groups = vec![diff.ops().to_vec()];
    }
    let hunks = groups
        .iter()
        .map(|ops| build_hunk(target_path, &diff, ops))
        .collect();
    WorkspaceDiffAnalysis {
        diff,
        groups,
        hunks,
    }
}

fn build_hunk(
    target_path: &str,
    diff: &TextDiff<'_, '_, str>,
    ops: &[DiffOp],
) -> WorkspaceDiffHunk {
    let changed = ops
        .iter()
        .filter(|op| op.tag() != DiffTag::Equal)
        .collect::<Vec<_>>();
    let first = changed
        .first()
        .expect("grouped diff hunks always include at least one change");
    let last = changed
        .last()
        .expect("grouped diff hunks always include at least one change");
    let base_range = first.old_range().start..last.old_range().end;
    let proposed_range = first.new_range().start..last.new_range().end;
    let mut patch = format!(
        "@@ -{},{} +{},{} @@\n",
        display_start(base_range.start, base_range.len()),
        base_range.len(),
        display_start(proposed_range.start, proposed_range.len()),
        proposed_range.len()
    );
    for op in ops {
        for change in diff.iter_changes(op) {
            patch.push(match change.tag() {
                ChangeTag::Equal => ' ',
                ChangeTag::Delete => '-',
                ChangeTag::Insert => '+',
            });
            patch.push_str(change.value());
            if !change.value().ends_with('\n') {
                patch.push('\n');
            }
        }
    }
    let mut id_digest = Sha256::new();
    id_digest.update(b"hyper-term.workspace.hunk.v1\0");
    id_digest.update((target_path.len() as u64).to_be_bytes());
    id_digest.update(target_path.as_bytes());
    id_digest.update((base_range.start as u64).to_be_bytes());
    id_digest.update((base_range.len() as u64).to_be_bytes());
    id_digest.update((proposed_range.start as u64).to_be_bytes());
    id_digest.update((proposed_range.len() as u64).to_be_bytes());
    id_digest.update((patch.len() as u64).to_be_bytes());
    id_digest.update(patch.as_bytes());
    WorkspaceDiffHunk {
        id: sha256_digest(id_digest.finalize()),
        base_start: display_start(base_range.start, base_range.len()),
        base_lines: base_range.len(),
        proposed_start: display_start(proposed_range.start, proposed_range.len()),
        proposed_lines: proposed_range.len(),
        patch,
    }
}

fn review_digest(
    target_path: &str,
    before: &str,
    artifact_after: &str,
    hunks: &[WorkspaceDiffHunk],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"hyper-term.workspace.diff-review.v1\0");
    digest.update((target_path.len() as u64).to_be_bytes());
    digest.update(target_path.as_bytes());
    digest.update(sha256_bytes(before.as_bytes()).as_bytes());
    digest.update(sha256_bytes(artifact_after.as_bytes()).as_bytes());
    for hunk in hunks {
        digest.update(hunk.id.as_bytes());
    }
    sha256_digest(digest.finalize())
}

fn display_start(index: usize, lines: usize) -> usize {
    if lines == 0 { index } else { index + 1 }
}

fn op_key(op: &DiffOp) -> (usize, usize, usize, usize) {
    let old = op.old_range();
    let new = op.new_range();
    (old.start, old.end, new.start, new.end)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    sha256_digest(Sha256::digest(bytes))
}

fn sha256_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_diff_review_produces_stable_separate_hunks() {
        let before = "one\ntwo\nkeep a\nkeep b\nkeep c\nkeep d\nkeep e\nkeep f\nkeep g\neight\n";
        let after = "one\nsecond\nkeep a\nkeep b\nkeep c\nkeep d\nkeep e\nkeep f\nkeep g\nlast\n";
        let first = review_workspace_diff("src/App.tsx", before, after);
        let second = review_workspace_diff("src/App.tsx", before, after);

        assert_eq!(first, second);
        assert_eq!(first.hunks.len(), 2);
        assert!(first.hunks[0].patch.contains("-two\n+second\n"));
        assert!(first.hunks[1].patch.contains("-eight\n+last\n"));
        assert!(first.hunks.iter().all(|hunk| hunk.id.len() == 64));
    }

    #[test]
    fn selecting_one_hunk_reconstructs_only_that_exact_change() {
        let before = "one\ntwo\nkeep a\nkeep b\nkeep c\nkeep d\nkeep e\nkeep f\nkeep g\neight\n";
        let after = "one\nsecond\nkeep a\nkeep b\nkeep c\nkeep d\nkeep e\nkeep f\nkeep g\nlast\n";
        let review = review_workspace_diff("src/App.tsx", before, after);
        let selected = select_workspace_hunks(
            "src/App.tsx",
            before,
            after,
            &review.review_digest,
            &[review.hunks[1].id.clone()],
        )
        .unwrap();

        assert_eq!(
            selected,
            "one\ntwo\nkeep a\nkeep b\nkeep c\nkeep d\nkeep e\nkeep f\nkeep g\nlast\n"
        );
    }

    #[test]
    fn selection_rejects_unknown_duplicate_and_stale_hunk_ids() {
        let before = "one\ntwo\n";
        let after = "one\nsecond\n";
        let review = review_workspace_diff("App.tsx", before, after);
        let hunk = review.hunks[0].id.clone();

        assert_eq!(
            select_workspace_hunks(
                "App.tsx",
                before,
                after,
                &review.review_digest,
                &[hunk.clone(), hunk]
            ),
            Err(WorkspaceDiffError::InvalidSelection)
        );
        assert_eq!(
            select_workspace_hunks(
                "App.tsx",
                before,
                after,
                &review.review_digest,
                &["f".repeat(64)]
            ),
            Err(WorkspaceDiffError::InvalidSelection)
        );
        assert_eq!(
            select_workspace_hunks(
                "App.tsx",
                "changed\n",
                after,
                &review.review_digest,
                &[review.hunks[0].id.clone()]
            ),
            Err(WorkspaceDiffError::InvalidSelection)
        );
    }

    #[test]
    fn all_hunks_reconstruct_the_artifact_byte_for_byte() {
        for (before, after) in [
            ("", "new file without newline"),
            ("old without newline", "new without newline"),
            ("a\r\nb\r\n", "a\r\nB\r\n"),
            ("delete me\n", ""),
        ] {
            let review = review_workspace_diff("file.txt", before, after);
            let ids = review
                .hunks
                .iter()
                .map(|hunk| hunk.id.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                select_workspace_hunks("file.txt", before, after, &review.review_digest, &ids)
                    .unwrap(),
                after
            );
        }
    }
}
