import { useEffect, useRef } from "react";
import { basicSetup, EditorView } from "codemirror";
import {
  autocompletion,
  type CompletionContext,
  type CompletionResult,
} from "@codemirror/autocomplete";
import { javascript } from "@codemirror/lang-javascript";
import {
  EditorSelection,
  EditorState,
  type Extension,
  StateEffect,
  StateField,
  type Text,
} from "@codemirror/state";
import {
  type Diagnostic as CodeMirrorDiagnostic,
  linter,
  lintGutter,
} from "@codemirror/lint";
import { Decoration } from "@codemirror/view";
import type {
  EditorCompletion,
  EditorLanguageService,
  EditorPosition,
} from "../editor-language-service.ts";

export interface EditorLocation {
  line: number;
  column: number;
}

interface CodeEditorProps {
  value: string;
  documentPath: string;
  draftFiles: Readonly<Record<string, string>>;
  onChange(value: string): void;
  readOnly?: boolean;
  revealLocation?: EditorLocation;
  revealRequest?: number;
  languageService?: EditorLanguageService;
  onLanguageStatus?(status: "idle" | "checking" | "ready" | "failed"): void;
  selection?: { anchor: number; head: number };
  onSelectionChange?(selection: { anchor: number; head: number }): void;
}

const setRuntimeErrorLine = StateEffect.define<number | null>();
const runtimeErrorLine = StateField.define({
  create: () => Decoration.none,
  update(lines, transaction) {
    let next = lines.map(transaction.changes);
    for (const effect of transaction.effects) {
      if (!effect.is(setRuntimeErrorLine)) continue;
      next = effect.value === null ? Decoration.none : Decoration.set([
        Decoration.line({ class: "cm-runtime-error-line" }).range(
          transaction.state.doc.line(effect.value).from,
        ),
      ]);
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const editorTheme = EditorView.theme({
  "&": { height: "100%", background: "transparent", color: "#e9ebdf" },
  ".cm-content": { padding: "16px 0", caretColor: "#d7ff72" },
  ".cm-gutters": {
    background: "transparent",
    color: "#68705d",
    border: "none",
  },
  ".cm-activeLine": { background: "rgba(215, 255, 114, .035)" },
  ".cm-runtime-error-line": {
    background: "rgba(255, 141, 131, .09)",
    boxShadow: "inset 2px 0 #ff8d83",
  },
  ".cm-activeLineGutter": { background: "transparent", color: "#b7c190" },
  ".cm-selectionBackground, ::selection": {
    background: "rgba(167, 215, 83, .22) !important",
  },
  ".cm-scroller": {
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  },
  ".cm-tooltip": {
    border: "1px solid #465238",
    borderRadius: "6px",
    background: "#171b14",
    color: "#e9ebdf",
    boxShadow: "0 12px 32px rgba(0, 0, 0, .48)",
  },
  ".cm-tooltip-autocomplete > ul": {
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  },
  ".cm-tooltip-autocomplete > ul > li": {
    color: "#dce4d2",
  },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
    background: "#344525",
    color: "#f5ffd8",
  },
  ".cm-completionDetail": { color: "#89917e" },
  ".cm-tooltip-lint .cm-diagnostic": {
    borderLeftColor: "#ff8d83",
    color: "#f2d5d1",
  },
  ".cm-tooltip-lint .cm-diagnosticSource": { color: "#a8b29a" },
});

export function CodeEditor(
  {
    value,
    documentPath,
    draftFiles,
    onChange,
    readOnly = false,
    revealLocation,
    revealRequest = 0,
    languageService,
    onLanguageStatus,
    selection,
    onSelectionChange,
  }: CodeEditorProps,
) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const onLanguageStatusRef = useRef(onLanguageStatus);
  onLanguageStatusRef.current = onLanguageStatus;
  const onSelectionChangeRef = useRef(onSelectionChange);
  onSelectionChangeRef.current = onSelectionChange;
  const draftFilesRef = useRef(draftFiles);
  draftFilesRef.current = draftFiles;

  useEffect(() => {
    if (!parent.current) return;
    const languageExtensions: Extension[] = [];
    if (languageService) {
      languageExtensions.push(
        lintGutter(),
        linter(async (editor) => {
          onLanguageStatusRef.current?.("checking");
          try {
            const diagnostics = await languageService.diagnostics(
              {
                ...draftFilesRef.current,
                [documentPath]: editor.state.doc.toString(),
              },
            );
            onLanguageStatusRef.current?.("ready");
            return diagnostics.map((diagnostic) => ({
              from: editorOffset(editor.state.doc, diagnostic.start),
              to: Math.max(
                editorOffset(editor.state.doc, diagnostic.start),
                editorOffset(editor.state.doc, diagnostic.end),
              ),
              severity: diagnostic.severity === "information"
                ? "info"
                : diagnostic.severity,
              message: diagnostic.message,
              source: "Deno LSP",
            } satisfies CodeMirrorDiagnostic));
          } catch (error) {
            if (error instanceof DOMException && error.name === "AbortError") {
              return [];
            }
            onLanguageStatusRef.current?.("failed");
            return [];
          }
        }, { delay: 420 }),
        autocompletion({
          override: [
            languageCompletionSource(
              languageService,
              documentPath,
              () => draftFilesRef.current,
            ),
          ],
          activateOnTyping: true,
        }),
      );
    }
    const next = new EditorView({
      parent: parent.current,
      state: EditorState.create({
        doc: value,
        selection: selection && selection.anchor <= value.length &&
            selection.head <= value.length
          ? EditorSelection.single(selection.anchor, selection.head)
          : undefined,
        extensions: [
          basicSetup,
          javascript({ jsx: true, typescript: true }),
          editorTheme,
          runtimeErrorLine,
          EditorView.contentAttributes.of({
            "aria-label": `Artifact source ${documentPath}`,
          }),
          EditorState.readOnly.of(readOnly),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              onChangeRef.current(update.state.doc.toString());
            }
            if (update.docChanged || update.selectionSet) {
              const main = update.state.selection.main;
              onSelectionChangeRef.current?.({
                anchor: main.anchor,
                head: main.head,
              });
            }
          }),
          ...languageExtensions,
        ],
      }),
    });
    view.current = next;
    return () => {
      view.current = null;
      next.destroy();
    };
  }, [documentPath, languageService, readOnly]);

  useEffect(() => {
    const current = view.current;
    if (!current || current.state.doc.toString() === value) return;
    current.dispatch({
      changes: { from: 0, to: current.state.doc.length, insert: value },
    });
  }, [value]);

  useEffect(() => {
    const current = view.current;
    if (
      !current || !selection || selection.anchor > current.state.doc.length ||
      selection.head > current.state.doc.length
    ) return;
    const main = current.state.selection.main;
    if (main.anchor === selection.anchor && main.head === selection.head) {
      return;
    }
    current.dispatch({
      selection: EditorSelection.single(selection.anchor, selection.head),
    });
  }, [selection?.anchor, selection?.head]);

  useEffect(() => {
    const current = view.current;
    if (!current) return;
    if (!revealLocation) {
      current.dispatch({ effects: setRuntimeErrorLine.of(null) });
      return;
    }
    const lineNumber = Math.min(
      Math.max(1, revealLocation.line),
      current.state.doc.lines,
    );
    const line = current.state.doc.line(lineNumber);
    const column = Math.min(
      Math.max(0, revealLocation.column - 1),
      line.length,
    );
    current.dispatch({
      selection: { anchor: line.from + column },
      effects: setRuntimeErrorLine.of(lineNumber),
      scrollIntoView: true,
    });
    current.focus();
  }, [revealLocation?.line, revealLocation?.column, revealRequest]);

  return <div className="code-editor" ref={parent} />;
}

function languageCompletionSource(
  languageService: EditorLanguageService,
  documentPath: string,
  readDraftFiles: () => Readonly<Record<string, string>>,
) {
  return async (
    context: CompletionContext,
  ): Promise<CompletionResult | null> => {
    const token = context.matchBefore(/[\w$]*/);
    const memberTrigger = context.pos > 0 &&
      context.state.doc.sliceString(context.pos - 1, context.pos) === ".";
    if (
      !context.explicit && !memberTrigger &&
      (!token || token.from === token.to)
    ) return null;
    const line = context.state.doc.lineAt(context.pos);
    const controller = new AbortController();
    context.addEventListener("abort", () => controller.abort(), {
      onDocChange: true,
    });
    const completions = await languageService.completions(
      {
        ...readDraftFiles(),
        [documentPath]: context.state.doc.toString(),
      },
      { line: line.number - 1, character: context.pos - line.from },
      controller.signal,
    );
    return {
      from: token?.from ?? context.pos,
      options: completions.map(toCodeMirrorCompletion),
      validFor: /^[\w$]*$/,
    };
  };
}

function toCodeMirrorCompletion(completion: EditorCompletion) {
  return {
    label: completion.label,
    apply: completion.insert_text,
    detail: completion.detail,
    type: completionType(completion.kind),
  };
}

function completionType(kind?: number): string | undefined {
  switch (kind) {
    case 2:
      return "method";
    case 3:
      return "function";
    case 4:
      return "class";
    case 5:
      return "property";
    case 6:
      return "variable";
    case 7:
      return "class";
    case 8:
      return "interface";
    case 9:
      return "namespace";
    case 10:
      return "property";
    case 14:
      return "keyword";
    case 15:
      return "text";
    default:
      return undefined;
  }
}

function editorOffset(document: Text, position: EditorPosition): number {
  const lineNumber = Math.min(Math.max(1, position.line + 1), document.lines);
  const line = document.line(lineNumber);
  return line.from + Math.min(Math.max(0, position.character), line.length);
}
