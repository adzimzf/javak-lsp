//! Where Java sources live, and what a Java source of a library declares.

use super::SourceLayout;

pub struct Java;

impl SourceLayout for Java {
    fn source_roots(&self) -> &'static [&'static str] {
        &["src/main/java", "src/test/java"]
    }

    /// A Java file is named after its one top-level public type; the others
    /// it may declare are package-private, which no other package names.
    fn library_types(&self, file_stem: &str, _text: &str) -> Vec<String> {
        vec![file_stem.to_owned()]
    }

    fn reads_library_text(&self) -> bool {
        false
    }
}
