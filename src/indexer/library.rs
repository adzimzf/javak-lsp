//! The sources of the project's libraries: the `-sources.jar`s (or the
//! decompiled ones) dependency sync resolves, and the platform's own — the
//! JDK's `src.zip` and the Kotlin standard library's sources jar
//! ([`crate::dependency_sync::platform`]).
//!
//! Each jar is listed once, by the types its files declare. A file is
//! extracted into the cache and parsed only when a workspace file names one
//! of its types, so a request reads the few library files it needs, not
//! every library on the classpath.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;
use tower_lsp::lsp_types::Url;

use super::layout;
use super::words::WordIndex;
use crate::language::Language;
use crate::parser::{syntax, Syn};

/// Whether a type's name is an anonymous class's binary name
/// (`StringModel$1`): a decompiler writes one out as a class of its own
/// where the class file does not say whose it is, and jdtls (JDT), which
/// reads it as anonymous, never lists it as a symbol.
fn is_anonymous(name: &str) -> bool {
    name.rsplit_once('$')
        .is_some_and(|(_, last)| !last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit()))
}

/// One source file of a library jar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LibraryFile {
    pub jar: PathBuf,
    /// The file's path inside the jar.
    pub entry: String,
    pub language: Language,
    /// The package the file declares, `""` for the unnamed one.
    pub package: String,
    /// Which of several files declaring the same name is read first:
    /// lower first ([`rank`]).
    pub rank: u8,
}

/// A type a library declares, where it is once extracted.
pub struct LibraryType {
    pub name: String,
    /// Its package and the types it is nested in.
    pub container: String,
    pub kind: super::words::TypeKind,
    pub uri: Url,
    pub line: u32,
    pub column: u32,
}

/// A library file, extracted and parsed.
pub struct Loaded {
    pub uri: Url,
    pub language: Language,
    pub text: Arc<String>,
    pub tree: Arc<Syn>,
    /// The types its declarations extend, by simple name.
    pub supertypes: Vec<String>,
}

/// What the indexed jars declare, by simple name.
#[derive(Default)]
struct Listing {
    /// Type name → the files declaring a type of that name, best first.
    types: HashMap<String, Vec<LibraryFile>>,
    /// Name of a top-level function (Kotlin) → the files declaring one.
    functions: HashMap<String, Vec<LibraryFile>>,
}

pub struct Libraries {
    by_name: RwLock<Listing>,
    loaded: DashMap<LibraryFile, Arc<Loaded>>,
    /// Where library files are extracted.
    extracted: PathBuf,
    /// The jars indexed.
    jars: RwLock<Vec<PathBuf>>,
    /// Each jar's words ([`WordIndex`]), read or built when first asked.
    words: DashMap<PathBuf, Arc<WordIndex>>,
    /// Each jar opened once: opening reads its whole directory, fifteen
    /// thousand entries for the JDK.
    archives: DashMap<PathBuf, Arc<std::sync::Mutex<zip::ZipArchive<std::fs::File>>>>,
    /// Each sources jar's entries its jar ships no class of
    /// ([`unshipped_entries`]), worked out when first asked.
    unshipped: DashMap<PathBuf, Arc<Vec<String>>>,
}

impl Default for Libraries {
    /// Extracting into `~/.cache/javak-lsp/sources`.
    fn default() -> Self {
        Self::extracting_into(
            crate::util::xdg_cache_base()
                .join("javak-lsp")
                .join("sources"),
        )
    }
}

impl Libraries {
    pub fn extracting_into(directory: PathBuf) -> Self {
        Self {
            by_name: RwLock::default(),
            loaded: DashMap::new(),
            extracted: directory,
            jars: RwLock::default(),
            words: DashMap::new(),
            archives: DashMap::new(),
            unshipped: DashMap::new(),
        }
    }

    /// Whether `entry` of the sources jar `jar` is a class its jar ships:
    /// a source of a class no jar holds (Lombok keeps its own classes as
    /// `.SCL.lombok` resources) can be used by nothing.
    fn ships(&self, jar: &Path, entry: &str) -> bool {
        let unshipped = match self.unshipped.get(jar) {
            Some(known) => known.clone(),
            None => {
                let entries = Arc::new(unshipped_entries(jar));
                self.unshipped.insert(jar.to_path_buf(), entries.clone());
                entries
            }
        };
        unshipped.binary_search_by(|hidden| hidden.as_str().cmp(entry)).is_err()
    }

    /// Index these sources jars, replacing what was indexed before.
    /// Returns how many type declarations were listed.
    pub fn replace(&self, jars: &[PathBuf]) -> usize {
        let mut listing = Listing::default();
        for jar in jars {
            if let Err(error) = list_jar(jar, &mut listing) {
                log::warn!("libraries: cannot read {}: {error}", jar.display());
            }
        }
        for files in listing.types.values_mut().chain(listing.functions.values_mut()) {
            files.sort_by_key(|file| file.rank);
        }
        let count = listing.types.values().map(Vec::len).sum();
        if let Ok(mut indexed) = self.by_name.write() {
            *indexed = listing;
        }
        self.loaded.clear();
        self.words.retain(|jar, _| jars.contains(jar));
        self.archives.clear();
        if let Ok(mut indexed) = self.jars.write() {
            *indexed = jars.to_vec();
        }
        count
    }

    /// The word index of `jar`: read from the cache, else built and kept
    /// there.
    fn word_index(&self, jar: &Path) -> Option<Arc<WordIndex>> {
        if let Some(index) = self.words.get(jar) {
            return Some(index.clone());
        }
        let stem = jar.file_stem()?.to_string_lossy().into_owned();
        let cached = self
            .extracted
            .join(format!("{stem}-{:016x}.words", jar_fingerprint(jar)?));
        let index = match WordIndex::load(&cached) {
            Ok(index) => index,
            Err(_) => {
                let index = WordIndex::build(jar).ok()?;
                if let Err(error) = index.save(&cached) {
                    log::warn!("libraries: cannot keep the words of {}: {error}", jar.display());
                }
                index
            }
        };
        let index = Arc::new(index);
        self.words.insert(jar.to_path_buf(), index.clone());
        Some(index)
    }

    /// Builds (or reads) the word index of every jar, so the first search
    /// does not wait for it.
    pub fn warm_word_indexes(&self) {
        let jars = self.jars.read().map(|jars| jars.clone()).unwrap_or_default();
        for jar in jars {
            let _ = self.word_index(&jar);
        }
    }

    /// Every library source file that writes `word`, with its text — read,
    /// not extracted nor kept.
    pub fn files_containing(&self, word: &str) -> Vec<(LibraryFile, String)> {
        self.files_where(|index| index.entries_with(word))
    }

    /// Every library source file that calls (or declares) a method called
    /// `name`, with its text.
    pub fn files_calling(&self, name: &str) -> Vec<(LibraryFile, String)> {
        self.files_where(|index| index.entries_calling(name))
    }

    /// Every library source file whose types may extend a type called
    /// `name`, or that creates an anonymous class of it, with its text.
    pub fn files_extending(&self, name: &str) -> Vec<(LibraryFile, String)> {
        self.files_where(|index| index.entries_extending(name))
    }

    fn files_where(&self, entries_of: impl Fn(&WordIndex) -> Vec<&str>) -> Vec<(LibraryFile, String)> {
        let jars = self.jars.read().map(|jars| jars.clone()).unwrap_or_default();
        let mut found = Vec::new();
        for jar in jars {
            let Some(index) = self.word_index(&jar) else {
                continue;
            };
            let entries = entries_of(&index);
            if entries.is_empty() {
                continue;
            }
            for entry in entries {
                let Some(language) = Language::of_path(entry) else {
                    continue;
                };
                // Examples kept beside the JDK's documentation, never
                // compiled: no class of theirs is searched.
                if entry.contains("/snippet-files/") || entry.contains("/doc-files/") || !self.ships(&jar, entry) {
                    continue;
                }
                let Some(text) = self.read_entry(&jar, entry) else {
                    continue;
                };
                let file = LibraryFile {
                    jar: jar.clone(),
                    entry: entry.to_owned(),
                    language,
                    package: layout(language).library_package(entry, &text),
                    rank: rank(entry),
                };
                found.push((file, text));
            }
        }
        found
    }

    /// The Java types the jars declare whose name the workspace-symbol
    /// `query` matches, each extracted so a client can open it: what jdtls
    /// lists of the libraries.
    pub fn types_matching(&self, query: &str) -> Vec<LibraryType> {
        let jars = self.jars.read().map(|jars| jars.clone()).unwrap_or_default();
        let mut found = Vec::new();
        for jar in jars {
            let Some(index) = self.word_index(&jar) else {
                continue;
            };
            let mut archive = None;
            for (declared, entry) in index.declared_types() {
                if Language::of_path(entry) != Some(Language::Java)
                    || is_anonymous(&declared.name)
                    || !self.ships(&jar, entry)
                    || !crate::feature::query::matches(query, &declared.name)
                    || entry.contains("/snippet-files/")
                    || entry.contains("/doc-files/")
                {
                    continue;
                }
                let Some(path) = self.extracted_path(&jar, entry) else {
                    continue;
                };
                if !path.is_file() {
                    let archive = archive.get_or_insert_with(|| {
                        std::fs::File::open(&jar).ok().and_then(|file| zip::ZipArchive::new(file).ok())
                    });
                    let mut text = String::new();
                    let read = archive
                        .as_mut()
                        .and_then(|archive| archive.by_name(entry).ok())
                        .is_some_and(|mut file| file.read_to_string(&mut text).is_ok());
                    if !read {
                        continue;
                    }
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&path, text);
                }
                let Ok(uri) = Url::from_file_path(&path) else {
                    continue;
                };
                let package = layout(Language::Java).library_package(entry, "");
                let container = [package.as_str(), declared.outer.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(".");
                found.push(LibraryType {
                    name: declared.name.clone(),
                    container,
                    kind: declared.kind,
                    uri,
                    line: declared.line,
                    column: declared.column,
                });
            }
            found.extend(self.kotlin_facades_matching(&jar, &index, query));
        }
        found
    }

    /// The classes Kotlin compiles a file's top-level functions into
    /// (`ListableBeanFactoryExtensions.kt` into
    /// `ListableBeanFactoryExtensionsKt`), whose names match `query`: Java
    /// code calls them by that name, and jdtls lists them as classes.
    fn kotlin_facades_matching(&self, jar: &Path, index: &WordIndex, query: &str) -> Vec<LibraryType> {
        let mut found = Vec::new();
        for entry in index.entries().filter(|entry| entry.ends_with(".kt")) {
            let stem = Path::new(entry).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let facade = format!("{stem}Kt");
            if !crate::feature::query::matches(query, &facade) || !self.ships(jar, entry) {
                continue;
            }
            let Some(text) = self.read_entry(jar, entry) else {
                continue;
            };
            // `@file:JvmName("…")` names the class otherwise.
            if text.contains("@file:JvmName") || layout(Language::Kotlin).library_functions(&text).is_empty() {
                continue;
            }
            let file = LibraryFile {
                jar: jar.to_path_buf(),
                entry: entry.to_owned(),
                language: Language::Kotlin,
                package: layout(Language::Kotlin).library_package(entry, &text),
                rank: rank(entry),
            };
            self.extract(&file, &text);
            let Some(uri) = self.uri_of(&file) else {
                continue;
            };
            found.push(LibraryType {
                name: facade,
                container: file.package,
                kind: super::words::TypeKind::Class,
                uri,
                line: 0,
                column: 0,
            });
        }
        found
    }

    /// The URI a library file has once extracted.
    pub fn uri_of(&self, file: &LibraryFile) -> Option<Url> {
        Url::from_file_path(self.extracted_path(&file.jar, &file.entry)?).ok()
    }

    /// The library file a client opened at `uri` (where [`Self::uri_of`]
    /// puts it).
    pub fn file_at(&self, uri: &Url) -> Option<LibraryFile> {
        let stem = Path::new(uri.path()).file_stem()?.to_str()?.to_owned();
        self.declaring(&stem)
            .into_iter()
            .find(|file| self.uri_of(file).as_ref() == Some(uri))
    }

    /// Whether `file` is the stub of a compiled class
    /// ([`crate::dependency_sync::stubs`]) not decompiled yet.
    pub fn is_undecompiled_stub(&self, file: &LibraryFile) -> bool {
        decompiled_class(&file.jar, &file.entry).is_some_and(|path| !path.is_file())
    }

    /// Reads `file` again: its class was decompiled in place of its stub.
    /// The extracted copy a client opens is rewritten with it.
    pub fn reread(&self, file: &LibraryFile) {
        self.loaded.remove(file);
        if let (Some(text), Some(path)) = (self.read_entry(&file.jar, &file.entry), self.extracted_path(&file.jar, &file.entry)) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, text);
        }
    }

    /// Writes a library file into the cache where [`Self::uri_of`] says,
    /// so a client can open it.
    pub fn extract(&self, file: &LibraryFile, text: &str) {
        let Some(path) = self.extracted_path(&file.jar, &file.entry) else {
            return;
        };
        if is_stale(&path, &file.jar, text) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, text);
        }
    }

    /// The library files that declare a type called `name`, best first.
    pub fn declaring(&self, name: &str) -> Vec<LibraryFile> {
        self.by_name
            .read()
            .ok()
            .and_then(|indexed| indexed.types.get(name).cloned())
            .unwrap_or_default()
    }

    /// The library files that declare a top-level function called `name`,
    /// best first.
    pub fn declaring_function(&self, name: &str) -> Vec<LibraryFile> {
        self.by_name
            .read()
            .ok()
            .and_then(|indexed| indexed.functions.get(name).cloned())
            .unwrap_or_default()
    }

    /// The simple names of the types whose name starts with `prefix`,
    /// whatever their case, sorted.
    pub fn types_starting(&self, prefix: &str) -> Vec<String> {
        let Ok(indexed) = self.by_name.read() else {
            return Vec::new();
        };
        let prefix = prefix.to_lowercase();
        let mut found: Vec<String> = indexed
            .types
            .keys()
            .filter(|name| name.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();
        found.sort();
        found
    }

    /// Whether some library file is in `package` or in a package below it.
    pub fn has_package(&self, package: &str) -> bool {
        let Ok(indexed) = self.by_name.read() else {
            return false;
        };
        let below = format!("{package}.");
        indexed
            .types
            .values()
            .flatten()
            .any(|file| file.package == package || file.package.starts_with(&below))
    }

    /// The simple names of the types the files of `package` declare.
    pub fn types_in_package(&self, package: &str) -> Vec<String> {
        let Ok(indexed) = self.by_name.read() else {
            return Vec::new();
        };
        let mut found: Vec<String> = indexed
            .types
            .iter()
            .filter(|(_, files)| files.iter().any(|file| file.package == package))
            .map(|(name, _)| name.clone())
            .collect();
        found.sort();
        found
    }

    /// The top-level functions whose key ([`super::SourceLayout::library_functions`])
    /// starts with `prefix`, each with the files declaring it.
    pub fn functions_starting(&self, prefix: &str) -> Vec<(String, Vec<LibraryFile>)> {
        let Ok(indexed) = self.by_name.read() else {
            return Vec::new();
        };
        let mut found: Vec<(String, Vec<LibraryFile>)> = indexed
            .functions
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, files)| (key.clone(), files.clone()))
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found
    }

    /// A library file extracted into the cache and parsed, once.
    pub fn load(&self, file: &LibraryFile) -> Option<Arc<Loaded>> {
        if let Some(loaded) = self.loaded.get(file) {
            return Some(loaded.clone());
        }
        let text = self.read_entry(&file.jar, &file.entry)?;
        let path = self.extracted_path(&file.jar, &file.entry)?;
        if is_stale(&path, &file.jar, &text) {
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::write(&path, &text).ok()?;
        }
        let uri = Url::from_file_path(&path).ok()?;
        let tree = syntax(file.language).tree(&text);
        let loaded = Arc::new(Loaded {
            uri,
            language: file.language,
            supertypes: super::type_names(&tree, true),
            text: Arc::new(text),
            tree: Arc::new(tree),
        });
        self.loaded.insert(file.clone(), loaded.clone());
        Some(loaded)
    }
}

/// Files that declare no type a name can stand for.
const NOT_DECLARING: &[&str] = &["module-info", "package-info"];

/// Every Java/Kotlin file of a jar, by the types and functions it declares.
/// The entries of a Maven sources jar (`x-1.0-sources.jar`) its jar
/// (`x-1.0.jar`, beside it) ships no class of: a Java file without its
/// `Name.class` (a source-only annotation a library drops when it shades),
/// any file of a package without a class (Lombok keeps its own as
/// `.SCL.lombok` resources). They are sources of nothing on the classpath.
/// Nothing where there is no such jar, or where no package is in both (a
/// sources jar laid out another way). Sorted.
fn unshipped_entries(sources: &Path) -> Vec<String> {
    let name = sources.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(stem) = name.strip_suffix("-sources.jar") else {
        return Vec::new();
    };
    let jar = sources.with_file_name(format!("{stem}.jar"));
    let names = |archive: &Path| -> Option<Vec<String>> {
        let file = std::fs::File::open(archive).ok()?;
        let zip = zip::ZipArchive::new(file).ok()?;
        Some(zip.file_names().map(str::to_owned).collect())
    };
    let (Some(binary), Some(source_entries)) = (names(&jar), names(sources)) else {
        return Vec::new();
    };
    let directory = |entry: &str| entry.rsplit_once('/').map_or("", |(directory, _)| directory).to_owned();
    let classes: std::collections::HashSet<&str> = binary
        .iter()
        .filter_map(|entry| entry.strip_suffix(".class"))
        .collect();
    let class_packages: std::collections::HashSet<String> = classes.iter().map(|class| directory(class)).collect();
    let source_entries: Vec<&String> = source_entries
        .iter()
        .filter(|entry| Language::of_path(entry).is_some())
        .collect();
    if !source_entries.iter().any(|entry| class_packages.contains(&directory(entry))) {
        return Vec::new();
    }
    let mut unshipped: Vec<String> = source_entries
        .into_iter()
        .filter(|entry| {
            if !class_packages.contains(&directory(entry)) {
                return true;
            }
            let Some(class) = entry.strip_suffix(".java") else {
                return false;
            };
            let stem = Path::new(class).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            // `package-info.java` and `module-info.java` compile to
            // classes of those names, where they compile to any.
            !classes.contains(class) && stem != "package-info" && stem != "module-info"
        })
        .cloned()
        .collect();
    unshipped.sort();
    unshipped
}

fn list_jar(jar: &Path, listing: &mut Listing) -> Result<(), String> {
    let unshipped = unshipped_entries(jar);
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name().to_owned();
        let Some(language) = Language::of_path(&name) else {
            continue;
        };
        if unshipped.binary_search(&name).is_ok() {
            continue;
        }
        let stem = Path::new(&name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if NOT_DECLARING.contains(&stem.as_str()) {
            continue;
        }
        let layout = layout(language);
        let mut text = String::new();
        if layout.reads_library_text() && entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let file = LibraryFile {
            jar: jar.to_path_buf(),
            entry: name.clone(),
            language,
            package: layout.library_package(&name, &text),
            rank: rank(&name),
        };
        for declared in layout.library_types(&stem, &text) {
            listing.types.entry(declared).or_default().push(file.clone());
        }
        for declared in layout.library_functions(&text) {
            let files = listing.functions.entry(declared).or_default();
            if !files.contains(&file) {
                files.push(file.clone());
            }
        }
    }
    Ok(())
}

/// The path of an entry below its module directory: the JDK's `src.zip`
/// keeps `java/lang/String.java` under `java.base/`.
pub fn below_module(entry: &str) -> (Option<&str>, &str) {
    match entry.split_once('/') {
        Some((first, rest)) if first.contains('.') => (Some(first), rest),
        _ => (None, entry),
    }
}

/// Which of the files declaring the same simple name is read first, lower
/// first: the platform's public API (`java.util.List`, `kotlin.String`)
/// before the rest of a library, the JDK's base module before its desktop
/// ones (`java.awt.List`), the JVM's `actual` declarations of the Kotlin
/// standard library before their common `expect`s, and internals last.
fn rank(entry: &str) -> u8 {
    let (module, path) = below_module(entry);
    let internal = ["sun/", "com/sun/", "jdk/", "kotlin/internal/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || path.contains("/internal/")
        || path.contains("/impl/");
    if internal {
        return 9;
    }
    let source_set = path.split_once('/').map(|(first, _)| first);
    let in_source_set = source_set.is_some_and(|set| set.ends_with("Main"));
    let path = if in_source_set {
        path.split_once('/').map_or(path, |(_, rest)| rest)
    } else {
        path
    };
    let platform = ["java/", "javax/", "kotlin/"]
        .iter()
        .any(|prefix| path.starts_with(prefix));
    let mut rank = if platform { 0 } else { 2 };
    if module.is_some_and(|module| module != "java.base") {
        rank += 1;
    }
    if source_set.is_some_and(|set| set != "jvmMain" && in_source_set) {
        rank += 1;
    }
    rank
}

/// Where the decompiled class of the stub `entry` of `jar` is kept, where
/// `jar` is a stubs jar.
fn decompiled_class(jar: &Path, entry: &str) -> Option<PathBuf> {
    use crate::dependency_sync::decompile;
    decompile::is_stubs_jar(jar).then(|| decompile::decompiled_class_path(jar, entry)).flatten()
}

/// Whether the extracted copy at `path` is to be written: it is missing,
/// or, for a stubs jar, whose files turn from stub to decompiled class,
/// it holds other text.
fn is_stale(path: &Path, jar: &Path, text: &str) -> bool {
    if !path.is_file() {
        return true;
    }
    crate::dependency_sync::decompile::is_stubs_jar(jar)
        && std::fs::read_to_string(path).is_ok_and(|written| written != text)
}

impl Libraries {
    /// An entry's text, through the jar opened once: for a stub whose
    /// class was decompiled, the decompiled class.
    fn read_entry(&self, jar: &Path, entry: &str) -> Option<String> {
        if let Some(decompiled) = decompiled_class(jar, entry).filter(|path| path.is_file())
            && let Ok(text) = std::fs::read_to_string(decompiled)
        {
            return Some(text);
        }
        let archive = match self.archives.get(jar) {
            Some(archive) => archive.clone(),
            None => {
                let opened = zip::ZipArchive::new(std::fs::File::open(jar).ok()?).ok()?;
                let archive = Arc::new(std::sync::Mutex::new(opened));
                self.archives.insert(jar.to_path_buf(), archive.clone());
                archive
            }
        };
        let mut archive = archive.lock().ok()?;
        let mut found = archive.by_name(entry).ok()?;
        let mut text = String::new();
        found.read_to_string(&mut text).ok()?;
        Some(text)
    }
}


impl Libraries {
    /// Where a library file is extracted, so a client can open it:
    /// `<extracted>/<jar>-<fingerprint>/<entry>`.
    fn extracted_path(&self, jar: &Path, entry: &str) -> Option<PathBuf> {
        let stem = jar.file_stem()?.to_string_lossy().into_owned();
        let fingerprint = jar_fingerprint(jar)?;
        let relative: PathBuf = entry
            .split('/')
            .filter(|part| !part.is_empty() && *part != "..")
            .collect();
        Some(
            self.extracted
                .join(format!("{stem}-{fingerprint:016x}"))
                .join(relative),
        )
    }
}

/// What tells one version of a jar from another: its path, size and
/// modification time.
pub fn jar_fingerprint(jar: &Path) -> Option<u64> {
    let metadata = std::fs::metadata(jar).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    jar.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub fn jar(directory: &Path, files: &[(&str, &str)]) -> PathBuf {
        jar_named(directory, "greeter-1.0-sources.jar", files)
    }

    pub fn jar_named(directory: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let path = directory.join(name);
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, text) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn a_jar_is_listed_by_the_types_its_files_declare() {
        let directory = tempfile::tempdir().unwrap();
        let jar = jar(
            directory.path(),
            &[
                ("com/acme/Greeter.java", "package com.acme;\npublic class Greeter {}\n"),
                ("com/acme/Shapes.kt", "package com.acme\nclass Circle\nobject Registry\n"),
                ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n"),
            ],
        );
        let libraries = Libraries::default();
        assert_eq!(libraries.replace(&[jar]), 3);
        assert_eq!(libraries.declaring("Greeter")[0].entry, "com/acme/Greeter.java");
        assert_eq!(libraries.declaring("Registry")[0].language, Language::Kotlin);
        assert!(libraries.declaring("Shapes").is_empty());
    }

    #[test]
    fn the_platforms_public_api_is_read_first() {
        let directory = tempfile::tempdir().unwrap();
        let jdk = jar_named(
            directory.path(),
            "src.zip",
            &[
                ("jdk.compiler/com/sun/tools/javac/util/List.java", "package com.sun.tools.javac.util;\n"),
                ("java.desktop/java/awt/List.java", "package java.awt;\n"),
                ("java.base/java/util/List.java", "package java.util;\n"),
                ("java.base/module-info.java", "module java.base {}\n"),
                ("java.base/java/util/package-info.java", "package java.util;\n"),
            ],
        );
        let stdlib = jar_named(
            directory.path(),
            "kotlin-stdlib-2.2.20-sources.jar",
            &[
                ("commonMain/kotlin/String.kt", "package kotlin\npublic expect class String\n"),
                ("jvmMain/kotlin/String.kt", "package kotlin\npublic actual class String\n"),
                (
                    "commonMain/generated/_Collections.kt",
                    "package kotlin.collections\npublic fun <T> List<T>.first(): T = this[0]\n\
                     internal fun <T> List<T>.secret(): T = this[0]\n",
                ),
            ],
        );
        let libraries = Libraries::default();
        libraries.replace(&[jdk, stdlib]);
        let lists: Vec<(String, String)> = libraries
            .declaring("List")
            .into_iter()
            .map(|file| (file.entry, file.package))
            .collect();
        assert_eq!(lists[0], ("java.base/java/util/List.java".to_owned(), "java.util".to_owned()));
        assert_eq!(lists[1].1, "java.awt");
        assert_eq!(lists[2].1, "com.sun.tools.javac.util");
        assert!(libraries.declaring("module-info").is_empty());
        assert!(libraries.declaring("package-info").is_empty());
        assert_eq!(libraries.declaring("String")[0].entry, "jvmMain/kotlin/String.kt");
        let first = libraries.declaring_function("List.first");
        assert_eq!(first[0].package, "kotlin.collections");
        assert!(libraries.declaring_function("List.secret").is_empty());
        let started: Vec<String> = libraries
            .functions_starting("List.fi")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(started, vec!["List.first"]);
    }
}

#[cfg(test)]
mod anonymous_tests {
    #[test]
    fn an_anonymous_class_name_is_told_apart() {
        assert!(super::is_anonymous("StringModel$1"));
        assert!(super::is_anonymous("Outer$Inner$12"));
        assert!(!super::is_anonymous("StringModel"));
        assert!(!super::is_anonymous("Flags$StringJoiner"));
        assert!(!super::is_anonymous("Price$"));
    }
}

#[cfg(test)]
mod unshipped_tests {
    use std::io::Write;

    fn zip_of(path: &std::path::Path, entries: &[&str]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for entry in entries {
            zip.start_file(*entry, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_source_its_jar_has_no_class_of_is_unshipped() {
        let directory = tempfile::tempdir().unwrap();
        let jar = directory.path().join("lombok-1.0.jar");
        let sources = directory.path().join("lombok-1.0-sources.jar");
        zip_of(&jar, &["lombok/Data.class", "lombok/launch/Main.class", "lombok/launch/Main$1.class", "lombok/core/Agent.SCL.lombok"]);
        zip_of(
            &sources,
            &["lombok/Data.java", "lombok/launch/Main.java", "lombok/launch/Dropped.java", "lombok/launch/package-info.java", "lombok/core/Agent.java", "lombok/core/Util.kt"],
        );
        assert_eq!(
            super::unshipped_entries(&sources),
            vec!["lombok/core/Agent.java".to_owned(), "lombok/core/Util.kt".to_owned(), "lombok/launch/Dropped.java".to_owned()]
        );
    }

    #[test]
    fn a_sources_jar_laid_out_otherwise_is_left_whole() {
        let directory = tempfile::tempdir().unwrap();
        let jar = directory.path().join("odd-1.0.jar");
        let sources = directory.path().join("odd-1.0-sources.jar");
        zip_of(&jar, &["a/A.class"]);
        zip_of(&sources, &["src/main/java/a/A.java"]);
        assert!(super::unshipped_entries(&sources).is_empty());
        let alone = directory.path().join("alone-1.0-sources.jar");
        zip_of(&alone, &["a/A.java"]);
        assert!(super::unshipped_entries(&alone).is_empty());
    }
}
