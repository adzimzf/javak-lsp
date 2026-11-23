//! Where Kotlin sources live, and what a Kotlin source of a library
//! declares.

use super::SourceLayout;

pub struct Kotlin;

/// Words that may stand before `class`, `interface` or `object` in a
/// top-level declaration.
const MODIFIERS: &[&str] = &[
    "public", "internal", "private", "protected", "abstract", "open", "final", "sealed", "data",
    "enum", "annotation", "inner", "value", "inline", "expect", "actual", "fun", "companion",
];

impl SourceLayout for Kotlin {
    /// The Kotlin Gradle plugin compiles `src/main/java` too.
    fn source_roots(&self) -> &'static [&'static str] {
        &[
            "src/main/kotlin",
            "src/test/kotlin",
            "src/main/java",
            "src/test/java",
        ]
    }

    /// A Kotlin file may declare any number of types, whatever its name:
    /// every `class`, `interface` and `object` written at the start of a
    /// line counts.
    fn library_types(&self, _file_stem: &str, text: &str) -> Vec<String> {
        let mut types = Vec::new();
        for line in text.lines() {
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            let mut words = line.split_whitespace();
            let keyword = words.find(|word| !MODIFIERS.contains(word) && !word.starts_with('@'));
            if !matches!(keyword, Some("class" | "interface" | "object")) {
                continue;
            }
            let name: String = words
                .next()
                .unwrap_or_default()
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if !name.is_empty() && !types.contains(&name) {
                types.push(name);
            }
        }
        types
    }

    fn reads_library_text(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_top_level_type_of_a_file_is_read() {
        let text = "package a\n\n@JvmInline\nvalue class Id(val raw: Int)\n\
                    data class Point(val x: Int)\nsealed interface Shape\nobject Registry {\n    class Inner\n}\nfun top() {}\n";
        assert_eq!(
            Kotlin.library_types("Shapes", text),
            vec!["Id", "Point", "Shape", "Registry"]
        );
    }
}
