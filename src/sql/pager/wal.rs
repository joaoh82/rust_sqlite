//! Write-Ahead Log (WAL) file format.
//!
//! Phase 4b introduces the `.sqlrite-wal` sidecar file. Writes don't go to
//! the main `.sqlrite` file anymore once the WAL is wired in (Phase 4c);
//! instead they append **frames** to this log, and a periodic checkpoint
//! (Phase 4d) applies frames back into the main file.
//!
//! This module is the format layer — header, frame, codec, reader,
//! writer. It doesn't know anything about the `Pager` yet; that wiring is
//! the next slice.
//!
//! **On-disk layout**
//!
//! ```text
//!   byte 0..32   WAL header
//!                   0..8    magic "SQLRWAL\0"
//!                   8..12   format version (u32 LE) = 1
//!                  12..16   page size     (u32 LE) = 4096
//!                  16..20   salt          (u32 LE) — random on create,
//!                                                    re-rolled per checkpoint
//!                  20..24   checkpoint seq (u32 LE) — bumps per checkpoint
//!                  24..32   reserved / zero
//!
//!   byte 32..    sequence of frames, each `FRAME_SIZE` bytes:
//!                   0..4    page number           (u32 LE)
//!                   4..8    commit-page-count     (u32 LE)
//!                             0 = dirty frame (part of an open write)
//!                            >0 = commit frame; value = page count at commit
//!                   8..12   salt (u32 LE)         — copied from WAL header,
//!                                                    detects truncation / file swap
//!                  12..16   checksum (u32 LE)     — rolling sum over the
//!                                                    frame header bytes
//!                                                    [0..12] + the payload
//!                  16..16+PAGE_SIZE  page bytes
//! ```
//!
//! **Checksum.** A rolling `rotate_left(1) + byte` sum over the
//! concatenation of the frame's first 12 header bytes (page_num,
//! commit-page-count, salt) and its PAGE_SIZE body. Catches bit flips
//! and most multi-byte corruption without pulling in a dep. The 13th
//! through 16th header bytes (the checksum field itself) are excluded
//! from the computation, obviously.
//!
//! **Torn-write recovery.** On open, the reader walks frames from the
//! start and verifies each checksum. The first invalid or incomplete
//! frame marks where the WAL effectively ends; anything past it is
//! treated as if it doesn't exist. Callers learn what's committed vs
//! what's speculative from `Wal::last_commit_offset` / the `is_commit`
//! flag of each scanned frame.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::page::PAGE_SIZE;
use crate::sql::pager::pager::{AccessMode, acquire_lock};

pub const WAL_HEADER_SIZE: usize = 32;
pub const WAL_MAGIC: &[u8; 8] = b"SQLRWAL\0";
pub const WAL_FORMAT_VERSION: u32 = 1;
pub const FRAME_HEADER_SIZE: usize = 16;
pub const FRAME_SIZE: usize = FRAME_HEADER_SIZE + PAGE_SIZE;

/// Parsed WAL header. `page_size` is redundant with the engine's compile-
/// time constant; we persist it for forward-compat and reject anything
/// that doesn't match at open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalHeader {
    pub salt: u32,
    pub checkpoint_seq: u32,
}

/// Parsed per-frame header (everything but the page body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub page_num: u32,
    pub commit_page_count: u32,
    pub salt: u32,
    pub checksum: u32,
}

impl FrameHeader {
    /// A commit frame is the "transaction barrier": every preceding dirty
    /// frame up to the previous commit frame (or the WAL header) belongs
    /// to the transaction this one seals.
    pub fn is_commit(&self) -> bool {
        self.commit_page_count != 0
    }
}

pub struct Wal {
    // File carries a Debug impl; we don't derive on Wal because we don't
    // want to dump the whole latest_frame map into the default Debug output.
    file: File,
    path: PathBuf,
    header: WalHeader,
    /// Page → byte offset of the LATEST frame carrying that page's
    /// content. Offsets point at the start of the 16-byte frame header.
    /// A reader consults this to resolve a page via the WAL before
    /// falling back to the main DB file (that's Phase 4c).
    latest_frame: HashMap<u32, u64>,
    /// Byte offset just past the last valid commit frame. Anything past
    /// this is uncommitted and should be ignored by readers. Equals
    /// `WAL_HEADER_SIZE` when there's nothing committed yet.
    last_commit_offset: u64,
    /// Post-commit page count carried in the most recent commit frame.
    last_commit_page_count: Option<u32>,
    /// Total valid frames (up to and including `last_commit_offset`).
    /// Used by the checkpointer in Phase 4d to decide whether to run.
    frame_count: usize,
}

impl Wal {
    /// Creates a fresh WAL file, truncating any existing one. Writes the
    /// header synchronously so a subsequent `open` sees a valid file even
    /// if the caller panics before appending any frames. Always takes an
    /// exclusive lock — create is a write operation by definition.
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        acquire_lock(&file, path, AccessMode::ReadWrite)?;

        let salt = random_salt();
        let header = WalHeader {
            salt,
            checkpoint_seq: 0,
        };
        let mut wal = Self {
            file,
            path: path.to_path_buf(),
            header,
            latest_frame: HashMap::new(),
            last_commit_offset: WAL_HEADER_SIZE as u64,
            last_commit_page_count: None,
            frame_count: 0,
        };
        wal.write_header()?;
        wal.file.flush()?;
        wal.file.sync_all()?;
        Ok(wal)
    }

    /// Opens an existing WAL file with an exclusive lock (read-write).
    /// Convenience wrapper around [`Wal::open_with_mode`].
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_mode(path, AccessMode::ReadWrite)
    }

    /// Opens an existing WAL file with the given access mode. In
    /// `ReadOnly` mode the file descriptor is opened read-only and the
    /// advisory lock is shared — multiple read-only openers may coexist.
    /// Walks every frame from the start, validates checksums, and builds
    /// the in-memory `latest_frame` index. A torn or corrupted frame is
    /// treated as the end of the log — its bytes and anything after stay
    /// on disk but are ignored by reads.
    pub fn open_with_mode(path: &Path, mode: AccessMode) -> Result<Self> {
        let mut file = match mode {
            AccessMode::ReadWrite => OpenOptions::new().read(true).write(true).open(path)?,
            AccessMode::ReadOnly => OpenOptions::new().read(true).open(path)?,
        };
        acquire_lock(&file, path, mode)?;

        let header = read_header(&mut file)?;
        let mut wal = Self {
            file,
            path: path.to_path_buf(),
            header,
            latest_frame: HashMap::new(),
            last_commit_offset: WAL_HEADER_SIZE as u64,
            last_commit_page_count: None,
            frame_count: 0,
        };
        wal.replay_frames()?;
        Ok(wal)
    }

    pub fn header(&self) -> WalHeader {
        self.header
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn last_commit_page_count(&self) -> Option<u32> {
        self.last_commit_page_count
    }

    /// Bulk-loads every committed page from the WAL into `dest`. Used by
    /// `Pager::open` to warm a WAL cache so subsequent reads don't have
    /// to seek back into the WAL file. Uncommitted frames are skipped
    /// (same rule as `read_page`).
    pub fn load_committed_into(
        &mut self,
        dest: &mut HashMap<u32, Box<[u8; PAGE_SIZE]>>,
    ) -> Result<()> {
        // Snapshot the page numbers upfront so we don't hold a borrow of
        // `self` while calling the mutating `read_page`.
        let pages: Vec<u32> = self.latest_frame.keys().copied().collect();
        for page_num in pages {
            if let Some(body) = self.read_page(page_num)? {
                dest.insert(page_num, body);
            }
        }
        Ok(())
    }

    /// Appends a new frame at the current end of file. `commit_page_count`
    /// of `None` writes a dirty (in-progress) frame; `Some(n)` writes a
    /// commit frame carrying the post-commit page count. On commit the
    /// frame is fsync'd; dirty frames are not — torn writes are recovered
    /// by the checksum check on next open.
    pub fn append_frame(
        &mut self,
        page_num: u32,
        content: &[u8; PAGE_SIZE],
        commit_page_count: Option<u32>,
    ) -> Result<()> {
        // Build the header in a buffer so we can checksum + write it
        // atomically alongside the body.
        let mut header_buf = [0u8; FRAME_HEADER_SIZE];
        header_buf[0..4].copy_from_slice(&page_num.to_le_bytes());
        header_buf[4..8].copy_from_slice(&commit_page_count.unwrap_or(0).to_le_bytes());
        header_buf[8..12].copy_from_slice(&self.header.salt.to_le_bytes());
        let sum = compute_checksum(&header_buf[0..12], content);
        header_buf[12..16].copy_from_slice(&sum.to_le_bytes());

        // Frame lands at the current tail.
        let offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&header_buf)?;
        self.file.write_all(content)?;

        // Commit frames sync; dirty frames are buffered.
        if commit_page_count.is_some() {
            self.file.flush()?;
            self.file.sync_all()?;
        }

        // Update in-memory state — the latest-frame map always points at the
        // newest frame, whether dirty or committed. Readers consult the
        // commit-barrier separately to decide what's visible.
        self.latest_frame.insert(page_num, offset);
        if let Some(pc) = commit_page_count {
            self.last_commit_offset = offset + FRAME_SIZE as u64;
            self.last_commit_page_count = Some(pc);
        }
        self.frame_count += 1;
        Ok(())
    }

    /// Reads the most recent committed copy of a page from the WAL, or
    /// `None` if no committed frame has been written for this page since
    /// the last checkpoint. Uncommitted (dirty) frames are skipped — a
    /// reader must only see committed state.
    pub fn read_page(&mut self, page_num: u32) -> Result<Option<Box<[u8; PAGE_SIZE]>>> {
        let Some(&offset) = self.latest_frame.get(&page_num) else {
            return Ok(None);
        };
        // If this frame sits past the last commit barrier it's
        // uncommitted — not visible.
        if offset + FRAME_SIZE as u64 > self.last_commit_offset {
            return Ok(None);
        }
        let (_hdr, body) = self.read_frame_at(offset)?;
        Ok(Some(body))
    }

    /// Truncates the WAL back to just the header and rolls the salt.
    /// Called by the checkpointer (Phase 4d) once it has applied
    /// accumulated frames to the main file.
    pub fn truncate(&mut self) -> Result<()> {
        self.header.salt = random_salt();
        self.header.checkpoint_seq = self.header.checkpoint_seq.wrapping_add(1);
        self.file.set_len(WAL_HEADER_SIZE as u64)?;
        self.write_header()?;
        self.file.flush()?;
        self.file.sync_all()?;
        self.latest_frame.clear();
        self.last_commit_offset = WAL_HEADER_SIZE as u64;
        self.last_commit_page_count = None;
        self.frame_count = 0;
        Ok(())
    }

    // ---- internal helpers ------------------------------------------------

    fn write_header(&mut self) -> Result<()> {
        let mut buf = [0u8; WAL_HEADER_SIZE];
        buf[0..8].copy_from_slice(WAL_MAGIC);
        buf[8..12].copy_from_slice(&WAL_FORMAT_VERSION.to_le_bytes());
        buf[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        buf[16..20].copy_from_slice(&self.header.salt.to_le_bytes());
        buf[20..24].copy_from_slice(&self.header.checkpoint_seq.to_le_bytes());
        // 24..32 zero
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&buf)?;
        Ok(())
    }

    /// Reads and parses one frame at `offset`. Returns `(header, body)`.
    /// Errors if the frame is truncated or the checksum fails.
    fn read_frame_at(&mut self, offset: u64) -> Result<(FrameHeader, Box<[u8; PAGE_SIZE]>)> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut header_buf = [0u8; FRAME_HEADER_SIZE];
        self.file.read_exact(&mut header_buf)?;
        let mut body = Box::new([0u8; PAGE_SIZE]);
        self.file.read_exact(body.as_mut())?;

        let page_num = u32::from_le_bytes(header_buf[0..4].try_into().unwrap());
        let commit_page_count = u32::from_le_bytes(header_buf[4..8].try_into().unwrap());
        let salt = u32::from_le_bytes(header_buf[8..12].try_into().unwrap());
        let stored_checksum = u32::from_le_bytes(header_buf[12..16].try_into().unwrap());

        if salt != self.header.salt {
            return Err(SQLRiteError::General(format!(
                "WAL frame at offset {offset}: salt mismatch (expected {:x}, got {:x})",
                self.header.salt, salt
            )));
        }
        let computed = compute_checksum(&header_buf[0..12], &body);
        if computed != stored_checksum {
            return Err(SQLRiteError::General(format!(
                "WAL frame at offset {offset}: bad checksum (expected {stored_checksum:x}, got {computed:x})"
            )));
        }

        Ok((
            FrameHeader {
                page_num,
                commit_page_count,
                salt,
                checksum: stored_checksum,
            },
            body,
        ))
    }

    /// Walks every frame from `WAL_HEADER_SIZE` to end-of-file, validating
    /// each checksum and building `latest_frame`. A frame with a salt
    /// mismatch or bad checksum marks the end of the usable log (earlier
    /// frames are still valid). The last commit frame we successfully
    /// read defines `last_commit_offset`.
    ///
    /// Key invariant: `latest_frame` only holds offsets of *committed*
    /// frames. Dirty frames belonging to an in-progress (or crashed)
    /// transaction accumulate in a pending map and are promoted on the
    /// commit frame that seals them — or discarded if the log ends before
    /// a commit arrives. Without this, an orphan dirty frame for page N
    /// would shadow the previous committed frame for page N, erasing it
    /// from visibility.
    fn replay_frames(&mut self) -> Result<()> {
        let file_len = self.file.seek(SeekFrom::End(0))?;
        let mut offset = WAL_HEADER_SIZE as u64;
        let mut pending: HashMap<u32, u64> = HashMap::new();
        while offset + FRAME_SIZE as u64 <= file_len {
            match self.read_frame_at(offset) {
                Ok((header, _body)) => {
                    self.frame_count += 1;
                    pending.insert(header.page_num, offset);
                    if header.is_commit() {
                        // Seal: promote all pending frames (including
                        // this commit frame itself) into latest_frame.
                        for (p, o) in pending.drain() {
                            self.latest_frame.insert(p, o);
                        }
                        self.last_commit_offset = offset + FRAME_SIZE as u64;
                        self.last_commit_page_count = Some(header.commit_page_count);
                    }
                    offset += FRAME_SIZE as u64;
                }
                // A bad frame is the torn-write boundary. Keep everything
                // before it.
                Err(_) => break,
            }
        }
        // Anything still in `pending` belongs to a transaction that never
        // committed (crash, or a writer that died mid-append). Drop it.
        Ok(())
    }
}

impl std::fmt::Debug for Wal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wal")
            .field("path", &self.path)
            .field("salt", &format_args!("{:#x}", self.header.salt))
            .field("checkpoint_seq", &self.header.checkpoint_seq)
            .field("frame_count", &self.frame_count)
            .field("last_commit_page_count", &self.last_commit_page_count)
            .finish()
    }
}

fn read_header(file: &mut File) -> Result<WalHeader> {
    let mut buf = [0u8; WAL_HEADER_SIZE];
    file.seek(SeekFrom::Start(0))?;
    // read_exact on a short file would bubble up as a generic io error —
    // surface it as "bad magic" instead so the caller gets a consistent
    // diagnosis regardless of whether the file is short-and-garbage or
    // long-and-garbage.
    if file.read_exact(&mut buf).is_err() {
        return Err(SQLRiteError::General(
            "file is not a SQLRite WAL (too short / bad magic)".to_string(),
        ));
    }
    if &buf[0..8] != WAL_MAGIC {
        return Err(SQLRiteError::General(
            "file is not a SQLRite WAL (bad magic)".to_string(),
        ));
    }
    let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    if version != WAL_FORMAT_VERSION {
        return Err(SQLRiteError::General(format!(
            "unsupported WAL format version {version}; this build understands {WAL_FORMAT_VERSION}"
        )));
    }
    let page_size = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
    if page_size != PAGE_SIZE {
        return Err(SQLRiteError::General(format!(
            "WAL page size {page_size} doesn't match engine's {PAGE_SIZE}"
        )));
    }
    let salt = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let checkpoint_seq = u32::from_le_bytes(buf[20..24].try_into().unwrap());
    Ok(WalHeader {
        salt,
        checkpoint_seq,
    })
}

fn random_salt() -> u32 {
    // Seeded from SystemTime. Crypto-grade randomness isn't needed — the
    // salt's only job is to make a post-truncate WAL file visibly
    // different from the pre-truncate one (so stale tail bytes can't
    // collide).
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_nanos() as u32) ^ (d.as_secs() as u32).rotate_left(13))
        .unwrap_or(0xdeadbeef)
}

/// Rolling sum over `(header_bytes ++ body)`. `rotate_left(1)` per byte
/// makes the checksum order-sensitive, so bit flips AND byte shuffles
/// are detected.
fn compute_checksum(header_bytes: &[u8], body: &[u8; PAGE_SIZE]) -> u32 {
    let mut sum: u32 = 0;
    for &b in header_bytes {
        sum = sum.rotate_left(1).wrapping_add(b as u32);
    }
    for &b in body.iter() {
        sum = sum.rotate_left(1).wrapping_add(b as u32);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_wal(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-wal-{pid}-{nanos}-{name}.wal"));
        p
    }

    fn page(byte: u8) -> Box<[u8; PAGE_SIZE]> {
        let mut b = Box::new([0u8; PAGE_SIZE]);
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = byte.wrapping_add(i as u8);
        }
        b
    }

    #[test]
    fn create_then_open_round_trips_an_empty_wal() {
        let p = tmp_wal("empty");
        let w = Wal::create(&p).unwrap();
        assert_eq!(w.frame_count(), 0);
        assert_eq!(w.last_commit_page_count(), None);
        let salt = w.header().salt;
        drop(w);

        let w2 = Wal::open(&p).unwrap();
        assert_eq!(w2.header().salt, salt);
        assert_eq!(w2.frame_count(), 0);
        assert_eq!(w2.last_commit_page_count(), None);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn single_commit_frame_round_trips() {
        let p = tmp_wal("one_frame");
        let mut w = Wal::create(&p).unwrap();
        let content = page(0xab);
        w.append_frame(7, &content, Some(42)).unwrap();
        assert_eq!(w.frame_count(), 1);
        assert_eq!(w.last_commit_page_count(), Some(42));
        drop(w);

        let mut w2 = Wal::open(&p).unwrap();
        assert_eq!(w2.frame_count(), 1);
        assert_eq!(w2.last_commit_page_count(), Some(42));
        let read = w2.read_page(7).unwrap().expect("frame should be visible");
        assert_eq!(read.as_ref(), content.as_ref());
        assert!(
            w2.read_page(99).unwrap().is_none(),
            "untouched page is None"
        );

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn multi_frame_commits_and_latest_wins() {
        // Three commits to the same page; the latest one should be what
        // read_page returns.
        let p = tmp_wal("latest_wins");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(1, &page(1), Some(10)).unwrap();
        w.append_frame(1, &page(2), Some(10)).unwrap();
        w.append_frame(1, &page(3), Some(10)).unwrap();
        w.append_frame(2, &page(9), Some(10)).unwrap();
        assert_eq!(w.frame_count(), 4);
        drop(w);

        let mut w2 = Wal::open(&p).unwrap();
        assert_eq!(w2.read_page(1).unwrap().unwrap().as_ref(), page(3).as_ref());
        assert_eq!(w2.read_page(2).unwrap().unwrap().as_ref(), page(9).as_ref());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn orphan_dirty_tail_preserves_previous_commit() {
        // A dirty frame at the tail with no commit frame following it
        // belongs to a transaction that never sealed. The reader must
        // fall back to the previous committed frame for that page rather
        // than treating the page as absent — otherwise a crash mid-write
        // would erase the page's last durable value.
        let p = tmp_wal("dirty_tail");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(5, &page(50), Some(10)).unwrap(); // committed V1
        w.append_frame(5, &page(51), None).unwrap(); // orphan dirty V2
        drop(w);

        let mut w2 = Wal::open(&p).unwrap();
        // latest_frame points at the committed offset, NOT the orphan's.
        // read_page returns V1 — the orphan is invisible.
        let got = w2
            .read_page(5)
            .unwrap()
            .expect("committed V1 should still be visible");
        assert_eq!(got.as_ref(), page(50).as_ref());
        // Both frames are still present on disk; frame_count reflects that.
        assert_eq!(w2.frame_count(), 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn uncommitted_frame_for_untouched_page_returns_none() {
        // Contrast with the previous test: a dirty frame for a page that
        // was never committed has no fallback, so read_page returns None.
        let p = tmp_wal("dirty_only");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(7, &page(70), None).unwrap(); // dirty, no commit
        drop(w);

        let mut w2 = Wal::open(&p).unwrap();
        assert_eq!(w2.read_page(7).unwrap(), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn truncate_resets_to_empty_and_rolls_salt() {
        let p = tmp_wal("truncate");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(1, &page(11), Some(5)).unwrap();
        w.append_frame(2, &page(22), Some(5)).unwrap();
        let seq_before = w.header().checkpoint_seq;
        let salt_before = w.header().salt;
        w.truncate().unwrap();
        assert_eq!(w.frame_count(), 0);
        assert_eq!(w.last_commit_page_count(), None);
        assert_eq!(w.header().checkpoint_seq, seq_before + 1);
        // Salt is randomly rolled; we can't assert a specific value, but
        // it should almost never match the previous one.
        let _ = salt_before; // the SystemTime-based salt can collide in a
        // theoretical tie; don't assert inequality to avoid flakes.

        // Drop w so its exclusive lock releases before we reopen the same
        // path for verification.
        drop(w);

        // After truncate, read_page returns None for pages we previously
        // wrote — the frames are gone.
        let mut w2 = Wal::open(&p).unwrap();
        assert_eq!(w2.frame_count(), 0);
        assert_eq!(w2.read_page(1).unwrap(), None);
        assert_eq!(w2.read_page(2).unwrap(), None);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn bad_magic_file_is_rejected() {
        let p = tmp_wal("bad_magic");
        std::fs::write(&p, b"not a WAL file").unwrap();
        let err = Wal::open(&p).unwrap_err();
        assert!(format!("{err}").contains("bad magic"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn corrupt_frame_body_marks_end_of_log() {
        // Write two valid commit frames, then flip a byte in the second
        // frame's body. The reader should accept the first frame and
        // treat the second as end-of-log.
        let p = tmp_wal("bit_flip");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(1, &page(0x11), Some(5)).unwrap();
        w.append_frame(2, &page(0x22), Some(5)).unwrap();
        drop(w);

        // Flip a byte in the second frame's body. Frame 2's body starts
        // at offset WAL_HEADER_SIZE + FRAME_SIZE + FRAME_HEADER_SIZE.
        let body_offset = WAL_HEADER_SIZE + FRAME_SIZE + FRAME_HEADER_SIZE;
        let mut buf = std::fs::read(&p).unwrap();
        buf[body_offset] ^= 0xff;
        std::fs::write(&p, &buf).unwrap();

        let mut w2 = Wal::open(&p).unwrap();
        // First frame survived.
        assert_eq!(
            w2.read_page(1).unwrap().unwrap().as_ref(),
            page(0x11).as_ref()
        );
        // Second frame was truncated out — its content isn't readable.
        assert_eq!(w2.read_page(2).unwrap(), None);
        assert_eq!(w2.frame_count(), 1);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn partial_trailing_frame_is_ignored() {
        // Write one valid frame, then append a half-frame's worth of
        // random bytes. The reader should stop cleanly at the valid
        // frame.
        let p = tmp_wal("partial");
        let mut w = Wal::create(&p).unwrap();
        w.append_frame(42, &page(42), Some(1)).unwrap();
        drop(w);
        {
            let mut f = OpenOptions::new().write(true).open(&p).unwrap();
            f.seek(SeekFrom::End(0)).unwrap();
            f.write_all(&[0xaa; 2000]).unwrap();
        }
        let mut w2 = Wal::open(&p).unwrap();
        assert_eq!(
            w2.read_page(42).unwrap().unwrap().as_ref(),
            page(42).as_ref()
        );
        assert_eq!(w2.frame_count(), 1);
        let _ = std::fs::remove_file(&p);
    }
}
