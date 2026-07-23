//! `dwg2geo` as a library.
//!
//! The CLI in `main.rs` is a thin wrapper over these modules. The native
//! backend additionally exposes an embedding API (`backend::native::embed`)
//! that converts DWG bytes to a GeoJSON string in-process, with no file I/O —
//! used by the WebAssembly build and any other embedder.

pub mod backend;
pub mod cli;
pub mod commands;
pub mod dwg;
pub mod report;
