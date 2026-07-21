# Hyper Term macOS release

Tags matching `vX.Y.Z` start `.github/workflows/release.yml`. The workflow
validates the Rust, Deno, and Native SDK layers, then builds separate Apple
Silicon and Intel application archives.

Release candidates are intentionally limited to one published GitHub Release
per `Asia/Shanghai` calendar day. Commits and validation may continue normally,
but a second tag on the same day fails before the expensive build begins. A
rerun of the same tag remains allowed so a failed build can be repaired without
creating another GitHub Release. There is no same-day override.

The Native CLI binary and checksum come from the pinned Native SDK release. Its
framework checkout is also pinned to the audited commit recorded by ADR 0013;
the workflow refuses to build when the release tag resolves elsewhere.

Before packaging either architecture, the validation job launches the complete
Rust supervisor with an automation-enabled Native renderer. The macOS smoke's
local default enforces Native SDK's 150 ms cold-start first-frame budget; the
shared release runner explicitly allows 1500 ms for virtualized-display startup.
The same run enforces Native's canvas frame budget, attaches the Terminal
WebView, exposes accessible Terminal and Agent creation controls, and handles
`Command-T` plus `Command-W`. Its Native snapshot, deterministic canvas
screenshot, and supervisor log are retained as the `macos-desktop-smoke`
workflow artifact.

A second macOS smoke opens a Rust-verified offline Bug Capsule. It asserts that
the cold-path GenUI WebView is mounted only for that Capsule, fetches the real
built Workbench index and hashed assets through its token-bound loopback URL,
and verifies the migrated replay-only Capsule projection. Native automation
cannot capture system WebView pixels, so this gate deliberately pairs the
Native view/sizing snapshot with HTTP and Rust projection checks instead of
claiming unsupported DOM screenshot coverage.

The repository also provides `scripts/smoke_macos_real_codex_acp.sh` as an
explicit developer-only post-package gate. When Codex is already authenticated,
it launches the assembled app with an automation-enabled Native renderer and
proves Native composer input reaches the bundled Codex ACP adapter, enters the
streaming/stop state, and returns to an enabled composer with the expected
Agent Block. This account-using gate is intentionally excluded from CI and
never invokes login. By default it does not authorize a tool call.

Set `HYPER_TERM_REAL_ACP_GENUI=1` for the stricter path: Codex ACP must call
`hyper_term.genui.compile`, the Native approval UI must authorize it, and the
Rust journal must contain an accepted artifact plus a successful MCP receipt.
The same run then opens the Native artifact-editor disclosure, verifies the
token-bound built Workbench and its hashed assets, loads the exact Rust source
and checkpoint, and requests diagnostics from the Rust-managed Deno LSP. It
also rejects an invalid gateway token and can retain the snapshot, screenshot,
supervisor log, and machine-readable editor evidence through
`HYPER_TERM_REAL_ACP_ARTIFACT_DIR`. The connector stays inside the Agent sandbox
while Rust launches the supervised Deno compiler outside that process tree,
avoiding nested Seatbelt.

The final bundle contains:

- `Contents/MacOS/hyper-term`: the Rust desktop supervisor and PTY authority;
- `Contents/MacOS/hyper-term-ui`: the Native SDK window and renderer;
- `Contents/MacOS/hyper-term-mcp`: the Agent-mode-only, brokered stdio MCP connector;
- `Contents/Resources/terminal`: the built terminal WebView assets;
- `Contents/Resources/runtime/deno`: the pinned, supervised Deno sidecar;
- `Contents/Resources/runtime/genui-compiler.js` and `esbuild.wasm`: the
  digest-checked cold-path GenUI compiler used only after broker approval;
- `Contents/Resources/runtime/acp`: the frozen production dependency trees for
  official Codex ACP 1.1.4 and Claude Agent ACP 0.59.0, plus a per-file digest
  manifest and the Deno lockfile used to reproduce them.

The validation job does not stop at adapter `--version` output. After building
the frozen runtime it launches the exact packaged Deno executable and both real
ACP entrypoints through `AcpAgentClient`. Codex completes `initialize` against a
deterministic external app-server fixture. Claude proceeds through
`initialize -> session/new`, starts the configured external Claude executable,
exchanges the official SDK stream-JSON initialization and context-usage control
frames, and returns a non-empty ACP session. These gates prove both packaged
adapters can load their dependency graphs, spawn only the configured provider
paths, translate their distinct provider protocols, and return official ACP
responses without requiring an account or network.

The ACP runtime build follows Deno's production links into one self-contained
tree but excludes the top-level `.pnpm` content store and installer metadata.
Those files duplicate the same dependencies and provider binaries and are not
runtime inputs. Both build and verification enforce the desktop loader's limit
of 8,192 files and 128 MiB before packaging. The 2026-07-20 arm64 ad-hoc package
probe contained 5,972 verified ACP files and reduced the complete application
from 1.9 GiB to 184 MiB while both offline adapter `--version` probes passed.
These figures are evidence for that toolchain snapshot, not a permanent package
size guarantee.

The adapter dependency tree may contain provider launcher metadata, but Hyper
Term deliberately excludes platform-specific Codex and Claude binaries. The
runtime contract requires a separately discovered, authenticated provider CLI;
the release handshakes supply the same boundary with deterministic Codex and
Claude fixtures instead of accidentally testing unusable bundled launchers.

Native SDK first creates an unsigned `.app`. The workflow then composes the
complete bundle, signs every Mach-O executable and the outer bundle, submits it
to Apple's notary service, staples the ticket, and finally creates the release
ZIP. Modifying the bundle after the signing step invalidates its signature.
The bundled Deno executable is re-signed with the reviewed V8/JIT entitlements
in `runtime/deno.entitlements.plist`. Rust gives the GenUI compiler no shell,
network, FFI, or workspace authority. The ACP adapters use the same pinned Deno
binary and an offline local dependency tree, then launch the user's
authenticated Codex or Claude executable inside the implemented macOS Tier 1
Seatbelt and managed-proxy boundary. The provider control process receives a
read-only workspace and a private writable session home. Opaque
provider-internal execution and hermetic acceptance still require Tier 2 under
ADR 0014.

Immediately before archiving, the final Rust entry point runs
`--verify-bundle`. This uses the paths derived from its actual `.app` location,
not build-directory overrides. It verifies the complete Terminal and Workbench
inventories against their Deno-generated sizes and SHA-256 digests, validates
the GenUI runtime manifest, rechecks every ACP runtime file and both adapter
entrypoints, and executes the packaged Deno only for an exact bounded 2.9.3
version probe. The check rejects extra frontend files and symbolic links, so a
successful build or signature alone cannot publish an incomplete or modified
application bundle.

The application can opt into that Tier 2 backend at launch with an explicit
`limactl` executable, local VZ-compatible image, and pinned SHA-256. The image
is not bundled or downloaded. ACP Terminal client capability is advertised
only after the desktop supervisor constructs the verified Lima runner; an
absent or partial configuration never degrades to host execution.

The application does not bundle Node or the large provider CLI binaries. It
only offers a bundled ACP provider when the matching `codex` or `claude`
executable is available locally, and may register an installed GitHub Copilot
CLI directly as `copilot --acp --stdio`. Rust verifies all bundled ACP runtime
files before advertising them, clears inherited API keys, bounds provider
readiness probes, and passes exact discovered executable paths to the gateway.
The Native renderer receives status metadata only and cannot launch providers.
An explicit ACP path always wins. Automatic startup prefers the verified
adapter bundle and uses a known global package only when that bundle is absent.
Codex recognizes both the Zed
and Agent Client Protocol packages; Claude recognizes its Agent Client Protocol
package. Recognition is bound to the canonical npm package root, manifest
identity, semantic version, and declared `bin` path rather than process output.

For pipeline testing, a pre-release tag may run without Apple secrets. In that
case the workflow ad-hoc signs the bundle and gives the asset an explicit
`-unsigned.zip` suffix. Stable tags never use this fallback: they fail unless
Developer ID signing and notarization are available.

Configure a protected GitHub environment named `Release` with these secrets:

- `APPLE_CERTIFICATE_P12`: base64-encoded Developer ID Application certificate;
- `APPLE_CERTIFICATE_PASSWORD`: password for the P12;
- `APPLE_SIGNING_IDENTITY`: full Developer ID Application identity;
- `APPLE_TEAM_ID`: Apple Developer team identifier;
- `APPLE_API_KEY_ID`: App Store Connect API key identifier;
- `APPLE_API_ISSUER_ID`: App Store Connect API issuer identifier;
- `APPLE_API_PRIVATE_KEY`: base64-encoded `.p8` API private key.

The stable part of the tag must match the Cargo workspace and
`apps/desktop/app.zon` versions. A stable tag creates a normal GitHub Release; a
tag with a suffix such as `v0.1.0-rc.1` creates a pre-release from application
version `0.1.0`. Re-running the workflow replaces the assets for an existing
release.
