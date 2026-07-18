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
}

export function CodeDiff({ original, modified, onChange }: CodeDiffProps) {
  const parent = useRef<HTMLDivElement>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

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
        doc: modified,
        extensions: [
          basicSetup,
          javascript({ jsx: true, typescript: true }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              onChangeRef.current(update.state.doc.toString());
            }
          }),
        ],
      },
    });
    return () => merge.destroy();
  }, [original]);

  return <div className="code-diff" ref={parent} />;
}
