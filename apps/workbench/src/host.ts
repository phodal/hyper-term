import type {
  AcceptedArtifact,
  ArtifactCandidate,
  UiIntent,
} from "./protocol.ts";

export interface HyperTermHost {
  readonly authority: "rust_host" | "demo_broker";
  acceptArtifact(candidate: ArtifactCandidate): Promise<AcceptedArtifact>;
  submitIntent(intent: UiIntent): Promise<void>;
}

declare global {
  interface Window {
    hyperTermHost?: HyperTermHost;
  }
}

export function resolveHost(): HyperTermHost {
  return globalThis.window?.hyperTermHost ?? new DemoBroker();
}

class DemoBroker implements HyperTermHost {
  readonly authority = "demo_broker" as const;

  async acceptArtifact(
    candidate: ArtifactCandidate,
  ): Promise<AcceptedArtifact> {
    if (candidate.schema_version !== 1 || candidate.bundle.length > 2_000_000) {
      throw new Error("artifact candidate violates the demo broker bounds");
    }
    const digest = await sha256(candidate.bundle + candidate.css);
    if (digest !== candidate.content_digest) {
      throw new Error("artifact candidate digest mismatch");
    }
    return {
      ...candidate,
      artifact_id: `demo:${digest.slice(0, 24)}`,
      accepted_by: "demo_broker",
    };
  }

  submitIntent(intent: UiIntent): Promise<void> {
    console.info("Demo broker received an inert UiIntent", intent);
    return Promise.resolve();
  }
}

async function sha256(value: string): Promise<string> {
  const bytes = new TextEncoder().encode(value);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
