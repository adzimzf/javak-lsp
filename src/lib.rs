//! A Java/Kotlin language server.
//!
//! The syntax layer is an interface (`parser::Syntax`) with a tree-sitter
//! implementation behind it, so a hand-written parser can take its place
//! without the server noticing.

pub mod backend;
pub mod completion;
pub mod highlight;
pub mod indexer;
pub mod parser;
pub mod query;
pub mod semantic;
pub mod signature;

pub use backend::backend_over;

#[cfg(test)]
mod integrationtest;
