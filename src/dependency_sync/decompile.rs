//! Decompile libraries that publish no sources jar — what IntelliJ does with
//! its bundled Fernflower — with Vineflower (the maintained Fernflower fork,
//! Apache-2.0), which turns Kotlin classes back into Kotlin and Java classes
//! into Java.
//!
//! The decompiler jar is fetched through the project's own build tool, so a
//! company mirror, proxy or credentials apply exactly as for any dependency:
//! the Gradle init script resolves it alongside the dependencies (see
//! `javak-lsp-dependencies.gradle`), Maven copies it with `dependency:copy`.
//! It runs on the user's own `java`.
//!
//! Each library is decompiled once per jar version into a cached
//! `…-decompiled-sources.jar`, which the JAR pipeline mounts like any
//! sources jar: go-to-definition, hover and find-references then work on
//! real code. A library that fails to decompile keeps the signature stub.
//!
//! A sources jar does not always hold every class of its jar: what a
//! library shades into its own jar (assertj's ByteBuddy, MapStruct's
//! FreeMarker, JUnit's univocity) is added at build time and has no sources
//! anywhere. jdtls reads those classes from the jar itself, as IntelliJ
//! does. The packages a sources jar has no file of are decompiled too, into
//! a `…-missing-sources.jar` mounted beside the real sources.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    installed_tool, launch, login_shell_environment, login_shell_path, run_tool, search_dirs,
    InstallLocations, ResolvedDependency,
};

/// The decompiler, as a Maven coordinate.
pub(crate) const DECOMPILER_COORDINATE: &str = "org.vineflower:vineflower:1.11.1";
const DECOMPILER_FILE: &str = "vineflower-1.11.1.jar";

/// One library is given at most this long to decompile.
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// At most this many libraries are decompiled per sync…
const MAX_DECOMPILED_JARS: usize = 100;
/// …each no bigger than this.
const MAX_JAR_BYTES: u64 = 64 * 1024 * 1024;
/// Libraries decompiled at the same time.
const PARALLEL_DECOMPILES: usize = 4;

pub(crate) const JAVA_INSTALL: InstallLocations = InstallLocations {
    home_variables: &["JAVA_HOME"],
    sdkman_candidate: "java",
    directories: &["/usr/lib/jvm/default-java/bin"],
};

fn decompiler_dir() -> PathBuf {
    crate::util::xdg_cache_base()
        .join("javak-lsp")
        .join("decompiler")
}

fn decompiled_dir() -> PathBuf {
    crate::util::xdg_cache_base()
        .join("javak-lsp")
        .join("decompiled")
}

/// The decompiler jar, when an earlier sync already fetched it.
pub(crate) fn cached_decompiler() -> Option<PathBuf> {
    let jar = decompiler_dir().join(DECOMPILER_FILE);
    jar.is_file().then_some(jar)
}

/// Keep the decompiler a build tool resolved (Gradle hands back a path in its
/// own cache) where the next sync finds it without asking again.
pub(crate) fn remember_decompiler(resolved: &Path) -> Option<PathBuf> {
    let destination = decompiler_dir().join(DECOMPILER_FILE);
    if destination.is_file() {
        return Some(destination);
    }
    std::fs::create_dir_all(decompiler_dir()).ok()?;
    let temporary = destination.with_extension("jar.tmp");
    std::fs::copy(resolved, &temporary).ok()?;
    std::fs::rename(&temporary, &destination).ok()?;
    Some(destination)
}

/// Fetch the decompiler with Maven (`dependency:copy`), through the
/// project's own settings — mirrors and credentials included.
pub(crate) fn fetch_decompiler_with_maven(root: &Path, launcher: &Path) -> Result<PathBuf, String> {
    let directory = decompiler_dir();
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let mut command = launch(launcher);
    command
        .current_dir(root)
        .args(["--batch-mode", "--quiet"])
        .arg(format!("{}:copy", super::maven::DEPENDENCY_PLUGIN))
        .arg(format!("-Dartifact={DECOMPILER_COORDINATE}"))
        .arg(format!("-DoutputDirectory={}", directory.display()));
    run_tool(
        command,
        "fetching the decompiler with Maven",
        super::SYNC_TIMEOUT,
    )
    .map_err(|failure| failure.message())?;
    cached_decompiler().ok_or_else(|| "Maven didn't copy the decompiler".to_owned())
}

/// The `java` to run the decompiler with: `$JAVA_HOME`, the login shell's
/// `JAVA_HOME`, then `java` on the server's or the login shell's `PATH`.
pub(crate) fn java_executable() -> Option<PathBuf> {
    let java_file = if cfg!(windows) { "java.exe" } else { "java" };
    let shell_java_home = login_shell_environment()
        .java_home
        .as_ref()
        .map(|home| PathBuf::from(home).join("bin").join(java_file))
        .filter(|java| java.is_file());
    installed_tool("java", &search_dirs(&JAVA_INSTALL))
        .or(shell_java_home)
        .or_else(|| installed_tool("java", login_shell_path()))
}

/// What of a dependency's jar is decompiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Missing {
    /// No sources jar: all of it.
    Everything,
    /// A sources jar without these packages (`org/assertj/core/internal`).
    Packages(Vec<String>),
}

/// What of `dependency` has no sources, where something has none.
pub(crate) fn missing_sources(dependency: &ResolvedDependency) -> Option<Missing> {
    let jar = dependency.jar.as_deref()?;
    let Some(sources) = dependency.sources.as_deref().filter(|sources| sources.is_file()) else {
        return Some(Missing::Everything);
    };
    let packages = missing_packages(jar, sources).ok()?;
    (!packages.is_empty()).then_some(Missing::Packages(packages))
}

/// The packages `jar` has classes in and `sources` has no source file of.
pub(crate) fn missing_packages(jar: &Path, sources: &Path) -> Result<Vec<String>, String> {
    let class_packages = entry_directories(jar, |entry| {
        entry.ends_with(".class")
            && !entry.starts_with("META-INF/")
            && !entry.ends_with("module-info.class")
            && !entry.ends_with("package-info.class")
    })?;
    let source_packages = entry_directories(sources, |entry| entry.ends_with(".java") || entry.ends_with(".kt"))?;
    Ok(class_packages
        .into_iter()
        .filter(|package| !source_packages.contains(package))
        .collect())
}

/// The directories of the entries of `archive` that `wanted` keeps,
/// sorted, each once.
fn entry_directories(archive: &Path, wanted: impl Fn(&str) -> bool) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(archive).map_err(|error| error.to_string())?;
    let zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut directories: Vec<String> = zip
        .file_names()
        .filter(|entry| wanted(entry))
        .map(|entry| entry.rsplit_once('/').map_or("", |(directory, _)| directory).to_owned())
        .collect();
    directories.sort();
    directories.dedup();
    Ok(directories)
}

/// Decompile what of every dependency has no sources — the whole jar of
/// one without a sources jar, the packages its sources jar lacks of
/// another — recording each result in `decompiled`. Failures are logged and
/// leave the dependency as it was.
pub(crate) fn decompile_missing_sources(
    dependencies: &mut [ResolvedDependency],
    java: &Path,
    decompiler: &Path,
) {
    let candidates: Vec<(usize, PathBuf, Missing)> = dependencies
        .iter()
        .enumerate()
        .filter_map(|(index, dependency)| {
            let jar = dependency.jar.clone()?;
            Some((index, jar, missing_sources(dependency)?))
        })
        .filter(|(_, jar, _)| is_decompilable(jar))
        .take(MAX_DECOMPILED_JARS)
        .collect();
    if candidates.is_empty() {
        return;
    }
    log::info!(
        "dependency sync: decompiling {} libraries without (all their) sources",
        candidates.len()
    );
    let chunk_size = candidates.len().div_ceil(PARALLEL_DECOMPILES).max(1);
    let results: Vec<(usize, PathBuf)> = std::thread::scope(|scope| {
        let workers: Vec<_> = candidates
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|(index, jar, missing)| match decompile_jar(java, decompiler, jar, missing) {
                            Ok(sources) => Some((*index, sources)),
                            Err(message) => {
                                log::warn!(
                                    "dependency sync: cannot decompile {}: {message}",
                                    jar.display()
                                );
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap_or_default())
            .collect()
    });
    for (index, sources) in results {
        dependencies[index].decompiled = Some(sources);
    }
}

/// A plain `.jar` (an `.aar` wraps its classes in another archive) small
/// enough to decompile.
fn is_decompilable(jar: &Path) -> bool {
    let is_jar = jar.extension().is_some_and(|extension| extension == "jar");
    let size_ok = std::fs::metadata(jar).is_ok_and(|metadata| metadata.len() <= MAX_JAR_BYTES);
    is_jar && size_ok
}

/// Where the decompiled sources of `jar` are cached: one file per jar
/// version, and for a jar with a sources jar one per set of packages it
/// lacks.
fn decompiled_jar_path(jar: &Path, missing: &Missing) -> Option<PathBuf> {
    let stem = jar.file_stem()?.to_string_lossy().into_owned();
    let fingerprint = crate::indexer::library::jar_fingerprint(jar)?;
    Some(match missing {
        Missing::Everything => decompiled_dir().join(format!("{stem}-{fingerprint:016x}-decompiled-sources.jar")),
        Missing::Packages(packages) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(packages, &mut hasher);
            let packages = std::hash::Hasher::finish(&hasher);
            decompiled_dir().join(format!("{stem}-{fingerprint:016x}-{packages:016x}-missing-sources.jar"))
        }
    })
}

/// Decompile what of `jar` is `missing` (once per jar version) into a
/// sources jar of `.kt` / `.java` files and return its path.
fn decompile_jar(java: &Path, decompiler: &Path, jar: &Path, missing: &Missing) -> Result<PathBuf, String> {
    let destination =
        decompiled_jar_path(jar, missing).ok_or_else(|| "cannot read the jar's metadata".to_owned())?;
    if destination.is_file() {
        return Ok(destination);
    }
    let work_dir = destination.with_extension("work");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir)
        .map_err(|error| format!("cannot create {}: {error}", work_dir.display()))?;
    let outcome = decompile_into(java, decompiler, jar, missing, &work_dir)
        .and_then(|()| package_sources(&work_dir.join("out"), &destination));
    let _ = std::fs::remove_dir_all(&work_dir);
    outcome.map(|()| destination)
}

fn decompile_into(java: &Path, decompiler: &Path, jar: &Path, missing: &Missing, work_dir: &Path) -> Result<(), String> {
    let out = work_dir.join("out");
    std::fs::create_dir_all(&out).map_err(|error| format!("cannot create {}: {error}", out.display()))?;
    let mut command = std::process::Command::new(java);
    command.arg("-jar").arg(decompiler).arg("-log=WARN");
    match missing {
        Missing::Everything => {
            command.arg(jar);
        }
        Missing::Packages(packages) => {
            // Only those packages' classes, with the whole jar beside them
            // as a library, so what they use of the rest is known.
            let part = work_dir.join("part.jar");
            copy_packages(jar, packages, &part)?;
            command.arg(format!("-e={}", jar.display())).arg(&part);
        }
    }
    command.arg(&out);
    run_tool(command, "the decompiler", DECOMPILE_TIMEOUT).map_err(|failure| failure.message())
}

/// A jar of the classes of `jar` that are in one of `packages`.
fn copy_packages(jar: &Path, packages: &[String], destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let output = std::fs::File::create(destination).map_err(|error| error.to_string())?;
    let mut zip = zip::ZipWriter::new(output);
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_owned();
        let directory = name.rsplit_once('/').map_or("", |(directory, _)| directory);
        if name.ends_with(".class") && packages.iter().any(|package| package == directory) {
            zip.raw_copy_file(entry).map_err(|error| error.to_string())?;
        }
    }
    zip.finish().map_err(|error| error.to_string())?;
    Ok(())
}

/// Zip every `.kt` / `.java` file under `directory` (at its package path)
/// into the sources jar `destination`.
pub(crate) fn package_sources(directory: &Path, destination: &Path) -> Result<(), String> {
    use std::io::Write;
    let mut files: Vec<PathBuf> = Vec::new();
    collect_sources(directory, &mut files);
    if files.is_empty() {
        return Err("the decompiler produced no sources".to_owned());
    }
    files.sort();
    let temporary = destination.with_extension("jar.tmp");
    let file = std::fs::File::create(&temporary)
        .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for path in &files {
        let entry = path
            .strip_prefix(directory)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        zip.start_file(entry, options)
            .map_err(|error| error.to_string())?;
        zip.write_all(&bytes).map_err(|error| error.to_string())?;
    }
    zip.finish().map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, destination).map_err(|error| error.to_string())
}

fn collect_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_sources(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "kt" || extension == "java")
        {
            files.push(path);
        }
    }
}
