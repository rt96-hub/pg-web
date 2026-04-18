//! pg-web CLI — library side.
//!
//! Commands are implemented as functions in submodules so tests can call
//! them directly without going through `main.rs` (which handles arg parsing).

pub mod init;
pub mod paths;
pub mod push;
mod templates;
