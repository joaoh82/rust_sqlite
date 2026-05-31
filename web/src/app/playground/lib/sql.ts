// Small SQL utilities for the browser playground: split a multi-statement
// script into individual statements, classify SELECTs, and render results
// as CSV. None of this parses SQL semantically — the engine does the real
// parsing. We only need to (a) chop on top-level `;` and (b) know whether
// to call `db.query` (row-producing) or `db.exec`.

import type { SqlriteRow } from "./wasm";

/**
 * Splits a script into individual statements on top-level semicolons,
 * skipping `;` that appear inside string literals (`'...'`, `"..."`),
 * line comments (`-- ...`), and block comments (`/* ... *​/`). Returns
 * trimmed, non-empty statements with their terminating `;` removed.
 *
 * This is deliberately a lexer, not a parser: vector literals (`[0.1,
 * 0.2]`) and the like contain no `;`, so a quote/comment-aware scan is
 * enough to keep dataset scripts intact.
 */
export function splitStatements(script: string): string[] {
  const out: string[] = [];
  let buf = "";
  let i = 0;
  const n = script.length;

  while (i < n) {
    const c = script[i];
    const next = script[i + 1];

    // Line comment — consume to end of line (keep it in buf so the engine
    // sees a faithful statement, but never split on a `;` inside it).
    if (c === "-" && next === "-") {
      while (i < n && script[i] !== "\n") {
        buf += script[i];
        i++;
      }
      continue;
    }
    // Block comment.
    if (c === "/" && next === "*") {
      buf += c;
      buf += next;
      i += 2;
      while (i < n && !(script[i] === "*" && script[i + 1] === "/")) {
        buf += script[i];
        i++;
      }
      if (i < n) {
        buf += "*/";
        i += 2;
      }
      continue;
    }
    // String literals — single or double quoted. SQL escapes a quote by
    // doubling it (`''`), which this handles naturally: the closing quote
    // is consumed, then the next char (another quote) re-opens.
    if (c === "'" || c === '"') {
      const quote = c;
      buf += c;
      i++;
      while (i < n) {
        buf += script[i];
        if (script[i] === quote) {
          i++;
          break;
        }
        i++;
      }
      continue;
    }
    if (c === ";") {
      const trimmed = buf.trim();
      if (trimmed.length > 0) out.push(trimmed);
      buf = "";
      i++;
      continue;
    }
    buf += c;
    i++;
  }

  const tail = buf.trim();
  if (tail.length > 0) out.push(tail);
  return out;
}

/**
 * True when a statement produces a result set and should go through
 * `db.query` rather than `db.exec`. SQLRite's row-producing statements
 * are `SELECT` and `WITH … SELECT`; everything else (DDL/DML/tx control)
 * is a no-rows `exec`.
 */
export function isRowProducing(statement: string): boolean {
  // Skip leading comments / whitespace before sniffing the keyword.
  const stripped = statement
    .replace(/^\s*(--[^\n]*\n|\/\*[\s\S]*?\*\/|\s)+/g, "")
    .trimStart();
  return /^(select|with)\b/i.test(stripped);
}

/** Quotes a single CSV cell per RFC 4180 when it contains `, " \n \r`. */
function csvCell(value: string | number | boolean | null): string {
  if (value === null) return "";
  const s = String(value);
  if (/[",\n\r]/.test(s)) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

/** Renders a result set as an RFC-4180 CSV string (header + rows). */
export function toCSV(columns: string[], rows: SqlriteRow[]): string {
  const lines: string[] = [];
  lines.push(columns.map(csvCell).join(","));
  for (const row of rows) {
    lines.push(columns.map((col) => csvCell(row[col] ?? null)).join(","));
  }
  return lines.join("\r\n");
}

/** Column names for a result set, preserving projection order. */
export function columnsOf(rows: SqlriteRow[]): string[] {
  if (rows.length === 0) return [];
  return Object.keys(rows[0]);
}

/** Human label for a JS-side cell type, shown in the results header. */
export function cellType(value: string | number | boolean | null): string {
  if (value === null) return "null";
  switch (typeof value) {
    case "number":
      return Number.isInteger(value) ? "int" : "real";
    case "boolean":
      return "bool";
    default:
      return "text";
  }
}
