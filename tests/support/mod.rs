use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Number(u64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
                .unwrap_or_else(|| panic!("missing JSON field {key}")),
            _ => panic!("expected JSON object for field {key}"),
        }
    }

    pub fn get_opt(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find_map(|(entry_key, value)| (entry_key == key).then_some(value)),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::String(value) => value,
            _ => panic!("expected JSON string"),
        }
    }

    pub fn as_u64(&self) -> u64 {
        match self {
            Self::Number(value) => *value,
            _ => panic!("expected JSON number"),
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            _ => panic!("expected JSON boolean"),
        }
    }

    pub fn as_array(&self) -> &[Json] {
        match self {
            Self::Array(values) => values,
            _ => panic!("expected JSON array"),
        }
    }

    pub fn as_object(&self) -> &[(String, Json)] {
        match self {
            Self::Object(entries) => entries,
            _ => panic!("expected JSON object"),
        }
    }
}

pub fn manifest() -> Json {
    parse_json(include_str!("../data/conformance-vectors.json"))
}

pub fn data_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(relative)
}

pub fn fixture(relative: &str) -> Vec<u8> {
    fs::read(data_path(relative)).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

pub fn parse_json(input: &str) -> Json {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    let value = parser.value();
    parser.whitespace();
    assert_eq!(parser.pos, parser.bytes.len(), "trailing JSON data");
    value
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn value(&mut self) -> Json {
        self.whitespace();
        match self.peek() {
            Some(b'n') => {
                self.literal(b"null");
                Json::Null
            }
            Some(b't') => {
                self.literal(b"true");
                Json::Bool(true)
            }
            Some(b'f') => {
                self.literal(b"false");
                Json::Bool(false)
            }
            Some(b'"') => Json::String(self.string()),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'0'..=b'9') => self.number(),
            other => panic!("unexpected JSON byte {other:?} at {}", self.pos),
        }
    }

    fn array(&mut self) -> Json {
        self.expect(b'[');
        let mut values = Vec::new();
        self.whitespace();
        if self.consume(b']') {
            return Json::Array(values);
        }
        loop {
            values.push(self.value());
            self.whitespace();
            if self.consume(b']') {
                return Json::Array(values);
            }
            self.expect(b',');
        }
    }

    fn object(&mut self) -> Json {
        self.expect(b'{');
        let mut entries = Vec::new();
        self.whitespace();
        if self.consume(b'}') {
            return Json::Object(entries);
        }
        loop {
            self.whitespace();
            let key = self.string();
            self.whitespace();
            self.expect(b':');
            entries.push((key, self.value()));
            self.whitespace();
            if self.consume(b'}') {
                return Json::Object(entries);
            }
            self.expect(b',');
        }
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut value = String::new();
        loop {
            let byte = self.take().expect("unterminated JSON string");
            match byte {
                b'"' => return value,
                b'\\' => {
                    let escaped = self.take().expect("unterminated JSON escape");
                    match escaped {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'/' => value.push('/'),
                        b'b' => value.push('\u{0008}'),
                        b'f' => value.push('\u{000c}'),
                        b'n' => value.push('\n'),
                        b'r' => value.push('\r'),
                        b't' => value.push('\t'),
                        b'u' => value.push(self.unicode_escape()),
                        _ => panic!("invalid JSON escape at {}", self.pos),
                    }
                }
                0x00..=0x1f => panic!("control byte in JSON string"),
                0x20..=0x7f => value.push(char::from(byte)),
                _ => {
                    let start = self.pos - 1;
                    let width = utf8_width(byte);
                    let end = start + width;
                    let text = std::str::from_utf8(&self.bytes[start..end])
                        .expect("invalid UTF-8 in JSON string");
                    value.push_str(text);
                    self.pos = end;
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> char {
        let mut value = 0_u32;
        for _ in 0..4 {
            let byte = self.take().expect("short JSON Unicode escape");
            value = value * 16
                + match byte {
                    b'0'..=b'9' => u32::from(byte - b'0'),
                    b'a'..=b'f' => u32::from(byte - b'a' + 10),
                    b'A'..=b'F' => u32::from(byte - b'A' + 10),
                    _ => panic!("invalid JSON Unicode escape"),
                };
        }
        char::from_u32(value).expect("invalid JSON Unicode scalar")
    }

    fn number(&mut self) -> Json {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).expect("JSON number text");
        Json::Number(text.parse().expect("JSON u64 number"))
    }

    fn literal(&mut self, literal: &[u8]) {
        let end = self.pos + literal.len();
        assert_eq!(&self.bytes[self.pos..end], literal, "invalid JSON literal");
        self.pos = end;
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: u8) {
        let found = self.take();
        assert_eq!(found, Some(expected), "unexpected JSON punctuation");
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => panic!("invalid UTF-8 lead byte"),
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hex(&hash.finish())
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    used: usize,
    length: u64,
}

impl Sha256 {
    pub const fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            used: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.length = self
            .length
            .wrapping_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.used != 0 {
            let take = (64 - self.used).min(bytes.len());
            self.block[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used == 64 {
                compress(&mut self.state, &self.block);
                self.used = 0;
            }
        }
        while bytes.len() >= 64 {
            let block: &[u8; 64] = bytes[..64].try_into().expect("SHA-256 block length");
            compress(&mut self.state, block);
            bytes = &bytes[64..];
        }
        self.block[..bytes.len()].copy_from_slice(bytes);
        self.used = bytes.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.block[self.used] = 0x80;
        self.used += 1;
        if self.used > 56 {
            self.block[self.used..].fill(0);
            compress(&mut self.state, &self.block);
            self.block = [0; 64];
        } else {
            self.block[self.used..56].fill(0);
        }
        self.block[56..].copy_from_slice(&bit_length.to_be_bytes());
        compress(&mut self.state, &self.block);
        let mut output = [0_u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        output
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut words = [0_u32; 64];
    for (index, chunk) in block.chunks_exact(4).enumerate() {
        words[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(sum1)
            .wrapping_add(choice)
            .wrapping_add(K[index])
            .wrapping_add(words[index]);
        let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = sum0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}
