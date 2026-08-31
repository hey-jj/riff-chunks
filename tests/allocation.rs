use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{Cursor, Read};

use riff_chunks::{ChunkReader, Chunks, Error, Riff, Seekable, Source};

const V12: &[u8] = include_bytes!("data/fixtures/ref-ffmpeg.avi");
const V13: &[u8] = include_bytes!("data/fixtures/ref-ffmpeg-rf64.wav");
const M02: &[u8] = include_bytes!("data/fixtures/m02-size-beyond-eof-huge.riff");

struct Counter;

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: The system allocator receives the unchanged layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout came from the system allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(size);
        // SAFETY: The pointer and layout came from the system allocator.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: Counter = Counter;

#[test]
fn zero_copy_slice() {
    let file = Riff::parse(V12).unwrap();
    let start = V12.as_ptr() as usize;
    let end = start + V12.len();
    reset();
    let mut count = 0;
    walk_slice(file.chunks(), start, end, &mut count).unwrap();
    let allocations = stop();
    assert_eq!(count, 23);
    assert_eq!(allocations, 0);
}

#[test]
fn zero_allocation_hot_path() {
    let mut reader = ChunkReader::new(Cursor::new(V12)).unwrap();
    reset();
    let mut count = 0;
    walk_stream(&mut reader, &mut count).unwrap();
    let allocations = stop();
    assert_eq!(count, 23);
    assert_eq!(allocations, 0);

    reset();
    let mut reader = ChunkReader::new(Seekable(Cursor::new(V13))).unwrap();
    let mut count = 0;
    walk_stream(&mut reader, &mut count).unwrap();
    let allocations = stop();
    assert_eq!(count, 5);
    assert_eq!(allocations, 1);

    reset();
    let file = Riff::parse(M02).unwrap();
    assert!(matches!(
        file.chunks().next().unwrap(),
        Err(Error::Truncated { .. })
    ));
    let mut reader = ChunkReader::new(Cursor::new(M02)).unwrap();
    assert!(matches!(reader.next(), Err(Error::Truncated { .. })));
    let allocations = stop();
    assert_eq!(allocations, 0);
}

#[test]
fn bounded_ds64_allocation_on_bare_read() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RF64");
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"ds64");
    bytes.extend_from_slice(&0xFFFF_FFF0_u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 24]);
    bytes.extend_from_slice(&357_913_937_u32.to_le_bytes());
    assert_eq!(bytes.len(), 48);

    let mut source = BareRead {
        bytes: &bytes,
        pos: 0,
    };
    reset();
    let error = ChunkReader::new(&mut source).err().unwrap();
    let allocations = stop();
    let largest = LARGEST.with(Cell::get);
    assert!(matches!(error, Error::Truncated { .. }), "got {error:?}");
    assert!(
        largest <= 128 * 1024,
        "largest single allocation was {largest} bytes across {allocations} allocations"
    );
}

fn reset() {
    ALLOCATIONS.with(|allocations| allocations.set(0));
    LARGEST.with(|largest| largest.set(0));
    ENABLED.with(|enabled| enabled.set(true));
}

fn stop() -> usize {
    ENABLED.with(|enabled| enabled.set(false));
    ALLOCATIONS.with(Cell::get)
}

fn count_allocation(size: usize) {
    ENABLED.with(|enabled| {
        if enabled.get() {
            ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
            LARGEST.with(|largest| largest.set(largest.get().max(size)));
        }
    });
}

struct BareRead<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Read for BareRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let available = &self.bytes[self.pos..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.pos += take;
        Ok(take)
    }
}

fn walk_slice(
    chunks: Chunks<'_>,
    start: usize,
    end: usize,
    count: &mut usize,
) -> Result<(), Error> {
    for item in chunks {
        let chunk = item?;
        let payload = chunk.payload.as_ptr() as usize;
        assert!(payload >= start && payload <= end);
        assert!(payload.saturating_add(chunk.payload.len()) <= end);
        *count += 1;
        if chunk.header.is_list() {
            walk_slice(chunk.children()?, start, end, count)?;
        }
    }
    Ok(())
}

fn walk_stream<R: Source>(reader: &mut ChunkReader<R>, count: &mut usize) -> Result<(), Error> {
    while let Some(header) = reader.next()? {
        *count += 1;
        if header.is_list() {
            reader.enter()?;
            walk_stream(reader, count)?;
            reader.leave()?;
        }
    }
    Ok(())
}
