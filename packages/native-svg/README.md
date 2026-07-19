# native-svg

`native-svg` converts a Native SDK `canvas.DisplayList` into a standalone,
GitHub-renderable SVG. It preserves the SDK's computed layout, transforms,
clips, opacity, gradients, vector paths, design-token colors, and bundled
Geist glyph outlines.

The Hyper Term README renderer imports the real desktop `main.zig` and
`app.native`, builds the same widget tree used by the application, and then
passes its display list to this package:

```sh
deno task render:readme
```

The command updates `docs/assets/hyper-term-ui.svg`. Run `zig build test`
inside this directory to exercise the reusable display-list converter, or
`zig build check-readme` to fail when the tracked preview is stale.
