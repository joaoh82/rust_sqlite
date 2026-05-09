"use client";

import { useState } from "react";
import { SITE } from "@/lib/site";
import { InstallBar } from "./install-bar";

type SdkKey = "rust" | "python" | "node" | "go" | "c" | "wasm";

type Sdk = {
  name: string;
  install: string;
  version: string;
  registry: string;
  note: string;
  ext: string;
  code: string;
};

const SDKS: Record<SdkKey, Sdk> = {
  rust: {
    name: "Rust",
    install: `cargo add sqlrite-engine`,
    version: SITE.version,
    registry: "crates.io",
    note: "Native — no FFI hop. Imported as `use sqlrite::…`.",
    ext: "rs",
    code: `use sqlrite::Connection;

fn main() -> sqlrite::Result<()> {
    let mut conn = Connection::open("app.sqlrite")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users \\
         (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    )?;
    conn.execute("INSERT INTO users (name) VALUES ('alice')")?;

    let rows = conn.query("SELECT id, name FROM users")?;
    for row in rows {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        println!("{id}: {name}");
    }
    Ok(())
}`,
  },
  python: {
    name: "Python",
    install: `pip install sqlrite`,
    version: SITE.version,
    registry: "PyPI",
    note: "DB-API 2.0 inspired · abi3-py38 wheels for every platform.",
    ext: "py",
    code: `import sqlrite

with sqlrite.connect("app.sqlrite") as conn:
    cur = conn.cursor()
    cur.execute("""CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL
    )""")
    cur.execute("INSERT INTO users (name) VALUES ('alice')")

    cur.execute("SELECT id, name FROM users")
    for row in cur.fetchall():
        print(row)`,
  },
  node: {
    name: "Node.js",
    install: `npm install @joaoh82/sqlrite`,
    version: SITE.version,
    registry: "npm · scoped",
    note: "better-sqlite3-style sync API. Prebuilt .node binaries.",
    ext: "mjs",
    code: `import { Database } from "@joaoh82/sqlrite";

const db = new Database("app.sqlrite");

db.exec(\`CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL
)\`);

const insert = db.prepare("INSERT INTO users (name) VALUES (?)");
insert.run("alice");

const all = db.prepare("SELECT id, name FROM users").all();
console.log(all);`,
  },
  go: {
    name: "Go",
    install: `go get github.com/joaoh82/rust_sqlite/sdk/go`,
    version: `v${SITE.version}`,
    registry: "VCS · proxy.golang.org",
    note: "database/sql driver — cgo against libsqlrite_c.",
    ext: "go",
    code: `package main

import (
    "database/sql"
    "fmt"
    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

func main() {
    db, _ := sql.Open("sqlrite", "app.sqlrite")
    defer db.Close()

    db.Exec(\`CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL)\`)
    db.Exec("INSERT INTO users (name) VALUES (?)", "alice")

    rows, _ := db.Query("SELECT id, name FROM users")
    for rows.Next() {
        var id int64; var name string
        rows.Scan(&id, &name)
        fmt.Println(id, name)
    }
}`,
  },
  c: {
    name: "C",
    install: `make -C examples/c run`,
    version: SITE.version,
    registry: "libsqlrite_c · cbindgen header",
    note: "Opaque pointers · thread-local last-error · stable C ABI.",
    ext: "c",
    code: `#include "sqlrite.h"
#include <stdio.h>

int main(void) {
    sqlrite_db* db;
    if (sqlrite_open("app.sqlrite", &db) != SQLRITE_OK) {
        fprintf(stderr, "%s\\n", sqlrite_errmsg());
        return 1;
    }

    sqlrite_execute(db,
        "CREATE TABLE IF NOT EXISTS users "
        "(id INTEGER PRIMARY KEY, name TEXT NOT NULL)");
    sqlrite_execute(db,
        "INSERT INTO users (name) VALUES ('alice')");

    sqlrite_close(db);
    return 0;
}`,
  },
  wasm: {
    name: "WASM",
    install: `npm install @joaoh82/sqlrite-wasm`,
    version: SITE.version,
    registry: "npm · ~500 KB gzipped",
    note: "Engine runs entirely in a browser tab.",
    ext: "ts",
    code: `import init, { Database } from "@joaoh82/sqlrite-wasm";

await init();
const db = new Database();          // in-memory

db.exec(\`CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL
)\`);
db.exec("INSERT INTO users (name) VALUES ('alice')");

const rows = db.query("SELECT id, name FROM users");
console.log(rows);                   // [{ id: 1, name: "alice" }]`,
  },
};

export function SDKShowcase() {
  const [tab, setTab] = useState<SdkKey>("rust");
  const sdk = SDKS[tab];

  return (
    <section id="sdks">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">05 · embedding</span>
          <div>
            <h2>One engine. Six languages.</h2>
            <p className="sub">
              The same Rust core — wrapped, never reimplemented. SDKs ship as
              prebuilt binaries so there&rsquo;s no toolchain to install just to
              use the database.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="sdk-tabs" role="tablist">
            {(Object.entries(SDKS) as [SdkKey, Sdk][]).map(([key, s]) => (
              <button
                key={key}
                role="tab"
                aria-selected={tab === key}
                className={`sdk-tab ${tab === key ? "active" : ""}`}
                onClick={() => setTab(key)}
              >
                {s.name}
              </button>
            ))}
          </div>
          <div className="sdk-panel">
            <div className="sdk-meta">
              <h3>{sdk.name}</h3>
              <p
                className="dim"
                style={{ marginTop: 6, fontSize: 13.5 }}
              >
                {sdk.note}
              </p>
              <InstallBar cmd={sdk.install} />
              <div style={{ marginTop: 18 }}>
                <div className="meta-row">
                  <span>version</span>
                  <span className="v">{sdk.version}</span>
                </div>
                <div className="meta-row">
                  <span>registry</span>
                  <span className="v">{sdk.registry}</span>
                </div>
                <div className="meta-row">
                  <span>license</span>
                  <span className="v">MIT</span>
                </div>
              </div>
            </div>
            <div className="code-block">
              <div className="code-head">
                <span>example.{sdk.ext}</span>
                <span>· copy-pasteable</span>
              </div>
              <div className="code-body">{sdk.code}</div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
