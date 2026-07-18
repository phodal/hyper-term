import { useEffect, useRef } from "react";
import { basicSetup, EditorView } from "codemirror";
import { javascript } from "@codemirror/lang-javascript";
import { EditorState, StateEffect, StateField } from "@codemirror/state";
import { Decoration } from "@codemirror/view";

export interface EditorLocation {
  line: number;
  column: number;
}

interface CodeEditorProps {
  value: string;
  onChange(value: string): void;
  readOnly?: boolean;
  revealLocation?: EditorLocation;
  revealRequest?: number;
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
});

export function CodeEditor(
  {
    value,
    onChange,
    readOnly = false,
    revealLocation,
    revealRequest = 0,
  }: CodeEditorProps,
) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    if (!parent.current) return;
    const next = new EditorView({
      parent: parent.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          basicSetup,
          javascript({ jsx: true, typescript: true }),
          editorTheme,
          runtimeErrorLine,
          EditorState.readOnly.of(readOnly),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              onChangeRef.current(update.state.doc.toString());
            }
          }),
        ],
      }),
    });
    view.current = next;
    return () => {
      view.current = null;
      next.destroy();
    };
  }, [readOnly]);

  useEffect(() => {
    const current = view.current;
    if (!current || current.state.doc.toString() === value) return;
    current.dispatch({
      changes: { from: 0, to: current.state.doc.length, insert: value },
    });
  }, [value]);

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
