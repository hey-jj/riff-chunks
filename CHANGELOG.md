# Changelog

## 0.1.0 - 2026-08-22

### Added

- Borrowed RIFF, RIFX, RF64, and BW64 parsing with explicit list descent.
- Streaming chunk reads with bounded payload access and seek-based skipping.
- `Debug` for `Riff`, `Chunks`, `ChunkReader`, `Payload`, and `ChunkWriter`.
- `ds64` table buffers grow only as the source delivers bytes, so a declared size cannot force a large allocation on a source with no known length.
- Reader positions count from the source position at construction, so a container embedded in a larger input parses the same on every source type.
- Low-level reads retry on `io::ErrorKind::Interrupted`, matching `Read::read_exact`.
- RIFF and RIFX writing with size patching and word alignment.
- Typed errors for malformed headers, bounds, padding, nesting, and 64-bit sizes.
- Notes: Pass custom readers by mutable reference. Pass the `Seekable` wrapper by value to use its separate sealed implementation for positional skipping.
- `finish` consumes the writer. An unbalanced call reports the open-scope count and leaves the output in its current state.
- `Write::write` reports a 32-bit size overflow with `io::ErrorKind::InvalidInput`. Scope methods return the crate's typed size error.
