"use client";

import { useEffect, useRef, useState } from "react";
import { SITE } from "@/lib/site";

type Line =
  | { type: "in"; text: string }
  | { type: "out"; text: string; cls?: "cmt" | "tbl" | "ok" | "dim" };

const SCRIPT: Line[] = [
  { type: "out", text: `SQLRite — ${SITE.version}` },
  {
    type: "out",
    text: "Connected to a transient in-memory database.",
    cls: "cmt",
  },
  {
    type: "in",
    text: "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);",
  },
  { type: "out", text: "ok" },
  { type: "in", text: "INSERT INTO users (name, age) VALUES ('alice', 30);" },
  { type: "out", text: "1 row inserted." },
  { type: "in", text: "INSERT INTO users (name, age) VALUES ('bob', 25);" },
  { type: "out", text: "1 row inserted." },
  {
    type: "in",
    text: "SELECT name FROM users WHERE age > 25 ORDER BY age DESC;",
  },
  {
    type: "out",
    text: "+-------+\n| name  |\n+-------+\n| alice |\n+-------+",
    cls: "tbl",
  },
  { type: "out", text: "SELECT executed. 1 row returned.", cls: "ok" },
  { type: "in", text: "BEGIN;" },
  { type: "in", text: "UPDATE users SET age = age + 1 WHERE name = 'bob';" },
  { type: "in", text: "COMMIT;" },
  {
    type: "out",
    text: "transaction committed (WAL frame #4127)",
    cls: "cmt",
  },
];

const KEYWORDS = new Set([
  "CREATE",
  "TABLE",
  "INSERT",
  "INTO",
  "VALUES",
  "SELECT",
  "FROM",
  "WHERE",
  "ORDER",
  "BY",
  "DESC",
  "ASC",
  "LIMIT",
  "UPDATE",
  "SET",
  "DELETE",
  "BEGIN",
  "COMMIT",
  "ROLLBACK",
  "PRIMARY",
  "KEY",
  "UNIQUE",
  "NOT",
  "NULL",
  "TEXT",
  "INTEGER",
  "REAL",
  "AND",
  "OR",
  "IF",
  "EXISTS",
  "INDEX",
]);

type Token = { kind: "kw" | "str" | "num" | "raw"; value: string };

function tokenize(line: string): Token[] {
  const out: Token[] = [];
  // Match strings, numbers, words, or one char of "other"
  const re = /'[^']*'|\d+|[A-Za-z_][A-Za-z0-9_]*|[^A-Za-z0-9_']+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line)) !== null) {
    const piece = m[0];
    if (piece.startsWith("'")) {
      out.push({ kind: "str", value: piece });
    } else if (/^\d+$/.test(piece)) {
      out.push({ kind: "num", value: piece });
    } else if (/^[A-Za-z_]/.test(piece) && KEYWORDS.has(piece.toUpperCase())) {
      out.push({ kind: "kw", value: piece });
    } else {
      out.push({ kind: "raw", value: piece });
    }
  }
  return out;
}

function HighlightedSql({ text }: { text: string }) {
  const tokens = tokenize(text);
  return (
    <>
      {tokens.map((t, i) => {
        if (t.kind === "kw") return <span key={i} className="kw">{t.value}</span>;
        if (t.kind === "str") return <span key={i} className="str">{t.value}</span>;
        if (t.kind === "num") return <span key={i} className="num">{t.value}</span>;
        return <span key={i}>{t.value}</span>;
      })}
    </>
  );
}

export function Terminal() {
  const [lines, setLines] = useState<Line[]>([]);
  const [typing, setTyping] = useState("");
  const [step, setStep] = useState(0);
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (step >= SCRIPT.length) {
      const t = setTimeout(() => {
        setLines([]);
        setStep(0);
      }, 4500);
      return () => clearTimeout(t);
    }
    const item = SCRIPT[step];
    if (item.type === "out") {
      const t = setTimeout(() => {
        setLines((l) => [...l, item]);
        setStep((s) => s + 1);
      }, 240);
      return () => clearTimeout(t);
    }
    let i = 0;
    setTyping("");
    const id = setInterval(() => {
      i += 1;
      setTyping(item.text.slice(0, i));
      if (i >= item.text.length) {
        clearInterval(id);
        setTimeout(() => {
          setLines((l) => [...l, item]);
          setTyping("");
          setStep((s) => s + 1);
        }, 380);
      }
    }, 22);
    return () => clearInterval(id);
  }, [step]);

  useEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
  }, [lines, typing]);

  const currentItem = step < SCRIPT.length ? SCRIPT[step] : null;

  return (
    <div className="term" aria-label="SQLRite REPL demo">
      <div className="term-bar">
        <span className="term-dot" />
        <span className="term-dot" />
        <span className="term-dot" />
        <span className="term-title">sqlrite — repl — in-memory</span>
      </div>
      <div className="term-body" ref={bodyRef}>
        {lines.map((l, i) => (
          <span key={i} className="term-line">
            {l.type === "in" ? (
              <>
                <span className="prompt">sqlrite{">"} </span>
                <HighlightedSql text={l.text} />
              </>
            ) : (
              <span className={l.cls ?? "dim"}>{l.text}</span>
            )}
          </span>
        ))}
        {currentItem && currentItem.type === "in" && (
          <span className="term-line">
            <span className="prompt">sqlrite{">"} </span>
            <HighlightedSql text={typing} />
            <span className="cursor" />
          </span>
        )}
      </div>
    </div>
  );
}
