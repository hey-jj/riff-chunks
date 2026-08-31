use core::fmt;
use core::str::FromStr;

/// A four-byte chunk identifier.
///
/// RIFF identifiers are often ASCII, but readers accept every byte value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fourcc(pub [u8; 4]);

impl Fourcc {
    /// The little-endian RIFF form identifier.
    pub const RIFF: Self = Self(*b"RIFF");
    /// The big-endian RIFX form identifier.
    pub const RIFX: Self = Self(*b"RIFX");
    /// The RF64 form identifier.
    pub const RF64: Self = Self(*b"RF64");
    /// The BW64 form identifier.
    pub const BW64: Self = Self(*b"BW64");
    /// The list chunk identifier.
    pub const LIST: Self = Self(*b"LIST");
    /// The RF64 size-table chunk identifier.
    pub const DS64: Self = Self(*b"ds64");
    /// The reserved filler chunk identifier.
    pub const JUNK: Self = Self(*b"JUNK");

    /// Creates an identifier from four bytes.
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    /// Creates an identifier from a four-byte string.
    ///
    /// This function is intended for constants and panics when `value` is not
    /// exactly four bytes. Use the [`FromStr`] implementation for runtime text.
    pub const fn from_str(value: &str) -> Self {
        let bytes = value.as_bytes();
        if bytes.len() != 4 {
            panic!("a Fourcc string must contain exactly four bytes");
        }
        Self([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Returns the four stored bytes.
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    /// Returns the identifier as text when every byte is ASCII.
    pub fn as_str(&self) -> Option<&str> {
        if self.0.iter().all(u8::is_ascii) {
            core::str::from_utf8(&self.0).ok()
        } else {
            None
        }
    }
}

impl fmt::Display for Fourcc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            if (0x20..=0x7e).contains(&byte) {
                formatter.write_str(core::str::from_utf8(&[byte]).unwrap_or("?"))?;
            } else {
                write!(formatter, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Fourcc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("\"")?;
        fmt::Display::fmt(self, formatter)?;
        formatter.write_str("\"")
    }
}

impl From<[u8; 4]> for Fourcc {
    fn from(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }
}

impl TryFrom<&[u8]> for Fourcc {
    type Error = FourccError;

    fn try_from(bytes: &[u8]) -> core::result::Result<Self, Self::Error> {
        if bytes.len() != 4 {
            return Err(FourccError {
                length: bytes.len(),
            });
        }
        Ok(Self([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

impl FromStr for Fourcc {
    type Err = FourccError;

    fn from_str(value: &str) -> core::result::Result<Self, Self::Err> {
        Self::try_from(value.as_bytes())
    }
}

/// Reports the byte length of an invalid textual or slice identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FourccError {
    length: usize,
}

impl FourccError {
    /// Returns the received byte length.
    pub const fn length(self) -> usize {
        self.length
    }
}

impl fmt::Display for FourccError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a Fourcc value must contain 4 bytes, received {}",
            self.length
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FourccError {}
