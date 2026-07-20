# Hyper Term macOS release

Tags matching `vX.Y.Z` start `.github/workflows/release.yml`. The workflow
validates the Rust, Deno, and Native SDK layers, then builds separate Apple
Silicon and Intel application archives.

The Native CLI binary and checksum come from the pinned Native SDK release. Its
framework checkout is also pinned to the audited commit recorded by ADR 0013;
the workflow refuses to build when the release tag resolves elsewhere.

Before packaging either architecture, the validation job launches the complete
Rust supervisor with an automation-enabled Native renderer. The macOS smoke
enforces a 750 ms cold-start first-frame cap for shared release runners and
Native's canvas frame budget, attaches the Terminal WebView, exposes accessible
Terminal and Agent creation controls, and handles `Command-T` plus `Command-W`.
Its Native snapshot, deterministic canvas screenshot, and supervisor log are
retained as the `macos-desktop-smoke` workflow artifact.

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
the frozen runtime it launches the exact packaged Deno executable and real
Codex ACP entrypoint, connects through `AcpAgentClient`, and completes an ACP
`initialize` exchange against a deterministic external Codex app-server
fixture. This proves the packaged adapter can load its dependency graph, spawn
the configured provider path, translate the provider handshake, and return the
official ACP capability response without requiring an account or network.

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
the release handshake supplies the same boundary with a deterministic fixture
instead of accidentally testing an unusable bundled launcher.

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
