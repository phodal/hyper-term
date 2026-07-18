import type { BlockDocument, BlockEnvelope, UiIntent } from "../protocol.ts";

interface BlockWorkbenchProps {
  document: BlockDocument;
  submitIntent(intent: UiIntent): Promise<void>;
}

export function BlockWorkbench(
  { document, submitIntent }: BlockWorkbenchProps,
) {
  return (
    <section className="timeline" aria-label="Terminal transcript">
      <header className="terminal-chrome">
        <span className="terminal-lights" aria-hidden="true">● ● ●</span>
        <div className="session-tabs">
          <button className="session-tab active" type="button">
            <span>⌁</span> zsh — hyper-term <small>●</small>
          </button>
          <button
            className="new-session"
            type="button"
            aria-label="New session"
          >
            ＋
          </button>
        </div>
        <code>~/ai/hyper-term</code>
        <span className="pty-status">PTY LIVE · seq 18</span>
      </header>
      <div className="terminal-task-bar">
        <span className="agent-chip">CODEX</span>
        <strong>
          {String(document.blocks[0]?.payload.title ?? "Untitled task")}
        </strong>
        <span>agent attached</span>
        <span className="revision-chip">ledger rev {document.revision}</span>
      </div>
      <div className="timeline-stream">
        <TerminalPrelude />
        {document.blocks.slice(1).map((block) => (
          <BlockCard
            key={block.block_id}
            block={block}
            submitIntent={submitIntent}
          />
        ))}
      </div>
      <Composer
        taskId={document.task_id}
        revision={document.revision}
        submitIntent={submitIntent}
      />
    </section>
  );
}

function TerminalPrelude() {
  return (
    <div className="terminal-prelude" aria-label="Shell transcript">
      <p>
        <span>12:41:52</span> Last login: Sat Jul 18 on ttys004
      </p>
      <p>
        <span>12:42:01</span> <b>phodal@studio</b> <i>hyper-term</i>{" "}
        % codex --acp
      </p>
      <p className="agent-attach">
        <span>12:42:02</span>{" "}
        ╭─ agent connected · codex · workspace write · context 18.4k
      </p>
      <p className="agent-attach">
        <span>12:42:02</span>{" "}
        ╰─ transcript journal enabled · effects require approval
      </p>
    </div>
  );
}

function BlockCard({
  block,
  submitIntent,
}: {
  block: BlockEnvelope;
  submitIntent(intent: UiIntent): Promise<void>;
}) {
  const payload = block.payload;
  const receiptOutcome = payload.type === "operation_receipt"
    ? String(
      payload.outcome ?? (payload.succeeded ? "succeeded" : "failed"),
    )
    : "";
  return (
    <article
      className={`block-card block-${block.kind}`}
      data-state={block.lifecycle}
    >
      <div className="block-rail">
        <span className="block-dot" />
        <span className="block-line" />
      </div>
      <div className="block-body">
        <header className="block-meta">
          <span>{labelFor(block.kind)}</span>
          <span className={`state state-${block.lifecycle}`}>
            {block.lifecycle}
          </span>
        </header>
        {payload.type === "message" && <p>{String(payload.text)}</p>}
        {payload.type === "operation" && (
          <>
            <strong>{String(payload.summary)}</strong>
            <div className="command-line">
              <span>❯</span> {String(payload.summary)}
            </div>
            <div className="capability-row">
              <span>risk · {String(payload.risk)}</span>
              <span>revision locked</span>
            </div>
          </>
        )}
        {payload.type === "approval" && (
          <div className="approval-panel">
            <div>
              <strong>{String(payload.prompt)}</strong>
              <p>
                Exact operation · revision {String(payload.operation_revision)}
              </p>
            </div>
            <div className="approval-actions">
              <button
                className="button-primary"
                type="button"
                onClick={() =>
                  void submitIntent({
                    type: "decide_permission",
                    task_id: block.task_id,
                    operation_id: String(payload.operation_id),
                    expected_revision: Number(payload.operation_revision),
                    decision: "allow_once",
                  })}
              >
                Allow once
              </button>
              <button type="button">Reject</button>
            </div>
          </div>
        )}
        {payload.type === "operation_receipt" && (
          <div className="receipt-panel" data-outcome={receiptOutcome}>
            <span className="receipt-icon">
              {receiptOutcome === "succeeded"
                ? "✓"
                : receiptOutcome === "unknown_execution"
                ? "?"
                : "!"}
            </span>
            <div>
              <strong>{String(payload.summary)}</strong>
              <p>
                {String(payload.executor)} · operation rev{" "}
                {String(payload.operation_revision)}
              </p>
              {receiptOutcome === "unknown_execution" && (
                <p className="receipt-warning">
                  Outcome unknown · review evidence before retrying
                </p>
              )}
              {typeof payload.result_digest === "string" && (
                <code>{payload.result_digest.slice(0, 16)}…</code>
              )}
            </div>
          </div>
        )}
        {payload.type === "artifact" && (
          <div className="receipt-panel">
            <span className="receipt-icon">◇</span>
            <div>
              <strong>Accepted isolated artifact</strong>
              <p>
                source r{String(
                  (payload.artifact as Record<string, unknown>)
                    ?.source_revision,
                )}
              </p>
              <code>
                {String(
                  (payload.artifact as Record<string, unknown>)
                    ?.content_digest,
                ).slice(0, 16)}…
              </code>
            </div>
          </div>
        )}
        {payload.type === "terminal" && (
          <div className="terminal-surface">
            <div className="terminal-title">
              <span className="terminal-lights">● ● ●</span>
              <span>{String(payload.command)}</span>
              <span>seq {String(payload.stream_sequence)}</span>
            </div>
            <pre><span className="dim">running 19 tests</span><br /><span className="ok">test result: ok.</span> 19 passed; 0 failed<br /><span className="prompt">hyper-term %</span></pre>
          </div>
        )}
        {payload.type === "review" && (
          <div className="review-ready">
            <span className="review-icon">✓</span>
            <div>
              <strong>Review ready</strong>
              <p>{String(payload.summary)}</p>
            </div>
            <button type="button">Open evidence</button>
          </div>
        )}
      </div>
    </article>
  );
}

function Composer({
  taskId,
  revision,
  submitIntent,
}: {
  taskId: string;
  revision: number;
  submitIntent(intent: UiIntent): Promise<void>;
}) {
  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault();
        const form = new FormData(event.currentTarget);
        const text = String(form.get("brief") ?? "").trim();
        if (!text) return;
        void submitIntent({
          type: "submit_task_draft",
          task_id: taskId,
          base_revision: revision,
          text,
          mode: "delegate",
        });
      }}
    >
      <div className="composer-tools">
        <span className="mode-chip">AGENT⌄</span>
        <button type="button" aria-label="Attach context">＋</button>
        <button type="button" aria-label="Push to talk">◉</button>
      </div>
      <textarea
        name="brief"
        rows={2}
        placeholder="Ask the agent, steer the task, or type /shell…"
      />
      <button className="send-button" type="submit" aria-label="Send intent">
        ↗
      </button>
    </form>
  );
}

function labelFor(kind: BlockEnvelope["kind"]): string {
  return ({
    task: "Task",
    message: "Agent",
    operation: "Operation",
    approval: "Attention required",
    receipt: "Execution receipt",
    artifact: "Agentic UI artifact",
    terminal: "Terminal",
    review: "Evidence",
    diagnostic: "Diagnostic",
  } as const)[kind];
}
