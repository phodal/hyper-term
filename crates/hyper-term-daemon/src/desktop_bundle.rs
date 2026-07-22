//! Integrity checks for resources embedded in the macOS application bundle.
//!
//! These checks are kept separate from desktop process supervision so the
//! packaged trust boundary can evolve without growing the launcher itself.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use hyper_term_drivers::sha256_file;
use serde::Deserialize;

use super::{ResolvedAcpAdapter, validate_executable};

const ACP_RUNTIME_MANIFEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const ACP_RUNTIME_MAX_FILES: usize = 8 * 1024;
const ACP_RUNTIME_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const BUNDLE_ASSET_MANIFEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const BUNDLE_ASSET_MAX_FILES: usize = 1024;
const BUNDLE_ASSET_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Deserialize)]
struct BundledAcpRuntimeManifest {
    schema_version: u32,
    runtime: BundledAcpRuntimeIdentity,
    adapters: Vec<BundledAcpAdapterManifest>,
    files: Vec<BundledAcpFileManifest>,
}

#[derive(Deserialize)]
struct BundledAcpRuntimeIdentity {
    name: String,
    version: String,
}

#[derive(Deserialize)]
struct BundledAcpAdapterManifest {
    provider_id: String,
    package: String,
    version: String,
    entrypoint: String,
    required_agent: String,
    entrypoint_sha256: String,
}

#[derive(Deserialize)]
struct BundledAcpFileManifest {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Copy)]
pub(super) enum AssetManifestKind {
    Frontend,
    Runtime,
}

#[derive(Deserialize)]
struct AssetBuildManifest {
    schema_version: u32,
    builder: Option<AssetBuilderIdentity>,
    runtime: Option<BundledAcpRuntimeIdentity>,
    protocol_version: Option<u32>,
    files: Vec<BundledAcpFileManifest>,
}

#[derive(Deserialize)]
struct AssetBuilderIdentity {
    runtime: String,
    version: String,
}

pub(super) fn verify_asset_manifest(
    root: &Path,
    kind: AssetManifestKind,
    required_files: &[&str],
) -> Result<usize, String> {
    let manifest_path = root.join("build-manifest.json");
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path).map_err(|error| {
        format!(
            "cannot inspect asset manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    if manifest_metadata.file_type().is_symlink()
        || !manifest_metadata.is_file()
        || manifest_metadata.len() == 0
        || manifest_metadata.len() > BUNDLE_ASSET_MANIFEST_MAX_BYTES
    {
        return Err(format!(
            "asset manifest size is invalid: {}",
            manifest_path.display()
        ));
    }
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|error| {
        format!(
            "cannot read asset manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: AssetBuildManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            format!(
                "cannot parse asset manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    let identity_is_supported = match kind {
        AssetManifestKind::Frontend
            if manifest.runtime.is_none() && manifest.protocol_version.is_none() =>
        {
            manifest
                .builder
                .as_ref()
                .is_some_and(|identity| identity.runtime == "deno" && identity.version == "2.9.3")
        }
        AssetManifestKind::Runtime
            if manifest.builder.is_none() && manifest.protocol_version == Some(1) =>
        {
            manifest
                .runtime
                .as_ref()
                .is_some_and(|identity| identity.name == "deno" && identity.version == "2.9.3")
        }
        _ => false,
    };
    if manifest.schema_version != 1 || !identity_is_supported {
        return Err(format!(
            "asset manifest identity is unsupported: {}",
            manifest_path.display()
        ));
    }
    if manifest.files.is_empty() || manifest.files.len() > BUNDLE_ASSET_MAX_FILES {
        return Err(format!(
            "asset manifest inventory is invalid: {}",
            manifest_path.display()
        ));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve asset root {}: {error}", root.display()))?;
    let mut inventoried = BTreeSet::new();
    let mut total_bytes = 0_u64;
    for file in &manifest.files {
        let relative_path = validate_bundled_relative_path(&file.path)?;
        if !is_sha256(&file.sha256) {
            return Err(format!("asset file digest is invalid: {}", file.path));
        }
        total_bytes = total_bytes
            .checked_add(file.bytes)
            .ok_or_else(|| "asset manifest byte count overflowed".to_owned())?;
        if total_bytes > BUNDLE_ASSET_MAX_TOTAL_BYTES {
            return Err(format!(
                "asset inventory exceeds its byte budget: {}",
                root.display()
            ));
        }
        let path = canonical_root.join(&relative_path);
        let link_metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("asset file is unavailable: {}: {error}", file.path))?;
        if link_metadata.file_type().is_symlink() {
            return Err(format!(
                "asset file must not be a symbolic link: {}",
                file.path
            ));
        }
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve asset file {}: {error}", file.path))?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(format!("asset file escapes its root: {}", file.path));
        }
        let metadata = std::fs::metadata(&canonical_path)
            .map_err(|error| format!("cannot inspect asset file {}: {error}", file.path))?;
        if !metadata.is_file() || metadata.len() != file.bytes {
            return Err(format!("asset file size changed: {}", file.path));
        }
        let actual = sha256_file(&canonical_path)
            .map_err(|error| format!("cannot hash asset file {}: {error}", file.path))?;
        if actual != file.sha256 {
            return Err(format!("asset file digest changed: {}", file.path));
        }
        if !inventoried.insert(file.path.clone()) {
            return Err(format!("asset file is duplicated: {}", file.path));
        }
    }
    for required in required_files {
        if !inventoried.contains(*required) {
            return Err(format!(
                "asset inventory is missing {required}: {}",
                root.display()
            ));
        }
    }
    if matches!(kind, AssetManifestKind::Frontend) {
        let mut actual = BTreeSet::new();
        collect_asset_files(&canonical_root, &canonical_root, &mut actual)?;
        actual.remove("build-manifest.json");
        if actual != inventoried {
            return Err(format!(
                "frontend asset inventory does not match its directory: {}",
                root.display()
            ));
        }
    }
    Ok(manifest.files.len())
}

fn collect_asset_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        format!(
            "cannot enumerate asset directory {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("cannot enumerate asset entry: {error}"))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect asset entry {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "asset directory contains a symbolic link: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_asset_files(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!(
                "asset directory contains a special file: {}",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("asset file escapes its root: {}", path.display()))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("asset path is not UTF-8: {}", path.display()))?;
        if files.len() == BUNDLE_ASSET_MAX_FILES || !files.insert(relative.to_owned()) {
            return Err(format!(
                "asset directory inventory is invalid: {}",
                root.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_bundled_deno(executable: &Path) -> Result<String, String> {
    let output = Command::new(executable)
        .arg("--version")
        .env_clear()
        .output()
        .map_err(|error| format!("cannot run packaged Deno: {error}"))?;
    if !output.status.success() || output.stdout.len() > 4096 || output.stderr.len() > 4096 {
        return Err("packaged Deno version probe failed or exceeded its output bound".into());
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| "packaged Deno version output is not UTF-8".to_owned())?
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    if version != "deno 2.9.3 (stable, release, aarch64-apple-darwin)"
        && version != "deno 2.9.3 (stable, release, x86_64-apple-darwin)"
    {
        return Err(format!("packaged Deno version is unsupported: {version}"));
    }
    Ok(version)
}

pub(super) fn load_bundled_acp_runtime(
    runtime_resources: &Path,
    deno_executable: &Path,
) -> Result<BTreeMap<String, ResolvedAcpAdapter>, String> {
    let root = runtime_resources.join("acp");
    let manifest_path = root.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(BTreeMap::new());
    }
    validate_executable(deno_executable)?;
    let manifest_metadata = std::fs::metadata(&manifest_path)
        .map_err(|error| format!("cannot inspect bundled ACP manifest: {error}"))?;
    if manifest_metadata.len() == 0 || manifest_metadata.len() > ACP_RUNTIME_MANIFEST_MAX_BYTES {
        return Err("bundled ACP manifest size is invalid".into());
    }
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("cannot read bundled ACP manifest: {error}"))?;
    let manifest: BundledAcpRuntimeManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("cannot parse bundled ACP manifest: {error}"))?;
    if manifest.schema_version != 1
        || manifest.runtime.name != "deno"
        || manifest.runtime.version != "2.9.3"
    {
        return Err("bundled ACP runtime identity is unsupported".into());
    }
    if manifest.files.is_empty() || manifest.files.len() > ACP_RUNTIME_MAX_FILES {
        return Err("bundled ACP file inventory is invalid".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve bundled ACP runtime: {error}"))?;
    let mut file_digests = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for file in manifest.files {
        let relative_path = validate_bundled_relative_path(&file.path)?;
        if !is_sha256(&file.sha256) {
            return Err(format!(
                "bundled ACP file metadata is invalid: {}",
                file.path
            ));
        }
        total_bytes = total_bytes
            .checked_add(file.bytes)
            .ok_or_else(|| "bundled ACP runtime byte count overflowed".to_owned())?;
        if total_bytes > ACP_RUNTIME_MAX_TOTAL_BYTES {
            return Err("bundled ACP runtime exceeds its byte budget".into());
        }
        let path = canonical_root.join(&relative_path);
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("bundled ACP file is unavailable: {}: {error}", file.path))?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(format!(
                "bundled ACP file escapes its runtime: {}",
                file.path
            ));
        }
        let metadata = std::fs::metadata(&canonical_path)
            .map_err(|error| format!("cannot inspect bundled ACP file {}: {error}", file.path))?;
        if !metadata.is_file() || metadata.len() != file.bytes {
            return Err(format!("bundled ACP file size changed: {}", file.path));
        }
        let actual = sha256_file(&canonical_path)
            .map_err(|error| format!("cannot hash bundled ACP file {}: {error}", file.path))?;
        if actual != file.sha256 {
            return Err(format!("bundled ACP file digest changed: {}", file.path));
        }
        if file_digests
            .insert(file.path.clone(), file.sha256)
            .is_some()
        {
            return Err(format!("bundled ACP file is duplicated: {}", file.path));
        }
    }

    if manifest.adapters.len() != 2 {
        return Err("bundled ACP adapter inventory is incomplete".into());
    }
    let mut adapters = BTreeMap::new();
    for adapter in manifest.adapters {
        let expected_package = match adapter.provider_id.as_str() {
            "codex-acp" if adapter.required_agent == "codex" => "@agentclientprotocol/codex-acp",
            "claude-acp" if adapter.required_agent == "claude" => {
                "@agentclientprotocol/claude-agent-acp"
            }
            _ => return Err("bundled ACP adapter identity is unsupported".into()),
        };
        if adapter.package != expected_package || adapter.version.is_empty() {
            return Err(format!(
                "bundled ACP package identity changed: {}",
                adapter.provider_id
            ));
        }
        let entrypoint = validate_bundled_relative_path(&adapter.entrypoint)?;
        let Some(file_digest) = file_digests.get(&adapter.entrypoint) else {
            return Err(format!(
                "bundled ACP entrypoint is not inventoried: {}",
                adapter.provider_id
            ));
        };
        if file_digest != &adapter.entrypoint_sha256 || !is_sha256(file_digest) {
            return Err(format!(
                "bundled ACP entrypoint digest changed: {}",
                adapter.provider_id
            ));
        }
        let entrypoint = canonical_root.join(entrypoint);
        let resolved = ResolvedAcpAdapter {
            executable: deno_executable.to_owned(),
            arguments: [
                "run",
                "--cached-only",
                "--no-config",
                "--node-modules-dir=manual",
                "-A",
            ]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(entrypoint.into_os_string()))
            .collect(),
            implementation_version: adapter.version,
        };
        if adapters
            .insert(adapter.provider_id.clone(), resolved)
            .is_some()
        {
            return Err(format!(
                "bundled ACP adapter is duplicated: {}",
                adapter.provider_id
            ));
        }
    }
    Ok(adapters)
}

pub(super) fn validate_bundled_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.len() > 4096 {
        return Err("bundled ACP path length is invalid".into());
    }
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "bundled ACP path must be normalized and relative: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
