//! MVCC commit log records — the WAL-resident representation of
//! `BEGIN CONCURRENT` writes (Phase 11.9).
//!
//! Per [`docs/concurrent-writes-plan.md`](../../../docs/concurrent-writes-plan.md):
//!
//! > WAL log record format: a new frame kind carrying
//! > `(table_id, rowid, op, payload)` tuples. Distinct from the
//! > existing per-page commit frame; the checkpointer flattens log
//! > records into page-level updates.
//!
//! ## What this gives us in our hybrid architecture
//!
//! Phase 11.4 ships `BEGIN CONCURRENT` commits that mirror writes
//! into both `MvStore` (in-memory) and `Database::tables` (legacy
//! save path). The legacy save handles durability — the tables
//! are page-encoded into the WAL and fsync'd. But `MvStore` lives
//! only in memory, so it starts empty on every reopen. That's
//! correct for single-session workloads (each session re-derives
//! conflict-detection state from new commits) but means MVCC's
//! conflict-detection window doesn't survive a process restart.
//!
//! Phase 11.9 closes that gap by also appending an MVCC log
//! record frame to the WAL on every successful concurrent commit.
//! On reopen, the WAL replay walks the MVCC frames in addition to
//! the page frames and re-populates `MvStore` with the committed
//! versions. Same fsync covers both — the MVCC frame is written
//! to the WAL buffer right before the legacy save fsync, so a
//! crash either loses both or commits both.
//!
//! ## Body layout (fits inside a 4 KiB frame body)
//!
//! ```text
//!   bytes  0..8    magic: "MVCC0001" (ASCII, no NUL)
//!   bytes  8..16   commit_ts: u64 LE
//!   bytes 16..18   record count: u16 LE  (max ~256 records / tx for v0)
//!   bytes 18..    record stream — each record:
//!     byte  0           op tag: 0 = Tombstone, 1 = Present
//!     bytes 1..3        table-name length: u16 LE
//!     bytes ..          table name: N bytes UTF-8
//!     bytes ..          rowid: i64 LE (8 bytes)
//!     if op = Present:
//!       bytes ..          column count: u16 LE
//!       for each column:
//!         bytes ..          name length: u16 LE
//!         bytes ..          name: N bytes UTF-8
//!         byte  ..          value type tag: 0 Null, 1 Int, 2 Real, 3 Text,
//!                                            4 Bool, 5 Vector
//!         bytes ..          value:
//!           Int:  i64 LE (8 bytes)
//!           Real: f64 LE (8 bytes)
//!           Text: u32 LE length + N bytes UTF-8
//!           Bool: 1 byte (0 / 1)
//!           Vector: u32 LE length + 4*N bytes f32 LE
//!     (Tombstone has no payload after rowid.)
//!   bytes N..PAGE_SIZE  zero-padded
//! ```
//!
//! The whole batch must fit in 4096 bytes (the frame body size).
//! v0 surfaces a typed error if encoding overflows; multi-frame
//! batches (for very large transactions) are a separate slice.
//!
//! ## Why one batch per commit
//!
//! A transaction's writes are committed atomically. Bundling them
//! into one frame means a single WAL fsync covers the whole batch:
//! we never end up with half a transaction durable. A torn frame
//! (the checksum catches it) drops the whole transaction, which is
//! the right rollback semantics.

use crate::error::{Result, SQLRiteError};
use crate::mvcc::{RowID, VersionPayload};
use crate::sql::db::table::Value;
use crate::sql::pager::page::PAGE_SIZE;

/// Marker stored in the frame header's `page_num` field that
/// distinguishes MVCC log-record frames from page-commit frames.
/// `u32::MAX` is safely outside the legal page-number range (max
/// realistic database has at most a few hundred million pages, far
/// short of `u32::MAX`).
///
/// Page-commit frames carry a real page number in `[0, page_count)`;
/// MVCC frames always carry this sentinel. The replayer branches
/// on it.
pub const MVCC_FRAME_MARKER: u32 = u32::MAX;

/// Magic bytes at the start of every encoded MVCC commit batch.
/// Reserves space for future format-version bumps without changing
/// the frame-level discriminator. The trailing `0001` is the v1
/// payload format version; bump on incompatible body changes.
pub const MVCC_BODY_MAGIC: &[u8; 8] = b"MVCC0001";

/// Maximum batch payload size — the frame body size, with the
/// magic + commit_ts + record-count header stripped off. Encoders
/// reject batches whose serialised form would exceed this.
pub const MVCC_BODY_PAYLOAD_CAP: usize = PAGE_SIZE - 8 - 8 - 2;

/// One row's worth of state at the moment of commit. Decoded from
/// the WAL on reopen, applied to `MvStore` (and re-applied to
/// `Database::tables` for the snapshot reader path) by the
/// replayer.
#[derive(Debug, Clone, PartialEq)]
pub struct MvccLogRecord {
    pub row: RowID,
    pub payload: VersionPayload,
}

impl MvccLogRecord {
    pub fn upsert(table: impl Into<String>, rowid: i64, columns: Vec<(String, Value)>) -> Self {
        Self {
            row: RowID::new(table, rowid),
            payload: VersionPayload::Present(columns),
        }
    }

    pub fn tombstone(table: impl Into<String>, rowid: i64) -> Self {
        Self {
            row: RowID::new(table, rowid),
            payload: VersionPayload::Tombstone,
        }
    }
}

/// All the writes a single `BEGIN CONCURRENT` transaction produced
/// at its commit. Encoded into one WAL frame body; replayed
/// atomically (a torn batch drops the whole transaction).
#[derive(Debug, Clone, PartialEq)]
pub struct MvccCommitBatch {
    pub commit_ts: u64,
    pub records: Vec<MvccLogRecord>,
}

impl MvccCommitBatch {
    /// Encodes `self` into a `PAGE_SIZE` byte buffer, zero-padded
    /// past the actual payload. The buffer is what
    /// `Wal::append_frame` writes as the frame body for
    /// `page_num = MVCC_FRAME_MARKER`.
    ///
    /// Returns an error if the encoded size would exceed
    /// `PAGE_SIZE` (a single transaction wrote more than ~4 KB of
    /// row data). v0 callers see this as a `SQLRiteError::General`;
    /// multi-frame batch support is a separate slice.
    pub fn encode(&self) -> Result<Box<[u8; PAGE_SIZE]>> {
        let mut buf = Box::new([0u8; PAGE_SIZE]);
        let mut cur = 0usize;
        write_bytes(&mut buf, &mut cur, MVCC_BODY_MAGIC)?;
        write_u64(&mut buf, &mut cur, self.commit_ts)?;
        if self.records.len() > u16::MAX as usize {
            return Err(SQLRiteError::General(format!(
                "MVCC log: too many records in one commit ({}); cap is {}",
                self.records.len(),
                u16::MAX
            )));
        }
        write_u16(&mut buf, &mut cur, self.records.len() as u16)?;
        for rec in &self.records {
            encode_record(&mut buf, &mut cur, rec)?;
        }
        Ok(buf)
    }

    /// Decodes a batch from a frame body. Strict: bad magic,
    /// truncated stream, unknown tags, or trailing-byte mismatches
    /// surface as typed errors. The caller (the WAL replayer) drops
    /// any frame that fails to decode and continues with the rest
    /// of the log.
    pub fn decode(body: &[u8]) -> Result<Self> {
        if body.len() < 8 + 8 + 2 {
            return Err(SQLRiteError::General(
                "MVCC log: body shorter than fixed header".to_string(),
            ));
        }
        if &body[0..8] != MVCC_BODY_MAGIC {
            return Err(SQLRiteError::General(format!(
                "MVCC log: bad magic, expected {:?}, got {:?}",
                MVCC_BODY_MAGIC,
                &body[0..8],
            )));
        }
        let commit_ts = read_u64(body, 8);
        let record_count = read_u16(body, 16) as usize;
        let mut cur = 18usize;
        let mut records = Vec::with_capacity(record_count);
        for _ in 0..record_count {
            records.push(decode_record(body, &mut cur)?);
        }
        Ok(Self { commit_ts, records })
    }
}

// ---------- encode helpers ------------------------------------------------

fn write_bytes(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, src: &[u8]) -> Result<()> {
    if *cur + src.len() > PAGE_SIZE {
        return Err(SQLRiteError::General(format!(
            "MVCC log: encoded batch exceeds {PAGE_SIZE}-byte frame body cap"
        )));
    }
    buf[*cur..*cur + src.len()].copy_from_slice(src);
    *cur += src.len();
    Ok(())
}

fn write_u16(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: u16) -> Result<()> {
    write_bytes(buf, cur, &v.to_le_bytes())
}

fn write_u32(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: u32) -> Result<()> {
    write_bytes(buf, cur, &v.to_le_bytes())
}

fn write_u64(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: u64) -> Result<()> {
    write_bytes(buf, cur, &v.to_le_bytes())
}

fn write_i64(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: i64) -> Result<()> {
    write_bytes(buf, cur, &v.to_le_bytes())
}

fn write_f64(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: f64) -> Result<()> {
    write_bytes(buf, cur, &v.to_le_bytes())
}

fn write_str(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, s: &str) -> Result<()> {
    if s.len() > u16::MAX as usize {
        return Err(SQLRiteError::General(format!(
            "MVCC log: string too long ({}); cap is {}",
            s.len(),
            u16::MAX,
        )));
    }
    write_u16(buf, cur, s.len() as u16)?;
    write_bytes(buf, cur, s.as_bytes())
}

fn encode_record(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, rec: &MvccLogRecord) -> Result<()> {
    let op: u8 = match rec.payload {
        VersionPayload::Tombstone => 0,
        VersionPayload::Present(_) => 1,
    };
    write_bytes(buf, cur, &[op])?;
    write_str(buf, cur, &rec.row.table)?;
    write_i64(buf, cur, rec.row.rowid)?;
    if let VersionPayload::Present(cols) = &rec.payload {
        if cols.len() > u16::MAX as usize {
            return Err(SQLRiteError::General(format!(
                "MVCC log: column count {} exceeds cap {}",
                cols.len(),
                u16::MAX
            )));
        }
        write_u16(buf, cur, cols.len() as u16)?;
        for (name, value) in cols {
            write_str(buf, cur, name)?;
            encode_value(buf, cur, value)?;
        }
    }
    Ok(())
}

fn encode_value(buf: &mut [u8; PAGE_SIZE], cur: &mut usize, v: &Value) -> Result<()> {
    match v {
        Value::Null => write_bytes(buf, cur, &[0u8]),
        Value::Integer(n) => {
            write_bytes(buf, cur, &[1u8])?;
            write_i64(buf, cur, *n)
        }
        Value::Real(f) => {
            write_bytes(buf, cur, &[2u8])?;
            write_f64(buf, cur, *f)
        }
        Value::Text(s) => {
            write_bytes(buf, cur, &[3u8])?;
            if s.len() > u32::MAX as usize {
                return Err(SQLRiteError::General(
                    "MVCC log: TEXT value exceeds u32 length cap".to_string(),
                ));
            }
            write_u32(buf, cur, s.len() as u32)?;
            write_bytes(buf, cur, s.as_bytes())
        }
        Value::Bool(b) => {
            write_bytes(buf, cur, &[4u8])?;
            write_bytes(buf, cur, &[*b as u8])
        }
        Value::Vector(elements) => {
            write_bytes(buf, cur, &[5u8])?;
            if elements.len() > u32::MAX as usize {
                return Err(SQLRiteError::General(
                    "MVCC log: VECTOR value exceeds u32 length cap".to_string(),
                ));
            }
            write_u32(buf, cur, elements.len() as u32)?;
            for x in elements {
                write_bytes(buf, cur, &x.to_le_bytes())?;
            }
            Ok(())
        }
    }
}

// ---------- decode helpers ------------------------------------------------

fn read_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(buf[at..at + 2].try_into().unwrap())
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

fn read_i64(buf: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

fn read_f64(buf: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

fn read_str(buf: &[u8], cur: &mut usize) -> Result<String> {
    if *cur + 2 > buf.len() {
        return Err(SQLRiteError::General(
            "MVCC log: truncated string length".to_string(),
        ));
    }
    let len = read_u16(buf, *cur) as usize;
    *cur += 2;
    if *cur + len > buf.len() {
        return Err(SQLRiteError::General(format!(
            "MVCC log: truncated string body (need {len} bytes)"
        )));
    }
    let s = std::str::from_utf8(&buf[*cur..*cur + len])
        .map_err(|e| SQLRiteError::General(format!("MVCC log: invalid UTF-8 in string: {e}")))?
        .to_string();
    *cur += len;
    Ok(s)
}

fn decode_record(buf: &[u8], cur: &mut usize) -> Result<MvccLogRecord> {
    if *cur + 1 > buf.len() {
        return Err(SQLRiteError::General(
            "MVCC log: truncated op tag".to_string(),
        ));
    }
    let op = buf[*cur];
    *cur += 1;
    let table = read_str(buf, cur)?;
    if *cur + 8 > buf.len() {
        return Err(SQLRiteError::General(
            "MVCC log: truncated rowid".to_string(),
        ));
    }
    let rowid = read_i64(buf, *cur);
    *cur += 8;
    let payload = match op {
        0 => VersionPayload::Tombstone,
        1 => {
            if *cur + 2 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated column count".to_string(),
                ));
            }
            let n = read_u16(buf, *cur) as usize;
            *cur += 2;
            let mut cols = Vec::with_capacity(n);
            for _ in 0..n {
                let name = read_str(buf, cur)?;
                let value = decode_value(buf, cur)?;
                cols.push((name, value));
            }
            VersionPayload::Present(cols)
        }
        other => {
            return Err(SQLRiteError::General(format!(
                "MVCC log: unknown op tag {other}"
            )));
        }
    };
    Ok(MvccLogRecord {
        row: RowID::new(table, rowid),
        payload,
    })
}

fn decode_value(buf: &[u8], cur: &mut usize) -> Result<Value> {
    if *cur + 1 > buf.len() {
        return Err(SQLRiteError::General(
            "MVCC log: truncated value tag".to_string(),
        ));
    }
    let tag = buf[*cur];
    *cur += 1;
    let value = match tag {
        0 => Value::Null,
        1 => {
            if *cur + 8 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated Integer value".to_string(),
                ));
            }
            let v = Value::Integer(read_i64(buf, *cur));
            *cur += 8;
            v
        }
        2 => {
            if *cur + 8 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated Real value".to_string(),
                ));
            }
            let v = Value::Real(read_f64(buf, *cur));
            *cur += 8;
            v
        }
        3 => {
            if *cur + 4 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated Text length".to_string(),
                ));
            }
            let len = read_u32(buf, *cur) as usize;
            *cur += 4;
            if *cur + len > buf.len() {
                return Err(SQLRiteError::General(format!(
                    "MVCC log: truncated Text body (need {len} bytes)"
                )));
            }
            let s = std::str::from_utf8(&buf[*cur..*cur + len])
                .map_err(|e| {
                    SQLRiteError::General(format!("MVCC log: invalid UTF-8 in Text: {e}"))
                })?
                .to_string();
            *cur += len;
            Value::Text(s)
        }
        4 => {
            if *cur + 1 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated Bool".to_string(),
                ));
            }
            let v = Value::Bool(buf[*cur] != 0);
            *cur += 1;
            v
        }
        5 => {
            if *cur + 4 > buf.len() {
                return Err(SQLRiteError::General(
                    "MVCC log: truncated Vector length".to_string(),
                ));
            }
            let n = read_u32(buf, *cur) as usize;
            *cur += 4;
            if *cur + n * 4 > buf.len() {
                return Err(SQLRiteError::General(format!(
                    "MVCC log: truncated Vector body (need {} bytes)",
                    n * 4
                )));
            }
            let mut elements = Vec::with_capacity(n);
            for _ in 0..n {
                let f = f32::from_le_bytes(buf[*cur..*cur + 4].try_into().unwrap());
                elements.push(f);
                *cur += 4;
            }
            Value::Vector(elements)
        }
        other => {
            return Err(SQLRiteError::General(format!(
                "MVCC log: unknown value tag {other}"
            )));
        }
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batch_round_trips() {
        let batch = MvccCommitBatch {
            commit_ts: 42,
            records: Vec::new(),
        };
        let bytes = batch.encode().unwrap();
        let back = MvccCommitBatch::decode(bytes.as_ref()).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn upsert_round_trips_with_every_value_kind() {
        let cols = vec![
            ("a_null".to_string(), Value::Null),
            ("an_int".to_string(), Value::Integer(-42)),
            ("a_real".to_string(), Value::Real(2.5)),
            ("a_text".to_string(), Value::Text("héllo".to_string())),
            ("a_bool".to_string(), Value::Bool(true)),
            ("a_vec".to_string(), Value::Vector(vec![1.0, -2.5, 3.25])),
        ];
        let batch = MvccCommitBatch {
            commit_ts: 99,
            records: vec![MvccLogRecord::upsert("accounts", 7, cols)],
        };
        let bytes = batch.encode().unwrap();
        let back = MvccCommitBatch::decode(bytes.as_ref()).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn multiple_records_in_one_batch_round_trip() {
        let batch = MvccCommitBatch {
            commit_ts: 100,
            records: vec![
                MvccLogRecord::upsert("t", 1, vec![("v".into(), Value::Integer(10))]),
                MvccLogRecord::upsert("t", 2, vec![("v".into(), Value::Integer(20))]),
                MvccLogRecord::tombstone("t", 3),
            ],
        };
        let bytes = batch.encode().unwrap();
        let back = MvccCommitBatch::decode(bytes.as_ref()).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn unicode_table_and_column_names_round_trip() {
        let batch = MvccCommitBatch {
            commit_ts: 1,
            records: vec![MvccLogRecord::upsert(
                "café_tablé",
                1,
                vec![("naïve_col".into(), Value::Text("日本語".into()))],
            )],
        };
        let bytes = batch.encode().unwrap();
        let back = MvccCommitBatch::decode(bytes.as_ref()).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn bad_magic_decode_errors() {
        let mut bytes = [0u8; PAGE_SIZE];
        bytes[0..8].copy_from_slice(b"NOTVALID");
        let err = MvccCommitBatch::decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("bad magic"));
    }

    #[test]
    fn truncated_body_decode_errors() {
        // Magic + commit_ts + claims 1 record, but no record bytes.
        let mut bytes = vec![0u8; 8 + 8 + 2];
        bytes[0..8].copy_from_slice(MVCC_BODY_MAGIC);
        bytes[16..18].copy_from_slice(&1u16.to_le_bytes());
        let err = MvccCommitBatch::decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("truncated"));
    }

    #[test]
    fn unknown_op_tag_decode_errors() {
        // Valid header, one record with op=42.
        let mut bytes = vec![0u8; 8 + 8 + 2 + 1 + 2 + 1 + 8];
        bytes[0..8].copy_from_slice(MVCC_BODY_MAGIC);
        bytes[16..18].copy_from_slice(&1u16.to_le_bytes());
        bytes[18] = 42; // unknown op
        bytes[19..21].copy_from_slice(&1u16.to_le_bytes()); // table name len = 1
        bytes[21] = b't';
        bytes[22..30].copy_from_slice(&0i64.to_le_bytes());
        let err = MvccCommitBatch::decode(&bytes).unwrap_err();
        assert!(format!("{err}").contains("unknown op tag"));
    }

    /// A batch larger than `PAGE_SIZE - header` should fail to
    /// encode rather than silently truncate. v0 supports up to ~4
    /// KB per transaction; multi-frame batches are a follow-up.
    #[test]
    fn oversized_batch_encode_errors() {
        // Build a batch with one huge text value that would exceed
        // PAGE_SIZE.
        let big = "x".repeat(PAGE_SIZE);
        let batch = MvccCommitBatch {
            commit_ts: 1,
            records: vec![MvccLogRecord::upsert(
                "t",
                1,
                vec![("c".into(), Value::Text(big))],
            )],
        };
        let err = batch.encode().unwrap_err();
        assert!(format!("{err}").contains("exceeds"));
    }

    /// Payload preserves declaration order — important for
    /// applying back to `Database::tables`.
    #[test]
    fn column_order_is_preserved() {
        let cols = vec![
            ("z".to_string(), Value::Integer(1)),
            ("a".to_string(), Value::Integer(2)),
            ("m".to_string(), Value::Integer(3)),
        ];
        let batch = MvccCommitBatch {
            commit_ts: 1,
            records: vec![MvccLogRecord::upsert("t", 1, cols.clone())],
        };
        let bytes = batch.encode().unwrap();
        let back = MvccCommitBatch::decode(bytes.as_ref()).unwrap();
        if let VersionPayload::Present(decoded_cols) = &back.records[0].payload {
            assert_eq!(
                decoded_cols
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
                vec!["z", "a", "m"]
            );
        } else {
            panic!("expected Present payload");
        }
    }
}
