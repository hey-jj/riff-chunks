use core::fmt;

use crate::Fourcc;

/// A crate result with [`Error`] as its error type.
pub type Result<T> = core::result::Result<T, Error>;

/// An error from parsing, streaming, or writing a container.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The underlying stream returned an error.
    #[cfg(feature = "std")]
    Io(std::io::Error),
    /// The first four bytes do not identify a supported form.
    NotRiff {
        /// The form bytes found in the input.
        found: [u8; 4],
    },
    /// Fewer than eight bytes remain for a chunk header, or fewer than twelve
    /// bytes remain for the file header.
    TruncatedHeader {
        /// Absolute offset of the incomplete header.
        offset: u64,
        /// Bytes available from the offset.
        available: u64,
    },
    /// A chunk payload ends before its declared size.
    Truncated {
        /// Identifier of the incomplete chunk.
        id: Fourcc,
        /// Declared payload size.
        declared: u64,
        /// Payload bytes available in the input.
        available: u64,
    },
    /// A child chunk extends beyond its list.
    ChildExceedsParent {
        /// Identifier of the child chunk.
        id: Fourcc,
        /// Declared child payload size.
        declared: u64,
        /// Bytes left in the list from the child header.
        remaining: u64,
    },
    /// An odd payload reaches the input end without a pad byte.
    MissingPad {
        /// Identifier of the unpadded chunk.
        id: Fourcc,
        /// Absolute offset where the pad byte belongs.
        offset: u64,
    },
    /// A list payload has no complete four-byte list type.
    ListTooShort {
        /// Identifier of the list chunk.
        id: Fourcc,
        /// Declared list payload size.
        size: u64,
    },
    /// List nesting reached the fixed limit.
    TooDeep {
        /// Maximum supported nesting depth.
        max: usize,
    },
    /// An RF64 or BW64 file does not start with a size table.
    MissingDs64 {
        /// Identifier of the first chunk.
        found: Fourcc,
    },
    /// The RF64 size table is too short for its declared entries.
    BadDs64 {
        /// Declared size of the table payload.
        size: u64,
    },
    /// A sentinel size has no matching 64-bit value.
    UnresolvedSize {
        /// Identifier whose size could not be resolved.
        id: Fourcc,
    },
    /// A list-only operation received another chunk type.
    NotAList {
        /// Identifier of the current chunk.
        id: Fourcc,
    },
    /// A list-leave operation was requested at the top level.
    NotInList,
    /// A streaming write was requested without an open chunk.
    NoOpenChunk,
    /// The caller's payload buffer cannot hold the remaining bytes.
    BufferTooSmall {
        /// Required buffer length.
        needed: u64,
    },
    /// A size cannot fit in a 32-bit container field.
    TooLarge {
        /// Size that exceeded the field.
        size: u64,
    },
    /// The writer has open chunks or lists.
    Unbalanced {
        /// Number of open scopes.
        open: usize,
    },
    /// The requested feature is unavailable.
    Unsupported(Feature),
}

/// A format feature that a requested operation does not support.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Feature {
    /// Writing RF64 and BW64 containers.
    Rf64Write,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "std")]
            Self::Io(error) => write!(formatter, "stream error: {error}"),
            Self::NotRiff { found } => write!(formatter, "unsupported form id: {}", Fourcc(*found)),
            Self::TruncatedHeader { offset, available } => write!(
                formatter,
                "incomplete header at offset {offset}: {available} bytes available"
            ),
            Self::Truncated {
                id,
                declared,
                available,
            } => write!(
                formatter,
                "chunk {id} declares {declared} bytes with {available} available"
            ),
            Self::ChildExceedsParent {
                id,
                declared,
                remaining,
            } => write!(
                formatter,
                "chunk {id} declares {declared} bytes with {remaining} bytes left in its list"
            ),
            Self::MissingPad { id, offset } => {
                write!(
                    formatter,
                    "chunk {id} is missing its pad byte at offset {offset}"
                )
            }
            Self::ListTooShort { id, size } => {
                write!(formatter, "list {id} has a {size}-byte payload")
            }
            Self::TooDeep { max } => write!(formatter, "list nesting exceeds {max} levels"),
            Self::MissingDs64 { found } => {
                write!(formatter, "64-bit form starts with {found} instead of ds64")
            }
            Self::BadDs64 { size } => write!(formatter, "invalid ds64 payload size {size}"),
            Self::UnresolvedSize { id } => write!(formatter, "no 64-bit size for chunk {id}"),
            Self::NotAList { id } => write!(formatter, "chunk {id} is not a list"),
            Self::NotInList => formatter.write_str("the reader is at the top level"),
            Self::NoOpenChunk => formatter.write_str("no writable chunk is open"),
            Self::BufferTooSmall { needed } => {
                write!(formatter, "payload buffer needs {needed} bytes")
            }
            Self::TooLarge { size } => write!(formatter, "size {size} exceeds a 32-bit field"),
            Self::Unbalanced { open } => write!(formatter, "writer has {open} open scopes"),
            Self::Unsupported(Feature::Rf64Write) => {
                formatter.write_str("RF64 and BW64 writing is unsupported")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    #[cfg(feature = "std")]
    Io(std::io::ErrorKind),
    TruncatedHeader(u64, u64),
    Truncated(Fourcc, u64, u64),
    ChildExceedsParent(Fourcc, u64, u64),
    MissingPad(Fourcc, u64),
    ListTooShort(Fourcc, u64),
    #[cfg(feature = "std")]
    TooDeep,
    #[cfg(feature = "std")]
    UnresolvedSize(Fourcc),
}

impl Failure {
    pub(crate) fn into_error(self) -> Error {
        match self {
            #[cfg(feature = "std")]
            Self::Io(kind) => Error::Io(std::io::Error::from(kind)),
            Self::TruncatedHeader(offset, available) => {
                Error::TruncatedHeader { offset, available }
            }
            Self::Truncated(id, declared, available) => Error::Truncated {
                id,
                declared,
                available,
            },
            Self::ChildExceedsParent(id, declared, remaining) => Error::ChildExceedsParent {
                id,
                declared,
                remaining,
            },
            Self::MissingPad(id, offset) => Error::MissingPad { id, offset },
            Self::ListTooShort(id, size) => Error::ListTooShort { id, size },
            #[cfg(feature = "std")]
            Self::TooDeep => Error::TooDeep {
                max: crate::MAX_DEPTH,
            },
            #[cfg(feature = "std")]
            Self::UnresolvedSize(id) => Error::UnresolvedSize { id },
        }
    }
}
