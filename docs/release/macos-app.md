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
- `Contents/Resources/terminal`: the built terminal WebView assets.

Native SDK first creates an unsigned `.app`. The workflow then composes the
complete bundle, signs every Mach-O executable and the outer bundle, submits it
to Apple's notary service, staples the ticket, and finally creates the release
ZIP. Modifying the bundle after the signing step invalidates its signature.

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
