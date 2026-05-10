import { SITE } from "@/lib/site";
import { highlightCode } from "@/lib/highlight";
import {
  SDKShowcaseClient,
  type SdkEntry,
} from "./sdk-showcase-client";

type SdkSource = Omit<SdkEntry, "codeHtml"> & {
  /** Raw snippet — highlighted at build time. */
  code: string;
  /** Shiki language id. */
  lang: string;
};

const SDKS: SdkSource[] = [
  {
    key: "rust",
    name: "Rust",
    lang: "rust",
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
  {
    key: "python",
    name: "Python",
    lang: "python",
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
  {
    key: "node",
    name: "Node.js",
    lang: "javascript",
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
  {
    key: "go",
    name: "Go",
    lang: "go",
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
  {
    key: "c",
    name: "C",
    lang: "c",
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
  {
    key: "wasm",
    name: "WASM",
    lang: "typescript",
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
];

export async function SDKShowcase() {
  const entries: SdkEntry[] = await Promise.all(
    SDKS.map(async ({ code, lang, ...rest }) => ({
      ...rest,
      codeHtml: await highlightCode(code, lang),
    })),
  );
  return <SDKShowcaseClient sdks={entries} />;
}
