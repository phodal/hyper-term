import { useEffect, useRef } from "react";
import { MergeView } from "@codemirror/merge";
import { javascript } from "@codemirror/lang-javascript";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { basicSetup } from "codemirror";

interface CodeDiffProps {
  original: string;
  modified: string;
  onChange(value: string): void;
  readOnlyModified?: boolean;
}

export function CodeDiff({
  original,
  modified,
  onChange,
  readOnlyModified = false,
}: CodeDiffProps) {
  const parent = useRef<HTMLDivElement>(null);
  const mergeRef = useRef<MergeView | undefined>(undefined);
  const onChangeRef = useRef(onChange);
  const modifiedRef = useRef(modified);
  onChangeRef.current = onChange;
  modifiedRef.current = modified;

  useEffect(() => {
    if (!parent.current) return;
    const merge = new MergeView({
      parent: parent.current,
      orientation: "a-b",
      highlightChanges: true,
      gutter: true,
      collapseUnchanged: { margin: 3, minSize: 6 },
      a: {
        doc: original,
        extensions: [
          basicSetup,
          javascript({ jsx: true, typescript: true }),
          EditorState.readOnly.of(true),
          EditorView.editable.of(false),
        ],
      },
      b: {
        doc: modifiedRef.current,
        extensions: [
          basicSetup,
          javascript({ jsx: true, typescript: true }),
          ...(readOnlyModified
            ? [
              EditorState.readOnly.of(true),
              EditorView.editable.of(false),
            ]
            : []),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              onChangeRef.current(update.state.doc.toString());
            }
          }),
        ],
      },
    });
    mergeRef.current = merge;
    return () => {
      mergeRef.current = undefined;
      merge.destroy();
    };
  }, [original, readOnlyModified]);

  useEffect(() => {
    const view = mergeRef.current?.b;
    if (!view || view.state.doc.toString() === modified) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: modified },
    });
  }, [modified]);

  return <div className="code-diff" ref={parent} />;
}
