//! The projects of a workspace, found the way jdtls imports them: every
//! Maven and Gradle build in the workspace, each of its modules, and each
//! module's source roots and Java level.
//!
//! jdtls looks for build files up to five directories down, and a build it
//! finds is imported whole, through the build tool: the modules a Maven
//! `pom.xml` lists, the projects a Gradle `settings.gradle` includes. A
//! module's files are compiled from its own source roots (`src/main/java`
//! unless its build says otherwise) at its own Java level.
//!
//! What the build files say is read here without running the build: the
//! `<modules>`, `<sourceDirectory>` and compiler levels of a `pom.xml`, the
//! `include`, `includeFlat`, `includeBuild` and `projectDir` of a Gradle
//! settings file. Where Gradle itself has said where each project keeps
//! its sources (the dependency sync asks it, [`ReportedModule`]), that
//! replaces what the files alone suggest.
//!
//! A workspace with no build file at all is one module at its root, with
//! the default source roots.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::dependency_sync::{declared_java_version, ReportedModule};
use crate::language::Language;

use super::layout;

/// How far below the workspace jdtls looks for a build file.
const SEARCH_DEPTH: usize = 5;

/// Directories never searched for build files: version control, IDE state,
/// build output, and jdtls's default `java.import.exclusions`.
const NOT_PROJECTS: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".kotlin",
    ".metadata",
    "build",
    "target",
    "out",
    "node_modules",
    "archetype-resources",
];

const POM: &str = "pom.xml";
const GRADLE_SETTINGS: &[&str] = &["settings.gradle.kts", "settings.gradle"];
const GRADLE_BUILDS: &[&str] = &["build.gradle.kts", "build.gradle"];

/// The modules of a workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projects {
    pub modules: Vec<Module>,
}

/// One module: a directory its build compiles as a unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub dir: PathBuf,
    pub roots: Vec<SourceRoot>,
    /// The Java source level its build names, or the one it inherits.
    pub java_level: Option<u32>,
}

/// A directory a module's packages start at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    pub dir: PathBuf,
    /// Only Kotlin is compiled from it (`src/main/kotlin`): a Java file
    /// there is in no package.
    pub kotlin_only: bool,
}

impl Projects {
    /// Every module of every build below `workspace`; one module at the
    /// workspace itself when there is no build.
    pub fn discover(workspace: &Path) -> Self {
        let mut projects = Self::default();
        let mut seen = HashSet::new();
        let (maven, gradle) = build_roots(workspace);
        for root in gradle {
            projects.gradle_build(&root, workspace, &mut seen);
        }
        for root in maven {
            projects.maven_module(&root, None, &mut seen);
        }
        if projects.modules.is_empty() {
            projects = Self::single(workspace);
            projects.modules[0].java_level = level_of(workspace);
        }
        projects
    }

    /// The workspace as one module at its root, with the default source
    /// roots: a workspace without a build.
    pub fn single(workspace: &Path) -> Self {
        Self {
            modules: vec![Module {
                dir: workspace.to_path_buf(),
                roots: default_roots(workspace, None, None),
                java_level: None,
            }],
        }
    }

    /// The module whose directory holds `path`: the innermost one, since a
    /// Maven parent's directory holds its modules'.
    pub fn module_of(&self, path: &Path) -> Option<&Module> {
        self.modules
            .iter()
            .filter(|module| path.starts_with(&module.dir))
            .max_by_key(|module| module.dir.components().count())
    }

    /// The source root `path` is in, for a file of `language`.
    pub fn source_root_of(&self, path: &Path, language: Language) -> Option<&Path> {
        self.module_of(path)?
            .roots
            .iter()
            .filter(|root| !(root.kotlin_only && language == Language::Java))
            .filter(|root| path.starts_with(&root.dir))
            .max_by_key(|root| root.dir.components().count())
            .map(|root| root.dir.as_path())
    }

    /// The Java level of the module `path` is in.
    pub fn java_level_of(&self, path: &Path) -> Option<u32> {
        self.module_of(path)?.java_level
    }

    /// What Gradle said of its projects: each one's source directories
    /// replace those its files suggested, and a project the settings file
    /// hid from the reading (one a plugin adds) joins the modules.
    pub(crate) fn apply_reported(&mut self, reported: &[ReportedModule]) {
        for project in reported {
            let roots: Vec<SourceRoot> = project
                .java
                .iter()
                .map(|dir| SourceRoot {
                    dir: dir.clone(),
                    kotlin_only: false,
                })
                .chain(
                    project
                        .kotlin
                        .iter()
                        .filter(|dir| !project.java.contains(dir))
                        .map(|dir| SourceRoot {
                            dir: dir.clone(),
                            kotlin_only: true,
                        }),
                )
                .collect();
            let level = project
                .java_level
                .as_deref()
                .and_then(|level| level.parse().ok());
            match self
                .modules
                .iter_mut()
                .find(|module| module.dir == project.dir)
            {
                Some(module) => {
                    if !roots.is_empty() {
                        module.roots = roots;
                    }
                    module.java_level = level.or(module.java_level);
                }
                None => self.modules.push(Module {
                    dir: project.dir.clone(),
                    roots,
                    java_level: level,
                }),
            }
        }
    }

    /// A Maven module and the modules it lists, each inheriting the Java
    /// level of the one that lists it unless it names its own.
    fn maven_module(&mut self, dir: &Path, parent_level: Option<u32>, seen: &mut HashSet<PathBuf>) {
        let dir = normalized(dir);
        if !seen.insert(dir.clone()) {
            return;
        }
        let Ok(pom) = std::fs::read_to_string(dir.join(POM)) else {
            return;
        };
        let build = xml_block(&pom, "build").unwrap_or_default();
        let main = xml_value(&build, "sourceDirectory").map(|path| maven_path(&dir, &path));
        let test = xml_value(&build, "testSourceDirectory").map(|path| maven_path(&dir, &path));
        let java_level = level_of(&dir).or(parent_level);
        self.modules.push(Module {
            roots: default_roots(&dir, main, test),
            dir: dir.clone(),
            java_level,
        });
        // Every `<module>`, those of profiles included: jdtls imports what
        // any profile may build.
        for module in xml_values(&pom, "module") {
            let path = maven_path(&dir, &module);
            let module_dir = if path.extension().is_some_and(|extension| extension == "xml") {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            } else {
                path
            };
            self.maven_module(&module_dir, java_level, seen);
        }
    }

    /// A Gradle build: its root project, every project its settings file
    /// includes, and every build it includes.
    fn gradle_build(&mut self, root: &Path, workspace: &Path, seen: &mut HashSet<PathBuf>) {
        let root = normalized(root);
        if !seen.insert(root.clone()) {
            return;
        }
        let root_level = level_of(&root);
        let settings = GRADLE_SETTINGS
            .iter()
            .find_map(|name| std::fs::read_to_string(root.join(name)).ok())
            .unwrap_or_default();
        let settings = settings_without_comments(&settings);
        let mut dirs = vec![root.clone()];
        let moved = moved_projects(&settings);
        for project in included(&settings, "include") {
            let dir = moved.iter().find(|(name, _)| *name == project).map_or_else(
                || root.join(project_path(&project)),
                |(_, dir)| root.join(dir),
            );
            dirs.push(dir);
        }
        for project in included(&settings, "includeFlat") {
            dirs.push(root.join("..").join(project_path(&project)));
        }
        for dir in dirs {
            let dir = normalized(&dir);
            if dir != root && !seen.insert(dir.clone()) {
                continue;
            }
            self.modules.push(Module {
                roots: default_roots(&dir, None, None),
                java_level: level_of(&dir).or(root_level),
                dir,
            });
        }
        for build in included(&settings, "includeBuild") {
            let included = normalized(&root.join(&build));
            if included.starts_with(workspace) {
                self.gradle_build(&included, workspace, seen);
            }
        }
    }
}

/// The top-most directories holding a `pom.xml`, and those holding a
/// Gradle settings or build file, searched separately as jdtls's Maven and
/// Gradle importers search: once a build is found, the directories below
/// it are its own to describe. A directory with both is Gradle's.
fn build_roots(workspace: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut maven = Vec::new();
    let mut gradle = Vec::new();
    find_roots(workspace, 0, &mut |dir| is_gradle(dir), &mut gradle);
    find_roots(
        workspace,
        0,
        &mut |dir| dir.join(POM).is_file() && !gradle.iter().any(|root| dir.starts_with(root)),
        &mut maven,
    );
    (maven, gradle)
}

fn is_gradle(dir: &Path) -> bool {
    GRADLE_SETTINGS
        .iter()
        .chain(GRADLE_BUILDS)
        .any(|name| dir.join(name).is_file())
}

fn find_roots(
    dir: &Path,
    depth: usize,
    is_root: &mut impl FnMut(&Path) -> bool,
    found: &mut Vec<PathBuf>,
) {
    if is_root(dir) {
        found.push(dir.to_path_buf());
        return;
    }
    if depth >= SEARCH_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !NOT_PROJECTS.contains(&name.as_ref())
        })
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        find_roots(&child, depth + 1, is_root, found);
    }
}

/// The source roots of a module at `dir`: its language layouts' defaults,
/// with a Maven build's own main and test directories in place of
/// `src/main/java` and `src/test/java`.
fn default_roots(dir: &Path, main: Option<PathBuf>, test: Option<PathBuf>) -> Vec<SourceRoot> {
    let java = layout(Language::Java).source_roots();
    let mut roots: Vec<SourceRoot> = Vec::new();
    for language in [Language::Java, Language::Kotlin] {
        for root in layout(language).source_roots() {
            let replaced = match *root {
                "src/main/java" => main.clone(),
                "src/test/java" => test.clone(),
                _ => None,
            };
            let root_dir = replaced.unwrap_or_else(|| dir.join(root));
            if roots.iter().any(|known| known.dir == root_dir) {
                continue;
            }
            roots.push(SourceRoot {
                dir: root_dir,
                kotlin_only: !java.contains(root),
            });
        }
    }
    roots
}

fn level_of(dir: &Path) -> Option<u32> {
    declared_java_version(dir).and_then(|level| level.parse().ok())
}

/// A path a `pom.xml` names, below the module's directory.
fn maven_path(dir: &Path, written: &str) -> PathBuf {
    let written = written.trim();
    let relative = ["${project.basedir}", "${basedir}"]
        .iter()
        .find_map(|prefix| written.strip_prefix(prefix))
        .map_or(written, |rest| rest.trim_start_matches(['/', '\\']));
    normalized(&dir.join(relative))
}

/// `dir` with its `.` and `..` resolved, without asking the disk.
fn normalized(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// The text of the first `<tag>…</tag>` of `text`.
fn xml_block(text: &str, tag: &str) -> Option<String> {
    xml_values(text, tag).into_iter().next()
}

fn xml_value(text: &str, tag: &str) -> Option<String> {
    xml_block(text, tag).filter(|value| !value.is_empty())
}

/// The text of every `<tag>…</tag>` of `text`, outside XML comments.
fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let text = without_xml_comments(text);
    text.split(&format!("<{tag}>"))
        .skip(1)
        .filter_map(|rest| rest.split_once(&format!("</{tag}>")))
        .map(|(value, _)| value.trim().to_owned())
        .collect()
}

fn without_xml_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        kept.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    kept.push_str(rest);
    kept
}

/// A Gradle settings file without its `//` and `/* */` comments.
fn settings_without_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(character) = characters.next() {
        if let Some(open) = quote {
            kept.push(character);
            if character == '\\' {
                kept.extend(characters.next());
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match (character, characters.peek()) {
            ('"' | '\'', _) => {
                quote = Some(character);
                kept.push(character);
            }
            ('/', Some('/')) => {
                while characters.peek().is_some_and(|next| *next != '\n') {
                    characters.next();
                }
            }
            ('/', Some('*')) => {
                characters.next();
                let mut previous = ' ';
                for next in characters.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            _ => kept.push(character),
        }
    }
    kept
}

/// The names one kind of statement includes: `include(":app", ":lib")`,
/// `include ':app', ':lib'` and the same spread over lines.
fn included(settings: &str, keyword: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(found) = settings[from..].find(keyword) {
        let start = from + found;
        let end = start + keyword.len();
        from = end;
        let before = settings[..start].chars().next_back();
        let after = settings[end..].chars().next();
        let stands_alone = !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && after.is_some_and(|c| c == '(' || c.is_whitespace());
        if !stands_alone {
            continue;
        }
        let rest = settings[end..].trim_start();
        let statement = if rest.starts_with('(') {
            rest.find(')').map_or(rest, |close| &rest[..close])
        } else {
            statement_on_its_lines(rest)
        };
        names.extend(string_literals(statement));
    }
    names
}

/// The text of a statement without parentheses: its line, and the lines
/// after it while each ends with a `,`.
fn statement_on_its_lines(rest: &str) -> &str {
    let mut end = 0;
    for line in rest.split_inclusive('\n') {
        end += line.len();
        if !line.trim_end().ends_with(',') {
            break;
        }
    }
    &rest[..end]
}

fn string_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut characters = text.char_indices();
    while let Some((start, character)) = characters.next() {
        if character != '"' && character != '\'' {
            continue;
        }
        let close = text[start + 1..].find(character);
        let Some(close) = close else {
            break;
        };
        literals.push(text[start + 1..start + 1 + close].to_owned());
        for _ in 0..close + 1 {
            characters.next();
        }
    }
    literals
}

/// `project(":lib").projectDir = file("libs/lib")`: the projects the
/// settings file moves, and where to.
fn moved_projects(settings: &str) -> Vec<(String, String)> {
    let mut moved = Vec::new();
    for statement in settings.split(['\n', ';']) {
        let Some((target, value)) = statement.split_once("projectDir") else {
            continue;
        };
        let Some(project) = target
            .split_once("project(")
            .and_then(|(_, rest)| string_literals(rest).into_iter().next())
        else {
            continue;
        };
        if let Some(dir) = string_literals(value).into_iter().next() {
            moved.push((project, dir));
        }
    }
    moved
}

/// The directory of an included project, below its build's root:
/// `:services:api` is `services/api`.
fn project_path(name: &str) -> PathBuf {
    name.trim_start_matches(':').split(':').collect()
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
