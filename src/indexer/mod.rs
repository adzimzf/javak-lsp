//! What the server knows: the open files, each parsed once, where the
//! workspace is, and the sources of its libraries ([`library`]).
//!
//! Where a language keeps its sources and what a library file of it
//! declares is behind [`SourceLayout`], implemented in `java` and `kotlin`.

pub mod java;
pub mod kotlin;
pub mod library;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use tower_lsp::lsp_types::{Location, Position, Range, SymbolInformation, SymbolKind, Url};

use crate::feature::query;
use crate::feature::semantic::{types_of, FileView, Snapshot};
use crate::language::Language;
use crate::parser::{syntax, Kind, Syn, Syntax};
use library::Libraries;

/// Where a language keeps its sources.
pub trait SourceLayout: Send + Sync {
    /// The directories a source tree is rooted at: a file's package is the
    /// path below one of these.
    fn source_roots(&self) -> &'static [&'static str];
    /// The types a source file of a library declares, by its file name and,
    /// where [`Self::reads_library_text`], its text.
    fn library_types(&self, file_stem: &str, text: &str) -> Vec<String>;
    /// Whether [`Self::library_types`] needs the file's text.
    fn reads_library_text(&self) -> bool;
}

/// The source layout of a language.
pub fn layout(language: Language) -> &'static dyn SourceLayout {
    match language {
        Language::Java => &java::Java,
        Language::Kotlin => &kotlin::Kotlin,
    }
}

/// How many library files one type name may bring in, and how many rounds
/// of supertypes are followed from them.
const FILES_PER_NAME: usize = 4;
const SUPERTYPE_ROUNDS: usize = 4;

/// An open file, parsed when it is opened or changed.
pub struct Document {
    pub language: Language,
    pub text: Arc<String>,
    pub tree: Arc<Syn>,
}

pub struct Indexer {
    /// Whether the root was given and no scan may change it (the tests pin
    /// it at a directory that does not exist).
    pub workspace_pinned: AtomicBool,
    pub workspace_root: OnceLock<PathBuf>,
    documents: DashMap<Url, Document>,
    pub libraries: Libraries,
}

impl Default for Indexer {
    fn default() -> Self {
        Self::new()
    }
}

impl Indexer {
    pub fn new() -> Self {
        Self {
            workspace_pinned: AtomicBool::new(false),
            workspace_root: OnceLock::new(),
            documents: DashMap::new(),
            libraries: Libraries::default(),
        }
    }

    pub fn syntax(&self, language: Language) -> &'static dyn Syntax {
        syntax(language)
    }

    pub fn open(&self, uri: Url, text: String, language: Language) {
        let tree = syntax(language).tree(&text);
        self.documents.insert(
            uri,
            Document {
                language,
                text: Arc::new(text),
                tree: Arc::new(tree),
            },
        );
    }

    pub fn change(&self, uri: &Url, text: String) {
        if let Some(mut document) = self.documents.get_mut(uri) {
            document.tree = Arc::new(syntax(document.language).tree(&text));
            document.text = Arc::new(text);
        }
    }

    pub fn close(&self, uri: &Url) {
        self.documents.remove(uri);
    }

    pub fn text(&self, uri: &Url) -> Option<String> {
        self.documents
            .get(uri)
            .map(|document| document.text.as_str().to_owned())
    }

    pub fn language(&self, uri: &Url) -> Option<Language> {
        self.documents.get(uri).map(|document| document.language)
    }

    /// The path of `uri` below the workspace root, `/`-separated.
    pub fn relative_path(&self, uri: &Url) -> Option<String> {
        let root = self.workspace_root.get()?;
        let path = uri.to_file_path().ok()?;
        let below = path.strip_prefix(root).ok()?;
        Some(below.to_string_lossy().replace('\\', "/"))
    }

    /// The package a file's path says it is in: the directories below a
    /// source root of its language, dotted. `None` where the path names no
    /// package — no source root above it, nothing below it, or a directory
    /// that is not an identifier.
    pub fn package_of_path(&self, uri: &Url) -> Option<String> {
        let relative = self.relative_path(uri)?;
        let language = self
            .language(uri)
            .or_else(|| Language::of_path(&relative))?;
        let below_root = layout(language)
            .source_roots()
            .iter()
            .find_map(|root| relative.strip_prefix(&format!("{root}/")))?;
        dotted_directories(below_root)
    }

    /// The package a file's folder stands for: the one its source root
    /// says, or else the folders below the workspace root.
    pub fn implied_package(&self, uri: &Url) -> Option<String> {
        if let Some(package) = self.package_of_path(uri) {
            return Some(package);
        }
        dotted_directories(&self.relative_path(uri)?)
    }
}

/// `com/demo/Main.java` as `com.demo`; `None` where a directory is not an
/// identifier or there is none.
fn dotted_directories(path: &str) -> Option<String> {
    let directories = Path::new(path).parent()?;
    let segments: Vec<&str> = directories
        .to_str()?
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() || !segments.iter().all(|segment| is_identifier(segment)) {
        return None;
    }
    Some(segments.join("."))
}

fn is_identifier(word: &str) -> bool {
    let mut characters = word.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && characters.all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

impl Indexer {
    /// Every open file, and the library files they name: what the semantic
    /// layer reads.
    pub fn snapshot(&self) -> Snapshot {
        let mut files: Vec<FileView> = self
            .documents
            .iter()
            .map(|document| FileView {
                uri: document.key().clone(),
                language: document.language,
                text: document.text.clone(),
                tree: document.tree.clone(),
                library: false,
            })
            .collect();
        let libraries = self.library_files_named_by(&files);
        files.extend(libraries);
        Snapshot { files }
    }

    /// The library files that declare a type the workspace files name, and
    /// the supertypes of those, a few levels up. A type the workspace
    /// declares itself is not looked for.
    fn library_files_named_by(&self, workspace: &[FileView]) -> Vec<FileView> {
        let declared: HashSet<&str> = workspace
            .iter()
            .flat_map(|file| types_of(file).into_iter().map(|(name, _)| name))
            .collect();
        let mut wanted: Vec<String> = workspace
            .iter()
            .flat_map(|file| type_names(&file.tree, false))
            .filter(|name| !declared.contains(name.as_str()))
            .collect();
        let mut seen_names: HashSet<String> = HashSet::new();
        let mut loaded: Vec<FileView> = Vec::new();
        let mut loaded_uris: HashSet<Url> = HashSet::new();
        for _ in 0..=SUPERTYPE_ROUNDS {
            let mut next = Vec::new();
            for name in wanted.drain(..) {
                if !seen_names.insert(name.clone()) {
                    continue;
                }
                for file in self.libraries.declaring(&name).iter().take(FILES_PER_NAME) {
                    let Some(library) = self.libraries.load(file) else {
                        continue;
                    };
                    if !loaded_uris.insert(library.uri.clone()) {
                        continue;
                    }
                    next.extend(type_names(&library.tree, true));
                    loaded.push(FileView {
                        uri: library.uri.clone(),
                        language: library.language,
                        text: library.text.clone(),
                        tree: library.tree.clone(),
                        library: true,
                    });
                }
            }
            if next.is_empty() {
                break;
            }
            wanted = next;
        }
        loaded
    }
}

/// The type names a tree writes: every type name and imported name of a
/// workspace file, only the supertypes of a library file.
fn type_names(tree: &Syn, supertypes_only: bool) -> Vec<String> {
    let mut names = Vec::new();
    let mut add = |name: &str| {
        let last = name.rsplit('.').next().unwrap_or(name).trim();
        let starts_upper = last.chars().next().is_some_and(char::is_uppercase);
        if starts_upper && !names.iter().any(|known: &String| known == last) {
            names.push(last.to_owned());
        }
    };
    if supertypes_only {
        for node in tree.walk() {
            let is_list = matches!(
                node.kind.as_str(),
                "superclass" | "super_interfaces" | "extends_interfaces"
            );
            if is_list {
                for named in node.walk() {
                    if named.kind == "type_identifier" {
                        add(&named.text);
                    }
                }
            }
        }
        return names;
    }
    for node in tree.walk() {
        match node.kind.as_str() {
            "type_identifier" | "identifier" => add(&node.text),
            "import_declaration" => {
                let written = node.text.trim_start_matches("import").trim();
                add(written.trim_end_matches(';').trim());
            }
            _ => {}
        }
    }
    names
}

impl Indexer {
    /// Whether any open file declares `name`: a name no source of the
    /// workspace declares belongs to a library, and is not the
    /// workspace's to rename.
    pub fn is_declared(&self, name: &str) -> bool {
        self.documents.iter().any(|document| {
            syntax(document.language)
                .declared_names(&document.text)
                .iter()
                .any(|declared| declared == name)
        })
    }

    /// Every type of every open file whose name the query matches.
    pub fn types_matching(&self, query: &str) -> Vec<SymbolInformation> {
        let mut found = Vec::new();
        for document in self.documents.iter() {
            let outline = syntax(document.language).outline(&document.text);
            for entry in outline {
                let is_type = matches!(entry.kind, Kind::Class | Kind::Interface | Kind::Enum);
                if !is_type || !query::matches(query, &entry.name) {
                    continue;
                }
                found.push(located(document.key().clone(), &entry));
            }
        }
        found
    }
}

#[allow(deprecated)]
fn located(uri: Url, entry: &crate::parser::Outline) -> SymbolInformation {
    let start = Position {
        line: entry.selection.line,
        character: entry.selection.column,
    };
    SymbolInformation {
        name: entry.name.clone(),
        kind: match entry.kind {
            Kind::Interface => SymbolKind::INTERFACE,
            Kind::Enum => SymbolKind::ENUM,
            _ => SymbolKind::CLASS,
        },
        tags: None,
        deprecated: None,
        location: Location {
            uri,
            range: Range { start, end: start },
        },
        container_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_is_the_directories_below_a_source_root() {
        let indexer = Indexer::new();
        indexer.workspace_root.set(PathBuf::from("/root")).unwrap();
        let package = |path: &str| {
            indexer.package_of_path(&Url::parse(&format!("file:///root/{path}")).unwrap())
        };
        assert_eq!(
            package("src/main/java/com/demo/Main.java").as_deref(),
            Some("com.demo")
        );
        assert_eq!(
            package("src/main/kotlin/com/demo/Main.kt").as_deref(),
            Some("com.demo")
        );
        assert_eq!(package("src/main/kotlin/com/demo/Main.java"), None);
        assert_eq!(package("src/main/java/Main.java"), None);
        assert_eq!(package("src/main/java/com/my-demo/Main.java"), None);
        assert_eq!(package("com/demo/Main.java"), None);
    }

    #[test]
    fn a_type_of_a_library_resolves_into_its_sources_jar() {
        let directory = tempfile::tempdir().unwrap();
        let jar = library::tests::jar(
            directory.path(),
            &[(
                "com/acme/Greeter.java",
                "package com.acme;\n\npublic class Greeter extends Base {\n    public String greet() {\n        return \"\";\n    }\n}\n",
            ), (
                "com/acme/Base.java",
                "package com.acme;\n\npublic class Base {\n    public void close() {\n    }\n}\n",
            ), (
                "com/acme/Unused.java",
                "package com.acme;\n\npublic class Unused {\n}\n",
            )],
        );
        let mut indexer = Indexer::new();
        indexer.libraries = Libraries::extracting_into(directory.path().join("sources"));
        indexer.libraries.replace(&[jar]);
        let main = Url::parse("file:///work/src/main/java/com/demo/Main.java").unwrap();
        indexer.open(
            main.clone(),
            "package com.demo;\n\nimport com.acme.Greeter;\n\nclass Main {\n    void run(Greeter greeter) {\n        greeter.close();\n    }\n}\n".to_owned(),
            Language::Java,
        );
        let snapshot = indexer.snapshot();
        let libraries: Vec<&str> = snapshot
            .files
            .iter()
            .filter(|file| file.library)
            .map(|file| file.uri.path().rsplit('/').next().unwrap())
            .collect();
        assert_eq!(libraries, vec!["Greeter.java", "Base.java"]);
        let found = snapshot
            .declaration_at(&main, crate::parser::Position { line: 6, column: 18 })
            .unwrap();
        assert!(found.file.library);
        assert!(found.file.uri.to_file_path().unwrap().is_file());
        assert_eq!(found.name.text, "close");
        assert_eq!(found.name.start.line, 3);
    }
}
