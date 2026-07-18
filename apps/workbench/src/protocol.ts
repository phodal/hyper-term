export const BLOCK_SCHEMA_VERSION = 1;

export type BlockKind =
  | "task"
  | "message"
  | "operation"
  | "approval"
  | "terminal"
  | "review"
  | "diagnostic";

export type BlockLifecycle =
  | "draft"
  | "queued"
  | "running"
  | "waiting"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "unknown_execution";

export type AttentionState =
  | "none"
  | "waiting_input"
  | "waiting_approval"
  | "failed"
  | "review_ready";

export interface BlockEnvelope {
  schema_version: number;
  block_id: string;
  block_revision: number;
  document_revision: number;
  parent_block_id: string | null;
  order_key: number;
  task_id: string;
  kind: BlockKind;
  render_slot: "session_header" | "timeline" | "attention" | "inspector";
  trust_class:
    | "trusted_chrome"
    | "untrusted_content"
    | "trusted_workbench"
    | "isolated_artifact";
  lifecycle: BlockLifecycle;
  attention: AttentionState;
  payload: Record<string, unknown> & { type: string };
  actions: BlockAction[];
}

export interface BlockAction {
  action_id: string;
  expected_block_revision: number;
  risk: "read_only" | "workspace_write" | "external_effect" | "destructive";
  required_capabilities: string[];
}

export interface BlockDocument {
  schema_version: number;
  task_id: string;
  revision: number;
  semantic_digest: string;
  blocks: BlockEnvelope[];
}

export interface BlockPatch {
  stream_sequence: number;
  base_revision: number;
  target_revision: number;
  operations: BlockOperation[];
}

export type BlockOperation =
  | { type: "upsert"; block: BlockEnvelope }
  | {
    type: "append_content";
    block_id: string;
    expected_previous_revision: number;
    block_revision: number;
    text: string;
  }
  | { type: "remove"; block_id: string };

export type UiIntent =
  | {
    type: "decide_permission";
    task_id: string;
    operation_id: string;
    expected_revision: number;
    decision: "allow_once" | "reject_once" | "cancelled";
  }
  | {
    type: "submit_task_draft";
    task_id: string;
    base_revision: number;
    text: string;
    mode: "ask" | "run" | "drive" | "delegate";
  }
  | {
    type: "select_diff_hunks";
    task_id: string;
    base_digest: string;
    hunk_ids: string[];
  };

export interface CompileDiagnostic {
  severity: "error" | "warning";
  text: string;
  file?: string;
  line?: number;
  column?: number;
}

export interface ArtifactCandidate {
  schema_version: 1;
  source_revision: number;
  entrypoint: string;
  bundle: string;
  css: string;
  source_map: string;
  content_digest: string;
  compiler: { name: "esbuild-wasm"; version: string };
  diagnostics: CompileDiagnostic[];
}

export interface AcceptedArtifact extends ArtifactCandidate {
  artifact_id: string;
  accepted_by: "rust_host" | "demo_broker";
}
