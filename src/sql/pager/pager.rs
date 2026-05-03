//! Long-lived page cache + WAL-backed commits.
//!
//! A `Pager` wraps an open `.sqlrite` file plus its `-wal` sidecar. It owns
//! three maps of page bytes:
//!
//! - `on_disk`:   snapshot of the main file as last checkpointed. Frozen
//!                across regular commits — the main file is only rewritten
//!                when the checkpointer (Phase 4d) runs.
//! - `wal_cache`: latest committed body for each page that has been
//!                appended to the WAL since the last checkpoint. Populated
//!                at open by replaying the WAL, and kept in lockstep with
//!                each successful `commit`.
//! - `staged`:    pages queued for the next commit, not yet in the WAL.
//!
//! **Read precedence.** `read_page` consults `staged → wal_cache → on_disk`,
//! so both uncommitted writes and WAL-resident committed writes shadow the
//! frozen main file. A bounds check against `current_header.page_count`
//! hides pages that have been logically truncated by a shrink-commit even
//! though their bytes are still present in `on_disk` (the real truncation
//! waits for the checkpointer).
//!
//! **Commit flow.** `commit` compares each staged page against the
//! effective committed state (wal_cache layered on on_disk) and appends a
//! WAL frame only for pages whose bytes actually differ. A final "commit"
//! frame for page 0 carries the new encoded header and the post-commit
//! page count in its `commit_page_count` field. That frame is fsync'd.
//! The main file is not touched.
//!
//! **Checkpoint flow (Phase 4d).** When the WAL accumulates past
//! `AUTO_CHECKPOINT_THRESHOLD_FRAMES` frames (tracked on `Wal`), `commit`
//! opportunistically folds them back into the main file: write every
//! WAL-resident page at its proper offset, overwrite the main-file
//! header, truncate the file to `page_count * PAGE_SIZE` bytes, `fsync`,
//! then `Wal::truncate` the sidecar (which rolls the salt so any stale
//! tail bytes from the old generation can't be misread as valid). Reads
//! stay consistent if a crash hits mid-checkpoint — the WAL still holds
//! the authoritative bytes until its header is rewritten, and the
//! checkpointer is idempotent, so rerunning is safe.
//!
//! This matters because higher layers re-serialize the entire database on
//! every auto-save. Without the diff, even a one-row UPDATE would append a
//! frame for every page of every table. With the diff, unchanged tables —
//! whose encoded pages hash identically across saves — simply stay out of
//! the WAL.
//!
//! **Locking (Phase 4a → 4e).** Every `Pager` takes an advisory lock on
//! its main file and on its WAL sidecar. The mode is driven by
//! [`AccessMode`]:
//!
//! - `ReadWrite` → `flock(LOCK_EX)` — one writer, no other openers.
//! - `ReadOnly`  → `flock(LOCK_SH)` — multiple readers coexist; any writer
//!   is excluded.
//!
//! Both locks are tied to their file descriptors and release
//! automatically when the `Pager` drops. On collision the opener gets
//! a clean typed error rather than racing silently. POSIX flock is
//! "multiple readers OR one writer", not both — true concurrent
//! reader-and-writer access would need a shared-memory coordination
//! file and read marks, which is not on the roadmap.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::file::FileStorage;
use crate::sql::pager::header::{DbHeader, decode_header, encode_header};
use crate::sql::pager::page::PAGE_SIZE;
use crate::sql::pager::wal::Wal;

/// Returns the WAL sidecar path for a main `.sqlrite` file: appends
/// the `-wal` suffix to the full path (so `foo.sqlrite` pairs with
/// `foo.sqlrite-wal`). Matches SQLite's convention.
pub(crate) fn wal_path_for(main: &Path) -> PathBuf {
    let mut os = main.as_os_str().to_owned();
    os.push("-wal");
    PathBuf::from(os)
}

/// How a `Pager` (or `Wal`) intends to use the file: mutating writes vs.
/// consistent-snapshot reads. Drives the OS-level lock mode, and the
/// Pager uses it to reject mutation attempts on read-only openers.
///
/// - `ReadWrite` takes `flock(LOCK_EX)` — one writer, no other openers.
/// - `ReadOnly`  takes `flock(LOCK_SH)` — multiple readers can coexist;
///   a writer is excluded.
///
/// This is POSIX-flock semantics, so "multiple readers AND one writer"
/// isn't supported yet. True concurrent reader-writer access would need
/// a shared-memory coordination file and read marks — that's deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

/// Acquires an advisory lock on `file`, mapping the OS-level "lock
/// held" error to a clean SQLRite error. `Exclusive` on Unix is
/// `flock(LOCK_EX | LOCK_NB)`; `Shared` is `flock(LOCK_SH | LOCK_NB)`.
/// On Windows, `LockFileEx` with the corresponding flags.
///
/// We call fs2's trait methods fully qualified because `std::fs::File`
/// gained its own `try_lock_*` inherent methods in Rust 1.84 with a
/// different error type — qualifying nails down which one we mean.
#[cfg(feature = "file-locks")]
pub(crate) fn acquire_lock(file: &File, path: &Path, mode: AccessMode) -> Result<()> {
    let res = match mode {
        AccessMode::ReadWrite => fs2::FileExt::try_lock_exclusive(file),
        AccessMode::ReadOnly => fs2::FileExt::try_lock_shared(file),
    };
    res.map_err(|e| {
        let how = match mode {
            AccessMode::ReadWrite => {
                "is in use (another process has it open; readers and writers are exclusive)"
            }
            AccessMode::ReadOnly => {
                "is locked for writing by another process (read-only open blocked until the writer closes)"
            }
        };
        SQLRiteError::General(format!(
            "database '{}' {how} ({e})",
            path.display()
        ))
    })
}

/// No-op variant for builds without the `file-locks` feature (most
/// notably the WASM SDK, where `fs2` doesn't compile against
/// wasm32-unknown-unknown). The Pager still refuses to touch a
/// read-only open via `AccessMode`, but there's no OS-level
/// multi-process coordination — the caller is trusted to avoid
/// conflicting opens. Fine for WASM, where file-backed opens
/// aren't exposed in the MVP anyway.
#[cfg(not(feature = "file-locks"))]
pub(crate) fn acquire_lock(_file: &File, _path: &Path, _mode: AccessMode) -> Result<()> {
    Ok(())
}

/// How many WAL frames may accumulate between auto-checkpoints before
/// `commit` opportunistically folds them back into the main file. Kept
/// low enough that the WAL stays bounded on write-heavy workloads;
/// high enough that small bursts don't thrash the main file. SQLite
/// defaults to 1000; our target DBs are smaller so 100 is plenty.
const AUTO_CHECKPOINT_THRESHOLD_FRAMES: usize = 100;

pub struct Pager {
    /// Main-file I/O handle. Regular commits leave it alone; the
    /// checkpointer writes accumulated WAL pages back here.
    storage: FileStorage,
    current_header: DbHeader,
    /// Byte snapshot of the main file as last checkpointed. The
    /// checkpointer is the only thing that mutates it.
    on_disk: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Pages queued for the next commit. `commit` drains this.
    staged: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// The committed WAL's view of each page. Populated at open by
    /// replaying the log, and kept in sync with each successful commit.
    /// Layered on top of `on_disk` for read resolution.
    wal_cache: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Write-ahead log sidecar. Present on a read-write Pager; `None`
    /// on a read-only Pager that either found no WAL on disk or doesn't
    /// retain the handle after initial replay. Reads consult
    /// `wal_cache` (already populated at open) either way.
    wal: Option<Wal>,
    /// `ReadWrite` allows `commit` / `checkpoint`; `ReadOnly` rejects
    /// them with a typed error. `stage_page` stays open on both modes
    /// (it only touches the in-memory `staged` map) — any staged bytes
    /// simply never reach disk on a read-only Pager because `commit` is
    /// the gate.
    access_mode: AccessMode,
}

impl Pager {
    /// Opens an existing database file for read-write access. Shorthand
    /// for [`Pager::open_with_mode`] with [`AccessMode::ReadWrite`].
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_mode(path, AccessMode::ReadWrite)
    }

    /// Opens an existing database file for read-only access — takes
    /// a shared advisory lock that coexists with other readers but is
    /// excluded by any writer. `commit` and `checkpoint` return a clean
    /// error rather than panic; `stage_page` stays a no-op-to-disk
    /// (bytes sit in the in-memory `staged` map that `commit` would
    /// have drained).
    ///
    /// If the WAL sidecar doesn't exist, the open succeeds with an
    /// empty `wal_cache` — a read-only caller can't materialize a
    /// sidecar on its own, and a DB that never had WAL writes is fine
    /// to read straight from the main file.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        Self::open_with_mode(path, AccessMode::ReadOnly)
    }

    /// Opens an existing database file with the given access mode.
    /// Loads every main-file page into `on_disk`, then opens the WAL
    /// sidecar (read-only mode uses a shared lock and skips sidecar
    /// creation; read-write creates the sidecar if missing) and layers
    /// committed frames into `wal_cache`.
    pub fn open_with_mode(path: &Path, mode: AccessMode) -> Result<Self> {
        let file = match mode {
            AccessMode::ReadWrite => OpenOptions::new().read(true).write(true).open(path)?,
            AccessMode::ReadOnly => OpenOptions::new().read(true).open(path)?,
        };
        acquire_lock(&file, path, mode)?;
        let mut storage = FileStorage::new(file);
        let mut header = storage.read_header()?;

        let mut on_disk = HashMap::with_capacity(header.page_count.saturating_sub(1) as usize);
        // page 0 is the header itself; regular pages live at 1..page_count.
        for page_num in 1..header.page_count {
            let buf = read_raw_page(&mut storage, page_num)?;
            on_disk.insert(page_num, buf);
        }

        let wal_path = wal_path_for(path);
        let (wal_handle, wal_cache) = match mode {
            AccessMode::ReadWrite => {
                // Create the sidecar if it's missing — a pre-Phase-4c
                // file or a DB that was hand-deleted down to just the
                // main file both need a fresh empty WAL to be writable.
                let mut wal = if wal_path.exists() {
                    Wal::open_with_mode(&wal_path, mode)?
                } else {
                    Wal::create(&wal_path)?
                };
                let mut cache: HashMap<u32, Box<[u8; PAGE_SIZE]>> = HashMap::new();
                wal.load_committed_into(&mut cache)?;
                (Some(wal), cache)
            }
            AccessMode::ReadOnly => {
                // Read-only mustn't create files. If the sidecar is
                // absent, treat the WAL as empty and serve reads from
                // the main file alone.
                if wal_path.exists() {
                    let mut wal = Wal::open_with_mode(&wal_path, mode)?;
                    let mut cache: HashMap<u32, Box<[u8; PAGE_SIZE]>> = HashMap::new();
                    wal.load_committed_into(&mut cache)?;
                    // We don't need to retain the WAL handle in
                    // read-only mode — the cache is all reads need and
                    // dropping the handle releases the shared lock on
                    // the sidecar early. Keep it, though, so the lock
                    // spans the whole Pager lifetime: a checkpointer
                    // process grabbing LOCK_EX on the WAL while our
                    // reader still has wal_cache loaded would be
                    // correct for reads but surprising semantically.
                    (Some(wal), cache)
                } else {
                    (None, HashMap::new())
                }
            }
        };

        // If the WAL committed a new page 0, that frame's body is the
        // up-to-date header — decode it and let it override what the
        // main file's stale header says.
        if let Some(page0) = wal_cache.get(&0) {
            header = decode_header(page0.as_ref())?;
        } else if let Some(w) = wal_handle.as_ref()
            && let Some(committed_pc) = w.last_commit_page_count()
        {
            // Belt-and-suspenders: even if the latest commit frame didn't
            // land on page 0 (shouldn't happen under the current commit
            // layout, but keeps us correct if that ever changes), trust
            // its page count.
            header.page_count = committed_pc;
        }

        Ok(Self {
            storage,
            current_header: header,
            on_disk,
            staged: HashMap::new(),
            wal_cache,
            wal: wal_handle,
            access_mode: mode,
        })
    }

    /// Creates a fresh database file. Page 0 is the header; page 1 is an
    /// empty `TableLeaf` that serves as the initial `sqlrite_master` root
    /// (zero rows, no user tables yet). A matching empty WAL sidecar is
    /// created alongside it — any pre-existing WAL at the target path is
    /// truncated.
    pub fn create(path: &Path) -> Result<Self> {
        use crate::sql::pager::page::{PAGE_HEADER_SIZE, PageType};
        use crate::sql::pager::table_page::TablePage;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        acquire_lock(&file, path, AccessMode::ReadWrite)?;
        let mut storage = FileStorage::new(file);

        let empty_master = TablePage::empty();
        let mut page1 = Box::new([0u8; PAGE_SIZE]);
        page1[0] = PageType::TableLeaf as u8;
        page1[1..5].copy_from_slice(&0u32.to_le_bytes());
        page1[5..7].copy_from_slice(&0u16.to_le_bytes());
        page1[PAGE_HEADER_SIZE..].copy_from_slice(empty_master.as_bytes());

        let header = DbHeader {
            page_count: 2,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        };

        // Write the file synchronously so the initial create is durable and
        // subsequent `Pager::open` calls see a valid header + page 1.
        storage.seek_to(0)?;
        storage.write_all(&encode_header(&header))?;
        storage.write_all(page1.as_ref())?;
        storage.flush()?;

        // Sidecar WAL — fresh, no frames yet.
        let wal = Wal::create(&wal_path_for(path))?;

        let mut on_disk = HashMap::new();
        on_disk.insert(1, page1);

        Ok(Self {
            storage,
            current_header: header,
            on_disk,
            staged: HashMap::new(),
            wal_cache: HashMap::new(),
            wal: Some(wal),
            access_mode: AccessMode::ReadWrite,
        })
    }

    pub fn header(&self) -> DbHeader {
        self.current_header
    }

    /// Returns the mode this Pager was opened in. Callers can use this
    /// to bail out of a write path earlier than the Pager itself would.
    pub fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    fn require_writable(&self, op: &'static str) -> Result<()> {
        if self.access_mode == AccessMode::ReadOnly {
            return Err(SQLRiteError::General(format!(
                "cannot {op}: database is opened read-only"
            )));
        }
        Ok(())
    }

    /// Reads a page, preferring staged content, then the WAL-committed
    /// overlay, then the frozen main-file snapshot. Returns `None` for
    /// pages beyond the current page count (pages that have been logically
    /// truncated by a shrink-commit stay in `on_disk` until checkpoint,
    /// but a bounds check hides them from readers).
    pub fn read_page(&self, page_num: u32) -> Option<&[u8; PAGE_SIZE]> {
        // Staged pages are "the future" and should always shadow everything
        // else, even pages we're about to extend beyond the old page count.
        if let Some(b) = self.staged.get(&page_num) {
            return Some(b);
        }
        // A page that's been logically dropped shouldn't be readable even
        // if its bytes linger in on_disk until the next checkpoint.
        if page_num >= self.current_header.page_count {
            return None;
        }
        if let Some(b) = self.wal_cache.get(&page_num) {
            return Some(b.as_ref());
        }
        self.on_disk.get(&page_num).map(|b| b.as_ref())
    }

    /// Queues `bytes` as the new content of page `page_num`. The write only
    /// reaches disk when `commit` is called.
    pub fn stage_page(&mut self, page_num: u32, bytes: [u8; PAGE_SIZE]) {
        self.staged.insert(page_num, Box::new(bytes));
    }

    /// Discards all staged pages. Useful when beginning a new full re-save
    /// from scratch; the higher layer can also just overwrite pages without
    /// clearing since `stage_page` replaces.
    pub fn clear_staged(&mut self) {
        self.staged.clear();
    }

    /// Commits all staged pages into the WAL. Only pages whose bytes differ
    /// from the effective committed state (wal_cache layered on on_disk)
    /// produce frames. A final commit frame carries the new page 0 (encoded
    /// header) and is fsync'd; that seals the transaction. The main file is
    /// left untouched — it only changes when the checkpointer (Phase 4d)
    /// runs.
    ///
    /// Returns the number of dirty *data* frames appended (excluding the
    /// implicit page-0 commit frame that's always written).
    pub fn commit(&mut self, new_header: DbHeader) -> Result<usize> {
        self.require_writable("commit")?;
        let wal = self
            .wal
            .as_mut()
            .expect("read-write Pager must carry a WAL handle");

        // Decide which staged pages carry bytes that aren't already live.
        // Effective committed state = wal_cache overlaid on on_disk.
        let staged = std::mem::take(&mut self.staged);
        let mut dirty: Vec<(u32, Box<[u8; PAGE_SIZE]>)> = staged
            .into_iter()
            .filter(|(n, bytes)| {
                let existing = self.wal_cache.get(n).or_else(|| self.on_disk.get(n));
                match existing {
                    Some(e) => e.as_ref() != bytes.as_ref(),
                    None => true,
                }
            })
            .collect();
        // Append in ascending page order so the log replays deterministically
        // and sequential reads during checkpoint stay sequential.
        dirty.sort_by_key(|(n, _)| *n);
        let writes = dirty.len();

        for (n, bytes) in &dirty {
            wal.append_frame(*n, bytes.as_ref(), None)?;
        }

        // Seal the transaction. The commit frame carries the new page 0
        // (encoded header) in its body and the new page count in its
        // commit_page_count field — together they're the single atomic
        // record that says "this is the new committed state".
        let page0 = encode_header(&new_header);
        wal.append_frame(0, &page0, Some(new_header.page_count))?;
        let frame_count_after_commit = wal.frame_count();

        // Promote every frame we just wrote into wal_cache so subsequent
        // reads see the latest committed bytes without touching the WAL.
        for (n, bytes) in dirty {
            self.wal_cache.insert(n, bytes);
        }
        self.wal_cache.insert(0, Box::new(page0));

        self.current_header = new_header;

        // Keep the WAL bounded. Under write-heavy load, un-flushed frames
        // accumulate; past the threshold we fold them back into the main
        // file opportunistically so open doesn't have to replay an
        // arbitrarily long log on the next start.
        if frame_count_after_commit >= AUTO_CHECKPOINT_THRESHOLD_FRAMES {
            self.checkpoint()?;
        }

        Ok(writes)
    }

    /// Folds all WAL-resident pages back into the main file and truncates
    /// the WAL. Returns the number of data pages written to the main
    /// file (excludes the header).
    ///
    /// **Crash safety — two fsync barriers.** The main-file writes happen
    /// in two phases separated by a barrier, matching SQLite's checkpoint
    /// ordering:
    ///
    /// 1. Write every `wal_cache` data page at its `page_num * PAGE_SIZE`
    ///    offset in the main file.
    /// 2. **`fsync`** — force those data pages to stable storage *before*
    ///    the header publishes the new state. Without this barrier, a
    ///    filesystem or disk-cache reordering could land the header first,
    ///    leaving a main file that claims "N pages" over stale data.
    /// 3. Rewrite the main-file header at offset 0. This is the
    ///    checkpoint's "commit point" — after it hits disk the main file
    ///    alone tells the truth.
    /// 4. `set_len` shrinks the tail if `page_count` dropped.
    /// 5. **`fsync`** — force the header + set_len durable.
    /// 6. `Wal::truncate` resets the sidecar (rolls salt, writes new
    ///    header, fsync). Running this *after* the main file is fully
    ///    durable means a crash between 5 and 6 leaves a stale WAL over a
    ///    current main file; readers still see the right bytes because
    ///    wal_cache (replayed from the stale WAL on next open) would be
    ///    byte-identical to what's in the main file. A retry of
    ///    `checkpoint` then truncates cleanly.
    ///
    /// A crash between 1 and 2 can leave partial data-page writes, but
    /// since the header hasn't moved yet, the main file still reads as
    /// its pre-checkpoint self — the WAL is intact and authoritative,
    /// and a retry rewrites the same bytes.
    pub fn checkpoint(&mut self) -> Result<usize> {
        self.require_writable("checkpoint")?;
        // `require_writable` already guaranteed we're ReadWrite; in
        // ReadWrite mode `wal` is always `Some` (it's only `None` for
        // ReadOnly opens of a DB that had no sidecar on disk).
        let wal_frame_count = self.wal.as_ref().map(|w| w.frame_count()).unwrap_or(0);

        // Nothing to flush? Skip the fsyncs and get out.
        if wal_frame_count == 0 && self.wal_cache.is_empty() {
            return Ok(0);
        }

        // Step 1 — write every WAL-resident data page to the main file.
        // Page 0 (header) is handled separately via write_header, and any
        // pages past the new page count are skipped here (set_len will
        // drop them when the file shrinks).
        let page_count = self.current_header.page_count;
        let mut pages: Vec<u32> = self
            .wal_cache
            .keys()
            .copied()
            .filter(|&n| n != 0 && n < page_count)
            .collect();
        pages.sort_unstable();
        let written = pages.len();
        for page_num in &pages {
            let bytes = self
                .wal_cache
                .get(page_num)
                .expect("iterated key must resolve");
            self.storage
                .seek_to((*page_num as u64) * (PAGE_SIZE as u64))?;
            self.storage.write_all(bytes.as_ref())?;
        }

        // Step 2 — first durability barrier. Data pages must hit stable
        // storage before the header publishes the new page count /
        // schema root, or a reordered writeback could expose a
        // half-migrated file on crash.
        if written > 0 {
            self.storage.flush()?;
        }

        // Step 3 — rewrite the main-file header. This is the checkpoint's
        // atomic record.
        self.storage.write_header(&self.current_header)?;

        // Step 4 — shrink the main file if the committed page count is
        // smaller than what the file physically holds.
        self.storage.truncate_to_pages(page_count)?;

        // Step 5 — second durability barrier. Makes header + set_len
        // durable together before we touch the WAL.
        self.storage.flush()?;

        // Step 6 — reset the WAL sidecar. Runs before the in-memory
        // cache swap so that if `wal.truncate` fails (disk full, EIO)
        // we leave the in-memory state untouched rather than having
        // wal_cache empty + on_disk updated + WAL un-truncated, which
        // the Pager can't easily recover from on its own. Here a
        // failure means the main file is already consistent on disk
        // (steps 2 + 5 fsynced it); we just leave the stale WAL in
        // place for the next checkpoint attempt.
        self.wal
            .as_mut()
            .expect("read-write Pager must carry a WAL handle")
            .truncate()?;

        // Promote wal_cache into on_disk and drop everything that's no
        // longer live. Page 0 is special — it's never materialized in
        // on_disk (we read it lazily via storage.read_header on open).
        for (n, bytes) in self.wal_cache.drain().filter(|(n, _)| *n != 0) {
            if n < page_count {
                self.on_disk.insert(n, bytes);
            }
        }
        self.on_disk.retain(|&n, _| n < page_count);

        Ok(written)
    }
}

fn read_raw_page(storage: &mut FileStorage, page_num: u32) -> Result<Box<[u8; PAGE_SIZE]>> {
    storage.seek_to((page_num as u64) * (PAGE_SIZE as u64))?;
    let mut buf = Box::new([0u8; PAGE_SIZE]);
    storage.read_exact(buf.as_mut())?;
    Ok(buf)
}

impl std::fmt::Debug for Pager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pager")
            .field("access_mode", &self.access_mode)
            .field("page_count", &self.current_header.page_count)
            .field("schema_root_page", &self.current_header.schema_root_page)
            .field("cached_pages", &self.on_disk.len())
            .field("staged_pages", &self.staged.len())
            .field("wal_pages", &self.wal_cache.len())
            .field(
                "wal_frames",
                &self.wal.as_ref().map(|w| w.frame_count()).unwrap_or(0),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-pager-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    /// Remove both the main file and its `-wal` sidecar — leaving either
    /// behind can destabilize later test runs on the same tmp dir.
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(wal_path_for(path));
    }

    fn make_page(first_byte: u8) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        buf[0] = first_byte;
        buf
    }

    #[test]
    fn create_then_open_round_trips() {
        let path = tmp_path("create_open");
        {
            let p = Pager::create(&path).unwrap();
            assert_eq!(p.header().page_count, 2);
            assert_eq!(p.header().schema_root_page, 1);
        }
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 2);
        cleanup(&path);
    }

    #[test]
    fn create_spawns_wal_sidecar() {
        // Phase 4c: `Pager::create` must produce an empty WAL sidecar
        // alongside the main file so the first commit has somewhere to
        // append frames.
        use crate::sql::pager::wal::WAL_HEADER_SIZE;
        let path = tmp_path("wal_sidecar");
        let _p = Pager::create(&path).unwrap();
        let wal = wal_path_for(&path);
        assert!(wal.exists(), "WAL sidecar should exist after create");
        // An empty WAL is just its header.
        let len = std::fs::metadata(&wal).unwrap().len();
        assert_eq!(
            len, WAL_HEADER_SIZE as u64,
            "fresh WAL should be header-only"
        );
        cleanup(&path);
    }

    #[test]
    fn commit_writes_only_dirty_pages() {
        let path = tmp_path("diff");
        let mut p = Pager::create(&path).unwrap();

        // Initial state: page 1 is the empty-catalog schema page.
        // Stage three "table-data" pages.
        p.stage_page(2, make_page(0xAA));
        p.stage_page(3, make_page(0xBB));
        p.stage_page(4, make_page(0xCC));
        let writes = p
            .commit(DbHeader {
                page_count: 5,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        // 3 dirty data pages (pages 2, 3, 4). The page-0 commit frame is
        // implicit and not counted.
        assert_eq!(writes, 3);

        // Re-stage the same bytes for pages 2 and 3, and changed bytes for 4.
        p.stage_page(2, make_page(0xAA));
        p.stage_page(3, make_page(0xBB));
        p.stage_page(4, make_page(0xDD));
        let writes = p
            .commit(DbHeader {
                page_count: 5,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        assert_eq!(writes, 1, "only the changed page should have been written");

        // Reopen and confirm the content is as expected. The bytes live in
        // the WAL — the main file still has the empty init state — so this
        // also verifies the WAL-replay path.
        drop(p);
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.read_page(2).unwrap()[0], 0xAA);
        assert_eq!(p2.read_page(3).unwrap()[0], 0xBB);
        assert_eq!(p2.read_page(4).unwrap()[0], 0xDD);

        cleanup(&path);
    }

    #[test]
    fn second_pager_on_same_file_is_rejected() {
        // Phase 4a regression: two simultaneous read-write Pagers against
        // the same file used to silently race. Now the second one must
        // error out. Phase 4e reworded the lock-contention message; the
        // stable substring we assert on is "in use".
        let path = tmp_path("lock_contention");
        let _first = Pager::create(&path).unwrap();

        let second = Pager::open(&path);
        assert!(second.is_err(), "expected lock-contention error, got Ok");
        let msg = format!("{}", second.unwrap_err());
        assert!(
            msg.contains("in use"),
            "error message should signal lock contention; got: {msg}"
        );

        // After the first Pager drops, both the main-file and WAL locks
        // release and a fresh open succeeds — confirming the locks are
        // tied to Pager lifetime, not leaked across instances.
        drop(_first);
        let third = Pager::open(&path);
        assert!(third.is_ok(), "reopen after drop should succeed: {third:?}");

        cleanup(&path);
    }

    #[test]
    fn commit_leaves_main_file_untouched_and_shrink_hides_dropped_pages() {
        // Phase 4c: commits now go to the WAL; the main file stays frozen
        // until the checkpointer runs. Page-count shrinks still hide the
        // logically-dropped pages from readers (via a bounds check in
        // read_page) even though their bytes linger in the main file.
        let path = tmp_path("shrink");
        let mut p = Pager::create(&path).unwrap();
        let main_size_after_create = std::fs::metadata(&path).unwrap().len();

        p.stage_page(2, make_page(1));
        p.stage_page(3, make_page(2));
        p.stage_page(4, make_page(3));
        p.commit(DbHeader {
            page_count: 5,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();

        // Main file unchanged: the page-2..4 bytes went into the WAL.
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            main_size_after_create,
            "main file must stay frozen across commits"
        );
        // WAL, however, has grown: 3 dirty frames + 1 commit frame.
        let wal_size = std::fs::metadata(wal_path_for(&path)).unwrap().len();
        assert!(
            wal_size > 32,
            "WAL should contain frames after a commit, got size {wal_size}"
        );

        // Shrink to 3 pages.
        p.commit(DbHeader {
            page_count: 3,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();

        // Page 4 is now logically dropped — read_page hides it.
        assert!(p.read_page(4).is_none());
        // And page 2 is still visible under the new count.
        assert_eq!(p.read_page(2).unwrap()[0], 1);

        // Reopen confirms the committed page count survives.
        drop(p);
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 3);
        assert!(p2.read_page(4).is_none());

        cleanup(&path);
    }

    #[test]
    fn wal_replay_on_reopen_restores_committed_state() {
        // End-to-end: do a commit, close, reopen, and verify every staged
        // page is visible. This is the core Phase 4c promise — committed
        // writes survive a close/reopen via the WAL even though the main
        // file wasn't touched.
        let path = tmp_path("wal_replay");
        {
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0x11));
            p.stage_page(3, make_page(0x22));
            p.commit(DbHeader {
                page_count: 4,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }

        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 4);
        assert_eq!(p2.read_page(2).unwrap()[0], 0x11);
        assert_eq!(p2.read_page(3).unwrap()[0], 0x22);
        cleanup(&path);
    }

    #[test]
    fn orphan_dirty_frame_in_wal_is_invisible_on_reopen() {
        // Simulates a crash between a dirty frame being written and the
        // commit frame being appended. The Pager's open-time WAL replay
        // should not surface the dirty bytes — reads must still return
        // the previous-committed content.
        let path = tmp_path("orphan_dirty");
        {
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0xCC));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }

        // Open the WAL directly and append a dirty frame for page 2 with
        // *different* bytes — no commit frame follows. A later
        // `Pager::open` must ignore this orphan frame.
        {
            let mut w = crate::sql::pager::wal::Wal::open(&wal_path_for(&path)).unwrap();
            let mut other = Box::new([0u8; PAGE_SIZE]);
            other[0] = 0x99;
            w.append_frame(2, &other, None).unwrap();
        }

        let p = Pager::open(&path).unwrap();
        assert_eq!(
            p.read_page(2).unwrap()[0],
            0xCC,
            "orphan dirty frame must not shadow the last committed page"
        );
        cleanup(&path);
    }

    #[test]
    fn two_commits_only_stage_the_delta() {
        // Diffing vs. the effective state (wal_cache + on_disk) means a
        // repeated identical commit writes zero dirty data frames. A commit
        // frame is still appended, but that's implicit.
        let path = tmp_path("diff_delta");
        let mut p = Pager::create(&path).unwrap();
        p.stage_page(2, make_page(0x77));
        let first = p
            .commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        assert_eq!(first, 1);

        // Stage the same byte again.
        p.stage_page(2, make_page(0x77));
        let second = p
            .commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        assert_eq!(second, 0, "no data frames should be re-appended");

        cleanup(&path);
    }

    // -------------------------------------------------------------------
    // Phase 4d — Checkpointer
    // -------------------------------------------------------------------

    #[test]
    fn explicit_checkpoint_folds_wal_into_main_file_and_truncates_wal() {
        use crate::sql::pager::wal::WAL_HEADER_SIZE;
        let path = tmp_path("ckpt_explicit");
        let mut p = Pager::create(&path).unwrap();

        p.stage_page(2, make_page(0xA1));
        p.stage_page(3, make_page(0xB2));
        p.commit(DbHeader {
            page_count: 4,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();

        // Pre-checkpoint: WAL has frames, main file is still the initial size.
        let wal = wal_path_for(&path);
        assert!(std::fs::metadata(&wal).unwrap().len() > WAL_HEADER_SIZE as u64);

        let written = p.checkpoint().unwrap();
        assert_eq!(written, 2, "both data pages should flush to main file");

        // WAL is now empty (just the header) with a rolled salt + bumped seq.
        let wal_len = std::fs::metadata(&wal).unwrap().len();
        assert_eq!(wal_len, WAL_HEADER_SIZE as u64);

        // Main file is exactly page_count pages long.
        let main_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(main_len, 4 * PAGE_SIZE as u64);

        // Drop + reopen: main file alone must carry the latest content.
        // (The WAL is empty, so any surviving correctness is on the main file.)
        drop(p);
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 4);
        assert_eq!(p2.read_page(2).unwrap()[0], 0xA1);
        assert_eq!(p2.read_page(3).unwrap()[0], 0xB2);

        cleanup(&path);
    }

    #[test]
    fn checkpoint_is_idempotent() {
        // Two back-to-back checkpoints: the second must be a no-op and
        // must not error. (The first drains wal_cache; the second sees
        // nothing to do.)
        let path = tmp_path("ckpt_idempotent");
        let mut p = Pager::create(&path).unwrap();
        p.stage_page(2, make_page(0x42));
        p.commit(DbHeader {
            page_count: 3,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();

        let first = p.checkpoint().unwrap();
        assert_eq!(first, 1);
        let second = p.checkpoint().unwrap();
        assert_eq!(second, 0, "second checkpoint should be a no-op");

        cleanup(&path);
    }

    #[test]
    fn checkpoint_with_shrink_truncates_main_file() {
        // Grow to 5 pages, checkpoint; shrink to 3 pages, checkpoint.
        // After the second checkpoint the main file must physically
        // be 3 * PAGE_SIZE bytes — previous-tail pages are gone.
        let path = tmp_path("ckpt_shrink");
        let mut p = Pager::create(&path).unwrap();
        p.stage_page(2, make_page(1));
        p.stage_page(3, make_page(2));
        p.stage_page(4, make_page(3));
        p.commit(DbHeader {
            page_count: 5,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();
        p.checkpoint().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            5 * PAGE_SIZE as u64
        );

        // Shrink.
        p.commit(DbHeader {
            page_count: 3,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();
        p.checkpoint().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            3 * PAGE_SIZE as u64,
            "main file should shrink to new page_count after checkpoint"
        );
        // Page 4 is gone both physically and logically.
        assert!(p.read_page(4).is_none());

        cleanup(&path);
    }

    #[test]
    fn auto_checkpoint_fires_past_frame_threshold() {
        // Do just enough commits to push the WAL past
        // AUTO_CHECKPOINT_THRESHOLD_FRAMES. After the crossing commit,
        // the WAL should be back to header-only (auto-checkpoint ran)
        // while the main file carries every committed byte.
        use crate::sql::pager::wal::WAL_HEADER_SIZE;
        let path = tmp_path("ckpt_auto");
        let mut p = Pager::create(&path).unwrap();

        // Each commit appends: 1 dirty data frame + 1 commit frame for
        // page 0 = 2 frames. So ceil(THRESHOLD / 2) commits gets us past
        // the trigger.
        let commits_needed = AUTO_CHECKPOINT_THRESHOLD_FRAMES.div_ceil(2);
        for i in 0..commits_needed {
            p.stage_page(2, make_page((i & 0xff) as u8));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }

        // Auto-checkpoint must have fired at least once during that loop.
        let wal_len = std::fs::metadata(wal_path_for(&path)).unwrap().len();
        assert_eq!(
            wal_len, WAL_HEADER_SIZE as u64,
            "auto-checkpoint should have truncated the WAL"
        );

        // Last committed byte for page 2 is the latest (commits_needed - 1 & 0xff).
        let expected = ((commits_needed - 1) & 0xff) as u8;
        assert_eq!(p.read_page(2).unwrap()[0], expected);

        cleanup(&path);
    }

    // -------------------------------------------------------------------
    // Phase 4e — shared/exclusive lock modes
    // -------------------------------------------------------------------

    #[test]
    fn two_read_only_openers_coexist() {
        // Phase 4e: multiple read-only openers take shared locks and
        // must not exclude each other.
        let path = tmp_path("ro_coexist");
        {
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0x55));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }

        let reader1 = Pager::open_read_only(&path).unwrap();
        let reader2 = Pager::open_read_only(&path).unwrap();
        // Both see the committed content.
        assert_eq!(reader1.read_page(2).unwrap()[0], 0x55);
        assert_eq!(reader2.read_page(2).unwrap()[0], 0x55);
        assert_eq!(reader1.access_mode(), AccessMode::ReadOnly);

        cleanup(&path);
    }

    #[test]
    fn read_write_blocks_read_only_and_vice_versa() {
        // A live exclusive lock blocks a shared-lock open, and a live
        // shared lock blocks an exclusive-lock open. Both error messages
        // mention that the database is in use.
        let path = tmp_path("rw_vs_ro");
        let _writer = Pager::create(&path).unwrap();

        // Writer holds LOCK_EX — reader can't take LOCK_SH.
        let reader_attempt = Pager::open_read_only(&path);
        assert!(reader_attempt.is_err());
        let msg = format!("{}", reader_attempt.unwrap_err());
        assert!(
            msg.contains("locked for writing"),
            "read-only open while writer holds lock should mention writer; got: {msg}"
        );

        drop(_writer);

        // Now a reader comes in; a second read-write must be rejected.
        let _reader = Pager::open_read_only(&path).unwrap();
        let writer_attempt = Pager::open(&path);
        assert!(writer_attempt.is_err());
        let msg = format!("{}", writer_attempt.unwrap_err());
        assert!(
            msg.contains("in use"),
            "read-write open while reader holds lock should mention contention; got: {msg}"
        );

        cleanup(&path);
    }

    #[test]
    fn read_only_pager_rejects_mutations() {
        let path = tmp_path("ro_rejects");
        {
            // Seed with some content so an RO open has something to read.
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0x33));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }

        let mut ro = Pager::open_read_only(&path).unwrap();
        let commit_err = ro
            .commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap_err();
        assert!(
            format!("{commit_err}").contains("read-only"),
            "commit on RO pager should surface 'read-only'; got: {commit_err}"
        );
        let ckpt_err = ro.checkpoint().unwrap_err();
        assert!(
            format!("{ckpt_err}").contains("read-only"),
            "checkpoint on RO pager should surface 'read-only'; got: {ckpt_err}"
        );

        // Reads still work.
        assert_eq!(ro.read_page(2).unwrap()[0], 0x33);

        cleanup(&path);
    }

    #[test]
    fn read_only_open_without_wal_sidecar_succeeds() {
        // A file-backed DB whose -wal sidecar was deleted (or a Phase-
        // 4a-vintage file predating Phase 4c) must still be openable
        // read-only. The Pager serves reads straight from on_disk with
        // an empty wal_cache.
        let path = tmp_path("ro_no_wal");
        {
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0x44));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
            // Force the WAL into the main file before we nuke it.
            p.checkpoint().unwrap();
        }
        // Nuke the sidecar.
        std::fs::remove_file(wal_path_for(&path)).unwrap();

        let ro = Pager::open_read_only(&path).unwrap();
        assert_eq!(ro.read_page(2).unwrap()[0], 0x44);
        // No WAL materialized by a read-only open.
        assert!(!wal_path_for(&path).exists());
        cleanup(&path);
    }

    #[test]
    fn reopen_after_crash_between_data_write_and_header_write_recovers_via_wal() {
        // Simulates a crash between step 2 (data-page fsync) and step 3
        // (header write) of `checkpoint`: the main file has new data
        // pages but still carries the old header, AND the WAL still
        // holds every committed frame. Next open must reconstruct the
        // post-commit view via the WAL (wal_cache[0] overrides the stale
        // main-file header).
        use std::io::{Seek, SeekFrom, Write};

        let path = tmp_path("ckpt_crash_mid_flush");
        {
            let mut p = Pager::create(&path).unwrap();
            p.stage_page(2, make_page(0xEE));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
            // Manually write the committed page 2 into the main file at
            // offset 2*PAGE_SIZE to simulate the first half of a
            // checkpoint that only got as far as step 2. The header
            // stays at the pre-commit state (page_count=2 from create).
            // Drop the pager first so its exclusive lock releases.
        }
        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(2 * PAGE_SIZE as u64)).unwrap();
            f.write_all(&make_page(0xEE)).unwrap();
            f.sync_all().unwrap();
            // NB: we didn't extend the file past its original length in
            // the create-only state; the write_all grew it implicitly.
            // The header at offset 0 is still the original "page_count=2".
        }

        // Reopen. Main-file header says 2 pages; WAL replay should
        // override that to 3, and wal_cache[2] should shadow whatever
        // the main file now holds for page 2 (which happens to be the
        // same byte here — the point is the Pager doesn't depend on
        // that coincidence).
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 3);
        assert_eq!(p2.read_page(2).unwrap()[0], 0xEE);
        cleanup(&path);
    }

    #[test]
    fn auto_checkpoint_crosses_threshold_mid_loop() {
        // Pins the exact-threshold semantics: `commit` must trigger a
        // checkpoint as soon as the WAL's frame count hits the threshold,
        // not later. Catches a regression where someone accidentally
        // lowers it to `>` or bumps it into a different accounting.
        let path = tmp_path("ckpt_threshold_crossing");
        let mut p = Pager::create(&path).unwrap();
        let commits_to_cross = AUTO_CHECKPOINT_THRESHOLD_FRAMES.div_ceil(2);
        for i in 0..commits_to_cross - 1 {
            p.stage_page(2, make_page((i & 0xff) as u8));
            p.commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
                format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
            })
            .unwrap();
        }
        // One short of the threshold — WAL must not yet have been flushed.
        let pre = std::fs::metadata(wal_path_for(&path)).unwrap().len();
        assert!(
            pre > crate::sql::pager::wal::WAL_HEADER_SIZE as u64,
            "WAL should still carry frames right before the crossing commit"
        );

        // The crossing commit: this one's the trigger.
        p.stage_page(2, make_page(0xff));
        p.commit(DbHeader {
            page_count: 3,
            schema_root_page: 1,
            format_version: crate::sql::pager::header::FORMAT_VERSION_BASELINE,
        })
        .unwrap();
        let post = std::fs::metadata(wal_path_for(&path)).unwrap().len();
        assert_eq!(
            post,
            crate::sql::pager::wal::WAL_HEADER_SIZE as u64,
            "WAL must be header-only right after the threshold-crossing commit"
        );

        cleanup(&path);
    }
}
