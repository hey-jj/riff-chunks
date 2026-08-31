use core::fmt;
use core::iter::FusedIterator;

use crate::error::Failure;
use crate::{Error, Fourcc, Result};

/// Maximum number of list levels that a reader or writer can enter.
pub const MAX_DEPTH: usize = 32;

/// The container form and its byte order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    /// The little-endian RIFF form.
    Riff,
    /// A big-endian RIFX container.
    Rifx,
    /// An RF64 container with 64-bit sizes in a `ds64` chunk.
    Rf64,
    /// A BW64 container with the RF64 layout.
    Bw64,
}

impl Form {
    /// Returns true for the big-endian RIFX form.
    pub const fn is_big_endian(self) -> bool {
        matches!(self, Self::Rifx)
    }

    /// Returns true when the form requires a leading `ds64` chunk.
    pub const fn has_ds64(self) -> bool {
        matches!(self, Self::Rf64 | Self::Bw64)
    }

    #[cfg(feature = "std")]
    pub(crate) const fn id(self) -> Fourcc {
        match self {
            Self::Riff => Fourcc::RIFF,
            Self::Rifx => Fourcc::RIFX,
            Self::Rf64 => Fourcc::RF64,
            Self::Bw64 => Fourcc::BW64,
        }
    }
}

/// The twelve-byte container header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Container form.
    pub form: Form,
    /// Declared size of the bytes after the size field.
    pub size: u32,
    /// Four-byte form type such as `WAVE`, `AVI `, or `WEBP`.
    pub form_type: Fourcc,
}

/// The resolved contents of an RF64 or BW64 size table.
#[derive(Clone, Copy, Debug)]
pub struct Ds64<'a> {
    /// Resolved container size.
    pub riff_size: u64,
    /// Resolved size for a `data` chunk with a sentinel size.
    pub data_size: u64,
    /// Informational sample count.
    pub sample_count: u64,
    pub(crate) table: &'a [u8],
}

impl<'a> Ds64<'a> {
    /// Iterates over the non-`data` size entries in file order.
    pub fn table(&self) -> impl Iterator<Item = (Fourcc, u64)> + 'a {
        self.table.chunks_exact(12).map(|entry| {
            let id = Fourcc([entry[0], entry[1], entry[2], entry[3]]);
            let size = read_u64_le(&entry[4..12]);
            (id, size)
        })
    }

    /// Resolves a sentinel size for `id`.
    pub fn size_of(&self, id: Fourcc) -> Option<u64> {
        if id == Fourcc(*b"data") {
            Some(self.data_size)
        } else {
            self.table()
                .find_map(|(entry_id, size)| (entry_id == id).then_some(size))
        }
    }
}

/// A resolved chunk header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChunkHeader {
    /// Chunk identifier.
    pub id: Fourcc,
    /// Resolved payload size without a pad byte.
    pub size: u64,
    /// Absolute offset of the eight-byte chunk header.
    pub offset: u64,
    /// List depth, with zero at the top level.
    pub depth: u8,
}

impl ChunkHeader {
    /// Returns true when this is a `LIST` chunk.
    pub fn is_list(&self) -> bool {
        self.id.0 == Fourcc::LIST.0
    }

    /// Returns the payload size including word-alignment padding.
    ///
    /// The result saturates only for `u64::MAX`, which cannot have a
    /// representable padded extent. Readers reject that size before slicing.
    pub const fn padded_size(&self) -> u64 {
        self.size.saturating_add(self.size & 1)
    }

    /// Returns the header, payload, and padding size.
    ///
    /// The result saturates when the mathematical extent exceeds `u64`.
    pub const fn extent(&self) -> u64 {
        8_u64.saturating_add(self.padded_size())
    }
}

/// A borrowed view of a complete container.
pub struct Riff<'a> {
    data: &'a [u8],
    header: Header,
    ds64: Option<Ds64<'a>>,
}

impl fmt::Debug for Riff<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Riff")
            .field("header", &self.header)
            .field("ds64", &self.ds64)
            .field("len", &self.data.len())
            .finish()
    }
}

impl<'a> Riff<'a> {
    /// Parses the file header and any required 64-bit size table.
    ///
    /// # Examples
    ///
    /// ```
    /// use riff_chunks::{Fourcc, Riff};
    ///
    /// let bytes = b"RIFF\x10\x00\x00\x00TESTdata\x04\x00\x00\x00abcd";
    /// let file = Riff::parse(bytes)?;
    /// let chunk = file.chunks().find(Fourcc::from_str("data"))?.unwrap();
    ///
    /// assert_eq!(chunk.payload, b"abcd");
    /// # Ok::<(), riff_chunks::Error>(())
    /// ```
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = parse_header(data)?;
        let ds64 = if header.form.has_ds64() {
            Some(parse_borrowed_ds64(data)?)
        } else {
            None
        };
        Ok(Self { data, header, ds64 })
    }

    /// Returns the parsed file header.
    pub const fn header(&self) -> Header {
        self.header
    }

    /// Returns the RF64 or BW64 size table when present.
    pub const fn ds64(&self) -> Option<Ds64<'a>> {
        self.ds64
    }

    /// Creates an iterator over top-level chunks.
    pub fn chunks(&self) -> Chunks<'a> {
        Chunks::new(
            &self.data[12..],
            12,
            0,
            self.header.form.is_big_endian(),
            self.ds64,
            false,
        )
    }

    /// Returns all bytes after the twelve-byte file header.
    pub fn body(&self) -> &'a [u8] {
        &self.data[12..]
    }
}

/// A chunk whose payload borrows from the input.
#[derive(Clone, Copy, Debug)]
pub struct Chunk<'a> {
    /// Resolved chunk header.
    pub header: ChunkHeader,
    /// Exact payload bytes without a pad byte.
    pub payload: &'a [u8],
    big_endian: bool,
    ds64: Option<Ds64<'a>>,
}

impl<'a> Chunk<'a> {
    /// Returns the first four payload bytes as the list type.
    pub fn list_type(&self) -> Result<Fourcc> {
        if !self.header.is_list() {
            return Err(Error::NotAList { id: self.header.id });
        }
        if self.payload.len() < 4 {
            return Err(Error::ListTooShort {
                id: self.header.id,
                size: self.header.size,
            });
        }
        Ok(Fourcc([
            self.payload[0],
            self.payload[1],
            self.payload[2],
            self.payload[3],
        ]))
    }

    /// Creates an iterator over this list's direct children.
    pub fn children(&self) -> Result<Chunks<'a>> {
        self.list_type()?;
        if usize::from(self.header.depth) >= MAX_DEPTH {
            return Err(Error::TooDeep { max: MAX_DEPTH });
        }
        Ok(Chunks::new(
            &self.payload[4..],
            self.header.offset.saturating_add(12),
            self.header.depth + 1,
            self.big_endian,
            self.ds64,
            true,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
enum IterState {
    Running,
    Pending(Failure),
    Done,
}

/// An iterator over one chunk nesting level.
pub struct Chunks<'a> {
    data: &'a [u8],
    pos: usize,
    base: u64,
    depth: u8,
    big_endian: bool,
    ds64: Option<Ds64<'a>>,
    parent_bound: bool,
    state: IterState,
}

impl fmt::Debug for Chunks<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Chunks")
            .field("pos", &self.pos)
            .field("base", &self.base)
            .field("depth", &self.depth)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<'a> Chunks<'a> {
    fn new(
        data: &'a [u8],
        base: u64,
        depth: u8,
        big_endian: bool,
        ds64: Option<Ds64<'a>>,
        parent_bound: bool,
    ) -> Self {
        Self {
            data,
            pos: 0,
            base,
            depth,
            big_endian,
            ds64,
            parent_bound,
            state: IterState::Running,
        }
    }

    /// Finds the next chunk with `id` at this nesting level.
    pub fn find(&mut self, id: Fourcc) -> Result<Option<Chunk<'a>>> {
        for item in self.by_ref() {
            let chunk = item?;
            if chunk.header.id == id {
                return Ok(Some(chunk));
            }
        }
        Ok(None)
    }

    fn fail(&mut self, failure: Failure) -> Option<Result<Chunk<'a>>> {
        self.state = IterState::Done;
        Some(Err(failure.into_error()))
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            IterState::Done => return None,
            IterState::Pending(failure) => return self.fail(failure),
            IterState::Running => {}
        }

        let remaining = self.data.len().saturating_sub(self.pos);
        if remaining == 0 {
            self.state = IterState::Done;
            return None;
        }

        let offset = self
            .base
            .saturating_add(u64::try_from(self.pos).unwrap_or(u64::MAX));
        if remaining < 8 {
            return self.fail(Failure::TruncatedHeader(
                offset,
                u64::try_from(remaining).unwrap_or(u64::MAX),
            ));
        }

        let header_bytes = &self.data[self.pos..self.pos + 8];
        let id = Fourcc([
            header_bytes[0],
            header_bytes[1],
            header_bytes[2],
            header_bytes[3],
        ]);
        let size32 = read_u32(&header_bytes[4..8], self.big_endian);
        let size = match resolve_size(size32, id, self.ds64) {
            Ok(size) => size,
            Err(error) => {
                self.state = IterState::Done;
                return Some(Err(error));
            }
        };

        if id == Fourcc::LIST && size < 4 {
            return self.fail(Failure::ListTooShort(id, size));
        }

        let payload_end = match 8_u64.checked_add(size) {
            Some(end) => end,
            None => {
                return self.fail(Failure::Truncated(
                    id,
                    size,
                    u64::try_from(remaining - 8).unwrap_or(u64::MAX),
                ));
            }
        };
        let extent = match payload_end.checked_add(size & 1) {
            Some(end) => end,
            None => {
                return self.fail(Failure::Truncated(
                    id,
                    size,
                    u64::try_from(remaining - 8).unwrap_or(u64::MAX),
                ));
            }
        };
        let remaining_u64 = u64::try_from(remaining).unwrap_or(u64::MAX);

        if self.parent_bound && extent > remaining_u64 {
            return self.fail(Failure::ChildExceedsParent(id, size, remaining_u64));
        }
        if payload_end > remaining_u64 {
            return self.fail(Failure::Truncated(
                id,
                size,
                remaining_u64.saturating_sub(8),
            ));
        }

        let payload_size = match usize::try_from(size) {
            Ok(size) => size,
            Err(_) => {
                return self.fail(Failure::Truncated(
                    id,
                    size,
                    remaining_u64.saturating_sub(8),
                ));
            }
        };
        let payload_start = self.pos + 8;
        let payload_stop = match payload_start.checked_add(payload_size) {
            Some(stop) => stop,
            None => {
                return self.fail(Failure::Truncated(
                    id,
                    size,
                    remaining_u64.saturating_sub(8),
                ));
            }
        };
        let payload = &self.data[payload_start..payload_stop];
        let missing_pad = extent > remaining_u64;
        self.pos = if missing_pad {
            payload_stop
        } else {
            match usize::try_from(extent) {
                Ok(extent) => self.pos + extent,
                Err(_) => self.data.len(),
            }
        };

        if missing_pad {
            self.state = IterState::Pending(Failure::MissingPad(
                id,
                offset.saturating_add(8).saturating_add(size),
            ));
        }

        Some(Ok(Chunk {
            header: ChunkHeader {
                id,
                size,
                offset,
                depth: self.depth,
            },
            payload,
            big_endian: self.big_endian,
            ds64: self.ds64,
        }))
    }
}

impl FusedIterator for Chunks<'_> {}

pub(crate) fn parse_header(data: &[u8]) -> Result<Header> {
    if data.len() < 12 {
        return Err(Error::TruncatedHeader {
            offset: 0,
            available: u64::try_from(data.len()).unwrap_or(u64::MAX),
        });
    }
    let found = [data[0], data[1], data[2], data[3]];
    let form = form_from_id(found)?;
    Ok(Header {
        form,
        size: read_u32(&data[4..8], form.is_big_endian()),
        form_type: Fourcc([data[8], data[9], data[10], data[11]]),
    })
}

pub(crate) fn form_from_id(found: [u8; 4]) -> Result<Form> {
    match Fourcc(found) {
        Fourcc::RIFF => Ok(Form::Riff),
        Fourcc::RIFX => Ok(Form::Rifx),
        Fourcc::RF64 => Ok(Form::Rf64),
        Fourcc::BW64 => Ok(Form::Bw64),
        _ => Err(Error::NotRiff { found }),
    }
}

pub(crate) fn read_u32(bytes: &[u8], big_endian: bool) -> u32 {
    let value = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if big_endian {
        u32::from_be_bytes(value)
    } else {
        u32::from_le_bytes(value)
    }
}

pub(crate) fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

pub(crate) fn parse_ds64_payload(payload: &[u8], declared: u64) -> Result<Ds64<'_>> {
    if declared < 28 || payload.len() < 28 {
        return Err(Error::BadDs64 { size: declared });
    }
    let table_len = read_u32(&payload[24..28], false);
    let table_bytes = match u64::from(table_len).checked_mul(12) {
        Some(bytes) => bytes,
        None => return Err(Error::BadDs64 { size: declared }),
    };
    let required = match 28_u64.checked_add(table_bytes) {
        Some(required) => required,
        None => return Err(Error::BadDs64 { size: declared }),
    };
    if required > declared {
        return Err(Error::BadDs64 { size: declared });
    }
    let required_usize = match usize::try_from(required) {
        Ok(required) if required <= payload.len() => required,
        _ => return Err(Error::BadDs64 { size: declared }),
    };
    Ok(Ds64 {
        riff_size: read_u64_le(&payload[0..8]),
        data_size: read_u64_le(&payload[8..16]),
        sample_count: read_u64_le(&payload[16..24]),
        table: &payload[28..required_usize],
    })
}

fn parse_borrowed_ds64(data: &[u8]) -> Result<Ds64<'_>> {
    if data.len() < 20 {
        return Err(Error::TruncatedHeader {
            offset: 12,
            available: u64::try_from(data.len().saturating_sub(12)).unwrap_or(u64::MAX),
        });
    }
    let id = Fourcc([data[12], data[13], data[14], data[15]]);
    if id != Fourcc::DS64 {
        return Err(Error::MissingDs64 { found: id });
    }
    let size = u64::from(read_u32(&data[16..20], false));
    if size < 28 {
        return Err(Error::BadDs64 { size });
    }
    let available = data.len().saturating_sub(20);
    let size_usize = match usize::try_from(size) {
        Ok(size) if size <= available => size,
        _ => {
            return Err(Error::Truncated {
                id,
                declared: size,
                available: u64::try_from(available).unwrap_or(u64::MAX),
            });
        }
    };
    parse_ds64_payload(&data[20..20 + size_usize], size)
}

pub(crate) fn resolve_size(size32: u32, id: Fourcc, ds64: Option<Ds64<'_>>) -> Result<u64> {
    if size32 != u32::MAX {
        return Ok(u64::from(size32));
    }
    match ds64 {
        Some(ds64) => ds64.size_of(id).ok_or(Error::UnresolvedSize { id }),
        None => Ok(u64::from(size32)),
    }
}
