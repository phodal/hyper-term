import { useEffect, useMemo, useState } from "react";
import {
  type BugCapsule,
  OfflineBugCapsuleClient,
} from "../debug-capsule-client.ts";
import type { RuntimeTraceEvent } from "../runtime-trace-client.ts";

type CapsuleState =
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | { kind: "ready"; capsule: BugCapsule };

export function CapsuleWorkbench() {
  const token = useMemo(() => {
    const candidate =
      new URLSearchParams(globalThis.location.search).get("token") ?? "";
    return /^[A-Za-z0-9_-]{32,128}$/.test(candidate) ? candidate : undefined;
  }, []);
  const [state, setState] = useState<CapsuleState>({ kind: "loading" });

  useEffect(() => {
    if (!token) {
      setState({ kind: "failed", message: "Offline viewer token is invalid." });
      return;
    }
    const controller = new AbortController();
    new OfflineBugCapsuleClient({ token }).open(controller.signal).then(
      (capsule) => setState({ kind: "ready", capsule }),
    ).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setState({
        kind: "failed",
        message: error instanceof Error ? error.message : String(error),
      });
    });
    return () => controller.abort();
  }, [token]);

  if (state.kind === "loading") {
    return (
      <CapsuleStatus
        title="Opening capsule"
        detail="Rust is verifying the bounded local file…"
      />
    );
  }
  if (state.kind === "failed") {
    return (
      <CapsuleStatus title="Capsule rejected" detail={state.message} failed />
    );
  }
  return <VerifiedCapsule capsule={state.capsule} />;
}

function CapsuleStatus({
  title,
  detail,
  failed = false,
}: {
  title: string;
  detail: string;
  failed?: boolean;
}) {
  return (
    <main className="capsule-state" data-failed={failed || undefined}>
      <span className="capsule-state-mark">{failed ? "!" : "◎"}</span>
      <strong>{title}</strong>
      <p>{detail}</p>
    </main>
  );
}

function VerifiedCapsule({ capsule }: { capsule: BugCapsule }) {
  const replayable = capsule.runtime.events.filter(isReplayable);
  const [cursor, setCursor] = useState(
    replayable.at(-1)?.event_sequence ?? 0,
  );
  const visibleEvents = capsule.runtime.events.filter((event) =>
    cursor === 0 || event.event_sequence <= cursor
  );
  return (
    <main className="capsule-workbench">
      <header className="capsule-header">
        <div>
          <span className="eyebrow">Offline semantic replay</span>
          <h1>{capsule.artifact.entrypoint}</h1>
        </div>
        <div className="capsule-verification">
          <span>✓ Rust verified</span>
          <code>{capsule.capsule_digest.slice(0, 12)}</code>
        </div>
      </header>

      <section className="capsule-summary" aria-label="Capsule identity">
        <Summary
          label="Artifact"
          value={capsule.artifact.artifact_id.slice(0, 8)}
        />
        <Summary
          label="Source"
          value={`r${capsule.artifact.source_revision}`}
        />
        <Summary
          label="Compiler"
          value={`${capsule.artifact.compiler.name} ${capsule.artifact.compiler.version}`}
        />
        <Summary
          label="Runtime"
          value={`${capsule.environment.os} · ${capsule.environment.architecture}`}
        />
      </section>

      <div className="capsule-grid">
        <section className="capsule-panel capsule-replay">
          <header>
            <div>
              <strong>Semantic replay</strong>
              <span>
                {visibleEvents.length} / {capsule.runtime.events.length} events
              </span>
            </div>
            <span className="capsule-no-effects">No live effects</span>
          </header>
          {replayable.length > 0 && (
            <div className="capsule-cursors" aria-label="Replay checkpoints">
              {replayable.map((event) => (
                <button
                  type="button"
                  key={event.event_sequence}
                  data-selected={cursor === event.event_sequence || undefined}
                  onClick={() => setCursor(event.event_sequence)}
                >
                  #{event.event_sequence} {event.kind}
                </button>
              ))}
            </div>
          )}
          <div className="capsule-events">
            {visibleEvents.length === 0 && (
              <p className="capsule-empty">
                No semantic runtime events were exported.
              </p>
            )}
            {visibleEvents.map((event) => (
              <article key={event.event_sequence}>
                <span className="trace-node" />
                <div>
                  <strong>#{event.event_sequence} · {event.name}</strong>
                  <small>
                    {event.kind} · {event.payload_digest.slice(0, 10)}
                    {event.redacted ? " · redacted" : ""}
                  </small>
                  {event.kind !== "console" && event.kind !== "error" && (
                    <pre>{JSON.stringify(event.payload, null, 2)}</pre>
                  )}
                </div>
              </article>
            ))}
          </div>
        </section>

        <aside className="capsule-panel capsule-inventory-panel">
          <header>
            <div>
              <strong>Export inventory</strong>
              <span>Exact Rust projection</span>
            </div>
          </header>
          <div className="capsule-inventory-list">
            {capsule.inventory.map((entry) => (
              <details key={entry.category}>
                <summary>
                  <span>{entry.category.replaceAll("_", " ")}</span>
                  <b data-inclusion={entry.inclusion}>
                    {entry.inclusion.replaceAll("_", " ")}
                  </b>
                </summary>
                <p>{entry.reason}</p>
                <small>
                  {entry.item_count} item(s) · {formatBytes(entry.byte_count)}
                </small>
              </details>
            ))}
          </div>
        </aside>
      </div>

      <footer className="capsule-footer">
        <span>replay only</span>
        <span>source bodies excluded</span>
        <span>Shell · ACP · MCP · Computer Use disabled</span>
      </footer>
    </main>
  );
}

function Summary({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function isReplayable(event: RuntimeTraceEvent): boolean {
  return event.kind === "action" || event.kind === "checkpoint" ||
    event.kind === "effect_receipt";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KiB`;
}
