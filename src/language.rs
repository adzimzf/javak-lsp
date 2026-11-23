//! The languages the server reads. Each layer — parser, features, indexer,
//! backend — keeps what is particular to a language in a `java` and a
//! `kotlin` module behind an interface of its own, and picks the module
//! with this.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Java,
    Kotlin,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::Java, Language::Kotlin];

    /// The language a file is written in, by its extension.
    pub fn of_path(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1;
        match extension {
            "java" => Some(Language::Java),
            "kt" | "kts" => Some(Language::Kotlin),
            _ => None,
        }
    }
}
