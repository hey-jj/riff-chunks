use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use riff_chunks::{ChunkReader, Error, Seekable, Source};

const DATA_SIZE: u64 = 8 * 1024 * 1024 * 1024;
const LIST_OFFSET: u64 = 80 + DATA_SIZE;
const FILE_LEN: u64 = LIST_OFFSET + 26;

#[test]
fn seekable_sparse_perf_vector() {
    let path = temp_path();
    let cleanup = Cleanup(path.clone());
    create_sparse_file(&path);

    let file = File::open(&path).unwrap();
    let counted = CountingFile {
        file,
        bytes_read: Cell::new(0),
    };
    let start = Instant::now();
    let mut reader = ChunkReader::new(Seekable(counted)).unwrap();
    let mut count = 0;
    walk(&mut reader, &mut count).unwrap();
    let elapsed = start.elapsed();
    let counted = reader.into_inner().0;

    assert_eq!(count, 5);
    assert!(elapsed < Duration::from_millis(50), "elapsed {elapsed:?}");
    assert!(counted.bytes_read.get() < 64 * 1024);
    drop(cleanup);
}

fn create_sparse_file(path: &PathBuf) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.set_len(FILE_LEN).unwrap();

    let mut header = Vec::new();
    header.extend_from_slice(b"RF64");
    header.extend_from_slice(&u32::MAX.to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"ds64");
    header.extend_from_slice(&28_u32.to_le_bytes());
    header.extend_from_slice(&(FILE_LEN - 8).to_le_bytes());
    header.extend_from_slice(&DATA_SIZE.to_le_bytes());
    header.extend_from_slice(&0_u64.to_le_bytes());
    header.extend_from_slice(&0_u32.to_le_bytes());
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&[0; 16]);
    header.extend_from_slice(b"data");
    header.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(header.len(), 80);
    file.write_all(&header).unwrap();

    file.seek(SeekFrom::Start(LIST_OFFSET)).unwrap();
    file.write_all(b"LIST").unwrap();
    file.write_all(&18_u32.to_le_bytes()).unwrap();
    file.write_all(b"INFO").unwrap();
    file.write_all(b"ISFT").unwrap();
    file.write_all(&5_u32.to_le_bytes()).unwrap();
    file.write_all(b"perf!").unwrap();
    file.write_all(&[0]).unwrap();
    file.flush().unwrap();
}

fn walk<R: Source>(reader: &mut ChunkReader<R>, count: &mut usize) -> Result<(), Error> {
    while let Some(header) = reader.next()? {
        *count += 1;
        if header.is_list() {
            reader.enter()?;
            walk(reader, count)?;
            reader.leave()?;
        }
    }
    Ok(())
}

struct CountingFile {
    file: File,
    bytes_read: Cell<usize>,
}

impl Read for CountingFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read(buffer)?;
        self.bytes_read.set(self.bytes_read.get() + read);
        Ok(read)
    }
}

impl Seek for CountingFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temp_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "riff-chunks-perf-{}-{nonce}.riff",
        std::process::id()
    ))
}
