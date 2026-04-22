//! Variable-length integer encoding.
//!
//! Unsigned integers use **LEB128** — little-endian base 128. Each byte
//! carries 7 data bits; the high bit is 1 while more bytes follow and 0 on
//! the last byte. Values 0..127 fit in one byte.
//!
//! Signed integers use **ZigZag** mapping (positive and negative interleaved
//! into the unsigned range) and are then LEB128-encoded. Small positive
//! values like rowid=1 take one byte; small negative values do too.
//!
//! These are the encodings used for lengths, column counts, rowids, and
//! Integer cell values. Fixed-width encodings stay in place for tags (u8)
//! and `Real` values (f64, 8 bytes).

use crate::error::{Result, SQLRiteError};

/// Upper bound on bytes for a 64-bit LEB128 value: `ceil(64 / 7) = 10`.
pub const MAX_VARINT_BYTES: usize = 10;

/// Appends a LEB128-encoded `u64` to `out`. Returns the number of bytes written.
pub fn write_u64(out: &mut Vec<u8>, mut value: u64) -> usize {
    let mut written = 0;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        written += 1;
        if value == 0 {
            out.push(byte);
            return written;
        }
        byte |= 0x80;
        out.push(byte);
    }
}

/// Writes a ZigZag-encoded signed `i64` as LEB128. Returns bytes written.
pub fn write_i64(out: &mut Vec<u8>, value: i64) -> usize {
    write_u64(out, zigzag_encode(value))
}

/// Reads a LEB128 `u64` from `buf` starting at `pos`. Returns `(value, bytes_consumed)`.
pub fn read_u64(buf: &[u8], pos: usize) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for i in 0..MAX_VARINT_BYTES {
        let byte = *buf.get(pos + i).ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "varint read past buffer end at offset {}",
                pos + i
            ))
        })?;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok((result, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return Err(SQLRiteError::Internal(
                "varint u64 overflow (more than 10 bytes)".to_string(),
            ));
        }
    }
    Err(SQLRiteError::Internal(
        "varint u64 overflow (no terminator in 10 bytes)".to_string(),
    ))
}

/// Reads a ZigZag-encoded signed `i64` (LEB128). Returns `(value, bytes_consumed)`.
pub fn read_i64(buf: &[u8], pos: usize) -> Result<(i64, usize)> {
    let (u, n) = read_u64(buf, pos)?;
    Ok((zigzag_decode(u), n))
}

/// Returns the number of bytes `write_u64(value)` would produce, without writing.
pub fn u64_len(value: u64) -> usize {
    let mut v = value;
    let mut n = 0;
    loop {
        v >>= 7;
        n += 1;
        if v == 0 {
            return n;
        }
    }
}

/// Same as `u64_len` for a zigzagged signed `i64`.
pub fn i64_len(value: i64) -> usize {
    u64_len(zigzag_encode(value))
}

#[inline]
fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_u(v: u64) {
        let mut buf = Vec::new();
        let n = write_u64(&mut buf, v);
        assert_eq!(n, buf.len());
        assert_eq!(n, u64_len(v));
        let (back, consumed) = read_u64(&buf, 0).unwrap();
        assert_eq!(back, v);
        assert_eq!(consumed, n);
    }

    fn round_trip_i(v: i64) {
        let mut buf = Vec::new();
        let n = write_i64(&mut buf, v);
        assert_eq!(n, buf.len());
        assert_eq!(n, i64_len(v));
        let (back, consumed) = read_i64(&buf, 0).unwrap();
        assert_eq!(back, v);
        assert_eq!(consumed, n);
    }

    #[test]
    fn u64_round_trips_cover_boundaries() {
        for v in [
            0u64,
            1,
            127,              // last 1-byte value
            128,              // first 2-byte value
            16_383,           // last 2-byte value
            16_384,           // first 3-byte value
            u32::MAX as u64,
            u64::MAX,
        ] {
            round_trip_u(v);
        }
    }

    #[test]
    fn i64_round_trips_cover_signs_and_boundaries() {
        for v in [
            0i64,
            1,
            -1,
            63,
            -64,
            64,
            -65,
            i32::MAX as i64,
            i32::MIN as i64,
            i64::MAX,
            i64::MIN,
        ] {
            round_trip_i(v);
        }
    }

    #[test]
    fn reading_past_buffer_end_errors_cleanly() {
        // A single high-bit byte "needs more" but there isn't more.
        let buf = [0x80u8];
        let err = read_u64(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("varint"));
    }

    #[test]
    fn malformed_overlong_varint_errors() {
        // 11 consecutive high-bit bytes would overflow.
        let buf = [0xff; 11];
        let err = read_u64(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("overflow"));
    }

    #[test]
    fn small_positive_zigzag_is_one_byte() {
        assert_eq!(i64_len(0), 1);
        assert_eq!(i64_len(1), 1);
        assert_eq!(i64_len(63), 1);
        assert_eq!(i64_len(-1), 1);
        assert_eq!(i64_len(-64), 1);
    }

    #[test]
    fn concatenated_varints_read_sequentially() {
        let mut buf = Vec::new();
        write_u64(&mut buf, 7);
        write_i64(&mut buf, -42);
        write_u64(&mut buf, 999);
        let (a, n1) = read_u64(&buf, 0).unwrap();
        let (b, n2) = read_i64(&buf, n1).unwrap();
        let (c, _) = read_u64(&buf, n1 + n2).unwrap();
        assert_eq!((a, b, c), (7, -42, 999));
    }
}
