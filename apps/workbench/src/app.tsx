import { useMemo, useState } from "react";
import { BlockWorkbench } from "./components/block-workbench.tsx";
import { ArtifactWorkbench } from "./components/artifact-workbench.tsx";
import { GenUiStudio } from "./components/genui-studio.tsx";
import { resolveHost } from "./host.ts";
import type { UiIntent } from "./protocol.ts";
import { sampleDocument } from "./sample-document.ts";

export function App() {
  if (
    new URLSearchParams(globalThis.location.search).get("surface") ===
      "artifact"
  ) {
    return <ArtifactWorkbench />;
  }
  return <DemoWorkbench />;
}

function DemoWorkbench() {
  const host = useMemo(resolveHost, []);
  const [notice, setNotice] = useState<string>();

  async function submitIntent(intent: UiIntent): Promise<void> {
    await host.submitIntent(intent);
    setNotice(
      host.authority === "demo_broker"
        ? "Intent captured by the inert demo broker; no machine effect occurred."
        : "Intent sent to the Rust permission broker.",
    );
    globalThis.setTimeout(() => setNotice(undefined), 2800);
  }

  return (
    <div className="app-shell">
      <nav className="activity-bar" aria-label="Workbench activity">
        <div className="brand-mark">H</div>
        <button className="active" type="button" aria-label="Terminal sessions">
          ⌁
        </button>
        <button type="button" aria-label="Agent tasks">◎</button>
        <button type="button" aria-label="Code review">⌘</button>
        <button type="button" aria-label="Artifacts">◇</button>
        <span className="activity-spacer" />
        <button type="button" aria-label="Settings">⚙</button>
      </nav>
      <section className="task-rail" aria-label="Terminal sessions">
        <header>
          <span>Sessions</span>
          <button type="button" aria-label="New terminal session">＋</button>
        </header>
        <label className="task-search">
          ⌕<input aria-label="Search sessions" placeholder="Find a session" />
        </label>
        <div className="rail-section-label">
          Needs attention <span>1</span>
        </div>
        <button className="task-item active" type="button">
          <span className="task-state waiting" />
          <span>
            <strong>~/ai/hyper-term</strong>
            <small>codex · approval · now</small>
          </span>
        </button>
        <div className="rail-section-label">
          Agent attached <span>2</span>
        </div>
        <button className="task-item" type="button">
          <span className="task-state running" />
          <span>
            <strong>codex / ACP probe</strong>
            <small>pty 02 · working · 2m</small>
          </span>
        </button>
        <button className="task-item" type="button">
          <span className="task-state running" />
          <span>
            <strong>claude / diff review</strong>
            <small>pty 03 · working · 8m</small>
          </span>
        </button>
        <div className="rail-section-label">
          Local shells <span>1</span>
        </div>
        <button className="task-item" type="button">
          <span className="task-state complete" />
          <span>
            <strong>zsh · hyper-term</strong>
            <small>idle · seq 18 · today</small>
          </span>
        </button>
        <footer>
          <span className="daemon-light" /> hyperd connected <small>v1</small>
        </footer>
      </section>
      <main className="workspace">
        <div className="attention-bar">
          <span className="attention-pulse" />
          <strong>1 operation needs approval</strong>
          <span>The exact command and base revision are locked.</span>
          <button type="button">Review</button>
        </div>
        <div className="workspace-grid">
          <BlockWorkbench
            document={sampleDocument}
            submitIntent={submitIntent}
          />
          <GenUiStudio host={host} />
        </div>
      </main>
      {notice && <div className="toast" role="status">{notice}</div>}
    </div>
  );
}
