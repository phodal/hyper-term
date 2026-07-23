import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app.tsx";
import {
  applyWorkbenchColorScheme,
  workbenchColorScheme,
} from "./workbench-theme.ts";
import "./styles.css";
import "./visual-quality.css";
import "./capsule.css";
import "./responsive.css";

const root = document.getElementById("root");
if (!root) throw new Error("workbench root is missing");
document.documentElement.dataset.surface =
  new URLSearchParams(globalThis.location.search).get("surface") ?? "demo";
const appearanceQuery = globalThis.matchMedia("(prefers-color-scheme: light)");
applyWorkbenchColorScheme(
  document.documentElement,
  workbenchColorScheme(appearanceQuery.matches),
);
appearanceQuery.addEventListener("change", (event) => {
  applyWorkbenchColorScheme(
    document.documentElement,
    workbenchColorScheme(event.matches),
  );
});

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
