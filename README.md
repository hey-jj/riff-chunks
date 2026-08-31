# riff-chunks

`riff-chunks` reads and writes Resource Interchange File Format containers. It supports RIFF and RIFX files, plus read access to RF64 and BW64 files.

The borrowed reader walks a byte slice without allocating or copying payloads. The streaming reader accepts owned standard readers and custom readers passed as `&mut R`. Wrap a reader in `Seekable` to skip unread payloads with seeks. The writer patches chunk, list, and file sizes when each scope closes.

The crate has no dependencies. Disable the default `std` feature to use the borrowed reader in a `no_std` program.

## Borrowed input

```rust
use riff_chunks::{Fourcc, Riff};

let bytes = b"RIFF\x10\x00\x00\x00TESTdata\x04\x00\x00\x00abcd";
let file = Riff::parse(bytes)?;
let chunk = file.chunks().find(Fourcc::from_str("data"))?.unwrap();

assert_eq!(chunk.payload, b"abcd");
# Ok::<(), riff_chunks::Error>(())
```

`Chunk::children` descends into one `LIST` level. The caller chooses which lists to enter, so finding an unrelated top-level chunk does not inspect nested payloads.

## Streaming input

```rust
use riff_chunks::{ChunkReader, Fourcc};
use std::io::{Cursor, Read};

let bytes = b"RIFF\x10\x00\x00\x00TESTdata\x04\x00\x00\x00abcd";
let mut reader = ChunkReader::new(Cursor::new(bytes.as_slice()))?;

let header = reader.next()?.unwrap();
assert_eq!(header.id, Fourcc::from_str("data"));

let mut payload = [0; 4];
reader.payload().read_exact(&mut payload)?;
assert_eq!(&payload, b"abcd");
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ChunkReader::next` skips unread bytes from the previous chunk. A plain reader discards them through a 4 KiB stack buffer. `Seekable<R>` moves over them with `Seek`.

## Writing

```rust
use riff_chunks::{ChunkWriter, Form, Fourcc};
use std::io::Cursor;

let cursor = Cursor::new(Vec::new());
let mut writer = ChunkWriter::new(cursor, Form::Riff, Fourcc::from_str("WAVE"))?;
writer.chunk(Fourcc::from_str("data"), b"abc")?;
let bytes = writer.finish()?.into_inner();

assert_eq!(&bytes[0..4], b"RIFF");
assert_eq!(&bytes[12..16], b"data");
assert_eq!(&bytes[20..23], b"abc");
assert_eq!(bytes[23], 0);
# Ok::<(), riff_chunks::Error>(())
```

RF64 and BW64 writing returns `Unsupported(Feature::Rf64Write)` before writing bytes. Their 64-bit sizes require a `ds64` layout that this release reads and exposes through `Riff::ds64` and `ChunkReader::ds64`.

## Input limits

The readers cap list nesting at 32 levels. Size arithmetic uses checked `u64` operations. Streaming payload reads stay within the declared chunk size, and `read_payload` requires a caller-owned buffer large enough for the remaining bytes.

The file header's declared size is available through `Header::size`. Readers walk to the real input boundary because files produced by streaming tools may leave that field stale. Writers calculate and patch the field.

## License

MIT

