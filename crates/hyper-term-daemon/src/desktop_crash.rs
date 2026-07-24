//! Private, bounded crash metadata for the Native renderer supervisor.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct RendererCrashReport {
    schema_version: u16,
    recorded_at_unix_ms: u128,
    component: &'static str,
    product_version: &'static str,
    source_commit: &'static str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    crash_sequence: usize,
    completed_restarts: usize,
    restart_limit: usize,
    will_restart: bool,
}

pub(super) fn write_renderer_crash_report(
    path: &Path,
    status: ExitStatus,
    crash_sequence: usize,
    completed_restarts: usize,
    restart_limit: usize,
    will_restart: bool,
) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "crash report path is a symbolic link: {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("crash report path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create crash report directory: {error}"))?;
    let temporary = parent.join(format!(
        ".renderer-crash-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("cannot create private crash report: {error}"))?;
        let report = RendererCrashReport {
            schema_version: 1,
            recorded_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            component: "native_renderer",
            product_version: env!("CARGO_PKG_VERSION"),
            source_commit: option_env!("HYPER_TERM_SOURCE_COMMIT").unwrap_or("unknown"),
            exit_code: status.code(),
            signal: status.signal(),
            crash_sequence,
            completed_restarts,
            restart_limit,
            will_restart,
        };
        serde_json::to_writer(&mut file, &report)
            .map_err(|error| format!("cannot serialize crash report: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("cannot finish crash report: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync crash report: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("cannot publish crash report: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
