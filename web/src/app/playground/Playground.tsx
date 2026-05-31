"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Editor } from "./components/Editor";
import { ResultsPane, type RunResult } from "./components/ResultsPane";
import { loadSqlrite, type Database, type SqlriteRow } from "./lib/wasm";
import { isRowProducing, splitStatements } from "./lib/sql";
import { DATASETS, WELCOME_SQL, findDataset } from "./lib/datasets";
import {
  buildShareUrl,
  clearAll,
  loadEditor,
  loadSession,
  readShareHash,
  saveEditor,
  saveSession,
  storageMode,
  type StorageMode,
} from "./lib/persist";

type ExecOutcome = {
  error?: { index: number; statement: string; message: string };
  lastRows?: SqlriteRow[];
  hadRows: boolean;
  mutating: string[];
  count: number;
};

function errMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

/** Runs each statement in order against `db`, collecting the mutating ones
 * (for the replay log) and the last row-producing result (for display). */
function executeAll(db: Database, statements: string[]): ExecOutcome {
  let lastRows: SqlriteRow[] | undefined;
  let hadRows = false;
  const mutating: string[] = [];
  for (let i = 0; i < statements.length; i++) {
    const s = statements[i];
    try {
      if (isRowProducing(s)) {
        lastRows = db.query(s);
        hadRows = true;
      } else {
        db.exec(s);
        mutating.push(s);
      }
    } catch (e) {
      return {
        error: { index: i, statement: s, message: errMessage(e) },
        lastRows,
        hadRows,
        mutating,
        count: i,
      };
    }
  }
  return { lastRows, hadRows, mutating, count: statements.length };
}

export function Playground() {
  const [sqlText, setSqlText] = useState<string>(WELCOME_SQL);
  const [result, setResult] = useState<RunResult>({ kind: "idle" });
  const [status, setStatus] = useState<string>("Booting the WASM engine…");
  const [ready, setReady] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<StorageMode>("none");
  const [datasetId, setDatasetId] = useState<string>("");

  const dbRef = useRef<Database | null>(null);
  const DatabaseRef = useRef<(new () => Database) | null>(null);
  const sessionLog = useRef<string[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // ---- boot: load wasm, restore session + editor --------------------------
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const { Database } = await loadSqlrite();
        if (cancelled) return;
        DatabaseRef.current = Database;
        const db = new Database();
        dbRef.current = db;

        // Replay the saved mutating-statement log into the fresh DB.
        const savedSession = await loadSession();
        let replayed = 0;
        if (savedSession) {
          const stmts = splitStatements(savedSession);
          for (const s of stmts) {
            try {
              db.exec(s);
              sessionLog.current.push(s);
              replayed++;
            } catch {
              // A previously-good statement failed on replay (shouldn't
              // happen, but never wedge the playground over it).
              break;
            }
          }
        }

        const m = await storageMode();
        if (cancelled) return;
        setMode(m);

        // Editor contents: share hash wins, then saved editor, then welcome.
        const hashSql = readShareHash(window.location.hash);
        const savedEditor = hashSql ? null : await loadEditor();
        const initial = hashSql ?? savedEditor ?? WELCOME_SQL;
        setSqlText(initial);

        setReady(true);
        setStatus(
          replayed > 0
            ? `Restored ${replayed} statement${replayed === 1 ? "" : "s"} from your last session. Ready.`
            : "Ready. Run with the button or Cmd/Ctrl+Enter.",
        );
      } catch (e) {
        if (!cancelled) setLoadError(errMessage(e));
      }
    })();
    return () => {
      cancelled = true;
      dbRef.current?.free();
      dbRef.current = null;
    };
  }, []);

  // ---- debounced editor persistence --------------------------------------
  useEffect(() => {
    if (!ready) return;
    const id = setTimeout(() => {
      void saveEditor(sqlText);
    }, 600);
    return () => clearTimeout(id);
  }, [sqlText, ready]);

  // ---- run ----------------------------------------------------------------
  const run = useCallback(async () => {
    const db = dbRef.current;
    if (!db) return;
    const statements = splitStatements(sqlText);
    if (statements.length === 0) {
      setResult({ kind: "message", text: "Nothing to run." });
      return;
    }
    const t0 = performance.now();
    const outcome = executeAll(db, statements);
    const elapsed = performance.now() - t0;

    if (outcome.mutating.length > 0) {
      sessionLog.current.push(...outcome.mutating);
      void saveSession(sessionLog.current.join(";\n") + ";");
    }
    void saveEditor(sqlText);

    if (outcome.error) {
      const { index, message } = outcome.error;
      setResult({
        kind: "error",
        text: `Statement ${index + 1} of ${statements.length} failed:\n${message}`,
      });
      setStatus(`Error at statement ${index + 1}.`);
      return;
    }

    if (outcome.hadRows && outcome.lastRows) {
      setResult({ kind: "rows", rows: outcome.lastRows, elapsedMs: elapsed });
      setStatus(
        `Ran ${statements.length} statement${statements.length === 1 ? "" : "s"} in ${elapsed.toFixed(1)} ms.`,
      );
    } else {
      setResult({
        kind: "message",
        text: `OK — ${statements.length} statement${statements.length === 1 ? "" : "s"} executed, no result set (${elapsed.toFixed(1)} ms).`,
      });
      setStatus(`Ran ${statements.length} statement${statements.length === 1 ? "" : "s"}.`);
    }
  }, [sqlText]);

  // ---- fresh DB helper ----------------------------------------------------
  const freshDb = useCallback((): Database | null => {
    const Ctor = DatabaseRef.current;
    if (!Ctor) return null;
    dbRef.current?.free();
    const db = new Ctor();
    dbRef.current = db;
    sessionLog.current = [];
    return db;
  }, []);

  // ---- load a sample dataset ---------------------------------------------
  const loadDataset = useCallback(
    (id: string) => {
      const ds = findDataset(id);
      if (!ds) return;
      const db = freshDb();
      if (!db) return;
      setDatasetId(id);

      const setupStmts = splitStatements(ds.setup);
      const t0 = performance.now();
      const setupOutcome = executeAll(db, setupStmts);
      if (setupOutcome.mutating.length > 0) {
        sessionLog.current.push(...setupOutcome.mutating);
        void saveSession(sessionLog.current.join(";\n") + ";");
      }
      if (setupOutcome.error) {
        setResult({
          kind: "error",
          text: `Failed to load "${ds.label}": ${setupOutcome.error.message}`,
        });
        return;
      }

      // Drop the sample query into the editor and run it for instant payoff.
      setSqlText(ds.sampleQuery);
      void saveEditor(ds.sampleQuery);
      const queryStmts = splitStatements(ds.sampleQuery);
      const queryOutcome = executeAll(db, queryStmts);
      const elapsed = performance.now() - t0;
      if (queryOutcome.error) {
        setResult({ kind: "error", text: queryOutcome.error.message });
      } else if (queryOutcome.hadRows && queryOutcome.lastRows) {
        setResult({
          kind: "rows",
          rows: queryOutcome.lastRows,
          elapsedMs: elapsed,
        });
      } else {
        setResult({ kind: "message", text: `Loaded "${ds.label}".` });
      }
      setStatus(`Loaded the ${ds.label} dataset. ${ds.blurb}`);
    },
    [freshDb],
  );

  // ---- reset --------------------------------------------------------------
  const reset = useCallback(async () => {
    const db = freshDb();
    if (!db) return;
    setDatasetId("");
    setSqlText(WELCOME_SQL);
    setResult({ kind: "idle" });
    await clearAll();
    setStatus("Reset — fresh in-memory database.");
  }, [freshDb]);

  // ---- share --------------------------------------------------------------
  const share = useCallback(async () => {
    const url = buildShareUrl(sqlText);
    try {
      window.history.replaceState(null, "", url);
    } catch {
      window.location.hash = url.split("#")[1] ?? "";
    }
    try {
      await navigator.clipboard.writeText(url);
      setStatus("Shareable link copied to clipboard (SQL encoded in the URL).");
    } catch {
      setStatus("Shareable link is in your address bar (copy failed — clipboard blocked).");
    }
  }, [sqlText]);

  // ---- download .sql ------------------------------------------------------
  const downloadSql = useCallback(() => {
    const body =
      sessionLog.current.length > 0
        ? sessionLog.current.join(";\n") + ";\n"
        : sqlText;
    const content =
      `-- SQLRite playground export — replay this to rebuild the database.\n` +
      `-- https://sqlritedb.com/playground\n\n` +
      body;
    const blob = new Blob([content], { type: "application/sql" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "sqlrite-session.sql";
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
    setStatus("Downloaded sqlrite-session.sql.");
  }, [sqlText]);

  // ---- upload .sql --------------------------------------------------------
  const onUploadFile = useCallback(
    async (file: File) => {
      const text = await file.text();
      const db = freshDb();
      if (!db) return;
      setDatasetId("");
      setSqlText(text);
      void saveEditor(text);
      const stmts = splitStatements(text);
      const t0 = performance.now();
      const outcome = executeAll(db, stmts);
      const elapsed = performance.now() - t0;
      if (outcome.mutating.length > 0) {
        sessionLog.current.push(...outcome.mutating);
        void saveSession(sessionLog.current.join(";\n") + ";");
      }
      if (outcome.error) {
        setResult({
          kind: "error",
          text: `Statement ${outcome.error.index + 1} failed: ${outcome.error.message}`,
        });
      } else if (outcome.hadRows && outcome.lastRows) {
        setResult({ kind: "rows", rows: outcome.lastRows, elapsedMs: elapsed });
      } else {
        setResult({
          kind: "message",
          text: `Loaded ${file.name} — ${outcome.count} statement${outcome.count === 1 ? "" : "s"}.`,
        });
      }
      setStatus(`Opened ${file.name}.`);
    },
    [freshDb],
  );

  const storageBadge =
    mode === "opfs"
      ? "saved to OPFS"
      : mode === "local"
        ? "saved to localStorage"
        : "not persisted";

  if (loadError) {
    return (
      <div className="pg-shell">
        <div className="pg-result-error" role="alert">
          <strong>Couldn&apos;t load the WASM engine</strong>
          <pre>{loadError}</pre>
          <p style={{ margin: "8px 0 0", color: "var(--color-fg-mute)" }}>
            The playground needs WebAssembly + ES modules. Try the latest
            Chrome, Firefox, or Safari.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="pg-shell">
      <div className="pg-toolbar" role="toolbar" aria-label="Playground actions">
        <button
          type="button"
          className="pg-btn-primary"
          onClick={() => void run()}
          disabled={!ready}
        >
          ▸ Run
          <span className="pg-kbd">⌘↵</span>
        </button>

        <label className="pg-field">
          <span className="pg-field-label">Sample</span>
          <select
            className="pg-select"
            value={datasetId}
            disabled={!ready}
            onChange={(e) => loadDataset(e.target.value)}
            aria-label="Load a sample dataset"
          >
            <option value="" disabled>
              Load dataset…
            </option>
            {DATASETS.map((d) => (
              <option key={d.id} value={d.id}>
                {d.label}
              </option>
            ))}
          </select>
        </label>

        <button
          type="button"
          className="pg-btn"
          onClick={() => void reset()}
          disabled={!ready}
        >
          Reset DB
        </button>
        <button
          type="button"
          className="pg-btn"
          onClick={() => void share()}
          disabled={!ready}
        >
          Share
        </button>
        <button
          type="button"
          className="pg-btn"
          onClick={downloadSql}
          disabled={!ready}
        >
          Download .sql
        </button>
        <button
          type="button"
          className="pg-btn"
          onClick={() => fileInputRef.current?.click()}
          disabled={!ready}
        >
          Upload .sql
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept=".sql,text/plain,application/sql"
          style={{ display: "none" }}
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void onUploadFile(f);
            e.target.value = "";
          }}
        />

        <span className="pg-storage-badge" title="Where your session is saved">
          {storageBadge}
        </span>
      </div>

      <p className="pg-status" aria-live="polite">
        {status}
      </p>

      <div className="pg-panes">
        <section className="pg-pane pg-pane-editor" aria-label="SQL editor">
          <Editor value={sqlText} onChange={setSqlText} onRun={() => void run()} />
        </section>
        <section className="pg-pane pg-pane-results" aria-label="Results">
          <ResultsPane result={result} />
        </section>
      </div>
    </div>
  );
}
