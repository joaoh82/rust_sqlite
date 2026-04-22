//! Long-lived page cache + diffing commits.
//!
//! A `Pager` wraps an open `.sqlrite` file and tracks two maps of page bytes:
//!
//! - `on_disk`: a snapshot of what we believe is already written to the file
//!   (populated on `open` by reading every page, and updated after each
//!   successful `commit`).
//! - `staged`:  pages queued for the next commit, not yet written.
//!
//! On `commit` we compare each staged page against the `on_disk` snapshot and
//! only issue a write if the bytes actually differ. The header is always
//! rewritten (it's cheap and usually does change: at minimum the page count).
//!
//! This matters because higher layers re-serialize the entire database on
//! every auto-save. Without the diff, even a one-row UPDATE rewrites every
//! page of every table. With the diff, unchanged tables — whose bincode blob
//! hashes identically across saves — simply stay on disk.
//!
//! **Locking (Phase 4a).** Every `Pager` takes an exclusive advisory lock
//! on its backing file (`fs2::FileExt::try_lock_exclusive`). If another
//! SQLRite process is already holding the lock, `open` / `create` return a
//! clean `database is already opened by another process` error instead of
//! silently racing. The lock is tied to the file descriptor and released
//! automatically when the `Pager` drops. This is exclusive — one writer,
//! no concurrent readers. Phase 4e upgrades to shared/exclusive modes
//! once WAL is in.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt;

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::file::FileStorage;
use crate::sql::pager::header::{DbHeader, encode_header};
use crate::sql::pager::page::PAGE_SIZE;

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
    storage: FileStorage,
    current_header: DbHeader,
    /// Byte snapshot of every page currently on disk. Kept in lockstep with
    /// what the file contains after each `commit`.
    on_disk: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    /// Pages queued for the next commit. `commit` drains this.
    staged: HashMap<u32, Box<[u8; PAGE_SIZE]>>,
}

impl Pager {
    /// Opens an existing database file and loads every page into the
    /// `on_disk` snapshot. Returns `Err` if the header is invalid, or
    /// if another SQLRite process already holds an exclusive lock on
    /// the file.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        lock_exclusive(&file, path)?;
        let mut storage = FileStorage::new(file);
        let header = storage.read_header()?;

        let mut on_disk = HashMap::with_capacity(header.page_count.saturating_sub(1) as usize);
        // page 0 is the header itself; regular pages live at 1..page_count.
        for page_num in 1..header.page_count {
            let buf = read_raw_page(&mut storage, page_num)?;
            on_disk.insert(page_num, buf);
        }

        Ok(Self {
            storage,
            current_header: header,
            on_disk,
            staged: HashMap::new(),
        })
    }

    /// Creates a fresh database file. Page 0 is the header; page 1 is an
    /// empty `TableLeaf` that serves as the initial `sqlrite_master` root
    /// (zero rows, no user tables yet).
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

        let mut on_disk = HashMap::new();
        on_disk.insert(1, page1);

        Ok(Self {
            storage,
            current_header: header,
            on_disk,
            staged: HashMap::new(),
        })
    }

    pub fn header(&self) -> DbHeader {
        self.current_header
    }

    /// Reads a page, preferring staged content, then the on-disk snapshot.
    /// Returns `None` for pages beyond the current page count.
    pub fn read_page(&self, page_num: u32) -> Option<&[u8; PAGE_SIZE]> {
        if let Some(b) = self.staged.get(&page_num) {
            return Some(b);
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

    /// Flushes staged pages to disk, writing only pages whose bytes differ
    /// from the `on_disk` snapshot. Always writes the header last. If the
    /// new page count is smaller than the old one, the file is truncated to
    /// release the unused tail.
    pub fn commit(&mut self, new_header: DbHeader) -> Result<usize> {
        // Decide which staged pages are actually dirty vs. no-op writes.
        let staged = std::mem::take(&mut self.staged);
        let mut dirty: Vec<(u32, Box<[u8; PAGE_SIZE]>)> = staged
            .into_iter()
            .filter(|(n, bytes)| match self.on_disk.get(n) {
                Some(existing) => existing.as_ref() != bytes.as_ref(),
                None => true,
            })
            .collect();
        // Write in ascending page order so the OS gets sequential I/O.
        dirty.sort_by_key(|(n, _)| *n);
        let writes = dirty.len();
        for (n, bytes) in &dirty {
            self.storage
                .seek_to((*n as u64) * (PAGE_SIZE as u64))?;
            self.storage.write_all(bytes.as_ref())?;
        }

        // Header write is always issued — it's one page and it's usually dirty.
        self.storage.write_header(&new_header)?;

        // Shrink the file if we now use fewer pages. Old tail pages would
        // otherwise be reachable via seek-beyond-end on next open.
        if new_header.page_count < self.current_header.page_count {
            self.storage.truncate_to_pages(new_header.page_count)?;
        }

        self.storage.flush()?;

        // Promote dirty writes into the on_disk snapshot.
        for (n, bytes) in dirty {
            self.on_disk.insert(n, bytes);
        }
        // Drop on_disk entries for pages that no longer exist.
        self.on_disk.retain(|&n, _| n < new_header.page_count);

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
        let _ = std::fs::remove_file(&path);
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
        // 3 dirty data pages (pages 2, 3, 4). Header always written on top.
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

        // Reopen and confirm the content is as expected.
        drop(p);
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.read_page(2).unwrap()[0], 0xAA);
        assert_eq!(p2.read_page(3).unwrap()[0], 0xBB);
        assert_eq!(p2.read_page(4).unwrap()[0], 0xDD);

        let _ = std::fs::remove_file(&path);
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

        // After the first Pager drops, the lock releases and a fresh
        // open succeeds — confirming the lock is tied to Pager lifetime,
        // not leaked across instances.
        drop(_first);
        let third = Pager::open(&path);
        assert!(third.is_ok(), "reopen after drop should succeed: {third:?}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn commit_truncates_file_when_page_count_shrinks() {
        let path = tmp_path("shrink");
        let mut p = Pager::create(&path).unwrap();
        p.stage_page(2, make_page(1));
        p.stage_page(3, make_page(2));
        p.stage_page(4, make_page(3));
        p.commit(DbHeader {
            page_count: 5,
            schema_root_page: 1,
        })
        .unwrap();

        let size_before = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size_before, 5 * PAGE_SIZE as u64);

        // Shrink to 3 pages.
        p.commit(DbHeader {
            page_count: 3,
            schema_root_page: 1,
        })
        .unwrap();
        let size_after = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size_after, 3 * PAGE_SIZE as u64);

        // Reopen confirms only pages 1 and 2 are present.
        drop(p);
        let p2 = Pager::open(&path).unwrap();
        assert_eq!(p2.header().page_count, 3);
        assert!(p2.read_page(4).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
