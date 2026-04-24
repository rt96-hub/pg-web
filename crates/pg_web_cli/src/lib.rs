//! pg-web CLI — library side.
//!
//! Commands are implemented as functions in submodules so tests can call
//! them directly without going through `main.rs` (which handles arg parsing).

pub mod dev;
pub mod env;
pub mod init;
pub mod migrate;
pub mod paths;
pub mod push;
pub mod stack;
mod templates;
