//! The sources of the project's libraries: the `-sources.jar`s (or the
//! decompiled ones) dependency sync resolves.
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
use crate::language::Language;
use crate::parser::{syntax, Syn};

/// One source file of a library jar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LibraryFile {
    pub jar: PathBuf,
    /// The file's path inside the jar.
    pub entry: String,
    pub language: Language,
}

/// A library file, extracted and parsed.
pub struct Loaded {
    pub uri: Url,
    pub language: Language,
    pub text: Arc<String>,
    pub tree: Arc<Syn>,
}

pub struct Libraries {
    /// Simple type name → the library files declaring a type of that name.
    by_type: RwLock<HashMap<String, Vec<LibraryFile>>>,
    loaded: DashMap<LibraryFile, Arc<Loaded>>,
    /// Where library files are extracted.
    extracted: PathBuf,
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
            by_type: RwLock::default(),
            loaded: DashMap::new(),
            extracted: directory,
        }
    }

    /// Index these sources jars, replacing what was indexed before.
    pub fn replace(&self, jars: &[PathBuf]) -> usize {
        let mut by_type: HashMap<String, Vec<LibraryFile>> = HashMap::new();
        for jar in jars {
            if let Err(error) = list_jar(jar, &mut by_type) {
                log::warn!("libraries: cannot read {}: {error}", jar.display());
            }
        }
        let count = by_type.values().map(Vec::len).sum();
        if let Ok(mut indexed) = self.by_type.write() {
            *indexed = by_type;
        }
        self.loaded.clear();
        count
    }

    /// The library files that declare a type called `name`.
    pub fn declaring(&self, name: &str) -> Vec<LibraryFile> {
        self.by_type
            .read()
            .ok()
            .and_then(|indexed| indexed.get(name).cloned())
            .unwrap_or_default()
    }

    /// A library file extracted into the cache and parsed, once.
    pub fn load(&self, file: &LibraryFile) -> Option<Arc<Loaded>> {
        if let Some(loaded) = self.loaded.get(file) {
            return Some(loaded.clone());
        }
        let text = read_entry(&file.jar, &file.entry).ok()?;
        let path = self.extracted_path(&file.jar, &file.entry)?;
        if !path.is_file() {
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::write(&path, &text).ok()?;
        }
        let uri = Url::from_file_path(&path).ok()?;
        let tree = syntax(file.language).tree(&text);
        let loaded = Arc::new(Loaded {
            uri,
            language: file.language,
            text: Arc::new(text),
            tree: Arc::new(tree),
        });
        self.loaded.insert(file.clone(), loaded.clone());
        Some(loaded)
    }
}

/// Every Java/Kotlin file of a jar, by the types it declares.
fn list_jar(jar: &Path, by_type: &mut HashMap<String, Vec<LibraryFile>>) -> Result<(), String> {
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
        let stem = Path::new(&name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let layout = layout(language);
        let mut text = String::new();
        if layout.reads_library_text() && entry.read_to_string(&mut text).is_err() {
            continue;
        }
        for declared in layout.library_types(&stem, &text) {
            by_type.entry(declared).or_default().push(LibraryFile {
                jar: jar.to_path_buf(),
                entry: name.clone(),
                language,
            });
        }
    }
    Ok(())
}

fn read_entry(jar: &Path, entry: &str) -> Result<String, String> {
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut found = archive.by_name(entry).map_err(|error| error.to_string())?;
    let mut text = String::new();
    found
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    Ok(text)
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
pub(super) mod tests {
    use super::*;
    use std::io::Write;

    pub fn jar(directory: &Path, files: &[(&str, &str)]) -> PathBuf {
        let path = directory.join("greeter-1.0-sources.jar");
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
}
