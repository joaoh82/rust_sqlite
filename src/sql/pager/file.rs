//! Page-indexed file I/O: read/write whole pages by page number, plus the
//! special page-0 header. Deliberately thin — this is the only place that
//! touches `std::fs`, so higher layers deal in pages, not offsets.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::Result;
use crate::sql::pager::header::{DbHeader, decode_header, encode_header};
use crate::sql::pager::page::PAGE_SIZE;

pub struct FileStorage {
    file: File,
}

impl FileStorage {
    pub fn new(file: File) -> Self {
        Self { file }
    }

    pub fn read_header(&mut self) -> Result<DbHeader> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = [0u8; PAGE_SIZE];
        self.file.read_exact(&mut buf)?;
        decode_header(&buf)
    }

    pub fn write_header(&mut self, header: &DbHeader) -> Result<()> {
        let buf = encode_header(header);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&buf)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Low-level byte I/O helpers used by the `Pager` to bypass the per-page
    /// encoding when it's managing its own raw buffers.

    pub fn seek_to(&mut self, offset: u64) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(())
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.file.read_exact(buf)?;
        Ok(())
    }

    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.file.write_all(buf)?;
        Ok(())
    }

    /// Shrinks the backing file to `page_count` pages. Any bytes beyond the
    /// new length are discarded.
    pub fn truncate_to_pages(&mut self, page_count: u32) -> Result<()> {
        self.file
            .set_len((page_count as u64) * (PAGE_SIZE as u64))?;
        Ok(())
    }
}
