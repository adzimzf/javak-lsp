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

    /// A Kotlin file need not sit at its package's path (the standard
    /// library keeps `kotlin.collections` in `generated/_Collections.kt`):
    /// its `package` header says.
    fn library_package(&self, _entry: &str, text: &str) -> String {
        text.lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("package "))
            .map(|package| package.trim().trim_end_matches(';').trim().to_owned())
            .unwrap_or_default()
    }

    /// Every `fun` written at the start of a line; an extension by its
    /// receiver type and name, so a lookup reads only the files extending
    /// the types it has: `fun <T> Iterable<T>.first(): T` declares
    /// `Iterable.first`, `fun <T> T.also(…)` declares `*.also`.
    fn library_functions(&self, text: &str) -> Vec<String> {
        let mut functions = Vec::new();
        for line in text.lines() {
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            // `fun` is a modifier of `fun interface` too.
            let mut words = line
                .split_whitespace()
                .skip_while(|word| *word != "fun" && (MODIFIERS.contains(word) || word.starts_with('@')));
            if words.next() != Some("fun") || words.next() == Some("interface") {
                continue;
            }
            // What a library keeps to itself is not looked for from outside.
            let hidden = line
                .split_whitespace()
                .take_while(|word| *word != "fun")
                .any(|word| word == "private" || word == "internal");
            if hidden {
                continue;
            }
            let Some(name) = function_key(line) else {
                continue;
            };
            if !functions.contains(&name) {
                functions.push(name);
            }
        }
        functions
    }
}

/// The key a `fun` line is listed by ([`function_key`]): an extension's
/// receiver, `*` where the receiver is one of the function's own type
/// parameters.
pub fn extension_key(receiver: &str, name: &str) -> String {
    format!("{receiver}.{name}")
}

/// What a `fun` line declares: its name, after the receiver's type when
/// it is an extension ([`extension_key`]).
fn function_key(line: &str) -> Option<String> {
    let after_fun = line.split_once("fun ")?.1.trim_start();
    let (type_parameters, rest) = match after_fun.strip_prefix('<') {
        Some(rest) => {
            let end = closing_angle(rest)?;
            (type_parameter_names(&rest[..end]), rest[end + 1..].trim_start())
        }
        None => (Vec::new(), after_fun),
    };
    let mut depth = 0usize;
    let mut signature = String::new();
    for character in rest.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            '(' if depth == 0 => break,
            _ if depth == 0 => signature.push(character),
            _ => {}
        }
    }
    let signature = signature.trim();
    let (receiver, name) = match signature.rsplit_once('.') {
        Some((receiver, name)) => (Some(receiver), name),
        None => (None, signature),
    };
    let name = name.trim().trim_matches('`');
    let is_name = name
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && name.chars().all(|character| character.is_alphanumeric() || character == '_');
    if !is_name {
        return None;
    }
    let Some(receiver) = receiver else {
        return Some(name.to_owned());
    };
    let receiver = receiver
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_end_matches('?');
    let receiver = receiver.rsplit('.').next().unwrap_or(receiver).trim();
    let receiver = if type_parameters.iter().any(|parameter| parameter == receiver) {
        "*"
    } else {
        receiver
    };
    Some(extension_key(receiver, name))
}

/// Where the `<…>` a text starts inside of closes.
fn closing_angle(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (at, character) in text.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// `T`, `R` of `T, reified R : Comparable<R>`.
fn type_parameter_names(list: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for character in list.chars().chain(std::iter::once(',')) {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let name = current
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .last()
                    .unwrap_or_default()
                    .to_owned();
                if !name.is_empty() {
                    names.push(name);
                }
                current.clear();
            }
            _ if depth == 0 => current.push(character),
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_top_level_function_of_a_file_is_read() {
        let text = "@file:JvmName(\"CollectionsKt\")\npackage kotlin.collections\n\n\
                    public fun <T> listOf(vararg elements: T): List<T> = TODO()\n\
                    public inline fun <T, R> Iterable<T>.map(transform: (T) -> R): List<R> {\n    fun inner() {}\n}\n\
                    public actual inline fun println(message: Any?) {}\n\
                    public inline fun <T, reified R : Comparable<R>> T.also(block: (T) -> Unit): T = this\n\
                    public fun CharSequence?.isNullOrEmpty(): Boolean = false\n\
                    public fun <K, V> kotlin.collections.Map<out K, V>.toList(): List<Pair<K, V>> = TODO()\n\
                    internal fun <K, V> Map<out K, V>.`get or null`(key: K) = null\n";
        assert_eq!(
            Kotlin.library_functions(text),
            vec!["listOf", "Iterable.map", "println", "*.also", "CharSequence.isNullOrEmpty", "Map.toList"]
        );
        assert_eq!(Kotlin.library_package("generated/_Collections.kt", text), "kotlin.collections");
    }

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
