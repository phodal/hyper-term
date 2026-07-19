# Hyper Term macOS release

Tags matching `vX.Y.Z` start `.github/workflows/release.yml`. The workflow
validates the Rust, Deno, and Native SDK layers, then builds separate Apple
Silicon and Intel application archives.

The Native CLI binary and checksum come from the pinned Native SDK release. Its
framework checkout is also pinned to the audited commit recorded by ADR 0013;
the workflow refuses to build when the release tag resolves elsewhere.

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

The ACP runtime build follows Deno's production links into one self-contained
tree but excludes the top-level `.pnpm` content store and installer metadata.
Those files duplicate the same dependencies and provider binaries and are not
runtime inputs. Both build and verification enforce the desktop loader's limit
of 8,192 files and 128 MiB before packaging. The 2026-07-20 arm64 ad-hoc package
probe contained 5,972 verified ACP files and reduced the complete application
from 1.9 GiB to 184 MiB while both offline adapter `--version` probes passed.
These figures are evidence for that toolchain snapshot, not a permanent package
size guarantee.

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
