//! NewGit object model: identity, canonical encoding, on-disk envelope.

pub mod envelope;
pub mod id;
pub mod types;

pub use envelope::{decode as decode_envelope, encode as encode_envelope};
pub use id::ObjectId;
pub use types::*;
