import { useEffect, useRef } from "react";
import { basicSetup, EditorView } from "codemirror";
import { javascript } from "@codemirror/lang-javascript";
import { EditorState } from "@codemirror/state";

interface CodeEditorProps {
  value: string;
  onChange(value: string): void;
  readOnly?: boolean;
}

const editorTheme = EditorView.theme({
  "&": { height: "100%", background: "transparent", color: "#e9ebdf" },
  ".cm-content": { padding: "16px 0", caretColor: "#d7ff72" },
  ".cm-gutters": {
    background: "transparent",
    color: "#68705d",
    border: "none",
  },
  ".cm-activeLine": { background: "rgba(215, 255, 114, .035)" },
  ".cm-activeLineGutter": { background: "transparent", color: "#b7c190" },
  ".cm-selectionBackground, ::selection": {
    background: "rgba(167, 215, 83, .22) !important",
  },
  ".cm-scroller": {
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  },
});

export function CodeEditor(
  { value, onChange, readOnly = false }: CodeEditorProps,
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

  return <div className="code-editor" ref={parent} />;
}
