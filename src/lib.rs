//! A Java/Kotlin language server.
//!
//! Each layer keeps a language's particulars in a `java` and a `kotlin`
//! module behind an interface of its own:
//!
//! - `parser` — `Syntax`: parsing, outline, folds; Kotlin is lowered to the
//!   Java-shaped tree the semantic layer reads;
//! - `feature` — `LanguageFeatures`: how signatures are written, keywords,
//!   what a reference includes;
//! - `indexer` — `SourceLayout`: source roots, what a library file declares;
//! - `backend` — `LanguageProtocol`: the protocol's language ids.
//!
//! `dependency_sync` asks the project's build tool for its libraries; the
//! backend runs it and the indexer reads the sources jars it resolves.
//! `debugger` answers the `vscode.java.*` commands jdtls' debug plugin
//! does, and runs Microsoft's java-debug behind them.

pub mod backend;
pub(crate) mod classfile;
pub(crate) mod debugger;
pub(crate) mod dependency_sync;
pub mod feature;
pub mod indexer;
pub mod language;
pub mod parser;
pub mod util;

pub use backend::backend_over;

#[cfg(test)]
mod integrationtest;
