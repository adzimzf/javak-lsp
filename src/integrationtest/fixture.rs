//! The source of an integration case: a few files and one cursor, written
//! as a single string.
//!
//! ```text
//! //- /com/demo/Greeter.java
//! package com.demo;
//! class Greeter { String greet() { return ""; } }
//! //- /com/demo/Main.java
//! package com.demo;
//! class Main { void run(Greeter greeter) { greeter.gr$0eet(); } }
//! ```
//!
//! A `//- /path` line starts a file; `$0` marks where the request is asked
//! and is removed from the text. A fixture has exactly one `$0`.
//!
//! A source file is opened in the editor unless its header ends in
//! `(not open)`: then, like a build file, it is only on disk, where the
//! server has to find it the way it finds the rest of a project.
//!
//! ```text
//! //- /com/demo/Greeter.java (not open)
//! ```

const FILE_HEADER: &str = "//- /";
const NOT_OPEN: &str = "(not open)";
const CURSOR: &str = "$0";

pub(super) struct Fixture {
    pub(super) files: Vec<FixtureFile>,
    pub(super) cursor: Cursor,
}

pub(super) struct FixtureFile {
    /// Path below the workspace root, `/`-separated.
    pub(super) path: String,
    pub(super) text: String,
    /// Marked `(not open)`: on disk, and never opened in the editor.
    not_open: bool,
}

impl FixtureFile {
    /// Whether the editor opens the file: a source file not marked
    /// `(not open)`. A build file (`pom.xml`) is never opened.
    pub(super) fn is_opened(&self) -> bool {
        !self.not_open && crate::language::Language::of_path(&self.path).is_some()
    }
}

/// A zero-based LSP position in one of the fixture's files.
pub(super) struct Cursor {
    pub(super) path: String,
    pub(super) line: u32,
    pub(super) character: u32,
}

impl Fixture {
    pub(super) fn parse(source: &str) -> Self {
        let mut files: Vec<FixtureFile> = Vec::new();
        for line in source.trim_start().lines() {
            if let Some(header) = line.strip_prefix(FILE_HEADER) {
                let header = header.trim();
                let path = header.strip_suffix(NOT_OPEN);
                files.push(FixtureFile {
                    path: path.unwrap_or(header).trim().to_owned(),
                    text: String::new(),
                    not_open: path.is_some(),
                });
                continue;
            }
            let file = files
                .last_mut()
                .expect("a fixture starts with a `//- /path` line");
            file.text.push_str(line);
            file.text.push('\n');
        }
        let cursor = take_cursor(&mut files);
        Self { files, cursor }
    }
}

fn take_cursor(files: &mut [FixtureFile]) -> Cursor {
    let mut cursors = Vec::new();
    for file in files.iter_mut() {
        while let Some(offset) = file.text.find(CURSOR) {
            file.text.replace_range(offset..offset + CURSOR.len(), "");
            cursors.push(cursor_at(&file.path, &file.text, offset));
        }
    }
    assert_eq!(cursors.len(), 1, "a fixture has exactly one `$0`");
    cursors.remove(0)
}

fn cursor_at(path: &str, text: &str, offset: usize) -> Cursor {
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Cursor {
        path: path.to_owned(),
        line: before.matches('\n').count() as u32,
        character: before[line_start..].encode_utf16().count() as u32,
    }
}

impl Fixture {
    /// Line `index` (zero-based) of the file at `/path`.
    pub(super) fn line(&self, path: &str, index: usize) -> Option<&str> {
        let file = self
            .files
            .iter()
            .find(|file| path.strip_prefix('/') == Some(file.path.as_str()))?;
        file.text.lines().nth(index)
    }

    /// Whether `name` is written somewhere in the fixture as a whole word.
    pub(super) fn spells(&self, name: &str) -> bool {
        self.files.iter().any(|file| {
            file.text
                .split(|character: char| !is_identifier_part(character))
                .any(|word| word == name)
        })
    }

    /// The identifier the cursor is in or right after.
    pub(super) fn word_at_cursor(&self) -> String {
        let line = self
            .line(&format!("/{}", self.cursor.path), self.cursor.line as usize)
            .unwrap_or_default();
        let units: Vec<u16> = line.encode_utf16().collect();
        let split = (self.cursor.character as usize).min(units.len());
        let before = String::from_utf16_lossy(&units[..split]);
        let after = String::from_utf16_lossy(&units[split..]);
        let head: String = before
            .chars()
            .rev()
            .take_while(|character| is_identifier_part(*character))
            .collect();
        let tail: String = after
            .chars()
            .take_while(|character| is_identifier_part(*character))
            .collect();
        head.chars().rev().chain(tail.chars()).collect()
    }
}

fn is_identifier_part(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}
