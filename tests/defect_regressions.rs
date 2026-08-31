use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use riff_chunks::{
    ChunkReader, ChunkWriter, Chunks, Error, Feature, Form, Fourcc, Riff, Seekable, Source,
    MAX_DEPTH,
};

const V04: &[u8] = include_bytes!("data/fixtures/v04-nested-list.riff");
const V07: &[u8] = include_bytes!("data/fixtures/v07-rifx.riff");
const V08: &[u8] = include_bytes!("data/fixtures/v08-rf64-small.riff");
const V12: &[u8] = include_bytes!("data/fixtures/ref-ffmpeg.avi");
const V14: &[u8] = include_bytes!("data/fixtures/v14-bw64-small.riff");
const V15: &[u8] = include_bytes!("data/fixtures/v15-riff-size-zero.riff");
const V16: &[u8] = include_bytes!("data/fixtures/v16-riff-size-max.riff");
const V17: &[u8] = include_bytes!("data/fixtures/v17-binary-id.riff");
const M02: &[u8] = include_bytes!("data/fixtures/m02-size-beyond-eof-huge.riff");
const M05: &[u8] = include_bytes!("data/fixtures/m05-trailing-partial-header.riff");
const M06: &[u8] = include_bytes!("data/fixtures/m06-deep-list-40.riff");

#[test]
fn form_rifx() {
    let file = Riff::parse(V07).unwrap();
    assert_eq!(file.header().form, Form::Rifx);
    let chunk = file.chunks().next().unwrap().unwrap();
    assert_eq!(chunk.header.id, Fourcc::from_str("abcd"));
    assert_eq!(chunk.payload, b"wxyz");

    let cursor = Cursor::new(Vec::new());
    let mut writer = ChunkWriter::new(cursor, Form::Rifx, Fourcc::from_str("TEST")).unwrap();
    writer.chunk(Fourcc::from_str("abcd"), b"wxyz").unwrap();
    assert_eq!(writer.finish().unwrap().into_inner(), V07);
}

#[test]
fn form_rf64() {
    for (bytes, form) in [(V08, Form::Rf64), (V14, Form::Bw64)] {
        let file = Riff::parse(bytes).unwrap();
        assert_eq!(file.header().form, form);
        let table = file.ds64().unwrap();
        assert_eq!(table.riff_size, 106);
        assert_eq!(table.data_size, 8);
        assert_eq!(table.sample_count, 4);
        assert_eq!(table.size_of(Fourcc::from_str("tabl")), Some(6));
        assert_eq!(table.size_of(Fourcc::from_str("data")), Some(8));
    }

    let mut output = Cursor::new(Vec::new());
    let error = ChunkWriter::new(&mut output, Form::Rf64, Fourcc::from_str("WAVE"))
        .err()
        .unwrap();
    assert!(matches!(error, Error::Unsupported(Feature::Rf64Write)));
    assert!(output.into_inner().is_empty());
}

#[test]
fn fused_after_error() {
    let file = Riff::parse(M05).unwrap();
    let mut chunks = file.chunks();
    assert!(chunks.next().unwrap().is_ok());
    assert!(matches!(
        chunks.next().unwrap(),
        Err(Error::TruncatedHeader { .. })
    ));
    assert!(chunks.next().is_none());
    assert!(chunks.next().is_none());

    let cursor = Cursor::new(M05);
    let mut reader = ChunkReader::new(cursor).unwrap();
    assert!(reader.next().unwrap().is_some());
    let first = reader.next().unwrap_err();
    assert!(matches!(first, Error::TruncatedHeader { .. }));
    let position = reader.position();
    for _ in 0..3 {
        assert!(matches!(reader.next(), Err(Error::TruncatedHeader { .. })));
        assert_eq!(reader.position(), position);
    }
}

#[test]
fn fourcc_display_never_panics() {
    let chunk = Riff::parse(V17).unwrap().chunks().next().unwrap().unwrap();
    assert_eq!(chunk.header.id.to_string(), "\\xff\\x00\\x80\\x0a");
    assert_eq!(format!("{:?}", chunk.header.id), "\"\\xff\\x00\\x80\\x0a\"");
    for byte in u8::MIN..=u8::MAX {
        for slot in 0..4 {
            let mut value = *b"safe";
            value[slot] = byte;
            let _ = Fourcc(value).to_string();
            let _ = format!("{:?}", Fourcc(value));
        }
    }
}

#[test]
fn deep_nesting() {
    let file = Riff::parse(M06).unwrap();
    let mut count = 0;
    let error = drain_lists(file.chunks(), &mut count).unwrap_err();
    assert_eq!(count, 33);
    assert!(matches!(error, Error::TooDeep { max: MAX_DEPTH }));
}

#[test]
fn riff_size_is_advisory_on_read() {
    for bytes in [V15, V16] {
        let file = Riff::parse(bytes).unwrap();
        assert_eq!(file.chunks().count(), 5);
    }
}

#[test]
fn streaming_no_whole_file_read() {
    let mut source = CountingRead {
        bytes: V12,
        pos: 0,
        total: 0,
        largest: 0,
    };
    let mut reader = ChunkReader::new(&mut source).unwrap();
    let mut chunks = 0;
    drain_stream(&mut reader, &mut chunks).unwrap();
    drop(reader);
    assert_eq!(chunks, 23);
    assert_eq!(source.total, V12.len());
    assert!(
        source.largest <= 4096,
        "largest request was {}",
        source.largest
    );

    let cursor = Cursor::new(M02);
    let mut reader = ChunkReader::new(cursor).unwrap();
    assert!(matches!(reader.next(), Err(Error::Truncated { .. })));
}

#[test]
fn caller_sized_payload_buffer() {
    let bytes = include_bytes!("data/fixtures/v01-minimal-even.riff");
    let mut reader = ChunkReader::new(Cursor::new(bytes.as_slice())).unwrap();
    assert_eq!(reader.next().unwrap().unwrap().size, 4);
    let mut short = [0; 3];
    assert!(matches!(
        reader.read_payload(&mut short),
        Err(Error::BufferTooSmall { needed: 4 })
    ));
    let mut payload = [0; 4];
    assert_eq!(reader.read_payload(&mut payload).unwrap(), 4);
    assert_eq!(&payload, b"wxyz");
}

#[test]
fn writer_alignment_lists_and_streaming_chunks() {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ChunkWriter::new(cursor, Form::Riff, Fourcc::from_str("TEST")).unwrap();
    writer.begin_list(Fourcc::from_str("INFO")).unwrap();
    writer.begin_chunk(Fourcc::from_str("INAM")).unwrap();
    writer.write_all(b"abc").unwrap();
    writer.end_chunk().unwrap();
    writer.end_list().unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 28);
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16);
    assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 3);
    assert_eq!(bytes.last(), Some(&0));

    let cursor = Cursor::new(Vec::new());
    let mut writer = ChunkWriter::new(cursor, Form::Riff, Fourcc::from_str("TEST")).unwrap();
    assert!(writer.write_all(b"x").is_err());
    writer.begin_list(Fourcc::from_str("INFO")).unwrap();
    assert!(matches!(
        writer.finish(),
        Err(Error::Unbalanced { open: 1 })
    ));
}

#[test]
fn fuzz_walk() {
    let start = Instant::now();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(fixture_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 35);
    let mut state = 0x3c6e_f372_fe94_f82b_u64;
    for path in paths {
        let seed = std::fs::read(path).unwrap();
        for iteration in 0..256 {
            let mut bytes = seed.clone();
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            if !bytes.is_empty() {
                let index = usize::try_from(state).unwrap_or(usize::MAX) % bytes.len();
                bytes[index] ^= u8::try_from(state >> 32).unwrap_or(0);
            }
            if iteration % 7 == 0 && !bytes.is_empty() {
                bytes.truncate(iteration % bytes.len());
            }
            let outcome = std::panic::catch_unwind(|| {
                if let Ok(file) = Riff::parse(&bytes) {
                    let mut count = 0;
                    let _ = drain_lists(file.chunks(), &mut count);
                }
                let cursor = Cursor::new(bytes.as_slice());
                if let Ok(mut reader) = ChunkReader::new(Seekable(cursor)) {
                    let mut count = 0;
                    let _ = drain_stream(&mut reader, &mut count);
                }
                let mut plain = PlainRead {
                    bytes: &bytes,
                    pos: 0,
                };
                if let Ok(mut reader) = ChunkReader::new(&mut plain) {
                    let mut count = 0;
                    let _ = drain_stream(&mut reader, &mut count);
                }
            });
            assert!(outcome.is_ok());
        }
    }
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn embedded_container_at_nonzero_offset() {
    let mut buffer = vec![0xA5; 16];
    buffer.extend_from_slice(V04);

    let mut reference_source = Cursor::new(V04);
    let mut reference_reader = ChunkReader::new(&mut reference_source).unwrap();
    let reference = collect(&mut reference_reader);
    assert!(reference.iter().any(|(header, _)| header.is_list()));

    let mut plain = PlainRead {
        bytes: &buffer,
        pos: 16,
    };
    let mut reader = ChunkReader::new(&mut plain).unwrap();
    assert_eq!(collect(&mut reader), reference, "plain read");

    let mut cursor = Cursor::new(buffer.as_slice());
    cursor.set_position(16);
    let mut reader = ChunkReader::new(cursor).unwrap();
    assert_eq!(collect(&mut reader), reference, "cursor");

    let path = std::env::temp_dir().join(format!(
        "riff-chunks-embedded-{}-{}.riff",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, &buffer).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(16)).unwrap();
    let mut reader = ChunkReader::new(file).unwrap();
    let from_file = collect(&mut reader);
    let _ = std::fs::remove_file(&path);
    assert_eq!(from_file, reference, "file");

    let mut cursor = Cursor::new(buffer.as_slice());
    cursor.set_position(16);
    let mut reader = ChunkReader::new(Seekable(cursor)).unwrap();
    assert_eq!(collect(&mut reader), reference, "seekable");

    let mut cursor = Cursor::new(buffer.as_slice());
    cursor.set_position(16);
    let mut reader = ChunkReader::new(Seekable(cursor)).unwrap();
    let first = reader.next().unwrap().unwrap();
    while reader.next().unwrap().is_some() {}
    reader.seek_to(&first).unwrap();
    assert_eq!(reader.next().unwrap().unwrap(), first);
}

#[test]
fn interrupted_reads_are_retried() {
    let mut source = InterruptingRead {
        bytes: V12,
        pos: 0,
        interrupt: true,
    };
    let mut reader = ChunkReader::new(&mut source).unwrap();
    let chunks = collect(&mut reader);
    assert_eq!(chunks.len(), 23);
}

#[test]
fn public_types_implement_debug() {
    let file = Riff::parse(V15).unwrap();
    let rendered = format!("{file:?} {:?}", file.chunks());
    assert!(rendered.contains("Riff") && rendered.contains("Chunks"));
    let mut reader = ChunkReader::new(Cursor::new(V15)).unwrap();
    assert!(format!("{reader:?}").contains("ChunkReader"));
    assert!(format!("{:?}", reader.payload()).contains("Payload"));
    let writer = ChunkWriter::new(
        Cursor::new(Vec::new()),
        Form::Riff,
        Fourcc::from_str("TEST"),
    )
    .unwrap();
    assert!(format!("{writer:?}").contains("ChunkWriter"));
}

#[test]
fn writer_rejects_u32_overflow_before_write() {
    let sink = SparseSink::default();
    let mut writer = ChunkWriter::new(sink, Form::Riff, Fourcc::from_str("TEST")).unwrap();
    writer.begin_chunk(Fourcc::from_str("data")).unwrap();
    let block = vec![0_u8; 1024 * 1024];
    for _ in 0..4095 {
        assert_eq!(writer.write(&block).unwrap(), block.len());
    }
    let position = writer.position();
    let error = writer.write(&block).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(writer.position(), position);
}

fn drain_lists(chunks: Chunks<'_>, count: &mut usize) -> Result<(), Error> {
    for item in chunks {
        let chunk = item?;
        *count += 1;
        if chunk.header.is_list() {
            drain_lists(chunk.children()?, count)?;
        }
    }
    Ok(())
}

fn drain_stream<R: Source>(reader: &mut ChunkReader<R>, count: &mut usize) -> Result<(), Error> {
    while let Some(header) = reader.next()? {
        *count += 1;
        if header.is_list() {
            reader.enter()?;
            drain_stream(reader, count)?;
            reader.leave()?;
        }
    }
    Ok(())
}

fn collect<R: Source>(reader: &mut ChunkReader<R>) -> Vec<(riff_chunks::ChunkHeader, Vec<u8>)> {
    let mut out = Vec::new();
    collect_level(reader, &mut out).unwrap();
    out
}

fn collect_level<R: Source>(
    reader: &mut ChunkReader<R>,
    out: &mut Vec<(riff_chunks::ChunkHeader, Vec<u8>)>,
) -> Result<(), Error> {
    while let Some(header) = reader.next()? {
        if header.is_list() {
            out.push((header, Vec::new()));
            reader.enter()?;
            collect_level(reader, out)?;
            reader.leave()?;
        } else {
            let mut payload = vec![0; usize::try_from(header.size).unwrap()];
            reader.read_payload(&mut payload)?;
            out.push((header, payload));
        }
    }
    Ok(())
}

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

struct InterruptingRead<'a> {
    bytes: &'a [u8],
    pos: usize,
    interrupt: bool,
}

impl Read for InterruptingRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.interrupt {
            self.interrupt = false;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "interrupted",
            ));
        }
        self.interrupt = true;
        let available = &self.bytes[self.pos..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.pos += take;
        Ok(take)
    }
}

struct CountingRead<'a> {
    bytes: &'a [u8],
    pos: usize,
    total: usize,
    largest: usize,
}

impl Read for CountingRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.largest = self.largest.max(buffer.len());
        let available = &self.bytes[self.pos..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.pos += take;
        self.total += take;
        Ok(take)
    }
}

#[derive(Default)]
struct SparseSink {
    position: u64,
    length: u64,
}

impl Write for SparseSink {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.position = self
            .position
            .saturating_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        self.length = self.length.max(self.position);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for SparseSink {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.position = match position {
            std::io::SeekFrom::Start(position) => position,
            std::io::SeekFrom::End(delta) => add_signed(self.length, delta)?,
            std::io::SeekFrom::Current(delta) => add_signed(self.position, delta)?,
        };
        Ok(self.position)
    }
}

fn add_signed(base: u64, delta: i64) -> std::io::Result<u64> {
    if delta >= 0 {
        base.checked_add(delta.unsigned_abs())
    } else {
        base.checked_sub(delta.unsigned_abs())
    }
    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek overflow"))
}
