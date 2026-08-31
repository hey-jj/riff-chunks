use std::fmt;
use std::io::{self, Seek, SeekFrom, Write};

use crate::{Error, Feature, Form, Fourcc, Result, MAX_DEPTH};

#[derive(Clone, Copy, Debug)]
struct ListFrame {
    size_field: u64,
    payload_start: u64,
}

impl ListFrame {
    const EMPTY: Self = Self {
        size_field: 0,
        payload_start: 0,
    };
}

#[derive(Clone, Copy, Debug)]
struct ChunkFrame {
    size_field: u64,
    size: u64,
}

/// A seekable RIFF or RIFX writer.
///
/// Chunk and list sizes are patched when their scopes close. The file size is
/// patched by [`finish`](Self::finish).
pub struct ChunkWriter<W: Write + Seek> {
    w: W,
    form: Form,
    pos: u64,
    top_size_field: u64,
    top_payload_start: u64,
    lists: [ListFrame; MAX_DEPTH],
    list_depth: usize,
    chunk: Option<ChunkFrame>,
}

impl<W: Write + Seek> ChunkWriter<W> {
    /// Writes a file header with placeholder size fields.
    ///
    /// RF64 and BW64 return [`Unsupported`](Error::Unsupported) before any
    /// bytes are written.
    pub fn new(mut output: W, form: Form, form_type: Fourcc) -> Result<Self> {
        if form.has_ds64() {
            return Err(Error::Unsupported(Feature::Rf64Write));
        }
        let start = output.stream_position().map_err(Error::Io)?;
        output.write_all(form.id().as_bytes()).map_err(Error::Io)?;
        output.write_all(&[0; 4]).map_err(Error::Io)?;
        output.write_all(form_type.as_bytes()).map_err(Error::Io)?;
        Ok(Self {
            w: output,
            form,
            pos: start.saturating_add(12),
            top_size_field: start.saturating_add(4),
            top_payload_start: start.saturating_add(8),
            lists: [ListFrame::EMPTY; MAX_DEPTH],
            list_depth: 0,
            chunk: None,
        })
    }

    /// Writes one complete chunk and its optional zero pad byte.
    pub fn chunk(&mut self, id: Fourcc, payload: &[u8]) -> Result<()> {
        self.require_container()?;
        let size = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if size > u64::from(u32::MAX) {
            return Err(Error::TooLarge { size });
        }
        self.write_all_counted(id.as_bytes())?;
        self.write_size(u32::try_from(size).map_err(|_| Error::TooLarge { size })?)?;
        self.write_all_counted(payload)?;
        if size & 1 != 0 {
            self.write_all_counted(&[0])?;
        }
        Ok(())
    }

    /// Opens a chunk for writes through the [`Write`] implementation.
    pub fn begin_chunk(&mut self, id: Fourcc) -> Result<()> {
        self.require_container()?;
        self.write_all_counted(id.as_bytes())?;
        let size_field = self.pos;
        self.write_all_counted(&[0; 4])?;
        self.chunk = Some(ChunkFrame {
            size_field,
            size: 0,
        });
        Ok(())
    }

    /// Closes the current chunk, patches its size, and writes any pad byte.
    pub fn end_chunk(&mut self) -> Result<()> {
        let frame = self.chunk.ok_or(Error::NoOpenChunk)?;
        self.patch_size(frame.size_field, frame.size)?;
        if frame.size & 1 != 0 {
            self.write_all_counted(&[0])?;
        }
        self.chunk = None;
        Ok(())
    }

    /// Opens a `LIST` and writes its four-byte list type.
    pub fn begin_list(&mut self, list_type: Fourcc) -> Result<()> {
        self.require_container()?;
        if self.list_depth >= MAX_DEPTH {
            return Err(Error::TooDeep { max: MAX_DEPTH });
        }
        self.write_all_counted(Fourcc::LIST.as_bytes())?;
        let size_field = self.pos;
        self.write_all_counted(&[0; 4])?;
        let payload_start = self.pos;
        self.write_all_counted(list_type.as_bytes())?;
        self.lists[self.list_depth] = ListFrame {
            size_field,
            payload_start,
        };
        self.list_depth += 1;
        Ok(())
    }

    /// Closes the current list and patches its payload size.
    pub fn end_list(&mut self) -> Result<()> {
        if self.chunk.is_some() {
            return Err(Error::Unbalanced {
                open: self.open_count(),
            });
        }
        if self.list_depth == 0 {
            return Err(Error::NotInList);
        }
        let frame = self.lists[self.list_depth - 1];
        let size = self.pos.saturating_sub(frame.payload_start);
        self.patch_size(frame.size_field, size)?;
        self.list_depth -= 1;
        Ok(())
    }

    /// Patches the file size and returns the wrapped output.
    pub fn finish(mut self) -> Result<W> {
        let open = self.open_count();
        if open != 0 {
            return Err(Error::Unbalanced { open });
        }
        let size = self.pos.saturating_sub(self.top_payload_start);
        self.patch_size(self.top_size_field, size)?;
        Ok(self.w)
    }

    /// Returns the absolute output position.
    pub const fn position(&self) -> u64 {
        self.pos
    }

    fn require_container(&self) -> Result<()> {
        if self.chunk.is_some() {
            Err(Error::Unbalanced {
                open: self.open_count(),
            })
        } else {
            Ok(())
        }
    }

    fn open_count(&self) -> usize {
        self.list_depth + usize::from(self.chunk.is_some())
    }

    fn write_size(&mut self, size: u32) -> Result<()> {
        let bytes = if self.form.is_big_endian() {
            size.to_be_bytes()
        } else {
            size.to_le_bytes()
        };
        self.write_all_counted(&bytes)
    }

    fn write_all_counted(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes).map_err(Error::Io)?;
        self.pos = self
            .pos
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or(Error::TooLarge { size: u64::MAX })?;
        Ok(())
    }

    fn patch_size(&mut self, size_field: u64, size: u64) -> Result<()> {
        let size32 = u32::try_from(size).map_err(|_| Error::TooLarge { size })?;
        let end = self.pos;
        self.w
            .seek(SeekFrom::Start(size_field))
            .map_err(Error::Io)?;
        let bytes = if self.form.is_big_endian() {
            size32.to_be_bytes()
        } else {
            size32.to_le_bytes()
        };
        self.w.write_all(&bytes).map_err(Error::Io)?;
        self.w.seek(SeekFrom::Start(end)).map_err(Error::Io)?;
        Ok(())
    }
}

impl<W: Write + Seek> fmt::Debug for ChunkWriter<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChunkWriter")
            .field("form", &self.form)
            .field("pos", &self.pos)
            .field("list_depth", &self.list_depth)
            .field("chunk", &self.chunk)
            .finish_non_exhaustive()
    }
}

impl<W: Write + Seek> Write for ChunkWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let frame = match self.chunk {
            Some(frame) => frame,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    Error::NoOpenChunk,
                ));
            }
        };
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let next_size = frame.size.saturating_add(requested);
        if next_size > u64::from(u32::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                Error::TooLarge { size: next_size },
            ));
        }
        let written = self.w.write(buffer)?;
        let written_u64 = u64::try_from(written).unwrap_or(u64::MAX);
        self.pos = self.pos.saturating_add(written_u64);
        if let Some(frame) = self.chunk.as_mut() {
            frame.size = frame.size.saturating_add(written_u64);
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}
