//! `nod-dylan` — Sprint 01 placeholder for the ported Dylan kernel library.
//!
//! This crate is unusual: its real payload is **Dylan source**, not Rust.
//! Once Sprints 05+ light up the loader, the upstream `dylan` library
//! (from `E:\opendylan\sources\dylan\`) is ported file-by-file into
//! `dylan-sources/` and loaded by `nod-loader` at startup. This Rust
//! stub exists only because Cargo wants every workspace member to be a
//! Rust package.

/// Placeholder so the crate compiles cleanly.
#[doc(hidden)]
pub fn _placeholder() {}
