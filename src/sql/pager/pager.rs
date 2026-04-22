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
//! This matters because higher layers re-serialize the entire database on
//! every auto-save. Without the diff, even a one-row UPDATE would append a
//! frame for every page of every table. With the diff, unchanged tables —
//! whose encoded pages hash identically across saves — simply stay out of
//! the WAL.
//!
//! **Locking (Phase 4a).** Every `Pager` takes an exclusive advisory lock
//! on its main file and on its WAL sidecar (`fs2::FileExt::try_lock_exclusive`).
//! If another SQLRite process is already holding either lock, `open` /
//! `create` return a clean `database is already opened by another process`
//! error instead of silently racing. Both locks are tied to their file
//! descriptors and release automatically when the `Pager` drops. This is
//! exclusive — one writer, no concurrent readers. Phase 4e upgrades to
//! shared/exclusive modes.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::file::FileStorage;
use crate::sql::pager::header::{DbHeader, decode_header, encode_header};
use crate::sql::pager::page::PAGE_SIZE;
use crate::sql::pager::wal::Wal;

/// Returns the WAL sidecar path for a main `.sqlrite` file: appends
/// the `-wal` suffix to the full path (so `foo.sqlrite` pairs with
/// `foo.sqlrite-wal`). Matches SQLite's convention.
fn wal_path_for(main: &Path) -> PathBuf {
    let mut os = main.as_os_str().to_owned();
    os.push("-wal");
    PathBuf::from(os)
}

/// Acquires an exclusive advisory lock on `file`, mapping the OS-level
/// "lock held" error to a clean SQLRite error that mentions the path. On
/// Unix this is `flock(LOCK_EX | LOCK_NB)`; on Windows, `LockFileEx` with
/// `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`.
fn lock_exclusive(file: &File, path: &Path) -> Result<()> {
    file.try_lock_exclusive().map_err(|e| {
        SQLRiteError::General(format!(
            "database '{}' is already opened by another process ({e})",
            path.display()
        ))
    })
}

pub struct Pager {
    /// Main-file I/O handle. After open/create, regular commits leave it
    /// alone; only the Phase 4d checkpointer will reach back into it to
    /// flush WAL frames and truncate the tail.
    #[allow(dead_code)]
    storage: FileStorage,
    current_header: DbHeader,
    /// Byte snapshot of the main file as last checkpointed. Never changes
    /// during regular commits — only the Phase 4d checkpointer mutates it
    /// (and the corresponding main file) by flushing WAL frames here.
    on_disk: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Pages queued for the next commit. `commit` drains this.
    staged: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// The committed WAL's view of each page. Populated at open by
    /// replaying the log, and kept in sync with each successful commit.
    /// Layered on top of `on_disk` for read resolution.
    wal_cache: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Write-ahead log sidecar. Holds every committed change since the
    /// last checkpoint. Drops automatically with the `Pager`, releasing
    /// its exclusive file lock.
    wal: Wal,
}

impl Pager {
    /// Opens an existing database file and loads every page into the
    /// `on_disk` snapshot, then opens (or creates) its `-wal` sidecar and
    /// layers any committed frames into `wal_cache`. Returns `Err` if the
    /// header is invalid, or if another SQLRite process already holds an
    /// exclusive lock on either file.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        lock_exclusive(&file, path)?;
        let mut storage = FileStorage::new(file);
        let mut header = storage.read_header()?;

        let mut on_disk = HashMap::with_capacity(header.page_count.saturating_sub(1) as usize);
        // page 0 is the header itself; regular pages live at 1..page_count.
        for page_num in 1..header.page_count {
            let buf = read_raw_page(&mut storage, page_num)?;
            on_disk.insert(page_num, buf);
        }

        // Open the WAL sidecar — create one if it doesn't exist yet, so a
        // pre-WAL database file (Phase 4a / earlier) naturally gets a fresh
        // empty WAL on first open under Phase 4c.
        let wal_path = wal_path_for(path);
        let mut wal = if wal_path.exists() {
            Wal::open(&wal_path)?
        } else {
            Wal::create(&wal_path)?
        };

        let mut wal_cache: HashMap<u32, Box<[u8; PAGE_SIZE]>> = HashMap::new();
        wal.load_committed_into(&mut wal_cache)?;

        // If the WAL committed a new page 0, that frame's body is the
        // up-to-date header — decode it and let it override what the main
        // file's stale header says.
        if let Some(page0) = wal_cache.get(&0) {
            header = decode_header(page0.as_ref())?;
        } else if let Some(committed_pc) = wal.last_commit_page_count() {
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
            wal,
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
        lock_exclusive(&file, path)?;
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
            wal,
        })
    }

    pub fn header(&self) -> DbHeader {
        self.current_header
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
        // Decide which staged pages carry bytes that aren't already live.
        // Effective committed state = wal_cache overlaid on on_disk.
        let staged = std::mem::take(&mut self.staged);
        let mut dirty: Vec<(u32, Box<[u8; PAGE_SIZE]>)> = staged
            .into_iter()
            .filter(|(n, bytes)| {
                let existing = self
                    .wal_cache
                    .get(n)
                    .or_else(|| self.on_disk.get(n));
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
            self.wal
                .append_frame(*n, bytes.as_ref(), None)?;
        }

        // Seal the transaction. The commit frame carries the new page 0
        // (encoded header) in its body and the new page count in its
        // commit_page_count field — together they're the single atomic
        // record that says "this is the new committed state".
        let page0 = encode_header(&new_header);
        self.wal
            .append_frame(0, &page0, Some(new_header.page_count))?;

        // Promote every frame we just wrote into wal_cache so subsequent
        // reads see the latest committed bytes without touching the WAL.
        for (n, bytes) in dirty {
            self.wal_cache.insert(n, bytes);
        }
        self.wal_cache.insert(0, Box::new(page0));

        self.current_header = new_header;
        Ok(writes)
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
            .field("page_count", &self.current_header.page_count)
            .field("schema_root_page", &self.current_header.schema_root_page)
            .field("cached_pages", &self.on_disk.len())
            .field("staged_pages", &self.staged.len())
            .field("wal_pages", &self.wal_cache.len())
            .field("wal_frames", &self.wal.frame_count())
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
        // Phase 4a regression: two simultaneous Pagers against the same
        // file used to silently race. Now the second one must error out
        // with a "already opened by another process" message.
        let path = tmp_path("lock_contention");
        let _first = Pager::create(&path).unwrap();

        let second = Pager::open(&path);
        assert!(second.is_err(), "expected lock-contention error, got Ok");
        let msg = format!("{}", second.unwrap_err());
        assert!(
            msg.contains("already opened by another process"),
            "error message should mention lock contention; got: {msg}"
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
            })
            .unwrap();
        }

        // Open the WAL directly and append a dirty frame for page 2 with
        // *different* bytes — no commit frame follows. A later
        // `Pager::open` must ignore this orphan frame.
        {
            let mut w =
                crate::sql::pager::wal::Wal::open(&wal_path_for(&path)).unwrap();
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
            })
            .unwrap();
        assert_eq!(first, 1);

        // Stage the same byte again.
        p.stage_page(2, make_page(0x77));
        let second = p
            .commit(DbHeader {
                page_count: 3,
                schema_root_page: 1,
            })
            .unwrap();
        assert_eq!(second, 0, "no data frames should be re-appended");

        cleanup(&path);
    }
}
