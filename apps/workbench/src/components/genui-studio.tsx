import { useEffect, useRef, useState } from "react";
import type { HyperTermHost } from "../host.ts";
import type { AcceptedArtifact } from "../protocol.ts";
import { GenUiCompiler } from "../genui/compiler-client.ts";
import {
  mapPreviewRuntimeError,
  type RuntimeDiagnostic,
} from "../genui/runtime-diagnostic.ts";
import { CodeDiff } from "./code-diff.tsx";
import { CodeEditor } from "./code-editor.tsx";

const sampleOriginalSource = `import React from "react";

export default function AgentStatus() {
  return (
    <main style={{ padding: 28 }}>
      <p>Agent is working…</p>
    </main>
  );
}
`;

const sampleInitialSource = `import React from "react";

export default function AgentStatus() {
  const [expanded, setExpanded] = React.useState(false);
  return (
    <main style={{ padding: 28, fontFamily: "system-ui" }}>
      <section style={{
        border: "1px solid #39422e",
        borderRadius: 16,
        padding: 20,
        background: "linear-gradient(145deg, #1c2118, #12150f)",
      }}>
        <small style={{ color: "#9ba88a" }}>ACP · CODEX</small>
        <h2 style={{ margin: "10px 0 4px" }}>Verification complete</h2>
        <p style={{ color: "#aeb6a1" }}>19 tests passed · 0 effects replayed</p>
        <button onClick={() => setExpanded(!expanded)} style={{
          marginTop: 12,
          border: 0,
          borderRadius: 9,
          padding: "9px 13px",
          background: "#d7ff72",
          color: "#11140e",
          fontWeight: 700,
        }}>
          {expanded ? "Hide evidence" : "Show evidence"}
        </button>
        {expanded && <pre style={{ color: "#cbd5bc" }}>
          cargo test --workspace{"\\n"}deno task check{"\\n"}deno task build
        </pre>}
      </section>
    </main>
  );
}
`;

type StudioView = "code" | "diff" | "trace";

interface TraceEntry {
  revision: number;
  label: string;
  detail: string;
  state: "working" | "accepted" | "failed";
}

export interface GenUiStudioProps {
  host: HyperTermHost;
  initialSource?: string;
  baselineSource?: string;
  initialRevision?: number;
  heading?: string;
}

export function GenUiStudio({
  host,
  initialSource = sampleInitialSource,
  baselineSource = sampleOriginalSource,
  initialRevision = 0,
  heading = "Live artifact",
}: GenUiStudioProps) {
  const [source, setSource] = useState(initialSource);
  const [view, setView] = useState<StudioView>("code");
  const [status, setStatus] = useState("Compiler starting");
  const [error, setError] = useState<string>();
  const [accepted, setAccepted] = useState<AcceptedArtifact>();
  const [previewRuntime, setPreviewRuntime] = useState("idle");
  const [runtimeDiagnostic, setRuntimeDiagnostic] = useState<
    RuntimeDiagnostic
  >();
  const [revealRequest, setRevealRequest] = useState(0);
  const [previewBoot, setPreviewBoot] = useState(0);
  const [trace, setTrace] = useState<TraceEntry[]>([]);
  const compiler = useRef<GenUiCompiler | null>(null);
  const previewFrame = useRef<HTMLIFrameElement | null>(null);
  const previewChannel = useRef(crypto.randomUUID()).current;
  const revision = useRef(initialRevision);
  const previewUrl = new URL("./genui/preview.html", document.baseURI);
  previewUrl.hash = previewChannel;

  useEffect(() => {
    compiler.current = new GenUiCompiler();
    return () => compiler.current?.dispose();
  }, []);

  useEffect(() => {
    function receivePreviewEvent(event: MessageEvent) {
      if (event.source !== previewFrame.current?.contentWindow) return;
      const message = event.data as {
        type?: string;
        message?: string;
        stack?: string;
        channel_token?: string;
        artifact_id?: string;
        source_revision?: number;
        generated_line?: number;
        generated_column?: number;
      };
      if (message.channel_token !== previewChannel) return;
      if (message.type === "hyper_term_preview_boot") {
        setPreviewRuntime("booting runtime");
      } else if (message.type === "hyper_term_preview_ready") {
        if (
          accepted &&
          (message.artifact_id !== accepted.artifact_id ||
            message.source_revision !== accepted.source_revision)
        ) return;
        setRuntimeDiagnostic(undefined);
        setPreviewRuntime("ready");
      } else if (message.type === "hyper_term_preview_error") {
        if (
          !accepted || message.artifact_id !== accepted.artifact_id ||
          message.source_revision !== accepted.source_revision
        ) return;
        const diagnostic = mapPreviewRuntimeError(
          message,
          accepted.source_map,
        );
        setRuntimeDiagnostic(diagnostic);
        setPreviewRuntime(
          diagnostic.original
            ? `runtime error · ${diagnostic.original.file}:${diagnostic.original.line}:${diagnostic.original.column}`
            : "runtime error",
        );
        setView("code");
        setTrace((entries) =>
          [{
            revision: accepted.source_revision,
            label: "Preview runtime failed",
            detail: diagnostic.original
              ? `${diagnostic.original.file}:${diagnostic.original.line}:${diagnostic.original.column} · ${diagnostic.message}`
              : diagnostic.message,
            state: "failed" as const,
          }, ...entries].slice(0, 8)
        );
      }
    }
    globalThis.addEventListener("message", receivePreviewEvent);
    return () => globalThis.removeEventListener("message", receivePreviewEvent);
  }, [accepted, previewChannel]);

  useEffect(() => {
    if (!accepted || previewBoot === 0) return;
    setPreviewRuntime("loading artifact");
    previewFrame.current?.contentWindow?.postMessage({
      type: "hyper_term_render_artifact",
      schema_version: 1,
      channel_token: previewChannel,
      artifact: {
        artifact_id: accepted.artifact_id,
        source_revision: accepted.source_revision,
        content_digest: accepted.content_digest,
        bundle: accepted.bundle,
        css: accepted.css,
        source_map: accepted.source_map,
      },
    }, "*");
  }, [accepted, previewBoot, previewChannel]);

  useEffect(() => {
    setRuntimeDiagnostic(undefined);
    const timer = globalThis.setTimeout(async () => {
      const sourceRevision = ++revision.current;
      const activeCompiler = compiler.current;
      if (!activeCompiler) return;
      setStatus(`Building source r${sourceRevision}`);
      setError(undefined);
      setTrace((entries) =>
        [{
          revision: sourceRevision,
          label: "Compile candidate",
          detail: "bounded virtual filesystem → esbuild-wasm Worker",
          state: "working" as const,
        }, ...entries].slice(0, 8)
      );
      try {
        const candidate = await activeCompiler.compile(sourceRevision, source);
        const nextAccepted = await host.acceptArtifact(candidate);
        if (sourceRevision !== revision.current) return;
        setAccepted(nextAccepted);
        setStatus(`Accepted r${sourceRevision} · ${candidate.bundle.length} B`);
        setTrace((entries) =>
          [{
            revision: sourceRevision,
            label: "Artifact accepted",
            detail: `${nextAccepted.artifact_id} · ${nextAccepted.accepted_by}`,
            state: "accepted" as const,
          }, ...entries.filter((entry) => entry.revision !== sourceRevision)]
            .slice(0, 8)
        );
      } catch (compileError) {
        if (sourceRevision !== revision.current) return;
        const message = compileError instanceof Error
          ? compileError.message
          : String(compileError);
        setError(message);
        setStatus(`Rejected r${sourceRevision} · last good kept`);
        setTrace((entries) =>
          [{
            revision: sourceRevision,
            label: "Candidate rejected",
            detail: message,
            state: "failed" as const,
          }, ...entries.filter((entry) => entry.revision !== sourceRevision)]
            .slice(0, 8)
        );
      }
    }, 260);
    return () => clearTimeout(timer);
  }, [host, source]);

  const runtimeLocation = runtimeDiagnostic?.original;

  return (
    <aside className="studio" aria-label="Agentic UI Studio">
      <header className="studio-header">
        <div>
          <span className="eyebrow">Agentic UI Studio</span>
          <h2>{heading}</h2>
        </div>
        <span className={`compiler-status ${error ? "has-error" : ""}`}>
          <span /> {status}
        </span>
      </header>
      <div className="studio-tabs" role="tablist" aria-label="Artifact tools">
        {(["code", "diff", "trace"] as const).map((tab) => (
          <button
            key={tab}
            className={view === tab ? "active" : ""}
            type="button"
            role="tab"
            aria-selected={view === tab}
            onClick={() => setView(tab)}
          >
            {tab === "code" ? "Code" : tab === "diff" ? "Diff" : "Time Travel"}
          </button>
        ))}
        <span className="studio-spacer" />
        <span className="source-revision">source r{revision.current || 1}</span>
      </div>
      <div className="studio-editor">
        {view === "code" && (
          <CodeEditor
            value={source}
            onChange={setSource}
            revealLocation={runtimeLocation?.file === "/App.tsx"
              ? runtimeLocation
              : undefined}
            revealRequest={revealRequest}
          />
        )}
        {view === "diff" && (
          <CodeDiff
            original={baselineSource}
            modified={source}
            onChange={setSource}
          />
        )}
        {view === "trace" && <TraceTimeline entries={trace} />}
      </div>
      {error
        ? <div className="compile-error" role="alert">{error}</div>
        : runtimeDiagnostic && (
          <div
            className="compile-error runtime-error"
            role="alert"
            title={runtimeDiagnostic.stack}
          >
            <button
              type="button"
              onClick={() => {
                setView("code");
                setRevealRequest((request) => request + 1);
              }}
            >
              <strong>
                Runtime{runtimeLocation
                  ? ` · ${runtimeLocation.file}:${runtimeLocation.line}:${runtimeLocation.column}`
                  : ""}
              </strong>
              <span>{runtimeDiagnostic.message}</span>
            </button>
          </div>
        )}
      <div className="preview-header">
        <div>
          <span className="eyebrow">Isolated preview</span>
          <strong>
            {accepted?.artifact_id ?? "Waiting for accepted artifact"}
          </strong>
        </div>
        <div className="preview-badges">
          <span
            className={previewRuntime.startsWith("runtime error") ? "bad" : ""}
          >
            {previewRuntime}
          </span>
          <span>no network</span>
          <span>sandbox</span>
          <span>{host.authority}</span>
        </div>
      </div>
      <div className="preview-frame">
        <iframe
          ref={previewFrame}
          title="Accepted Agentic UI artifact"
          sandbox="allow-scripts"
          src={previewUrl.href}
          onLoad={() => {
            setPreviewRuntime("booting runtime");
            setPreviewBoot((generation) => generation + 1);
          }}
        />
      </div>
    </aside>
  );
}

function TraceTimeline({ entries }: { entries: TraceEntry[] }) {
  return (
    <div className="trace-timeline">
      <div className="trace-note">
        Semantic trace records revisions and accepted transitions. Replaying
        this view does not repeat Shell, MCP, ACP, or Computer Use effects.
      </div>
      {entries.length === 0 && (
        <p className="dim">Compile once to create a trace.</p>
      )}
      {entries.map((entry, index) => (
        <div
          className="trace-entry"
          data-state={entry.state}
          key={`${entry.revision}-${index}`}
        >
          <span className="trace-node" />
          <div>
            <strong>r{entry.revision} · {entry.label}</strong>
            <p>{entry.detail}</p>
          </div>
        </div>
      ))}
    </div>
  );
}
