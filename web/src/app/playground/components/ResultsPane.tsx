"use client";

import { useMemo } from "react";
import type { SqlriteRow } from "../lib/wasm";
import { cellType, columnsOf, toCSV } from "../lib/sql";

export type RunResult =
  | { kind: "idle" }
  | { kind: "message"; text: string }
  | { kind: "error"; text: string }
  | { kind: "rows"; rows: SqlriteRow[]; elapsedMs: number };

const mono =
  "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace";

function download(filename: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function ResultsPane({ result }: { result: RunResult }) {
  const columns = useMemo(
    () => (result.kind === "rows" ? columnsOf(result.rows) : []),
    [result],
  );

  if (result.kind === "idle") {
    return (
      <div className="pg-result-empty" aria-live="polite">
        Run a query to see results here.
      </div>
    );
  }

  if (result.kind === "error") {
    return (
      <div className="pg-result-error" role="alert">
        <strong>Error</strong>
        <pre>{result.text}</pre>
      </div>
    );
  }

  if (result.kind === "message") {
    return (
      <div className="pg-result-message" aria-live="polite">
        {result.text}
      </div>
    );
  }

  const { rows, elapsedMs } = result;

  if (rows.length === 0) {
    return (
      <div className="pg-result-message" aria-live="polite">
        Query OK — 0 rows ({elapsedMs.toFixed(1)} ms).
      </div>
    );
  }

  // Infer per-column type from the first non-null value, for the header chip.
  const colTypes = columns.map((col) => {
    const sample = rows.find((r) => r[col] !== null);
    return sample ? cellType(sample[col]) : "null";
  });

  return (
    <div className="pg-result-rows">
      <div className="pg-result-toolbar">
        <span className="pg-result-meta" aria-live="polite">
          {rows.length} row{rows.length === 1 ? "" : "s"} ·{" "}
          {columns.length} column{columns.length === 1 ? "" : "s"} ·{" "}
          {elapsedMs.toFixed(1)} ms
        </span>
        <button
          type="button"
          className="pg-btn-ghost"
          onClick={() =>
            download("sqlrite-result.csv", toCSV(columns, rows), "text/csv")
          }
        >
          Export CSV
        </button>
      </div>
      <div className="pg-table-scroll" tabIndex={0} aria-label="Query results">
        <table className="pg-table">
          <thead>
            <tr>
              <th className="pg-th-idx" scope="col">
                #
              </th>
              {columns.map((col, i) => (
                <th key={col} scope="col">
                  <span className="pg-col-name">{col}</span>
                  <span className="pg-col-type" style={{ fontFamily: mono }}>
                    {colTypes[i]}
                  </span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, ri) => (
              <tr key={ri}>
                <td className="pg-td-idx">{ri + 1}</td>
                {columns.map((col) => {
                  const v = row[col];
                  if (v === null) {
                    return (
                      <td key={col} className="pg-cell-null">
                        NULL
                      </td>
                    );
                  }
                  return (
                    <td
                      key={col}
                      className={
                        typeof v === "number"
                          ? "pg-cell-num"
                          : typeof v === "boolean"
                            ? "pg-cell-bool"
                            : "pg-cell-text"
                      }
                    >
                      {String(v)}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
