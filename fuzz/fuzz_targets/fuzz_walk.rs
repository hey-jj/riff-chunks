#![no_main]

use std::io::{Cursor, Read};

use libfuzzer_sys::fuzz_target;
use riff_chunks::{ChunkReader, Chunks, Error, Riff, Seekable, Source};

fuzz_target!(|data: &[u8]| {
    if let Ok(file) = Riff::parse(data) {
        let _ = drain_slice(file.chunks());
    }
    if let Ok(mut reader) = ChunkReader::new(Seekable(Cursor::new(data))) {
        let _ = drain_stream(&mut reader);
    }
    let mut plain = PlainRead { bytes: data, pos: 0 };
    if let Ok(mut reader) = ChunkReader::new(&mut plain) {
        let _ = drain_stream(&mut reader);
    }
});

/// A `Read`-only source, so the walker runs without a known input length.
struct PlainRead<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Read for PlainRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let available = &self.bytes[self.pos..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.pos += take;
        Ok(take)
    }
}

fn drain_slice(mut chunks: Chunks<'_>) -> Result<(), Error> {
    while let Some(item) = chunks.next() {
        let chunk = item?;
        if chunk.header.is_list() {
            drain_slice(chunk.children()?)?;
        }
    }
    Ok(())
}

fn drain_stream<R: Source>(reader: &mut ChunkReader<R>) -> Result<(), Error> {
    while let Some(header) = reader.next()? {
        if header.is_list() {
            reader.enter()?;
            drain_stream(reader)?;
            reader.leave()?;
        }
    }
    Ok(())
}

