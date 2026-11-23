//! Where Java sources live, and what a Java source of a library declares.

use super::library::below_module;
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

    /// A Java file sits at its package's path, below its module's
    /// directory in the JDK's `src.zip` (`java.base/java/lang/String.java`).
    fn library_package(&self, entry: &str, _text: &str) -> String {
        let (_, path) = below_module(entry);
        path.rsplit_once('/')
            .map(|(directories, _)| directories.replace('/', "."))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_of_a_library_file_is_its_path() {
        assert_eq!(Java.library_package("com/acme/Greeter.java", ""), "com.acme");
        assert_eq!(Java.library_package("java.base/java/lang/String.java", ""), "java.lang");
        assert_eq!(Java.library_package("Greeter.java", ""), "");
    }
}
