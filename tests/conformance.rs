mod support;

use std::io::{Cursor, Read};

use riff_chunks::{ChunkReader, ChunkWriter, Chunks, Ds64, Error, Form, Header, Riff, Source};
use support::{hex, manifest, sha256, Json, Sha256};

#[derive(Debug, PartialEq, Eq)]
struct Record {
    id: [u8; 4],
    size: u64,
    offset: u64,
    depth: u8,
    list_type: Option<[u8; 4]>,
    payload_hash: Option<String>,
}

type OwnedDs64 = Option<(u64, u64, u64, Vec<([u8; 4], u64)>)>;
type WalkSuccess = (Header, OwnedDs64, Vec<Record>);
type WalkFailure = (Option<Header>, OwnedDs64, Vec<Record>, Error);
type WalkResult = Result<WalkSuccess, WalkFailure>;

#[derive(Debug)]
struct ReadOnly<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Read for ReadOnly<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let available = &self.bytes[self.pos..];
        let take = available.len().min(buffer.len());
        buffer[..take].copy_from_slice(&available[..take]);
        self.pos += take;
        Ok(take)
    }
}

#[test]
fn malformed_typed_errors() {
    let data = manifest();
    assert_eq!(data.get("schema").as_str(), "riff-conformance-vectors/1");
    assert_eq!(data.get("max_depth").as_u64(), 32);
    let vectors = data.get("vectors").as_array();
    assert_eq!(vectors.len(), 36);

    for vector in vectors {
        let id = vector.get("id").as_str();
        if vector.get("kind").as_str() == "perf" {
            assert_eq!(id, "P01");
            continue;
        }
        let relative = vector.get("file").as_str();
        let bytes = support::fixture(relative);
        assert_eq!(
            bytes.len(),
            usize::try_from(vector.get("len").as_u64()).unwrap(),
            "length for {id}"
        );
        assert_eq!(
            sha256(&bytes),
            vector.get("sha256").as_str(),
            "hash for {id}"
        );

        let borrowed = collect_borrowed(&bytes);
        let mut source = ReadOnly {
            bytes: &bytes,
            pos: 0,
        };
        let streamed = collect_streamed(&mut source);

        match vector.get("kind").as_str() {
            "valid" => {
                let expected_header = vector.get("header");
                let expected_records = expected_records(vector.get("chunks"));
                let (header, ds64, records) =
                    borrowed.unwrap_or_else(|error| panic!("borrowed {id} failed: {error:?}"));
                assert_header(header, expected_header, id);
                assert_ds64(ds64, expected_header.get_opt("ds64"), id);
                assert_eq!(records, expected_records, "borrowed records for {id}");

                let (header, ds64, records) =
                    streamed.unwrap_or_else(|error| panic!("streamed {id} failed: {error:?}"));
                assert_header(header, expected_header, id);
                assert_ds64(ds64, expected_header.get_opt("ds64"), id);
                assert_eq!(records, expected_records, "streamed records for {id}");
                assert_eq!(
                    records.len(),
                    usize::try_from(vector.get("expect").get("chunk_count").as_u64()).unwrap(),
                    "chunk count for {id}"
                );
            }
            "error" => {
                let expected = vector.get("expect");
                let expected_class = expected
                    .get_opt("error_slice")
                    .unwrap_or_else(|| expected.get("error"))
                    .as_str();
                let borrowed_error = borrowed.expect_err(&format!("borrowed {id} succeeded"));
                assert_error(&borrowed_error, expected_class, expected.get("detail"), id);
                assert_eq!(
                    borrowed_error.2.len(),
                    usize::try_from(expected.get("chunks_before_error").as_u64()).unwrap(),
                    "borrowed count for {id}"
                );

                let stream_class = expected
                    .get_opt("error_stream")
                    .unwrap_or_else(|| expected.get("error"))
                    .as_str();
                let stream_error = streamed.expect_err(&format!("streamed {id} succeeded"));
                assert_error(&stream_error, stream_class, expected.get("detail"), id);
                let expected_count = if id == "M08" {
                    1
                } else {
                    usize::try_from(expected.get("chunks_before_error").as_u64()).unwrap()
                };
                assert_eq!(
                    stream_error.2.len(),
                    expected_count,
                    "streamed count for {id}"
                );
            }
            kind => panic!("unexpected vector kind {kind}"),
        }
    }
}

#[test]
fn byte_exact_roundtrip() {
    let data = manifest();
    let vectors = data.get("vectors").as_array();
    for item in data.get("roundtrip").as_array() {
        let id = item.get("source").as_str();
        let vector = vectors
            .iter()
            .find(|vector| vector.get("id").as_str() == id)
            .unwrap_or_else(|| panic!("missing roundtrip source {id}"));
        let bytes = support::fixture(vector.get("file").as_str());
        let (rewritten, error) = rewrite(&bytes);
        if id == "M04" {
            assert!(matches!(error, Some(Error::MissingPad { .. })));
        } else {
            assert!(error.is_none(), "rewrite walk for {id}: {error:?}");
        }
        assert_eq!(
            sha256(&rewritten),
            item.get("walk_then_rewrite_sha256").as_str(),
            "rewrite digest for {id}"
        );
        if item.get("byte_identical_to_source").as_bool() {
            assert_eq!(rewritten, bytes, "byte-exact rewrite for {id}");
        }
    }
}

fn collect_borrowed(bytes: &[u8]) -> WalkResult {
    let file = match Riff::parse(bytes) {
        Ok(file) => file,
        Err(error) => return Err((None, None, Vec::new(), error)),
    };
    let header = file.header();
    let ds64 = owned_ds64(file.ds64());
    let mut records = Vec::new();
    match drain_borrowed(file.chunks(), &mut records) {
        Ok(()) => Ok((header, ds64, records)),
        Err(error) => Err((Some(header), ds64, records, error)),
    }
}

fn drain_borrowed(chunks: Chunks<'_>, records: &mut Vec<Record>) -> Result<(), Error> {
    for item in chunks {
        let chunk = item?;
        let mut record = Record {
            id: chunk.header.id.0,
            size: chunk.header.size,
            offset: chunk.header.offset,
            depth: chunk.header.depth,
            list_type: None,
            payload_hash: None,
        };
        if chunk.header.is_list() {
            record.list_type = Some(chunk.list_type()?.0);
            records.push(record);
            drain_borrowed(chunk.children()?, records)?;
        } else {
            record.payload_hash = Some(sha256(chunk.payload));
            records.push(record);
        }
    }
    Ok(())
}

fn collect_streamed(source: &mut ReadOnly<'_>) -> WalkResult {
    let mut reader = match ChunkReader::new(source) {
        Ok(reader) => reader,
        Err(error) => return Err((None, None, Vec::new(), error)),
    };
    let header = reader.header();
    let ds64 = owned_ds64(reader.ds64());
    let mut records = Vec::new();
    match drain_streamed(&mut reader, &mut records) {
        Ok(()) => Ok((header, ds64, records)),
        Err(error) => Err((Some(header), ds64, records, error)),
    }
}

fn drain_streamed<R: Source>(
    reader: &mut ChunkReader<R>,
    records: &mut Vec<Record>,
) -> Result<(), Error> {
    loop {
        let header = match reader.next()? {
            Some(header) => header,
            None => return Ok(()),
        };
        let record = Record {
            id: header.id.0,
            size: header.size,
            offset: header.offset,
            depth: header.depth,
            list_type: None,
            payload_hash: None,
        };
        if header.is_list() {
            records.push(record);
            let list_type = reader.enter()?;
            records.last_mut().unwrap().list_type = Some(list_type.0);
            drain_streamed(reader, records)?;
            reader.leave()?;
        } else {
            let mut hash = Sha256::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = {
                    let mut payload = reader.payload();
                    match payload.read(&mut buffer) {
                        Ok(read) => read,
                        Err(_) => return Err(reader.next().unwrap_err()),
                    }
                };
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
            }
            let mut record = record;
            record.payload_hash = Some(hex(&hash.finish()));
            records.push(record);
        }
    }
}

fn expected_records(value: &Json) -> Vec<Record> {
    value
        .as_array()
        .iter()
        .map(|chunk| Record {
            id: bytes4(chunk.get("id_hex").as_str()),
            size: chunk.get("size").as_u64(),
            offset: chunk.get("offset").as_u64(),
            depth: u8::try_from(chunk.get("depth").as_u64()).unwrap(),
            list_type: chunk
                .get_opt("list_type")
                .map(|value| bytes4(&hex_from_text(value.as_str()))),
            payload_hash: chunk
                .get_opt("payload_sha256")
                .map(|value| value.as_str().to_owned()),
        })
        .collect()
}

fn owned_ds64(ds64: Option<Ds64<'_>>) -> OwnedDs64 {
    ds64.map(|table| {
        (
            table.riff_size,
            table.data_size,
            table.sample_count,
            table.table().map(|(id, size)| (id.0, size)).collect(),
        )
    })
}

fn assert_header(header: Header, expected: &Json, id: &str) {
    let form = match expected.get("form").as_str() {
        "RIFF" => Form::Riff,
        "RIFX" => Form::Rifx,
        "RF64" => Form::Rf64,
        "BW64" => Form::Bw64,
        value => panic!("unexpected form {value}"),
    };
    assert_eq!(header.form, form, "form for {id}");
    assert_eq!(
        u64::from(header.size),
        expected.get("riff_size").as_u64(),
        "size for {id}"
    );
    assert_eq!(
        header.form_type.0,
        bytes4(expected.get("form_type_hex").as_str()),
        "type for {id}"
    );
}

fn assert_ds64(actual: OwnedDs64, expected: Option<&Json>, id: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => {
            assert_eq!(
                actual.0,
                expected.get("riff_size").as_u64(),
                "64-bit file size for {id}"
            );
            assert_eq!(
                actual.1,
                expected.get("data_size").as_u64(),
                "64-bit data size for {id}"
            );
            assert_eq!(
                actual.2,
                expected.get("sample_count").as_u64(),
                "sample count for {id}"
            );
            let table: Vec<_> = expected
                .get("table")
                .as_object()
                .iter()
                .map(|(key, value)| (bytes4(&hex_from_text(key)), value.as_u64()))
                .collect();
            assert_eq!(actual.3, table, "64-bit table for {id}");
        }
        pair => panic!("size-table mismatch for {id}: {pair:?}"),
    }
}

fn assert_error(result: &WalkFailure, class: &str, detail: &Json, id: &str) {
    let error = &result.3;
    let actual = error_class(error);
    assert_eq!(actual, class, "error class for {id}: {error}");
    match error {
        Error::NotRiff { found } => assert_eq!(*found, bytes4(detail.get("found").as_str())),
        Error::TruncatedHeader { offset, available } => {
            assert_eq!(*offset, detail.get("offset").as_u64());
            assert_eq!(*available, detail.get("available").as_u64());
        }
        Error::Truncated {
            id,
            declared,
            available,
        } => {
            if let Some(expected_id) = detail.get_opt("id") {
                assert_eq!(id.to_string(), expected_id.as_str());
            }
            assert_eq!(*declared, detail.get("declared").as_u64());
            assert_eq!(*available, detail.get("available").as_u64());
        }
        Error::ChildExceedsParent {
            id,
            declared,
            remaining,
        } => {
            if class == "ChildExceedsParent" && result.2.len() != 1 || id.0 != *b"kid " {
                assert_eq!(id.to_string(), detail.get("id").as_str());
                assert_eq!(*declared, detail.get("declared").as_u64());
                assert_eq!(*remaining, detail.get("remaining").as_u64());
            }
        }
        Error::MissingPad { id, offset } => {
            assert_eq!(id.to_string(), detail.get("id").as_str());
            assert_eq!(*offset, detail.get("offset").as_u64());
        }
        Error::ListTooShort { id, size } => {
            assert_eq!(id.to_string(), detail.get("id").as_str());
            assert_eq!(*size, detail.get("size").as_u64());
        }
        Error::TooDeep { max } => {
            assert_eq!(u64::try_from(*max).unwrap(), detail.get("max").as_u64())
        }
        Error::MissingDs64 { found } => assert_eq!(found.to_string(), detail.get("found").as_str()),
        Error::BadDs64 { size } => assert_eq!(*size, detail.get("size").as_u64()),
        Error::UnresolvedSize { id } => assert_eq!(id.to_string(), detail.get("id").as_str()),
        _ => {}
    }
}

fn error_class(error: &Error) -> &'static str {
    match error {
        Error::Io(_) => "Io",
        Error::NotRiff { .. } => "NotRiff",
        Error::TruncatedHeader { .. } => "TruncatedHeader",
        Error::Truncated { .. } => "Truncated",
        Error::ChildExceedsParent { .. } => "ChildExceedsParent",
        Error::MissingPad { .. } => "MissingPad",
        Error::ListTooShort { .. } => "ListTooShort",
        Error::TooDeep { .. } => "TooDeep",
        Error::MissingDs64 { .. } => "MissingDs64",
        Error::BadDs64 { .. } => "BadDs64",
        Error::UnresolvedSize { .. } => "UnresolvedSize",
        Error::NotAList { .. } => "NotAList",
        Error::NotInList => "NotInList",
        Error::NoOpenChunk => "NoOpenChunk",
        Error::BufferTooSmall { .. } => "BufferTooSmall",
        Error::TooLarge { .. } => "TooLarge",
        Error::Unbalanced { .. } => "Unbalanced",
        Error::Unsupported(_) => "Unsupported",
        _ => "Unknown",
    }
}

fn rewrite(bytes: &[u8]) -> (Vec<u8>, Option<Error>) {
    let file = Riff::parse(bytes).unwrap();
    let cursor = Cursor::new(Vec::new());
    let mut writer = ChunkWriter::new(cursor, file.header().form, file.header().form_type).unwrap();
    let error = emit_level(file.chunks(), &mut writer).err();
    let output = writer.finish().unwrap().into_inner();
    (output, error)
}

fn emit_level(chunks: Chunks<'_>, writer: &mut ChunkWriter<Cursor<Vec<u8>>>) -> Result<(), Error> {
    for item in chunks {
        let chunk = item?;
        if chunk.header.is_list() {
            writer.begin_list(chunk.list_type()?)?;
            emit_level(chunk.children()?, writer)?;
            writer.end_list()?;
        } else {
            writer.chunk(chunk.header.id, chunk.payload)?;
        }
    }
    Ok(())
}

fn bytes4(value: &str) -> [u8; 4] {
    let bytes = decode_hex(value);
    bytes
        .try_into()
        .unwrap_or_else(|_| panic!("expected four bytes in {value}"))
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect()
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid hex digit"),
    }
}

fn hex_from_text(value: &str) -> String {
    hex(value.as_bytes())
}
