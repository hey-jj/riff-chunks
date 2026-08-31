use std::boxed::Box;
use std::fmt;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::vec::Vec;

use crate::borrowed::{form_from_id, parse_ds64_payload, read_u32, resolve_size};
use crate::error::Failure;
use crate::{ChunkHeader, Ds64, Error, Fourcc, Header, Result, MAX_DEPTH};

/// Largest single growth of the owned `ds64` buffer.
const DS64_RESERVE_STEP: usize = 64 * 1024;

mod sealed {
    pub trait Sealed {}
}

/// A sealed input type for [`ChunkReader`].
///
/// Custom [`Read`](io::Read) implementations work through `&mut R`. Owned
/// byte slices, cursors, files, boxes, and [`Seekable`] values are also
/// accepted.
pub trait Source: sealed::Sealed + Read {
    #[doc(hidden)]
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64>;

    #[doc(hidden)]
    fn base_position(&mut self) -> io::Result<u64> {
        Ok(0)
    }

    #[doc(hidden)]
    fn remaining_len(&mut self) -> io::Result<Option<u64>> {
        Ok(None)
    }
}

fn discard<R: Read + ?Sized>(reader: &mut R, mut amount: u64) -> io::Result<u64> {
    let mut buffer = [0_u8; 4096];
    let requested = amount;
    while amount != 0 {
        let limit = usize::try_from(amount)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = read_retrying(reader, &mut buffer[..limit])?;
        if read == 0 {
            break;
        }
        amount -= u64::try_from(read).unwrap_or(u64::MAX);
    }
    Ok(requested - amount)
}

impl<R: Read + ?Sized> sealed::Sealed for &mut R {}
impl<R: Read + ?Sized> Source for &mut R {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        discard(*self, amount)
    }
}

impl sealed::Sealed for &[u8] {}
impl Source for &[u8] {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        discard(self, amount)
    }

    fn remaining_len(&mut self) -> io::Result<Option<u64>> {
        Ok(Some(u64::try_from(self.len()).unwrap_or(u64::MAX)))
    }
}

impl<T: AsRef<[u8]>> sealed::Sealed for Cursor<T> {}
impl<T: AsRef<[u8]>> Source for Cursor<T> {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        discard(self, amount)
    }

    fn remaining_len(&mut self) -> io::Result<Option<u64>> {
        let len = u64::try_from(self.get_ref().as_ref().len()).unwrap_or(u64::MAX);
        Ok(Some(len.saturating_sub(self.position())))
    }
}

impl sealed::Sealed for File {}
impl Source for File {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        discard(self, amount)
    }

    fn remaining_len(&mut self) -> io::Result<Option<u64>> {
        let position = self.stream_position()?;
        Ok(Some(self.metadata()?.len().saturating_sub(position)))
    }
}

impl<R: Read + ?Sized> sealed::Sealed for Box<R> {}
impl<R: Read + ?Sized> Source for Box<R> {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        discard(self, amount)
    }
}

/// Marks a reader whose unread payloads can be skipped with `Seek`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Seekable<R: Read + Seek>(pub R);

impl<R: Read + Seek> Read for Seekable<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl<R: Read + Seek> sealed::Sealed for Seekable<R> {}
impl<R: Read + Seek> Source for Seekable<R> {
    fn skip_bytes(&mut self, amount: u64) -> io::Result<u64> {
        let mut remaining = amount;
        while remaining != 0 {
            let step = i64::try_from(remaining).unwrap_or(i64::MAX);
            self.0.seek(SeekFrom::Current(step))?;
            remaining -= u64::try_from(step).unwrap_or(u64::MAX);
        }
        Ok(amount)
    }

    fn base_position(&mut self) -> io::Result<u64> {
        self.0.stream_position()
    }

    fn remaining_len(&mut self) -> io::Result<Option<u64>> {
        let current = self.0.stream_position()?;
        let end = self.0.seek(SeekFrom::End(0))?;
        self.0.seek(SeekFrom::Start(current))?;
        Ok(Some(end.saturating_sub(current)))
    }
}

#[derive(Debug)]
struct OwnedDs64 {
    riff_size: u64,
    data_size: u64,
    sample_count: u64,
    bytes: Box<[u8]>,
}

impl OwnedDs64 {
    fn view(&self) -> Ds64<'_> {
        Ds64 {
            riff_size: self.riff_size,
            data_size: self.data_size,
            sample_count: self.sample_count,
            table: &self.bytes[28..],
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Current {
    header: ChunkHeader,
    consumed: u64,
    prefetched: u64,
    known_missing_pad: bool,
}

impl Current {
    fn remaining(self) -> u64 {
        self.header.size - self.consumed
    }
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    content_end: u64,
    after_end: u64,
    list: ChunkHeader,
}

impl Frame {
    const EMPTY: Self = Self {
        content_end: 0,
        after_end: 0,
        list: ChunkHeader {
            id: Fourcc([0; 4]),
            size: 0,
            offset: 0,
            depth: 0,
        },
    };
}

/// A streaming chunk walker.
pub struct ChunkReader<R: Source> {
    r: R,
    base: u64,
    pos: u64,
    header: Header,
    ds64: Option<OwnedDs64>,
    prefetched_header: Option<ChunkHeader>,
    bounds: [Frame; MAX_DEPTH],
    depth: usize,
    current: Option<Current>,
    failure: Option<Failure>,
    input_end: Option<u64>,
}

impl<R: Source> ChunkReader<R> {
    /// Reads the twelve-byte file header and any required `ds64` table.
    ///
    /// Positions and chunk offsets count from the source position at this
    /// call, so a container embedded inside a larger input parses the same
    /// as one starting at byte zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use riff_chunks::{ChunkReader, Fourcc};
    /// use std::io::{Cursor, Read};
    ///
    /// let bytes = b"RIFF\x10\x00\x00\x00TESTdata\x04\x00\x00\x00abcd";
    /// let mut reader = ChunkReader::new(Cursor::new(bytes.as_slice()))?;
    ///
    /// let header = reader.next()?.unwrap();
    /// assert_eq!(header.id, Fourcc::from_str("data"));
    ///
    /// let mut payload = [0; 4];
    /// reader.payload().read_exact(&mut payload)?;
    /// assert_eq!(&payload, b"abcd");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(mut source: R) -> Result<Self> {
        let base = source.base_position().map_err(Error::Io)?;
        let input_end = source.remaining_len().map_err(Error::Io)?;
        let mut bytes = [0_u8; 12];
        let header_read = read_partial(&mut source, &mut bytes).map_err(Error::Io)?;
        if header_read != bytes.len() {
            return Err(Error::TruncatedHeader {
                offset: 0,
                available: u64::try_from(header_read).unwrap_or(u64::MAX),
            });
        }
        let form = form_from_id([bytes[0], bytes[1], bytes[2], bytes[3]])?;
        let header = Header {
            form,
            size: read_u32(&bytes[4..8], form.is_big_endian()),
            form_type: Fourcc([bytes[8], bytes[9], bytes[10], bytes[11]]),
        };

        let mut reader = Self {
            r: source,
            base,
            pos: 12,
            header,
            ds64: None,
            prefetched_header: None,
            bounds: [Frame::EMPTY; MAX_DEPTH],
            depth: 0,
            current: None,
            failure: None,
            input_end,
        };
        if form.has_ds64() {
            reader.read_ds64()?;
        }
        Ok(reader)
    }

    /// Returns the parsed file header.
    pub const fn header(&self) -> Header {
        self.header
    }

    /// Returns the RF64 or BW64 size table when present.
    pub fn ds64(&self) -> Option<Ds64<'_>> {
        self.ds64.as_ref().map(OwnedDs64::view)
    }

    /// Returns the current list depth.
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the logical input position.
    ///
    /// The position counts from the source position at construction, so it
    /// matches the chunk offsets that [`next`](Self::next) reports.
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Advances to the next chunk at the current list level.
    ///
    /// Unread payload bytes and their optional pad byte are consumed first.
    /// A plain reader discards payload bytes through a 4 KiB stack buffer.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<ChunkHeader>> {
        if let Some(failure) = self.failure {
            return Err(failure.into_error());
        }
        self.finish_current()?;

        if let Some(header) = self.prefetched_header.take() {
            self.pos = header.offset.saturating_add(8);
            let prefetched = self
                .ds64
                .as_ref()
                .map(|table| u64::try_from(table.bytes.len()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            self.current = Some(Current {
                header,
                consumed: 0,
                prefetched,
                known_missing_pad: false,
            });
            return Ok(Some(header));
        }

        let level_end = self.level_end();
        if let Some(end) = level_end {
            if self.pos == end {
                return Ok(None);
            }
            if self.pos > end {
                return self.record(Failure::TruncatedHeader(self.pos, 0));
            }
            let remaining = end - self.pos;
            if remaining < 8 {
                return self.record(Failure::TruncatedHeader(self.pos, remaining));
            }
        }

        let offset = self.pos;
        let mut bytes = [0_u8; 8];
        let read = match read_partial(&mut self.r, &mut bytes) {
            Ok(read) => read,
            Err(error) => return self.record(Failure::Io(error.kind())),
        };
        self.pos = self
            .pos
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if read == 0 && level_end.is_none() {
            return Ok(None);
        }
        if read != bytes.len() {
            return self.record(Failure::TruncatedHeader(
                offset,
                u64::try_from(read).unwrap_or(u64::MAX),
            ));
        }

        let id = Fourcc([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let size32 = read_u32(&bytes[4..8], self.header.form.is_big_endian());
        let size = match resolve_size(size32, id, self.ds64()) {
            Ok(size) => size,
            Err(Error::UnresolvedSize { id }) => {
                return self.record(Failure::UnresolvedSize(id));
            }
            Err(error) => return Err(error),
        };
        if id == Fourcc::LIST && size < 4 {
            return self.record(Failure::ListTooShort(id, size));
        }

        let payload_end = match 8_u64.checked_add(size) {
            Some(end) => end,
            None => return self.record(Failure::Truncated(id, size, 0)),
        };
        let extent = match payload_end.checked_add(size & 1) {
            Some(extent) => extent,
            None => return self.record(Failure::Truncated(id, size, 0)),
        };
        let known_remaining = level_end.map(|end| end - offset);
        if self.depth != 0 {
            if let Some(remaining) = known_remaining {
                if extent > remaining {
                    return self.record(Failure::ChildExceedsParent(id, size, remaining));
                }
            }
        } else if let Some(remaining) = known_remaining {
            if payload_end > remaining {
                return self.record(Failure::Truncated(id, size, remaining.saturating_sub(8)));
            }
        }

        let known_missing_pad = self.depth == 0
            && known_remaining
                .map(|remaining| payload_end <= remaining && extent > remaining)
                .unwrap_or(false);
        let header = ChunkHeader {
            id,
            size,
            offset,
            depth: u8::try_from(self.depth).unwrap_or(u8::MAX),
        };
        self.current = Some(Current {
            header,
            consumed: 0,
            prefetched: 0,
            known_missing_pad,
        });
        Ok(Some(header))
    }

    /// Returns a bounded reader over the remaining current payload.
    pub fn payload(&mut self) -> Payload<'_, R> {
        Payload { reader: self }
    }

    /// Reads the remaining current payload into a caller-owned buffer.
    pub fn read_payload(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let needed = match self.current {
            Some(current) => current.remaining(),
            None => 0,
        };
        let buffer_len = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if buffer_len < needed {
            return Err(Error::BufferTooSmall { needed });
        }
        let needed_usize = usize::try_from(needed).map_err(|_| Error::BufferTooSmall { needed })?;
        let mut filled = 0;
        while filled < needed_usize {
            match self.read_current(&mut buffer[filled..needed_usize]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(_) => {
                    return Err(self
                        .failure
                        .unwrap_or(Failure::Io(io::ErrorKind::UnexpectedEof))
                        .into_error());
                }
            }
        }
        if filled != needed_usize {
            return Err(self
                .failure
                .unwrap_or(Failure::Truncated(
                    self.current
                        .map(|current| current.header.id)
                        .unwrap_or(Fourcc([0; 4])),
                    needed,
                    u64::try_from(filled).unwrap_or(u64::MAX),
                ))
                .into_error());
        }
        Ok(filled)
    }

    /// Enters the current `LIST` and returns its list type.
    pub fn enter(&mut self) -> Result<Fourcc> {
        if let Some(failure) = self.failure {
            return Err(failure.into_error());
        }
        let current = match self.current {
            Some(current) => current,
            None => return Err(Error::NotAList { id: Fourcc([0; 4]) }),
        };
        if !current.header.is_list() {
            return Err(Error::NotAList {
                id: current.header.id,
            });
        }
        if self.depth >= MAX_DEPTH {
            return self.record(Failure::TooDeep);
        }
        if current.header.size < 4 {
            return self.record(Failure::ListTooShort(
                current.header.id,
                current.header.size,
            ));
        }

        let mut bytes = [0_u8; 4];
        let mut filled = 0;
        while filled < bytes.len() {
            match self.read_current(&mut bytes[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(_) => {
                    return Err(self
                        .failure
                        .unwrap_or(Failure::Truncated(
                            current.header.id,
                            current.header.size,
                            u64::try_from(filled).unwrap_or(u64::MAX),
                        ))
                        .into_error());
                }
            }
        }
        if filled != bytes.len() {
            return self.record(Failure::Truncated(
                current.header.id,
                current.header.size,
                u64::try_from(filled).unwrap_or(u64::MAX),
            ));
        }

        let content_end = match current
            .header
            .offset
            .checked_add(8)
            .and_then(|start| start.checked_add(current.header.size))
        {
            Some(end) => end,
            None => {
                return self.record(Failure::Truncated(
                    current.header.id,
                    current.header.size,
                    4,
                ))
            }
        };
        let after_end = match content_end.checked_add(current.header.size & 1) {
            Some(end) => end,
            None => {
                return self.record(Failure::Truncated(
                    current.header.id,
                    current.header.size,
                    4,
                ))
            }
        };
        self.bounds[self.depth] = Frame {
            content_end,
            after_end,
            list: current.header,
        };
        self.depth += 1;
        self.current = None;
        Ok(Fourcc(bytes))
    }

    /// Skips the rest of the current list and resumes its parent level.
    pub fn leave(&mut self) -> Result<()> {
        if let Some(failure) = self.failure {
            return Err(failure.into_error());
        }
        if self.depth == 0 {
            return Err(Error::NotInList);
        }
        self.finish_current()?;
        let frame = self.bounds[self.depth - 1];
        if self.pos < frame.content_end {
            let amount = frame.content_end - self.pos;
            let skipped = self.skip_source(amount)?;
            if skipped != amount {
                return self.record(Failure::Truncated(
                    frame.list.id,
                    frame.list.size,
                    self.pos.saturating_sub(frame.list.offset.saturating_add(8)),
                ));
            }
        }
        if frame.after_end > frame.content_end && !self.read_pad(frame.list)? {
            return self.record(Failure::MissingPad(frame.list.id, frame.content_end));
        }
        self.depth -= 1;
        self.current = None;
        Ok(())
    }

    /// Returns the wrapped input.
    pub fn into_inner(self) -> R {
        self.r
    }

    fn read_ds64(&mut self) -> Result<()> {
        let mut header_bytes = [0_u8; 8];
        let read = read_partial(&mut self.r, &mut header_bytes).map_err(Error::Io)?;
        if read != header_bytes.len() {
            return Err(Error::TruncatedHeader {
                offset: 12,
                available: u64::try_from(read).unwrap_or(u64::MAX),
            });
        }
        let id = Fourcc([
            header_bytes[0],
            header_bytes[1],
            header_bytes[2],
            header_bytes[3],
        ]);
        if id != Fourcc::DS64 {
            return Err(Error::MissingDs64 { found: id });
        }
        let size = u64::from(read_u32(&header_bytes[4..8], false));
        if size < 28 {
            return Err(Error::BadDs64 { size });
        }
        if let Some(end) = self.input_end {
            let available = end.saturating_sub(20);
            if size > available {
                return Err(Error::Truncated {
                    id,
                    declared: size,
                    available,
                });
            }
        }

        let mut prefix = [0_u8; 28];
        let prefix_read = read_partial(&mut self.r, &mut prefix).map_err(Error::Io)?;
        if prefix_read != prefix.len() {
            return Err(Error::Truncated {
                id,
                declared: size,
                available: u64::try_from(prefix_read).unwrap_or(u64::MAX),
            });
        }
        let table_len = read_u32(&prefix[24..28], false);
        let required = 28_u64
            .checked_add(
                u64::from(table_len)
                    .checked_mul(12)
                    .ok_or(Error::BadDs64 { size })?,
            )
            .ok_or(Error::BadDs64 { size })?;
        if required > size {
            return Err(Error::BadDs64 { size });
        }
        let required_usize = usize::try_from(required).map_err(|_| Error::BadDs64 { size })?;
        // The buffer grows only as the source delivers table bytes, so a
        // declared size never drives a large allocation on its own.
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(required_usize.min(prefix.len() + DS64_RESERVE_STEP))
            .map_err(|_| Error::BadDs64 { size })?;
        bytes.extend_from_slice(&prefix);
        while bytes.len() < required_usize {
            let step = (required_usize - bytes.len()).min(DS64_RESERVE_STEP);
            bytes
                .try_reserve_exact(step)
                .map_err(|_| Error::BadDs64 { size })?;
            let filled = bytes.len();
            bytes.resize(filled + step, 0);
            let read = read_partial(&mut self.r, &mut bytes[filled..]).map_err(Error::Io)?;
            bytes.truncate(filled + read);
            if read < step {
                return Err(Error::Truncated {
                    id,
                    declared: size,
                    available: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                });
            }
        }
        let parsed = parse_ds64_payload(&bytes, size)?;
        let owned = OwnedDs64 {
            riff_size: parsed.riff_size,
            data_size: parsed.data_size,
            sample_count: parsed.sample_count,
            bytes: bytes.into_boxed_slice(),
        };
        self.ds64 = Some(owned);
        self.prefetched_header = Some(ChunkHeader {
            id,
            size,
            offset: 12,
            depth: 0,
        });
        Ok(())
    }

    fn level_end(&self) -> Option<u64> {
        if self.depth == 0 {
            self.input_end
        } else {
            Some(self.bounds[self.depth - 1].content_end)
        }
    }

    fn finish_current(&mut self) -> Result<()> {
        let mut current = match self.current {
            Some(current) => current,
            None => return Ok(()),
        };
        if current.remaining() != 0 {
            if current.consumed < current.prefetched {
                let replay = (current.prefetched - current.consumed).min(current.remaining());
                current.consumed += replay;
                self.pos = self.pos.saturating_add(replay);
            }
            let amount = current.remaining();
            if amount != 0 {
                let skipped = self.skip_source(amount)?;
                current.consumed += skipped;
                if skipped != amount {
                    self.current = Some(current);
                    return self.record(Failure::Truncated(
                        current.header.id,
                        current.header.size,
                        current.consumed,
                    ));
                }
            }
        }
        if current.header.size & 1 != 0
            && (current.known_missing_pad || !self.read_pad(current.header)?)
        {
            self.current = Some(current);
            return self.record(Failure::MissingPad(current.header.id, self.pos));
        }
        self.current = None;
        Ok(())
    }

    fn read_current(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(failure) = self.failure {
            return Err(io_error_for(failure));
        }
        let mut current = match self.current {
            Some(current) => current,
            None => return Ok(0),
        };
        if current.remaining() == 0 || buffer.is_empty() {
            return Ok(0);
        }
        let limit = usize::try_from(current.remaining())
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = if current.consumed < current.prefetched {
            let available = usize::try_from(current.prefetched - current.consumed)
                .unwrap_or(usize::MAX)
                .min(limit);
            let start = usize::try_from(current.consumed).unwrap_or(usize::MAX);
            let end = start.saturating_add(available);
            let table = match self.ds64.as_ref() {
                Some(table) if end <= table.bytes.len() => table,
                _ => {
                    self.failure = Some(Failure::Truncated(
                        current.header.id,
                        current.header.size,
                        current.consumed,
                    ));
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "prefetched payload ended early",
                    ));
                }
            };
            buffer[..available].copy_from_slice(&table.bytes[start..end]);
            available
        } else {
            match read_retrying(&mut self.r, &mut buffer[..limit]) {
                Ok(0) => {
                    self.failure = Some(Failure::Truncated(
                        current.header.id,
                        current.header.size,
                        current.consumed,
                    ));
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "chunk payload ended early",
                    ));
                }
                Ok(read) => read,
                Err(error) => {
                    self.failure = Some(Failure::Io(error.kind()));
                    return Err(error);
                }
            }
        };
        let read_u64 = u64::try_from(read).unwrap_or(u64::MAX);
        current.consumed = current.consumed.saturating_add(read_u64);
        self.pos = self.pos.saturating_add(read_u64);
        self.current = Some(current);
        Ok(read)
    }

    fn skip_source(&mut self, amount: u64) -> Result<u64> {
        let allowed = self
            .input_end
            .map(|end| amount.min(end.saturating_sub(self.pos)))
            .unwrap_or(amount);
        let skipped = match self.r.skip_bytes(allowed) {
            Ok(skipped) => skipped.min(allowed),
            Err(error) => return self.record(Failure::Io(error.kind())),
        };
        self.pos = self.pos.saturating_add(skipped);
        Ok(skipped)
    }

    fn read_pad(&mut self, _header: ChunkHeader) -> Result<bool> {
        if self.input_end.map(|end| self.pos >= end).unwrap_or(false) {
            return Ok(false);
        }
        let mut pad = [0_u8; 1];
        match read_retrying(&mut self.r, &mut pad) {
            Ok(0) => Ok(false),
            Ok(_) => {
                self.pos = self.pos.saturating_add(1);
                Ok(true)
            }
            Err(error) => self.record(Failure::Io(error.kind())),
        }
    }

    fn record<T>(&mut self, failure: Failure) -> Result<T> {
        self.failure = Some(failure);
        Err(failure.into_error())
    }
}

impl<R: Read + Seek> ChunkReader<Seekable<R>> {
    /// Seeks to a chunk header at the current depth.
    pub fn seek_to(&mut self, chunk: &ChunkHeader) -> Result<()> {
        if let Some(failure) = self.failure {
            return Err(failure.into_error());
        }
        if usize::from(chunk.depth) != self.depth {
            return Err(Error::NotInList);
        }
        if let Some(end) = self.level_end() {
            if chunk.offset >= end {
                return Err(Error::TruncatedHeader {
                    offset: chunk.offset,
                    available: 0,
                });
            }
        }
        let target = self.base.checked_add(chunk.offset).ok_or_else(|| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position overflow",
            ))
        })?;
        self.r.0.seek(SeekFrom::Start(target)).map_err(Error::Io)?;
        self.pos = chunk.offset;
        self.current = None;
        self.prefetched_header = None;
        Ok(())
    }
}

impl<R: Source> fmt::Debug for ChunkReader<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChunkReader")
            .field("header", &self.header)
            .field("pos", &self.pos)
            .field("depth", &self.depth)
            .field("current", &self.current.map(|current| current.header))
            .finish_non_exhaustive()
    }
}

/// A bounded reader over the current chunk payload.
pub struct Payload<'r, R: Source> {
    reader: &'r mut ChunkReader<R>,
}

impl<R: Source> Read for Payload<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read_current(buffer)
    }
}

impl<R: Source> fmt::Debug for Payload<'_, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remaining = self.reader.current.map(Current::remaining).unwrap_or(0);
        formatter
            .debug_struct("Payload")
            .field("remaining", &remaining)
            .finish()
    }
}

fn read_partial(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match read_retrying(reader, &mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(filled)
}

/// Reads once, retrying on [`io::ErrorKind::Interrupted`] like
/// [`Read::read_exact`] does.
fn read_retrying<R: Read + ?Sized>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            outcome => return outcome,
        }
    }
}

fn io_error_for(failure: Failure) -> io::Error {
    match failure {
        Failure::Io(kind) => io::Error::from(kind),
        _ => io::Error::new(io::ErrorKind::UnexpectedEof, failure.into_error()),
    }
}
