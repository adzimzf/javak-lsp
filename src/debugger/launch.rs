//! Launch-configuration answers jdtls gives from its Eclipse projects:
//! main classes, the runtime classpath and the `java` to run them with.
//!
//! A "project" here is a build module: the nearest directory holding a
//! Gradle or Maven build file. Its name is the module's path relative to the
//! workspace root (the root module takes the root directory's name), which is
//! what jdtls' `projectName` round-trips through launch configurations.

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;

use super::jvm_names::{main_classes, MainClass};
use crate::indexer::Indexer;

const BUILD_FILES: &[&str] = &[
    "build.gradle.kts",
    "build.gradle",
    "pom.xml",
    "settings.gradle.kts",
    "settings.gradle",
];

/// Directories never holding a build module of their own.
const SKIPPED_DIRECTORIES: &[&str] = &["build", "target", "node_modules", "out", "bin"];

/// How deep below the workspace root to look for build modules.
const MODULE_SEARCH_DEPTH: usize = 6;

/// Which compiled classes a launch needs, as jdtls' `resolveClasspath` scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClasspathScope {
    /// Main source set only: running an application.
    Runtime,
    /// Main and test source sets: running a test class.
    Test,
}

impl ClasspathScope {
    pub(crate) fn parse(scope: Option<&str>) -> Self {
        match scope {
            Some("test") => Self::Test,
            _ => Self::Runtime,
        }
    }
}

/// Every runnable class of the workspace's sources, optionally only those
/// under `under` (a file or directory).
pub(crate) fn resolve_main_classes(indexer: &Indexer, under: Option<&Path>) -> Vec<MainClass> {
    let root = indexer.workspace_root.get();
    let mut found: Vec<MainClass> = Vec::new();
    for file in indexer.workspace_files() {
        let Ok(path) = file.uri.to_file_path() else {
            continue;
        };
        if under.is_some_and(|parent| !path.starts_with(parent)) {
            continue;
        }
        let path_text = path.to_string_lossy().into_owned();
        for main_class in main_classes(file.language, &path_text, &file.text, &file.tree) {
            let project_name = match root {
                Some(root) => project_name(&module_dir_of(&path, root), root),
                None => String::new(),
            };
            found.push(MainClass {
                main_class,
                project_name,
                file_path: path_text.clone(),
            });
        }
    }
    found.sort_by(|left, right| left.main_class.cmp(&right.main_class));
    found.dedup();
    found
}

/// Classpath entries (directories and jars) to run `main_class`: the
/// module's own compiled classes first, then every other module's, then
/// the project's library jars.
pub(crate) fn resolve_classpath(
    indexer: &Indexer,
    main_class: &str,
    project_name: Option<&str>,
    scope: ClasspathScope,
) -> Vec<String> {
    let Some(root) = indexer.workspace_root.get() else {
        return Vec::new();
    };
    let own_module = project_name
        .filter(|name| !name.is_empty())
        .and_then(|name| module_dir_for_project(name, root))
        .or_else(|| {
            resolve_main_classes(indexer, None)
                .into_iter()
                .find(|candidate| candidate.main_class == main_class)
                .map(|candidate| module_dir_of(Path::new(&candidate.file_path), root))
        });

    let mut classpath: Vec<PathBuf> = Vec::new();
    if let Some(module) = &own_module {
        classpath.extend(output_dirs(module, scope));
    }
    for module in module_dirs(root) {
        if Some(&module) != own_module.as_ref() {
            classpath.extend(output_dirs(&module, ClasspathScope::Runtime));
        }
    }
    classpath.extend(
        crate::dependency_sync::load_manifest(root)
            .map(|manifest| manifest.jars())
            .unwrap_or_default(),
    );

    let mut unique: Vec<String> = Vec::with_capacity(classpath.len());
    for entry in classpath {
        let text = entry.to_string_lossy().into_owned();
        if !unique.contains(&text) {
            unique.push(text);
        }
    }
    unique
}

/// The compiled-class directories a Gradle (JVM, Kotlin `jvm()` target,
/// Android) or Maven module writes, the ones that exist.
pub(crate) fn output_dirs(module: &Path, scope: ClasspathScope) -> Vec<PathBuf> {
    let mut candidates: Vec<&str> = vec![
        "build/classes/java/main",
        "build/classes/kotlin/main",
        "build/classes/kotlin/jvm/main",
        "build/classes/java/jvm/main",
        "build/resources/main",
        "build/processedResources/jvm/main",
        "build/tmp/kotlin-classes/debug",
        "build/intermediates/javac/debug/classes",
        "target/classes",
    ];
    if scope == ClasspathScope::Test {
        candidates.extend([
            "build/classes/java/test",
            "build/classes/kotlin/test",
            "build/classes/kotlin/jvm/test",
            "build/resources/test",
            "build/processedResources/jvm/test",
            "build/tmp/kotlin-classes/debugUnitTest",
            "target/test-classes",
        ]);
    }
    candidates
        .into_iter()
        .map(|relative| module.join(relative))
        .filter(|directory| directory.is_dir())
        .collect()
}

/// The build module a source file belongs to: its nearest directory (not
/// above `root`) holding a build file, else `root`.
pub(crate) fn module_dir_of(file: &Path, root: &Path) -> PathBuf {
    let mut current = file.parent();
    while let Some(directory) = current {
        if !directory.starts_with(root) {
            break;
        }
        if has_build_file(directory) {
            return directory.to_path_buf();
        }
        current = directory.parent();
    }
    root.to_path_buf()
}

/// The `projectName` of a module: its path relative to `root`, or the root
/// directory's own name for the root module.
pub(crate) fn project_name(module: &Path, root: &Path) -> String {
    match module.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => {
            relative.to_string_lossy().replace('\\', "/")
        }
        _ => root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn module_dir_for_project(project_name: &str, root: &Path) -> Option<PathBuf> {
    let is_root_name = root
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == project_name);
    if is_root_name {
        return Some(root.to_path_buf());
    }
    // Gradle's `:feature:login` names are taken as well as paths.
    let relative = project_name.trim_start_matches(':').replace(':', "/");
    let candidate = root.join(relative);
    candidate.is_dir().then_some(candidate)
}

/// Every build module under `root`, `root` itself included when it has a
/// build file.
pub(crate) fn module_dirs(root: &Path) -> Vec<PathBuf> {
    let mut modules = Vec::new();
    collect_module_dirs(root, 0, &mut modules);
    modules
}

fn collect_module_dirs(directory: &Path, depth: usize, modules: &mut Vec<PathBuf>) {
    if has_build_file(directory) {
        modules.push(directory.to_path_buf());
    }
    if depth >= MODULE_SEARCH_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !name.starts_with('.') && !SKIPPED_DIRECTORIES.contains(&name.as_str())
        })
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        collect_module_dirs(&child, depth + 1, modules);
    }
}

fn has_build_file(directory: &Path) -> bool {
    BUILD_FILES
        .iter()
        .any(|build_file| directory.join(build_file).is_file())
}

/// The `java` launcher to run the debug server and the debuggee with: the
/// client's `javaHome`, then `$JAVA_HOME`, then the one the dependency sync
/// would run (the `PATH`, the login shell's JDK).
pub(crate) fn java_executable(configured_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(java) = configured_home.and_then(java_in_home) {
        return Some(java);
    }
    let from_environment = std::env::var_os("JAVA_HOME")
        .filter(|home| !home.is_empty())
        .and_then(|home| java_in_home(Path::new(&home)));
    from_environment.or_else(crate::dependency_sync::decompile::java_executable)
}

fn java_in_home(home: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "java.exe" } else { "java" };
    let java = home.join("bin").join(name);
    java.is_file().then_some(java)
}

/// The local path behind a `file://` URI.
pub(crate) fn file_path_of(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
