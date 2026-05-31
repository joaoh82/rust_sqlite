"use client";

import { useEffect, useRef } from "react";
import { basicSetup } from "codemirror";
import { sql, SQLite } from "@codemirror/lang-sql";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

// Dark theme tuned to the site palette (web/src/app/globals.css). CodeMirror
// themes are plain CSS-in-JS, so the values mirror the `--color-*` tokens
// with concrete hex/oklch so they render identically inside the editor's
// shadow of style scoping.
const sqlriteTheme = EditorView.theme(
  {
    "&": {
      color: "#e7e9ec",
      backgroundColor: "#0b0c0e",
      fontSize: "13.5px",
      height: "100%",
    },
    ".cm-content": {
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
      caretColor: "oklch(0.78 0.14 40)",
      padding: "12px 0",
    },
    ".cm-cursor, .cm-dropCursor": {
      borderLeftColor: "oklch(0.78 0.14 40)",
    },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      {
        backgroundColor: "oklch(0.78 0.14 40 / 0.18)",
      },
    ".cm-gutters": {
      backgroundColor: "#0b0c0e",
      color: "#6b7079",
      border: "none",
      borderRight: "1px solid #16191d",
    },
    ".cm-activeLine": { backgroundColor: "oklch(0.78 0.14 40 / 0.05)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent", color: "#9aa0a8" },
    ".cm-lineNumbers .cm-gutterElement": { padding: "0 12px 0 8px" },
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": { lineHeight: "1.6" },
  },
  { dark: true },
);

const sqlriteHighlight = HighlightStyle.define([
  { tag: [t.keyword, t.operatorKeyword], color: "oklch(0.82 0.12 280)" },
  { tag: [t.string, t.special(t.string)], color: "oklch(0.82 0.12 145)" },
  { tag: [t.number, t.bool, t.null], color: "oklch(0.82 0.12 60)" },
  { tag: [t.lineComment, t.blockComment], color: "#6b7079", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "oklch(0.78 0.10 230)" },
  { tag: [t.typeName, t.className], color: "oklch(0.78 0.13 155)" },
  { tag: t.punctuation, color: "#9aa0a8" },
]);

type EditorProps = {
  value: string;
  onChange: (next: string) => void;
  /** Fires on Cmd/Ctrl+Enter. */
  onRun: () => void;
};

export function Editor({ value, onChange, onRun }: EditorProps) {
  const parentRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  // Keep the latest callbacks reachable from the (mount-once) extensions
  // without rebuilding the editor on every render.
  const onRunRef = useRef(onRun);
  const onChangeRef = useRef(onChange);
  onRunRef.current = onRun;
  onChangeRef.current = onChange;

  useEffect(() => {
    if (!parentRef.current) return;

    const runKeymap = Prec.highest(
      keymap.of([
        {
          key: "Mod-Enter",
          preventDefault: true,
          run: () => {
            onRunRef.current();
            return true;
          },
        },
      ]),
    );

    const view = new EditorView({
      parent: parentRef.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          basicSetup,
          sql({ dialect: SQLite, upperCaseKeywords: false }),
          sqlriteTheme,
          syntaxHighlighting(sqlriteHighlight),
          runKeymap,
          EditorView.lineWrapping,
          EditorView.updateListener.of((u) => {
            if (u.docChanged) onChangeRef.current(u.state.doc.toString());
          }),
        ],
      }),
    });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // Mount once; external value changes are reconciled below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Reconcile programmatic value changes (dataset load, share-hash, reset).
  // When the change originated from typing, `value` already equals the doc,
  // so this is a no-op and there's no feedback loop.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const current = view.state.doc.toString();
    if (value !== current) {
      view.dispatch({
        changes: { from: 0, to: current.length, insert: value },
      });
    }
  }, [value]);

  return (
    <div
      ref={parentRef}
      className="pg-editor"
      role="textbox"
      aria-label="SQL editor"
      aria-multiline="true"
    />
  );
}
