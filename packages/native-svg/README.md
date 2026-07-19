# Hyper Term SVG adapter

This package is the Hyper Term-specific adapter for the Native SDK's reusable
`canvas.writeSvg` exporter. The generic implementation lives in Native SDK;
this directory only imports Hyper Term's real application module, resolves its
model and `.native` view, and constructs the canvas scene.

The Hyper Term README renderer imports the real desktop `main.zig` and
`app.native`, builds the same widget tree used by the application, and then
passes its display list to this package:

```sh
deno task render:readme
```

The command updates `docs/assets/hyper-term-ui.svg`. Run `zig build test`
inside this directory to compile the application adapter, or
`zig build check-readme` to fail when the tracked preview is stale. Native SDK
owns the converter's geometry, resource, image, font, and raster-fallback tests.
