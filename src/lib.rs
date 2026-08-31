#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]
//! RIFF container readers and a seekable writer.
//!
//! [`Riff`] borrows a complete input slice. [`ChunkReader`] reads a stream
//! when the `std` feature is active. Both readers leave list descent to the
//! caller and report resolved RF64 sizes.

mod borrowed;
mod error;
mod fourcc;

#[cfg(feature = "std")]
mod stream;
#[cfg(feature = "std")]
mod writer;

pub use borrowed::{Chunk, ChunkHeader, Chunks, Ds64, Form, Header, Riff, MAX_DEPTH};
pub use error::{Error, Feature, Result};
pub use fourcc::{Fourcc, FourccError};

#[cfg(feature = "std")]
pub use stream::{ChunkReader, Payload, Seekable, Source};
#[cfg(feature = "std")]
pub use writer::ChunkWriter;
