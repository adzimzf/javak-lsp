//! Which source files of a library jar write a word in their code, call a
//! method of a name, extend a type of a name, and which types they
//! declare — so a search for uses reads only the files that may have one:
//! the JDK's `src.zip` has fifteen thousand files, and reading them all
//! takes seconds.
//!
//! An index is built once per jar version (a few seconds for the JDK) and
//! kept next to the extracted sources, so a later session reads it back.
//! It is compact — the sorted words in one buffer, each word's files as
//! deltas in variable-length integers — about 14 MB for the JDK.

use std::io::{Read, Write};
use std::path::Path;

use crate::language::Language;

/// The words of one jar's source files, and the names its types extend.
pub struct WordIndex {
    /// The jar's source entries, by number.
    entries: Vec<String>,
    /// Every identifier a file's code writes (not its comments nor its
    /// literals: jdtls searches class files, which have neither).
    words: Table,
    /// The names a file calls or declares as methods: an identifier
    /// before `(`, or after `::`.
    calls: Table,
    /// The simple name of each type a file's types extend or implement,
    /// or an anonymous class of it creates: where a subtype may be.
    supers: Table,
    /// The types the files declare, top-level and member ones.
    types: Vec<DeclaredType>,
}

/// A type a library file declares, as a workspace-symbol search lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredType {
    pub name: String,
    /// The types it is nested in, outermost first, joined with `.`.
    pub outer: String,
    pub kind: TypeKind,
    /// The entry declaring it.
    pub entry: u32,
    /// Where its name is, zero-based (columns in UTF-16 units).
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Record,
}

impl TypeKind {
    fn code(self) -> u8 {
        match self {
            Self::Class => 0,
            Self::Interface => 1,
            Self::Enum => 2,
            Self::Annotation => 3,
            Self::Record => 4,
        }
    }

    fn of_code(code: u8) -> Self {
        match code {
            1 => Self::Interface,
            2 => Self::Enum,
            3 => Self::Annotation,
            4 => Self::Record,
            _ => Self::Class,
        }
    }
}

/// Words to the entries that have them.
#[derive(Default)]
struct Table {
    /// Every word once, sorted, back to back.
    words: Vec<u8>,
    /// Where each word starts in `words`; one more at the end.
    word_starts: Vec<u32>,
    /// Where each word's entries start in `postings`; one more at the end.
    posting_starts: Vec<u32>,
    /// Entry numbers, ascending per word, as varint deltas.
    postings: Vec<u8>,
}

impl Table {
    fn from(by_word: std::collections::HashMap<String, Vec<u32>>) -> Self {
        let mut sorted: Vec<(String, Vec<u32>)> = by_word.into_iter().collect();
        sorted.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut table = Self::default();
        for (word, files) in sorted {
            table.word_starts.push(table.words.len() as u32);
            table.words.extend_from_slice(word.as_bytes());
            table.posting_starts.push(table.postings.len() as u32);
            let mut previous = 0;
            for file in files {
                write_varint(&mut table.postings, file - previous);
                previous = file;
            }
        }
        table.word_starts.push(table.words.len() as u32);
        table.posting_starts.push(table.postings.len() as u32);
        table
    }

    /// The entry numbers that have `word`.
    fn entries_with(&self, word: &str) -> Vec<u32> {
        let count = self.word_starts.len().saturating_sub(1);
        let word_at = |index: usize| {
            &self.words[self.word_starts[index] as usize..self.word_starts[index + 1] as usize]
        };
        let (mut low, mut high) = (0, count);
        let mut found = None;
        while low < high {
            let middle = (low + high) / 2;
            match word_at(middle).cmp(word.as_bytes()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    found = Some(middle);
                    break;
                }
            }
        }
        let Some(found) = found else {
            return Vec::new();
        };
        let mut postings = &self.postings[self.posting_starts[found] as usize..self.posting_starts[found + 1] as usize];
        let mut entries = Vec::new();
        let mut entry = 0;
        while let Some(delta) = read_varint(&mut postings) {
            entry += delta;
            entries.push(entry);
        }
        entries
    }

    fn write(&self, out: &mut Vec<u8>) {
        for part in [&self.words, &self.postings] {
            out.extend_from_slice(&(part.len() as u64).to_le_bytes());
            out.extend_from_slice(part);
        }
        for starts in [&self.word_starts, &self.posting_starts] {
            out.extend_from_slice(&(starts.len() as u64).to_le_bytes());
            for start in starts {
                out.extend_from_slice(&start.to_le_bytes());
            }
        }
    }

    fn read(reader: &mut Reader<'_>) -> std::io::Result<Self> {
        let words = reader.bytes()?;
        let postings = reader.bytes()?;
        let word_starts = reader.numbers()?;
        let posting_starts = reader.numbers()?;
        Ok(Self { words, word_starts, posting_starts, postings })
    }
}

/// What [`WordIndex::load`] reads from.
struct Reader<'a> {
    rest: &'a [u8],
}

impl Reader<'_> {
    fn invalid() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "not a word index")
    }

    fn take(&mut self, length: usize) -> std::io::Result<&[u8]> {
        if self.rest.len() < length {
            return Err(Self::invalid());
        }
        let (taken, after) = self.rest.split_at(length);
        self.rest = after;
        Ok(taken)
    }

    fn length(&mut self) -> std::io::Result<usize> {
        let raw: [u8; 8] = self.take(8)?.try_into().map_err(|_| Self::invalid())?;
        Ok(u64::from_le_bytes(raw) as usize)
    }

    fn bytes(&mut self) -> std::io::Result<Vec<u8>> {
        let length = self.length()?;
        Ok(self.take(length)?.to_vec())
    }

    fn numbers(&mut self) -> std::io::Result<Vec<u32>> {
        let count = self.length()?;
        let raw = self.take(count.checked_mul(4).ok_or_else(Self::invalid)?)?;
        Ok(raw
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect())
    }
}

const MAGIC: &[u8; 8] = b"jkwords4";

impl WordIndex {
    /// Reads every source file of `jar`.
    pub fn build(jar: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(jar)?;
        let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
        let mut entries = Vec::new();
        let mut by_word: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        let mut by_super: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        let mut by_call: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        let mut types = Vec::new();
        let mut text = String::new();
        for index in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(index) else {
                continue;
            };
            if Language::of_path(entry.name()).is_none() {
                continue;
            }
            text.clear();
            if entry.read_to_string(&mut text).is_err() {
                continue;
            }
            let number = entries.len() as u32;
            entries.push(entry.name().to_owned());
            let tokens = code_tokens(&text);
            let mut seen = std::collections::HashSet::new();
            let mut called = std::collections::HashSet::new();
            for (at, word) in tokens.iter().enumerate() {
                if !is_identifier(word) {
                    continue;
                }
                if seen.insert(*word) {
                    by_word.entry((*word).to_owned()).or_default().push(number);
                }
                let is_call = tokens.get(at + 1) == Some(&"(")
                    || (at >= 2 && tokens[at - 1] == ":" && tokens[at - 2] == ":");
                if is_call && called.insert(*word) {
                    by_call.entry((*word).to_owned()).or_default().push(number);
                }
            }
            let mut seen = std::collections::HashSet::new();
            for name in supertype_names(&text) {
                if seen.insert(name) {
                    by_super.entry(name.to_owned()).or_default().push(number);
                }
            }
            types.extend(declared_types(&text, number));
        }
        Ok(Self {
            entries,
            words: Table::from(by_word),
            calls: Table::from(by_call),
            supers: Table::from(by_super),
            types,
        })
    }

    /// The entries that write `word`.
    pub fn entries_with(&self, word: &str) -> Vec<&str> {
        self.named(self.words.entries_with(word))
    }

    /// The entries that call (or declare) a method called `name`.
    pub fn entries_calling(&self, name: &str) -> Vec<&str> {
        self.named(self.calls.entries_with(name))
    }

    /// The entries whose types may extend a type called `name`.
    pub fn entries_extending(&self, name: &str) -> Vec<&str> {
        self.named(self.supers.entries_with(name))
    }

    /// The types the jar declares, each with the entry declaring it.
    pub fn declared_types(&self) -> impl Iterator<Item = (&DeclaredType, &str)> {
        self.types
            .iter()
            .filter_map(|declared| Some((declared, self.entries.get(declared.entry as usize)?.as_str())))
    }

    fn named(&self, numbers: Vec<u32>) -> Vec<&str> {
        numbers
            .into_iter()
            .filter_map(|number| self.entries.get(number as usize).map(String::as_str))
            .collect()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        let entries = self.entries.join("\n");
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        out.extend_from_slice(entries.as_bytes());
        self.words.write(&mut out);
        self.calls.write(&mut out);
        self.supers.write(&mut out);
        out.extend_from_slice(&(self.types.len() as u64).to_le_bytes());
        for declared in &self.types {
            for text in [&declared.name, &declared.outer] {
                out.extend_from_slice(&(text.len() as u64).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
            }
            out.push(declared.kind.code());
            for number in [declared.entry, declared.line, declared.column] {
                out.extend_from_slice(&number.to_le_bytes());
            }
        }
        // Written whole, then moved in place: a reader never sees half.
        let partial = path.with_extension("partial");
        std::fs::File::create(&partial)?.write_all(&out)?;
        std::fs::rename(partial, path)
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(Reader::invalid)?;
        let mut reader = Reader { rest };
        let entries = String::from_utf8(reader.bytes()?).map_err(|_| Reader::invalid())?;
        let words = Table::read(&mut reader)?;
        let calls = Table::read(&mut reader)?;
        let supers = Table::read(&mut reader)?;
        let count = reader.length()?;
        let mut types = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let name = String::from_utf8(reader.bytes()?).map_err(|_| Reader::invalid())?;
            let outer = String::from_utf8(reader.bytes()?).map_err(|_| Reader::invalid())?;
            let kind = TypeKind::of_code(reader.take(1)?[0]);
            let mut number = || -> std::io::Result<u32> {
                let raw: [u8; 4] = reader.take(4)?.try_into().map_err(|_| Reader::invalid())?;
                Ok(u32::from_le_bytes(raw))
            };
            let (entry, line, column) = (number()?, number()?, number()?);
            types.push(DeclaredType { name, outer, kind, entry, line, column });
        }
        Ok(Self {
            entries: if entries.is_empty() { Vec::new() } else { entries.split('\n').map(str::to_owned).collect() },
            words,
            calls,
            supers,
            types,
        })
    }
}

/// The simple names a source text's types extend or implement, and the
/// types it creates anonymous classes of, read from its tokens alone: a
/// superset (a type parameter's bound counts too), which a search then
/// checks by resolving.
pub fn supertype_names(text: &str) -> Vec<&str> {
    let tokens = code_tokens(text);
    let mut names = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "extends" | "implements" => {
                // A list of types up to the body: the last name of each,
                // outside its type arguments.
                let mut depth = 0usize;
                let mut last: Option<&str> = None;
                index += 1;
                while index < tokens.len() {
                    let token = tokens[index];
                    match token {
                        "<" => depth += 1,
                        ">" => depth = depth.saturating_sub(1),
                        ">>" => depth = depth.saturating_sub(2),
                        "," | "{" | "&" if depth == 0 => {
                            names.extend(last.take());
                            if token == "{" {
                                break;
                            }
                        }
                        "extends" | "implements" | "permits" if depth == 0 => {
                            names.extend(last.take());
                            index -= 1;
                            break;
                        }
                        "." | "?" | "super" => {}
                        _ if depth == 0 && is_identifier(token) => last = Some(token),
                        _ if depth == 0 => {
                            names.extend(last.take());
                            break;
                        }
                        _ => {}
                    }
                    index += 1;
                }
            }
            // `new Name<…>(…) {`: an anonymous class of `Name`.
            "new" => {
                let mut cursor = index + 1;
                let mut last = None;
                while cursor < tokens.len() && (is_identifier(tokens[cursor]) || tokens[cursor] == ".") {
                    if tokens[cursor] != "." {
                        last = Some(tokens[cursor]);
                    }
                    cursor += 1;
                }
                if cursor < tokens.len() && tokens[cursor] == "<" {
                    let mut depth = 0usize;
                    while cursor < tokens.len() {
                        match tokens[cursor] {
                            "<" => depth += 1,
                            ">" => depth = depth.saturating_sub(1),
                            ">>" => depth = depth.saturating_sub(2),
                            _ => {}
                        }
                        cursor += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                if cursor < tokens.len() && tokens[cursor] == "(" {
                    let mut depth = 0usize;
                    while cursor < tokens.len() {
                        match tokens[cursor] {
                            "(" => depth += 1,
                            ")" => depth = depth.saturating_sub(1),
                            _ => {}
                        }
                        cursor += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    if cursor < tokens.len() && tokens[cursor] == "{" {
                        names.extend(last);
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    names
}

/// The types a source text declares at its top level and as members of
/// those (not the local or anonymous ones a method body holds), read from
/// its tokens and the braces around them.
pub fn declared_types(text: &str, entry: u32) -> Vec<DeclaredType> {
    let tokens = placed_tokens(text);
    let mut found = Vec::new();
    // Each type body open: its name and the brace depth inside it.
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut pending: Option<(TypeKind, &str, usize)> = None;
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let place = |offset: usize| {
        let line = line_starts.partition_point(|start| *start <= offset) - 1;
        let column = text[line_starts[line]..offset].encode_utf16().count();
        (line as u32, column as u32)
    };
    for (index, (token, _)) in tokens.iter().enumerate() {
        let previous = index.checked_sub(1).map(|at| tokens[at].0);
        let next = tokens.get(index + 1);
        let after_next = tokens.get(index + 2).map(|(token, _)| *token);
        let kind = match *token {
            "class" => Some(TypeKind::Class),
            "interface" if previous == Some("@") => Some(TypeKind::Annotation),
            "interface" => Some(TypeKind::Interface),
            "enum" => Some(TypeKind::Enum),
            // `record` is a name like any other but before `Name(`.
            "record" if matches!(after_next, Some("(" | "<")) => Some(TypeKind::Record),
            _ => None,
        };
        if let (Some(kind), Some((name, offset))) = (kind, next) {
            // `Foo.class` names no type.
            if previous != Some(".") && is_identifier(name) {
                pending = Some((kind, name, *offset));
            }
            continue;
        }
        match *token {
            "{" => {
                depth += 1;
                if let Some((kind, name, offset)) = pending.take() {
                    // Directly in a type's body, or at the top: a member.
                    let in_a_body = open.last().is_none_or(|(_, inside)| *inside == depth - 1);
                    let top = open.is_empty() && depth == 1;
                    if top || (!open.is_empty() && in_a_body) {
                        let (line, column) = place(offset);
                        found.push(DeclaredType {
                            name: name.to_owned(),
                            outer: open.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join("."),
                            kind,
                            entry,
                            line,
                            column,
                        });
                        open.push((name.to_owned(), depth));
                    }
                }
            }
            "}" => {
                if open.last().is_some_and(|(_, inside)| *inside == depth) {
                    open.pop();
                }
                depth = depth.saturating_sub(1);
            }
            // `class` in `Foo.class;` or a forward mention never opens.
            ";" if depth == 0 => pending = None,
            _ => {}
        }
    }
    found
}

fn is_identifier(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
}

/// The tokens of Java (or Kotlin) source outside comments and literals:
/// identifiers, and each punctuation character on its own (`>>` kept
/// whole, as it closes two type argument lists).
fn code_tokens(text: &str) -> Vec<&str> {
    placed_tokens(text).into_iter().map(|(token, _)| token).collect()
}

/// [`code_tokens`], each with its byte offset.
fn placed_tokens(text: &str) -> Vec<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                    index += 1;
                }
                index += 2;
            }
            b'"' | b'\'' => {
                let quote = byte;
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            _ if byte.is_ascii_whitespace() => index += 1,
            _ if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80 => {
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_' || bytes[index] == b'$' || bytes[index] >= 0x80)
                {
                    index += 1;
                }
                tokens.push((&text[start..index], start));
            }
            b'>' if bytes.get(index + 1) == Some(&b'>') => {
                tokens.push((&text[index..index + 2], index));
                index += 2;
            }
            _ => {
                tokens.push((&text[index..index + 1], index));
                index += 1;
            }
        }
    }
    tokens
}

/// A type, or a field or enum constant of one, to find the uses of by
/// reading a file's tokens: what a search does in more library files than
/// it can parse.
pub struct Named<'a> {
    /// The package it is declared in: `java.lang.annotation`.
    pub package: &'a str,
    /// The type, and the types it is nested in, outermost first:
    /// `["ElementType"]`.
    pub owner: &'a [&'a str],
    /// The field or enum constant of `owner` to find; `None` for the type.
    pub member: Option<&'a str>,
    /// Whether `member` is an enum constant, written bare after `case`.
    pub enum_constant: bool,
}

/// Where a Java file's code writes `named` (byte offsets of the names),
/// told by its tokens, package and imports alone, as the compiler would
/// read them: `ElementType.TYPE` where `ElementType` is imported, in the
/// package, in `java.lang` or written in full; a bare `TYPE` statically
/// imported, or after `case`; the type's own name where it is visible.
/// A type the file declares itself of the same name hides the other.
pub fn named_uses(text: &str, named: &Named<'_>) -> Vec<usize> {
    let tokens = placed_tokens(text);
    let word = |index: usize| tokens.get(index).map_or("", |(token, _)| *token);
    let mut package = String::new();
    let mut imports: Vec<(bool, String)> = Vec::new();
    let mut declared: Vec<&str> = Vec::new();
    for (index, (token, _)) in tokens.iter().enumerate() {
        match *token {
            "package" if package.is_empty() && imports.is_empty() => package = dotted(&tokens[index + 1..]),
            "import" => {
                let is_static = word(index + 1) == "static";
                imports.push((is_static, dotted(&tokens[index + 1 + usize::from(is_static)..])));
            }
            "class" | "interface" | "enum" | "record" if index == 0 || word(index - 1) != "." => {
                if let Some(name) = tokens.get(index + 1).map(|(name, _)| *name).filter(|name| is_identifier(name)) {
                    declared.push(name);
                }
            }
            _ => {}
        }
    }
    let Some(&top) = named.owner.first() else {
        return Vec::new();
    };
    let full: Vec<&str> = named
        .package
        .split('.')
        .filter(|part| !part.is_empty())
        .chain(named.owner.iter().copied())
        .collect();
    let top_qualified = format!("{}.{top}", named.package);
    let in_package = package == named.package;
    let visible = {
        let own = declared.contains(&top);
        let imported_elsewhere = imports.iter().any(|(is_static, import)| {
            !is_static && import.rsplit('.').next() == Some(top) && *import != top_qualified
        });
        let imported = imports
            .iter()
            .any(|(is_static, import)| !is_static && (*import == top_qualified || *import == format!("{}.*", named.package)));
        if own {
            in_package
        } else if imported_elsewhere {
            false
        } else {
            imported || in_package || named.package == "java.lang"
        }
    };
    // The names before `.` that qualify the token at `index`.
    let qualifier = |index: usize| -> Vec<&str> {
        let mut chain = Vec::new();
        let mut at = index;
        while at >= 2 && word(at - 1) == "." && is_identifier(word(at - 2)) {
            chain.push(word(at - 2));
            at -= 2;
        }
        chain.reverse();
        chain
    };
    let mut found = Vec::new();
    match named.member {
        Some(member) => {
            let owner_path = named.owner.join(".");
            let statically_imported = imports.iter().any(|(is_static, import)| {
                *is_static
                    && (*import == format!("{}.{owner_path}.{member}", named.package)
                        || *import == format!("{}.{owner_path}.*", named.package))
            });
            for (index, (token, offset)) in tokens.iter().enumerate() {
                if *token != member || word(index + 1) == "(" {
                    continue;
                }
                let qualified_by = qualifier(index);
                let matches = if qualified_by.is_empty() {
                    let after_case = named.enum_constant && index > 0 && word(index - 1) == "case" && (visible || in_package);
                    (statically_imported || after_case) && (index == 0 || word(index - 1) != ".")
                } else {
                    qualified_by == full || (qualified_by == named.owner && visible)
                };
                if matches {
                    found.push(*offset);
                }
            }
        }
        None => {
            let name = *named.owner.last().unwrap_or(&top);
            let outer: Vec<&str> = full[..full.len() - 1].to_vec();
            for (index, (token, offset)) in tokens.iter().enumerate() {
                if *token != name || (index > 0 && matches!(word(index - 1), "class" | "interface" | "enum" | "record")) {
                    continue;
                }
                let qualified_by = qualifier(index);
                let matches = if qualified_by.is_empty() {
                    named.owner.len() == 1 && visible
                } else {
                    qualified_by == outer
                        || (named.owner.len() > 1 && qualified_by == named.owner[..named.owner.len() - 1] && visible)
                };
                if matches {
                    found.push(*offset);
                }
            }
        }
    }
    found
}

/// `a.b.C` from the tokens up to `;`, `*` included.
fn dotted(tokens: &[(&str, usize)]) -> String {
    tokens
        .iter()
        .map(|(token, _)| *token)
        .take_while(|token| *token != ";")
        .collect()
}

fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varint(input: &mut &[u8]) -> Option<u32> {
    let mut value = 0u32;
    let mut shift = 0;
    loop {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    fn uses(text: &str, named: &Named<'_>) -> Vec<String> {
        named_uses(text, named)
            .into_iter()
            .map(|offset| {
                let line = text[..offset].matches('\n').count() + 1;
                let end = text[offset..].find(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(text.len(), |end| offset + end);
                format!("{line}:{}", &text[offset..end])
            })
            .collect()
    }

    const TYPE: Named<'static> = Named {
        package: "java.lang.annotation",
        owner: &["ElementType"],
        member: Some("TYPE"),
        enum_constant: true,
    };

    #[test]
    fn a_field_is_used_where_its_type_is_imported_qualified_or_statically_imported() {
        let imported = "package a;\nimport java.lang.annotation.ElementType;\n@Target(ElementType.TYPE)\n@interface A {}\n";
        assert_eq!(uses(imported, &TYPE), ["3:TYPE"]);
        let on_demand = "package a;\nimport java.lang.annotation.*;\n@Target({ElementType.TYPE, ElementType.FIELD})\n@interface A {}\n";
        assert_eq!(uses(on_demand, &TYPE), ["3:TYPE"]);
        let full = "package a;\nclass A { Object o = java.lang.annotation.ElementType.TYPE; }\n";
        assert_eq!(uses(full, &TYPE), ["2:TYPE"]);
        let statically = "package a;\nimport static java.lang.annotation.ElementType.TYPE;\n@Target(TYPE)\n@interface A {}\n";
        assert_eq!(uses(statically, &TYPE), ["2:TYPE", "3:TYPE"]);
        let in_switch = "package a;\nimport java.lang.annotation.ElementType;\nclass A { void f(ElementType t) { switch (t) { case TYPE: } } }\n";
        assert_eq!(uses(in_switch, &TYPE), ["3:TYPE"]);
    }

    #[test]
    fn a_field_of_another_type_of_the_same_name_is_not_used() {
        let elsewhere = "package a;\nimport b.ElementType;\nclass A { Object o = ElementType.TYPE; }\n";
        assert!(uses(elsewhere, &TYPE).is_empty());
        let own = "package a;\nclass A { enum ElementType { TYPE } Object o = ElementType.TYPE; }\n";
        assert!(uses(own, &TYPE).is_empty());
        let not_imported = "package a;\nclass A { Object o = ElementType.TYPE; }\n";
        assert!(uses(not_imported, &TYPE).is_empty());
        let commented = "package a;\nimport java.lang.annotation.ElementType;\n// ElementType.TYPE\nclass A { String s = \"ElementType.TYPE\"; Object o = x.TYPE; int t = TYPE(); }\n";
        assert!(uses(commented, &TYPE).is_empty());
    }

    #[test]
    fn a_type_is_used_where_it_is_visible() {
        let list = Named { package: "java.util", owner: &["List"], member: None, enum_constant: false };
        let text = "package a;\nimport java.util.List;\nclass A { List<String> l; java.util.List m; java.awt.List n; }\n";
        assert_eq!(uses(text, &list), ["2:List", "3:List", "3:List"]);
        let awt = "package a;\nimport java.awt.List;\nclass A { List l; }\n";
        assert!(uses(awt, &list).is_empty());
        let long = Named { package: "java.lang", owner: &["Long"], member: None, enum_constant: false };
        assert_eq!(uses("package a;\nclass A { Long l; }\n", &long), ["2:Long"]);
    }

    use super::*;

    #[test]
    fn finds_the_files_writing_a_word_and_reads_itself_back() {
        let directory = tempfile::tempdir().unwrap();
        let jar = crate::indexer::library::tests::jar_named(
            directory.path(),
            "lib-sources.jar",
            &[
                ("a/A.java", "package a; class A { void run() { Thread.currentThread(); } }"),
                ("a/B.java", "package a; class B { /** {@link A#run()} */ void stop() {} }"),
                ("a/notes.txt", "run run run"),
            ],
        );
        let index = WordIndex::build(&jar).unwrap();
        // A javadoc's `{@link A#run()}` is no code: only `A` declares it.
        assert_eq!(index.entries_with("run"), vec!["a/A.java"]);
        assert_eq!(index.entries_calling("currentThread"), vec!["a/A.java"]);
        assert_eq!(index.entries_calling("stop"), vec!["a/B.java"]);
        assert_eq!(index.entries_with("currentThread"), vec!["a/A.java"]);
        assert!(index.entries_with("missing").is_empty());
        let saved = directory.path().join("lib.words");
        index.save(&saved).unwrap();
        let loaded = WordIndex::load(&saved).unwrap();
        assert_eq!(loaded.entries_with("stop"), vec!["a/B.java"]);
    }

    #[test]
    fn lists_the_top_level_and_member_types_not_the_local_ones() {
        let text = "package p;\nimport x.Y;\npublic class Outer<T> extends Base {\n    interface Inner { enum Mode { A, B; } }\n    void run() { class Local {} Object o = new Object() { class InAnonymous {} }; Class<?> c = String.class; }\n    @interface Note {}\n    record Point(int x) {}\n}\nclass Second {}\n";
        let found: Vec<(String, String, TypeKind, u32, u32)> = declared_types(text, 7)
            .into_iter()
            .map(|declared| (declared.name, declared.outer, declared.kind, declared.line, declared.column))
            .collect();
        assert_eq!(
            found,
            vec![
                ("Outer".to_owned(), String::new(), TypeKind::Class, 2, 13),
                ("Inner".to_owned(), "Outer".to_owned(), TypeKind::Interface, 3, 14),
                ("Mode".to_owned(), "Outer.Inner".to_owned(), TypeKind::Enum, 3, 27),
                ("Note".to_owned(), "Outer".to_owned(), TypeKind::Annotation, 5, 15),
                ("Point".to_owned(), "Outer".to_owned(), TypeKind::Record, 6, 11),
                ("Second".to_owned(), String::new(), TypeKind::Class, 8, 6),
            ]
        );
    }

    #[test]
    fn names_what_types_extend_and_what_anonymous_classes_create() {
        let text = "class A<T extends Comparable<T>> extends java.util.AbstractList<Map<K, V>> implements Iterable<T>, RandomAccess {\n\
            // extends Commented\n\
            Runnable r = new Runnable() { public void run() {} };\n\
            Object o = new Object();\n\
            String s = \"implements Quoted\";\n\
        }";
        assert_eq!(
            supertype_names(text),
            vec!["Comparable", "AbstractList", "Iterable", "RandomAccess", "Runnable"]
        );
    }
}
