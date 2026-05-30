//! Journal data layer — all SQL lives here.
//!
//! Wraps a `sqlrite::Connection` with:
//!   - the migration runner (one schema version today; bump on change)
//!   - entry CRUD + tag-association helpers
//!   - BM25 full-text search with snippet construction
//!   - `ask` integration (gated under the `ask` cargo feature)
//!   - export helpers (DB file copy, markdown-folder dump)
//!
//! Every method that talks to the engine takes `&mut self` because
//! `Connection::prepare` borrows `&mut Connection` to construct the
//! returned `Statement`. The Tauri command layer owns the only
//! `Connection` behind a `Mutex` (see `main.rs`), so this module
//! never worries about concurrency — but it does need a `mut` lock
//! guard at the callsite.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use sqlrite::{Connection, Value};

#[cfg(feature = "ask")]
use sqlrite::ConnectionAskExt;
#[cfg(feature = "ask")]
pub use sqlrite::ask::AskConfig;
#[cfg(feature = "ask")]
use sqlrite::ask::AskError;

/// Current schema version. Bump *and* add a migration arm in
/// [`JournalDb::migrate`] in the same commit.
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("engine: {0}")]
    Engine(#[from] sqlrite::SQLRiteError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("no database is open")]
    NoDb,

    #[error("validation: {0}")]
    Validation(String),

    #[cfg(feature = "ask")]
    #[error("ask: refusing to execute non-read-only SQL returned by the LLM: {0}")]
    AskNotReadOnly(String),

    #[cfg(feature = "ask")]
    #[error("ask: {0}")]
    Ask(#[from] AskError),
}

pub type JournalResult<T> = Result<T, JournalError>;

#[derive(Serialize, Clone, Debug)]
pub struct EntrySummary {
    pub id: i64,
    pub date: String,
    pub title: String,
    /// First ~160 chars of content. Cheap preview line for the list view.
    pub excerpt: String,
    pub updated_at: i64,
    pub tags: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub date: String,
    pub title: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub tags: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TagSummary {
    pub name: String,
    pub entry_count: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct SearchHit {
    pub id: i64,
    pub date: String,
    pub title: String,
    /// HTML-fragment-safe snippet with `<mark>...</mark>` around matched
    /// tokens. Frontend renders with `{@html}` and trusts the markup.
    pub snippet_html: String,
    pub score: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Stats {
    pub total_entries: i64,
    pub distinct_dates: i64,
    pub total_tags: i64,
}

#[cfg(feature = "ask")]
#[derive(Serialize, Clone, Debug)]
pub struct AskResult {
    pub sql: String,
    pub explanation: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ExportSummary {
    pub entry_count: i64,
    pub dest: String,
}

/// Wraps a `Connection` with journal-specific schema + helpers.
pub struct JournalDb {
    conn: Connection,
    path: Option<PathBuf>,
}

impl JournalDb {
    /// Returns the on-disk path the underlying connection was opened
    /// from, when known. `None` for in-memory test connections or
    /// callers that constructed via `with_connection`.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Wraps an open `Connection` and runs migrations to the current
    /// schema version. Useful when the caller wants to control the
    /// `Connection::open` itself (e.g. `main.rs` opening against the
    /// app-data dir before constructing this).
    pub fn with_connection(conn: Connection) -> JournalResult<Self> {
        let mut db = JournalDb { conn, path: None };
        db.migrate()?;
        Ok(db)
    }

    /// Same as [`Self::with_connection`] but records the originating
    /// path. (Used by tests + future callers that want the path
    /// reflected back via [`Self::path`].)
    #[allow(dead_code)]
    pub fn open(path: &Path) -> JournalResult<Self> {
        let conn = Connection::open(path)?;
        let mut db = JournalDb {
            conn,
            path: Some(path.to_path_buf()),
        };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory_for_test() -> JournalResult<Self> {
        let conn = Connection::open_in_memory()?;
        let mut db = JournalDb { conn, path: None };
        db.migrate()?;
        Ok(db)
    }

    // ----- migrations -------------------------------------------------

    fn migrate(&mut self) -> JournalResult<()> {
        let current = self.read_schema_version()?;
        if current >= SCHEMA_VERSION {
            return Ok(());
        }
        if current < 1 {
            self.apply_v1()?;
            self.write_schema_version(1)?;
        }
        Ok(())
    }

    fn read_schema_version(&mut self) -> JournalResult<i64> {
        // Probe by attempting a SELECT. On any error that looks like
        // "the table doesn't exist yet" — which the engine surfaces
        // at either prepare()/narrow time or query() time depending on
        // when it discovers the missing table — materialise the
        // schema_version table and return 0. Any other error
        // propagates as Engine.
        //
        // We use this probe instead of querying sqlrite_master because
        // in-memory `Database::new` instances don't materialise the
        // master catalog until the first save — so the catalog itself
        // can be missing.
        match probe_schema_version(&mut self.conn) {
            Ok(v) => Ok(v),
            Err(JournalError::Engine(e)) if is_missing_table_error(&e) => {
                self.conn
                    .execute("CREATE TABLE schema_version (version INTEGER PRIMARY KEY);")?;
                self.conn
                    .execute("INSERT INTO schema_version (version) VALUES (0);")?;
                Ok(0)
            }
            Err(other) => Err(other),
        }
    }

    fn write_schema_version(&mut self, v: i64) -> JournalResult<()> {
        self.conn.execute("DELETE FROM schema_version;")?;
        let mut stmt = self
            .conn
            .prepare("INSERT INTO schema_version (version) VALUES (?);")?;
        stmt.execute_with_params(&[Value::Integer(v)])?;
        Ok(())
    }

    fn apply_v1(&mut self) -> JournalResult<()> {
        self.conn.execute(
            "CREATE TABLE entries (
                id INTEGER PRIMARY KEY,
                date TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )?;
        // Speeds up date-range queries + updated_at ordering. Single-column
        // is all SQLRite supports today; that's enough.
        self.conn
            .execute("CREATE INDEX IF NOT EXISTS entries_date_idx ON entries (date);")?;
        self.conn
            .execute("CREATE INDEX IF NOT EXISTS entries_updated_idx ON entries (updated_at);")?;
        // Phase 8 BM25 full-text indexes. Two: content (long form,
        // primary search target) and title (short — we don't yet
        // surface BM25 over title alongside content, but having the
        // index in place means a future ranking pass that fuses the
        // two scores is a search.rs change with no migration).
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS entries_content_fts ON entries USING fts (content);",
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS entries_title_fts ON entries USING fts (title);",
        )?;

        // tags: name is UNIQUE so we can use the auto-index for lookups.
        self.conn.execute(
            "CREATE TABLE tags (
                id INTEGER PRIMARY KEY,
                name TEXT UNIQUE NOT NULL
            );",
        )?;
        // entry_tags: synthetic PK so we can keep INTEGER PRIMARY KEY
        // semantics; uniqueness of (entry_id, tag_id) enforced in app
        // code (the create_entry helper de-dupes the tag input).
        self.conn.execute(
            "CREATE TABLE entry_tags (
                id INTEGER PRIMARY KEY,
                entry_id INTEGER NOT NULL,
                tag_id INTEGER NOT NULL
            );",
        )?;
        self.conn
            .execute("CREATE INDEX IF NOT EXISTS entry_tags_entry_idx ON entry_tags (entry_id);")?;
        self.conn
            .execute("CREATE INDEX IF NOT EXISTS entry_tags_tag_idx ON entry_tags (tag_id);")?;
        Ok(())
    }

    // ----- entries ----------------------------------------------------

    pub fn count_entries(&mut self) -> JournalResult<i64> {
        let stmt = self.conn.prepare("SELECT id FROM entries;")?;
        let rows = stmt.query()?.collect_all()?;
        Ok(rows.len() as i64)
    }

    pub fn list_entries(&mut self, tag_filter: Option<&str>) -> JournalResult<Vec<EntrySummary>> {
        // Two paths: tag-filtered uses a join, unfiltered scans entries
        // ordered by date DESC. Both materialise (id, date, title,
        // content, updated_at) first; tag arrays are filled in a
        // second pass to keep us inside the engine's single-table
        // aggregate-and-join sweet spot.
        let mut summaries: Vec<EntrySummary> = if let Some(tag) = tag_filter {
            let stmt = self.conn.prepare(
                "SELECT entries.id, entries.date, entries.title, entries.content, \
                 entries.updated_at \
                 FROM entries \
                 JOIN entry_tags ON entry_tags.entry_id = entries.id \
                 JOIN tags ON tags.id = entry_tags.tag_id \
                 WHERE tags.name = ? \
                 ORDER BY entries.date DESC;",
            )?;
            let rows = stmt
                .query_with_params(&[Value::Text(tag.to_string())])?
                .collect_all()?;
            rows.into_iter()
                .map(|r| {
                    Ok::<_, JournalError>(EntrySummary {
                        id: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get(2)?,
                        excerpt: excerpt(&r.get::<String>(3)?),
                        updated_at: r.get(4)?,
                        tags: vec![],
                    })
                })
                .collect::<JournalResult<Vec<_>>>()?
        } else {
            let stmt = self.conn.prepare(
                "SELECT id, date, title, content, updated_at FROM entries \
                 ORDER BY date DESC;",
            )?;
            let rows = stmt.query()?.collect_all()?;
            rows.into_iter()
                .map(|r| {
                    Ok::<_, JournalError>(EntrySummary {
                        id: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get(2)?,
                        excerpt: excerpt(&r.get::<String>(3)?),
                        updated_at: r.get(4)?,
                        tags: vec![],
                    })
                })
                .collect::<JournalResult<Vec<_>>>()?
        };
        for s in summaries.iter_mut() {
            s.tags = self.tags_for_entry(s.id)?;
        }
        Ok(summaries)
    }

    pub fn get_entry(&mut self, id: i64) -> JournalResult<Entry> {
        let stmt = self.conn.prepare(
            "SELECT id, date, title, content, created_at, updated_at \
             FROM entries WHERE id = ?;",
        )?;
        let rows = stmt
            .query_with_params(&[Value::Integer(id)])?
            .collect_all()?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| JournalError::Validation(format!("entry {id} not found")))?;
        let tags = self.tags_for_entry(id)?;
        Ok(Entry {
            id: row.get(0)?,
            date: row.get(1)?,
            title: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
            tags,
        })
    }

    pub fn create_entry(
        &mut self,
        date: &str,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> JournalResult<i64> {
        validate_date(date)?;
        let now = unix_now();
        let mut ins = self.conn.prepare(
            "INSERT INTO entries (date, title, content, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?);",
        )?;
        ins.execute_with_params(&[
            Value::Text(date.to_string()),
            Value::Text(title.to_string()),
            Value::Text(content.to_string()),
            Value::Integer(now),
            Value::Integer(now),
        ])?;
        let id = last_entry_id(&mut self.conn)?;
        self.set_entry_tags(id, tags)?;
        Ok(id)
    }

    pub fn update_entry(
        &mut self,
        id: i64,
        date: &str,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> JournalResult<()> {
        validate_date(date)?;
        let now = unix_now();
        let mut upd = self.conn.prepare(
            "UPDATE entries SET date = ?, title = ?, content = ?, updated_at = ? WHERE id = ?;",
        )?;
        upd.execute_with_params(&[
            Value::Text(date.to_string()),
            Value::Text(title.to_string()),
            Value::Text(content.to_string()),
            Value::Integer(now),
            Value::Integer(id),
        ])?;
        self.clear_entry_tags(id)?;
        self.set_entry_tags(id, tags)?;
        Ok(())
    }

    pub fn delete_entry(&mut self, id: i64) -> JournalResult<()> {
        self.clear_entry_tags(id)?;
        let mut del = self.conn.prepare("DELETE FROM entries WHERE id = ?;")?;
        del.execute_with_params(&[Value::Integer(id)])?;
        Ok(())
    }

    // ----- tags -------------------------------------------------------

    fn tags_for_entry(&mut self, entry_id: i64) -> JournalResult<Vec<String>> {
        let stmt = self.conn.prepare(
            "SELECT tags.name FROM tags \
             JOIN entry_tags ON entry_tags.tag_id = tags.id \
             WHERE entry_tags.entry_id = ? \
             ORDER BY tags.name;",
        )?;
        let rows = stmt
            .query_with_params(&[Value::Integer(entry_id)])?
            .collect_all()?;
        let mut names = Vec::with_capacity(rows.len());
        for r in rows {
            names.push(r.get::<String>(0)?);
        }
        Ok(names)
    }

    fn clear_entry_tags(&mut self, entry_id: i64) -> JournalResult<()> {
        let mut del = self
            .conn
            .prepare("DELETE FROM entry_tags WHERE entry_id = ?;")?;
        del.execute_with_params(&[Value::Integer(entry_id)])?;
        Ok(())
    }

    fn set_entry_tags(&mut self, entry_id: i64, tags: &[String]) -> JournalResult<()> {
        // De-dupe and normalise (trim + lowercase). Empty tags drop on the floor.
        let mut seen = std::collections::BTreeSet::new();
        for raw in tags {
            let t = raw.trim().to_lowercase();
            if t.is_empty() {
                continue;
            }
            seen.insert(t);
        }
        for tag in seen {
            let tag_id = self.upsert_tag(&tag)?;
            let mut ins = self
                .conn
                .prepare("INSERT INTO entry_tags (entry_id, tag_id) VALUES (?, ?);")?;
            ins.execute_with_params(&[Value::Integer(entry_id), Value::Integer(tag_id)])?;
        }
        Ok(())
    }

    fn upsert_tag(&mut self, name: &str) -> JournalResult<i64> {
        // Lookup → insert if missing → re-lookup. The third step is
        // needed because `Statement::execute_with_params` returns a
        // status string, not the new rowid; SQLRite has no
        // `last_insert_rowid()` yet (see roadmap).
        {
            let stmt = self.conn.prepare("SELECT id FROM tags WHERE name = ?;")?;
            let rows = stmt
                .query_with_params(&[Value::Text(name.to_string())])?
                .collect_all()?;
            if let Some(r) = rows.into_iter().next() {
                return Ok(r.get::<i64>(0)?);
            }
        }
        {
            let mut ins = self.conn.prepare("INSERT INTO tags (name) VALUES (?);")?;
            ins.execute_with_params(&[Value::Text(name.to_string())])?;
        }
        let stmt = self.conn.prepare("SELECT id FROM tags WHERE name = ?;")?;
        let rows = stmt
            .query_with_params(&[Value::Text(name.to_string())])?
            .collect_all()?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| JournalError::Validation("tag insert failed".into()))?;
        Ok(row.get::<i64>(0)?)
    }

    pub fn list_tags(&mut self) -> JournalResult<Vec<TagSummary>> {
        // Per-tag counts: get all tags, then per-tag SELECT COUNT. The
        // single-table aggregator means a join-aggregate is not yet
        // supported (docs/supported-sql.md). Two passes is fine for
        // the kind of tag cardinality a journal will ever reach.
        let tag_pairs: Vec<(i64, String)> = {
            let stmt = self
                .conn
                .prepare("SELECT id, name FROM tags ORDER BY name;")?;
            let rows = stmt.query()?.collect_all()?;
            rows.into_iter()
                .map(|r| Ok::<_, JournalError>((r.get::<i64>(0)?, r.get::<String>(1)?)))
                .collect::<JournalResult<Vec<_>>>()?
        };
        let mut out = Vec::with_capacity(tag_pairs.len());
        for (tag_id, name) in tag_pairs {
            let cs = self
                .conn
                .prepare("SELECT id FROM entry_tags WHERE tag_id = ?;")?;
            let count_rows = cs
                .query_with_params(&[Value::Integer(tag_id)])?
                .collect_all()?;
            out.push(TagSummary {
                name,
                entry_count: count_rows.len() as i64,
            });
        }
        Ok(out)
    }

    // ----- search -----------------------------------------------------

    pub fn search(&mut self, query: &str) -> JournalResult<Vec<SearchHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }
        // Single FTS index (`entries.content`) drives the ranked query.
        // `bm25_score` returns descending relevance; `fts_match` is the
        // existence filter. The companion `entries.title` index doesn't
        // participate in scoring today — the engine doesn't yet
        // surface a multi-index OR-rank shape. Title hits could be
        // folded in later by a UNION + rerank without a migration.
        // SQLRite v0.10 doesn't yet allow `bm25_score(...)` in the
        // projection list (only aggregates), so we let it drive the
        // ORDER BY for ranking and synthesise a per-hit score on the
        // Rust side based on result position. The score is purely
        // informational in the UI — the BM25 ordering is what matters.
        let stmt = self.conn.prepare(
            "SELECT id, date, title, content \
             FROM entries \
             WHERE fts_match(content, ?) \
             ORDER BY bm25_score(content, ?) DESC \
             LIMIT 50;",
        )?;
        let rows = stmt
            .query_with_params(&[Value::Text(q.to_string()), Value::Text(q.to_string())])?
            .collect_all()?;
        let total = rows.len() as f64;
        let mut hits = Vec::with_capacity(rows.len());
        for (i, r) in rows.into_iter().enumerate() {
            let id: i64 = r.get(0)?;
            let date: String = r.get(1)?;
            let title: String = r.get(2)?;
            let content: String = r.get(3)?;
            // Surrogate score: 1.0 for the top hit, decaying linearly.
            // Will become a real bm25 value once the engine accepts
            // it in the projection list (track via a follow-up).
            let score = if total > 0.0 {
                1.0 - (i as f64) / total
            } else {
                0.0
            };
            hits.push(SearchHit {
                id,
                date,
                title,
                snippet_html: build_snippet(&content, q),
                score,
            });
        }
        Ok(hits)
    }

    // ----- stats ------------------------------------------------------

    pub fn stats(&mut self) -> JournalResult<Stats> {
        let entries = self.count_entries()?;
        let dates_stmt = self.conn.prepare("SELECT date FROM entries;")?;
        let date_rows = dates_stmt.query()?.collect_all()?;
        let mut date_set = std::collections::BTreeSet::new();
        for r in date_rows {
            date_set.insert(r.get::<String>(0)?);
        }
        let tags_stmt = self.conn.prepare("SELECT id FROM tags;")?;
        let tag_count = tags_stmt.query()?.collect_all()?.len() as i64;
        Ok(Stats {
            total_entries: entries,
            distinct_dates: date_set.len() as i64,
            total_tags: tag_count,
        })
    }

    // ----- ask --------------------------------------------------------

    #[cfg(feature = "ask")]
    pub fn ask(&mut self, question: &str, cfg: &AskConfig) -> JournalResult<AskResult> {
        let resp = self.conn.ask(question, cfg)?;
        let sql = resp.sql.trim().to_string();
        let explanation = resp.explanation.clone();
        // Read-only gate: a journal is for browsing your own notes, not
        // for the LLM to silently mutate them. Strip a trailing
        // semicolon, lowercase the first non-whitespace token, accept
        // only SELECT / WITH. Anything else returns a refusal.
        if !is_read_only(&sql) {
            return Err(JournalError::AskNotReadOnly(sql));
        }
        let stmt = self.conn.prepare(&sql)?;
        let rows_iter = stmt.query()?;
        let columns: Vec<String> = rows_iter.columns().to_vec();
        let rows = rows_iter.collect_all()?;
        let rendered: Vec<Vec<String>> = rows
            .into_iter()
            .map(|r| {
                (0..columns.len())
                    .map(|i| match r.get::<Value>(i) {
                        Ok(v) => display_value(&v),
                        Err(_) => String::new(),
                    })
                    .collect()
            })
            .collect();
        Ok(AskResult {
            sql,
            explanation,
            columns,
            rows: rendered,
        })
    }

    // No off-feature fallback for `ask` — the `ask_journal` Tauri
    // command is also feature-gated, so off-feature builds never call
    // this method.

    // ----- export -----------------------------------------------------

    pub fn export_db(&self, dest: &Path) -> JournalResult<()> {
        let src = self.path.as_deref().ok_or(JournalError::NoDb)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // We copy the file as-is. The engine flushes on every committing
        // statement, so the on-disk snapshot reflects the last commit
        // point even without an explicit checkpoint.
        std::fs::copy(src, dest)?;
        Ok(())
    }

    pub fn export_markdown(&mut self, dir: &Path) -> JournalResult<ExportSummary> {
        std::fs::create_dir_all(dir)?;
        let entries = self.list_entries(None)?;
        for s in &entries {
            let full = self.get_entry(s.id)?;
            let filename = format!("{}-{}.md", full.date, slugify(&full.title));
            let path = dir.join(filename);
            let mut body = String::new();
            body.push_str("---\n");
            body.push_str(&format!("title: {}\n", yaml_escape(&full.title)));
            body.push_str(&format!("date: {}\n", full.date));
            if !full.tags.is_empty() {
                body.push_str("tags:\n");
                for t in &full.tags {
                    body.push_str(&format!("  - {}\n", yaml_escape(t)));
                }
            }
            body.push_str("---\n\n");
            body.push_str(&full.content);
            if !body.ends_with('\n') {
                body.push('\n');
            }
            std::fs::write(&path, body)?;
        }
        Ok(ExportSummary {
            entry_count: entries.len() as i64,
            dest: dir.display().to_string(),
        })
    }
}

// ---------- helpers ---------------------------------------------------

fn probe_schema_version(conn: &mut Connection) -> JournalResult<i64> {
    let stmt = conn.prepare("SELECT version FROM schema_version;")?;
    let rows = stmt.query()?.collect_all()?;
    Ok(rows
        .first()
        .map(|r| r.get::<i64>(0))
        .transpose()?
        .unwrap_or(0))
}

fn is_missing_table_error(e: &sqlrite::SQLRiteError) -> bool {
    // The engine surfaces "Table 'foo' not found" / "Table 'foo'
    // doesn't exist" / "Unknown table 'foo'" depending on whether
    // it's the executor or the parser-narrowing pass that rejects.
    // Match on the rendered Display string to handle all of them.
    let s = e.to_string().to_ascii_lowercase();
    s.contains("not found") || s.contains("doesn't exist") || s.contains("unknown table")
}

fn last_entry_id(conn: &mut Connection) -> JournalResult<i64> {
    // SQLRite has neither `last_insert_rowid()` nor `MAX()` over
    // arbitrary types yet; ORDER BY id DESC LIMIT 1 is the portable
    // shape that works against the engine today. Called under the
    // command mutex right after the INSERT, so no race risk.
    let stmt = conn.prepare("SELECT id FROM entries ORDER BY id DESC LIMIT 1;")?;
    let rows = stmt.query()?.collect_all()?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| JournalError::Validation("INSERT did not produce an id".into()))?;
    Ok(row.get::<i64>(0)?)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn validate_date(date: &str) -> JournalResult<()> {
    if date.len() != 10
        || date.as_bytes()[4] != b'-'
        || date.as_bytes()[7] != b'-'
        || !date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                true
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(JournalError::Validation(format!(
            "date must be ISO YYYY-MM-DD, got {date:?}"
        )));
    }
    Ok(())
}

fn excerpt(content: &str) -> String {
    let line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim_start_matches('>')
        .trim_start_matches('-')
        .trim_start_matches('*')
        .trim();
    let max = 160;
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let cut: String = line.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(feature = "ask")]
fn display_value(v: &Value) -> String {
    match v {
        Value::Integer(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Real(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "".to_string(),
        Value::Vector(_) => v.to_display_string(),
    }
}

#[cfg(feature = "ask")]
fn is_read_only(sql: &str) -> bool {
    let trimmed = sql.trim().trim_end_matches(';').trim_start();
    let first = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(first.as_str(), "SELECT" | "WITH")
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "untitled".into()
    } else {
        trimmed
    }
}

fn yaml_escape(s: &str) -> String {
    let needs = s.contains(':')
        || s.starts_with('-')
        || s.starts_with('!')
        || s.starts_with('#')
        || s.contains('\n');
    if !needs {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Build an HTML snippet around the first FTS term match in `content`.
/// Tokenisation mirrors the engine's FTS tokeniser (ASCII split,
/// lowercase, no stemming — see `docs/fts.md`), so what the user sees
/// highlighted matches what BM25 actually ranked on.
fn build_snippet(content: &str, query: &str) -> String {
    let query_tokens: Vec<String> = tokenize(query);
    if query_tokens.is_empty() {
        return html_escape(&truncate_chars(content, 240));
    }
    let lower = content.to_lowercase();
    let mut first_match: Option<usize> = None;
    for tok in &query_tokens {
        if let Some(idx) = find_token(&lower, tok) {
            first_match = Some(match first_match {
                Some(prev) if prev < idx => prev,
                _ => idx,
            });
        }
    }
    let center = first_match.unwrap_or(0);
    let half = 100;
    let start = center.saturating_sub(half);
    let end = (center + half).min(content.len());
    let start = round_to_char_boundary(content, start, false);
    let end = round_to_char_boundary(content, end, true);
    let snippet = content[start..end].to_string();
    let prefix_ellipsis = start > 0;
    let suffix_ellipsis = end < content.len();

    let mut out = String::with_capacity(snippet.len() + 32);
    if prefix_ellipsis {
        out.push('…');
    }
    let lower_snip = snippet.to_lowercase();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for tok in &query_tokens {
        let mut from = 0;
        // Walk repeatedly using the boundary-aware finder so that
        // "rust" matches "rust here" but skips "trustworthy".
        while let Some(rel) = find_token(&lower_snip[from..], tok) {
            let abs = from + rel;
            ranges.push((abs, tok.len()));
            from = abs + tok.len();
        }
    }
    let merged = coalesce_ranges(ranges);
    let mut cursor = 0;
    for (off, len) in merged {
        if off > cursor {
            out.push_str(&html_escape(&snippet[cursor..off]));
        }
        out.push_str("<mark>");
        out.push_str(&html_escape(&snippet[off..off + len]));
        out.push_str("</mark>");
        cursor = off + len;
    }
    if cursor < snippet.len() {
        out.push_str(&html_escape(&snippet[cursor..]));
    }
    if suffix_ellipsis {
        out.push('…');
    }
    out.replace('\n', " ")
}

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn find_token(lower: &str, tok: &str) -> Option<usize> {
    // Token-boundary matches only: "rust" should not light up inside
    // "trustworthy". Confirm the preceding and following chars are
    // non-alphanumeric on each candidate offset.
    let mut from = 0;
    while let Some(rel) = lower[from..].find(tok) {
        let abs = from + rel;
        let before_ok = abs == 0
            || lower[..abs]
                .chars()
                .next_back()
                .map(|c| !c.is_ascii_alphanumeric())
                .unwrap_or(true);
        let after_ok = abs + tok.len() >= lower.len()
            || lower[abs + tok.len()..]
                .chars()
                .next()
                .map(|c| !c.is_ascii_alphanumeric())
                .unwrap_or(true);
        if before_ok && after_ok {
            return Some(abs);
        }
        from = abs + tok.len();
    }
    None
}

fn round_to_char_boundary(s: &str, mut idx: usize, ceil: bool) -> usize {
    if idx > s.len() {
        return s.len();
    }
    while idx > 0 && idx < s.len() && !s.is_char_boundary(idx) {
        if ceil {
            idx += 1;
        } else {
            idx -= 1;
        }
    }
    idx
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn coalesce_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|r| r.0);
    let mut out = vec![ranges[0]];
    for &(off, len) in &ranges[1..] {
        let last = out.last_mut().unwrap();
        if off <= last.0 + last.1 {
            let end = (off + len).max(last.0 + last.1);
            last.1 = end - last.0;
        } else {
            out.push((off, len));
        }
    }
    out
}

// ---------- tests -----------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> JournalDb {
        JournalDb::open_in_memory_for_test().expect("open in-memory")
    }

    #[test]
    fn migration_is_idempotent() {
        let mut d = db();
        d.migrate().expect("idempotent migrate");
        let s = d.stats().expect("stats");
        assert_eq!(s.total_entries, 0);
    }

    #[test]
    fn create_get_update_delete_entry() {
        let mut d = db();
        let id = d
            .create_entry(
                "2026-05-25",
                "First entry",
                "# Hello\n\nWriting about rust embedded databases.",
                &["rust".into(), "Database".into(), " rust ".into()],
            )
            .expect("create");
        let got = d.get_entry(id).expect("get");
        assert_eq!(got.title, "First entry");
        assert_eq!(got.tags, vec!["database".to_string(), "rust".to_string()]);
        d.update_entry(
            id,
            "2026-05-26",
            "Updated title",
            "Updated body about postgres.",
            &["postgres".into()],
        )
        .expect("update");
        let got = d.get_entry(id).expect("re-get");
        assert_eq!(got.date, "2026-05-26");
        assert_eq!(got.tags, vec!["postgres".to_string()]);
        d.delete_entry(id).expect("delete");
        assert!(d.get_entry(id).is_err());
    }

    #[test]
    fn list_filter_by_tag() {
        let mut d = db();
        d.create_entry("2026-05-20", "A", "body about rust", &["rust".into()])
            .unwrap();
        d.create_entry("2026-05-21", "B", "body about go", &["go".into()])
            .unwrap();
        let rust = d.list_entries(Some("rust")).unwrap();
        assert_eq!(rust.len(), 1);
        assert_eq!(rust[0].title, "A");
    }

    #[test]
    fn fts_search_with_highlighting() {
        let mut d = db();
        d.create_entry(
            "2026-05-20",
            "Day 1",
            "Today I went running by the river and felt great.",
            &[],
        )
        .unwrap();
        d.create_entry(
            "2026-05-21",
            "Day 2",
            "Worked on database internals all afternoon.",
            &[],
        )
        .unwrap();
        d.create_entry(
            "2026-05-22",
            "Day 3",
            "Long run in the morning, then more database work.",
            &[],
        )
        .unwrap();
        let hits = d.search("running").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Day 1");
        assert!(hits[0].snippet_html.contains("<mark>running</mark>"));
        let db_hits = d.search("database").unwrap();
        assert_eq!(db_hits.len(), 2);
        assert!(db_hits[0].snippet_html.contains("<mark>database</mark>"));
    }

    #[test]
    fn stats_counts() {
        let mut d = db();
        d.create_entry("2026-05-01", "a", "x", &["t1".into()])
            .unwrap();
        d.create_entry("2026-05-01", "b", "x", &["t1".into(), "t2".into()])
            .unwrap();
        d.create_entry("2026-05-02", "c", "x", &["t2".into()])
            .unwrap();
        let s = d.stats().unwrap();
        assert_eq!(s.total_entries, 3);
        assert_eq!(s.distinct_dates, 2);
        assert_eq!(s.total_tags, 2);
    }

    #[test]
    fn validate_date_rejects_bad_input() {
        assert!(validate_date("2026-05-25").is_ok());
        assert!(validate_date("2026/05/25").is_err());
        assert!(validate_date("not a date").is_err());
        assert!(validate_date("2026-5-25").is_err());
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("   "), "untitled");
        assert_eq!(slugify("Multi   spaces  here"), "multi-spaces-here");
    }

    #[test]
    fn snippet_highlights_token_boundary_only() {
        // "rust" should highlight, but the embedded "rust" inside
        // "trustworthy" must not.
        let out = build_snippet("This is trustworthy. We love rust here.", "rust");
        assert!(out.contains("<mark>rust</mark>"));
        assert!(!out.contains("t<mark>rust</mark>worthy"));
    }

    #[cfg(feature = "ask")]
    #[test]
    fn is_read_only_classifier() {
        assert!(is_read_only("SELECT 1;"));
        assert!(is_read_only("  select * from entries  "));
        assert!(is_read_only("WITH x AS (SELECT 1) SELECT * FROM x;"));
        assert!(!is_read_only("INSERT INTO entries VALUES (1)"));
        assert!(!is_read_only("DELETE FROM entries"));
        assert!(!is_read_only("UPDATE entries SET title = 'x'"));
    }
}
