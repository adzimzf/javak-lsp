//! What the server knows: the open files, and where the workspace is.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use dashmap::DashMap;
use tower_lsp::lsp_types::{Location, Position, Range, SymbolInformation, SymbolKind, Url};

use crate::parser::treesitter::{Language, TreeSitter};
use crate::parser::{Kind, Syntax};
use crate::query;
use crate::semantic::{FileView, Snapshot};

/// The directories a Java source tree is rooted at: a file's package is
/// the path below one of these.
const SOURCE_ROOTS: &[&str] = &["src/main/java", "src/test/java", "src/main/kotlin", "src/test/kotlin"];

pub struct Document {
    pub text: String,
    pub language: Language,
}

pub struct Indexer {
    /// Whether the root was given and no scan may change it (the tests pin
    /// it at a directory that does not exist).
    pub workspace_pinned: AtomicBool,
    pub workspace_root: OnceLock<PathBuf>,
    documents: DashMap<Url, Document>,
    java: TreeSitter,
    kotlin: TreeSitter,
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
            java: TreeSitter::new(Language::Java),
            kotlin: TreeSitter::new(Language::Kotlin),
        }
    }

    pub fn syntax(&self, language: Language) -> &dyn Syntax {
        match language {
            Language::Java => &self.java,
            Language::Kotlin => &self.kotlin,
        }
    }

    pub fn open(&self, uri: Url, text: String, language: Language) {
        self.documents.insert(uri, Document { text, language });
    }

    pub fn change(&self, uri: &Url, text: String) {
        if let Some(mut document) = self.documents.get_mut(uri) {
            document.text = text;
        }
    }

    pub fn close(&self, uri: &Url) {
        self.documents.remove(uri);
    }

    pub fn text(&self, uri: &Url) -> Option<String> {
        self.documents.get(uri).map(|document| document.text.clone())
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

    /// The package a file's path says it is in: the directories below its
    /// source root, dotted. `None` where the path names no package — no
    /// source root above it, nothing below it, or a directory that is not
    /// an identifier.
    pub fn package_of_path(&self, uri: &Url) -> Option<String> {
        let relative = self.relative_path(uri)?;
        let below_root = SOURCE_ROOTS
            .iter()
            .find_map(|root| relative.strip_prefix(&format!("{root}/")))?;
        let directories = Path::new(below_root).parent()?;
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
}

fn is_identifier(word: &str) -> bool {
    let mut characters = word.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && characters.all(|character| character.is_alphanumeric() || character == '_' || character == '$')
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
        assert_eq!(package("src/main/java/Main.java"), None);
        assert_eq!(package("src/main/java/com/my-demo/Main.java"), None);
        assert_eq!(package("com/demo/Main.java"), None);
    }
}

impl Indexer {
    /// Every open file, parsed: what the semantic layer reads.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            files: self
                .documents
                .iter()
                .map(|document| FileView {
                    uri: document.key().clone(),
                    text: document.text.clone(),
                    tree: self.syntax(document.language).tree(&document.text),
                })
                .collect(),
        }
    }

    /// Whether any open file declares `name`: a name no source of the
    /// workspace declares belongs to a library, and is not the
    /// workspace's to rename.
    pub fn is_declared(&self, name: &str) -> bool {
        self.documents.iter().any(|document| {
            self.syntax(document.language)
                .declared_names(&document.text)
                .iter()
                .any(|declared| declared == name)
        })
    }

    /// Every type of every open file whose name the query matches.
    pub fn types_matching(&self, query: &str) -> Vec<SymbolInformation> {
        let mut found = Vec::new();
        for document in self.documents.iter() {
            let outline = self.syntax(document.language).outline(&document.text);
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
