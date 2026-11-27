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
//! In [`DecompileMode::OnDemand`] (the default) a library gets a
//! `…-stubs-N.jar` of its declarations read from the class files
//! ([`super::stubs`], no JVM), and [`decompile_class`] decompiles one class
//! when go to definition opens it. A Kotlin library, and every library in
//! [`DecompileMode::Eager`], is decompiled whole, once per jar version, into
//! a cached `…-decompiled-sources.jar`. The JAR pipeline mounts either like
//! any sources jar. A library that fails stays without sources.
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
    DecompileMode, installed_tool, launch, login_shell_environment, login_shell_path, run_tool, search_dirs,
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
/// Libraries decompiled at the same time, at most: each decompiler runs on
/// one core (see [`DECOMPILER_JVM_OPTIONS`]), and half the machine's cores
/// stay free for the editor and the server.
const MAX_PARALLEL_DECOMPILES: usize = 2;

/// How the decompiler's JVM runs. Left to its defaults, one decompiler
/// takes every core and ~1.7 GB for guava: a worker thread per core, C2
/// compiler threads and G1's parallel collector. On one worker thread,
/// with the client compiler (C1) and the serial collector, it writes the
/// same files (byte for byte) with half the CPU time and a third of the
/// memory. No heap limit: a tight one only makes the collector thrash, and
/// a big library would fail to decompile.
const DECOMPILER_JVM_OPTIONS: &[&str] = &["-XX:+UseSerialGC", "-XX:TieredStopAtLevel=1"];
/// The decompiler's own options: warnings only, one worker thread.
const DECOMPILER_OPTIONS: &[&str] = &["-log=WARN", "--thread-count=1"];

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

/// What a library without (all its) sources is given in their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    /// The decompiled classes.
    Decompiled,
    /// The declarations read from the class files ([`super::stubs`]).
    Stubs,
}

/// What of `dependency` is still to do in `mode`: it has no sources for
/// it, and making them has not failed before.
pub(crate) fn to_decompile(dependency: &ResolvedDependency, mode: DecompileMode) -> Option<Missing> {
    let jar = dependency.jar.as_deref()?;
    if !mode.is_on() || !is_decompilable_jar(jar) {
        return None;
    }
    let missing = missing_sources(dependency)?;
    let failed = failed_before(jar, &missing, Output::Decompiled)
        || (mode == DecompileMode::OnDemand && failed_before(jar, &missing, Output::Stubs));
    (!failed).then_some(missing)
}

/// Whether `dependency` already has what `mode` gives it: decompiled
/// sources, or in [`DecompileMode::OnDemand`] stubs.
pub(crate) fn is_done(dependency: &ResolvedDependency, mode: DecompileMode) -> bool {
    dependency
        .decompiled
        .as_deref()
        .is_some_and(|path| path.is_file() && (mode != DecompileMode::Eager || !is_stubs_jar(path)))
}

/// What an earlier sync made of `dependency` for `mode`, where it is still
/// in the cache: decompiled sources first, as they hold everything stubs
/// do and the bodies too.
pub(crate) fn already_decompiled(dependency: &ResolvedDependency, mode: DecompileMode) -> Option<PathBuf> {
    let jar = dependency.jar.as_deref()?;
    let missing = missing_sources(dependency)?;
    let mut outputs = vec![Output::Decompiled];
    if mode == DecompileMode::OnDemand {
        outputs.push(Output::Stubs);
    }
    outputs
        .into_iter()
        .filter_map(|output| output_path(jar, &missing, output))
        .find(|path| path.is_file())
}

/// Whether `jar` is a stubs jar ([`Output::Stubs`]).
pub(crate) fn is_stubs_jar(jar: &Path) -> bool {
    jar.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(STUBS_SUFFIX))
}

/// The end of a stubs jar's name. The number is the stubs' format: a
/// javak-lsp that writes them otherwise writes them again.
const STUBS_SUFFIX: &str = "-stubs-1.jar";

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
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut class_packages: Vec<String> = zip
        .file_names()
        .filter(|entry| {
            entry.ends_with(".class")
                && !entry.starts_with("META-INF/")
                && !entry.ends_with("module-info.class")
                && !entry.ends_with("package-info.class")
        })
        .map(|entry| directory_of(entry).to_owned())
        .collect();
    class_packages.sort();
    class_packages.dedup();
    let source_packages = source_packages(sources, &class_packages)?;
    Ok(class_packages
        .into_iter()
        .filter(|package| !source_packages.contains(package))
        .collect())
}

fn directory_of(entry: &str) -> &str {
    entry.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// The packages (`org/assertj/core/api`) of `class_packages` the source
/// files of `sources` are compiled into. A file is first taken to be in the
/// package of its directory. Where that leaves a package without sources,
/// the Kotlin files are read: Kotlin libraries lay out their sources by
/// source set and drop the common prefix (`commonMain/flow/Flow.kt` of
/// `kotlinx.coroutines.flow`), and a file compiles into the package it
/// declares, or the one `@file:JvmPackageName` names (the standard
/// library's `kotlin.collections.unsigned`). A jar laid out the Java way
/// is never read.
fn source_packages(sources: &Path, class_packages: &[String]) -> Result<std::collections::HashSet<String>, String> {
    use crate::language::Language;
    use std::io::Read;
    let file = std::fs::File::open(sources).map_err(|error| error.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut packages: std::collections::HashSet<String> = zip
        .file_names()
        .filter(|entry| Language::of_path(entry).is_some())
        .map(|entry| directory_of(entry).to_owned())
        .collect();
    if class_packages.iter().all(|package| packages.contains(package)) {
        return Ok(packages);
    }
    for index in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(index) else {
            continue;
        };
        if Language::of_path(entry.name()) != Some(Language::Kotlin) {
            continue;
        }
        let name = entry.name().to_owned();
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let declared = crate::indexer::layout(Language::Kotlin).library_package(&name, &text);
        packages.insert(declared.replace('.', "/"));
        if let Some(jvm_package) = jvm_package_name(&text) {
            packages.insert(jvm_package.replace('.', "/"));
        }
    }
    Ok(packages)
}

/// The package `@file:JvmPackageName("…")` compiles a Kotlin file into.
fn jvm_package_name(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find_map(|line| {
        let rest = line
            .strip_prefix("@file:JvmPackageName(\"")
            .or_else(|| line.strip_prefix("@file:kotlin.jvm.JvmPackageName(\""))?;
        rest.split_once('"').map(|(package, _)| package)
    })
}

/// Give every dependency what `mode` gives it of what has no sources —
/// the whole jar of one without a sources jar, the packages its sources
/// jar lacks of another — recording each result in `decompiled`.
/// [`DecompileMode::OnDemand`] reads the class files in-process and leaves
/// the JVM to the Kotlin libraries; [`DecompileMode::Eager`] decompiles.
/// `decompiler` is asked for only when a JVM is needed. Failures are
/// logged and leave the dependency as it was.
pub(crate) fn decompile_missing_sources(
    dependencies: &mut [ResolvedDependency],
    mode: DecompileMode,
    decompiler: impl FnOnce() -> Result<(PathBuf, PathBuf), String>,
) {
    let mut candidates: Vec<(usize, PathBuf, Missing)> = dependencies
        .iter()
        .enumerate()
        .filter(|(_, dependency)| !is_done(dependency, mode))
        .filter_map(|(index, dependency)| Some((index, dependency.jar.clone()?, to_decompile(dependency, mode)?)))
        .collect();
    if candidates.is_empty() {
        return;
    }
    if mode == DecompileMode::OnDemand {
        let mut kotlin = Vec::new();
        let mut written = 0usize;
        for (index, jar, missing) in candidates {
            match write_stubs_of(&jar, &missing) {
                Ok(Some(stubs)) => {
                    dependencies[index].decompiled = Some(stubs);
                    written += 1;
                }
                Ok(None) => kotlin.push((index, jar, missing)),
                Err(message) => {
                    remember_failure(&jar, &missing, Output::Stubs, &message);
                    log::warn!("dependency sync: cannot read the classes of {}: {message}", jar.display());
                }
            }
        }
        log::info!(
            "dependency sync: read the declarations of {written} libraries without (all their) sources; \
             {} Kotlin libraries to decompile",
            kotlin.len()
        );
        candidates = kotlin;
        if candidates.is_empty() {
            return;
        }
    }
    candidates.truncate(MAX_DECOMPILED_JARS);
    let (java, decompiler) = match decompiler() {
        Ok(found) => found,
        Err(message) => {
            log::warn!("dependency sync: not decompiling libraries without sources: {message}");
            return;
        }
    };
    let (java, decompiler) = (java.as_path(), decompiler.as_path());
    log::info!(
        "dependency sync: decompiling {} libraries without (all their) sources",
        candidates.len()
    );
    let parallel = std::thread::available_parallelism()
        .map_or(1, |cores| cores.get() / 2)
        .clamp(1, MAX_PARALLEL_DECOMPILES);
    let chunk_size = candidates.len().div_ceil(parallel).max(1);
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
                                remember_failure(jar, missing, Output::Decompiled, &message);
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

/// The stubs of what of `jar` is `missing`, written once per jar version;
/// `None` for a Kotlin library.
fn write_stubs_of(jar: &Path, missing: &Missing) -> Result<Option<PathBuf>, String> {
    let destination = output_path(jar, missing, Output::Stubs).ok_or_else(|| "cannot read the jar's metadata".to_owned())?;
    if destination.is_file() {
        return Ok(Some(destination));
    }
    let packages = match missing {
        Missing::Everything => None,
        Missing::Packages(packages) => Some(packages.as_slice()),
    };
    match super::stubs::write_stubs(jar, packages, &destination)? {
        super::stubs::Stubbed::Written(_) => Ok(Some(destination)),
        super::stubs::Stubbed::Kotlin => Ok(None),
    }
}

/// A plain `.jar` (an `.aar` wraps its classes in another archive) small
/// enough to decompile.
fn is_decompilable_jar(jar: &Path) -> bool {
    let is_jar = jar.extension().is_some_and(|extension| extension == "jar");
    let size_ok = std::fs::metadata(jar).is_ok_and(|metadata| metadata.len() <= MAX_JAR_BYTES);
    is_jar && size_ok
}

/// Where what is made of `jar` is cached: one file per jar version, and
/// for a jar with a sources jar one per set of packages it lacks.
fn output_path(jar: &Path, missing: &Missing, output: Output) -> Option<PathBuf> {
    let stem = jar.file_stem()?.to_string_lossy().into_owned();
    let fingerprint = crate::indexer::library::jar_fingerprint(jar)?;
    let base = match missing {
        Missing::Everything => format!("{stem}-{fingerprint:016x}"),
        Missing::Packages(packages) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(packages, &mut hasher);
            let packages = std::hash::Hasher::finish(&hasher);
            format!("{stem}-{fingerprint:016x}-{packages:016x}-missing")
        }
    };
    Some(decompiled_dir().join(match output {
        Output::Decompiled if matches!(missing, Missing::Everything) => format!("{base}-decompiled-sources.jar"),
        Output::Decompiled => format!("{base}-sources.jar"),
        Output::Stubs => format!("{base}{STUBS_SUFFIX}"),
    }))
}

/// Where a failure to make `output` of `jar` is recorded, beside it.
fn failure_marker(jar: &Path, missing: &Missing, output: Output) -> Option<PathBuf> {
    output_path(jar, missing, output).map(|path| path.with_extension("failed"))
}

/// Whether making `output` of this version of `jar` failed before. A
/// library the decompiler cannot read (or that ran out of time) is not
/// decompiled again on every start, minutes of CPU each time for nothing;
/// a new version of it, or clearing the cache, tries again.
fn failed_before(jar: &Path, missing: &Missing, output: Output) -> bool {
    failure_marker(jar, missing, output).is_some_and(|marker| marker.is_file())
}

fn remember_failure(jar: &Path, missing: &Missing, output: Output, message: &str) {
    if let Some(marker) = failure_marker(jar, missing, output) {
        let _ = std::fs::create_dir_all(decompiled_dir());
        let _ = std::fs::write(marker, message);
    }
}

/// Decompile what of `jar` is `missing` (once per jar version) into a
/// sources jar of `.kt` / `.java` files and return its path.
fn decompile_jar(java: &Path, decompiler: &Path, jar: &Path, missing: &Missing) -> Result<PathBuf, String> {
    let destination =
        output_path(jar, missing, Output::Decompiled).ok_or_else(|| "cannot read the jar's metadata".to_owned())?;
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
    command
        .args(DECOMPILER_JVM_OPTIONS)
        .arg("-jar")
        .arg(decompiler)
        .args(DECOMPILER_OPTIONS);
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

/// One class is given at most this long to decompile: someone waits on it.
const CLASS_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the decompiled class `entry` (`com/acme/Box.java`) of the stubs
/// jar `stubs` is kept.
pub(crate) fn decompiled_class_path(stubs: &Path, entry: &str) -> Option<PathBuf> {
    let stem = stubs.file_stem()?.to_string_lossy().into_owned();
    let relative: PathBuf = entry
        .split('/')
        .filter(|part| !part.is_empty() && *part != "..")
        .collect();
    Some(decompiled_dir().join("classes").join(stem).join(relative))
}

/// Decompile the class whose stub is `entry` of the stubs jar `stubs`, its
/// nested classes with it, once: what go to definition opens in place of
/// the stub. Returns where the source is kept.
pub(crate) fn decompile_class(stubs: &Path, entry: &str) -> Result<PathBuf, String> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let destination = decompiled_class_path(stubs, entry).ok_or_else(|| "not a stubs jar".to_owned())?;
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if destination.is_file() {
        return Ok(destination);
    }
    let jar = stubs_origin(stubs)?;
    let class = entry.strip_suffix(".java").ok_or_else(|| format!("{entry} is no stub"))?;
    let java = java_executable().ok_or_else(|| "no `java` found".to_owned())?;
    let decompiler = cached_decompiler().ok_or_else(|| "the decompiler isn't fetched yet".to_owned())?;
    let work_dir = destination.with_extension("work");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(work_dir.join("out")).map_err(|error| format!("cannot create {}: {error}", work_dir.display()))?;
    let outcome = (|| {
        let part = work_dir.join("class.jar");
        copy_class(&jar, class, &part)?;
        let mut command = std::process::Command::new(&java);
        command
            .args(DECOMPILER_JVM_OPTIONS)
            .arg("-jar")
            .arg(&decompiler)
            .args(DECOMPILER_OPTIONS)
            // The whole jar beside it, so what the class uses is known.
            .arg(format!("-e={}", jar.display()))
            .arg(&part)
            .arg(work_dir.join("out"));
        run_tool(command, "the decompiler", CLASS_TIMEOUT).map_err(|failure| failure.message())?;
        let source = work_dir.join("out").join(entry);
        if !source.is_file() {
            return Err("the decompiler wrote no source of it".to_owned());
        }
        std::fs::create_dir_all(destination.parent().ok_or("no directory")?).map_err(|error| error.to_string())?;
        std::fs::rename(&source, &destination).map_err(|error| error.to_string())
    })();
    let _ = std::fs::remove_dir_all(&work_dir);
    outcome.map(|()| destination)
}

/// The jar a stubs jar was written from.
fn stubs_origin(stubs: &Path) -> Result<PathBuf, String> {
    use std::io::Read;
    let file = std::fs::File::open(stubs).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut origin = String::new();
    archive
        .by_name(super::stubs::ORIGIN_ENTRY)
        .map_err(|error| error.to_string())?
        .read_to_string(&mut origin)
        .map_err(|error| error.to_string())?;
    let jar = PathBuf::from(origin);
    if jar.is_file() {
        Ok(jar)
    } else {
        Err(format!("{} is gone", jar.display()))
    }
}

/// A jar of `class` (internal name) of `jar` and its nested classes.
fn copy_class(jar: &Path, class: &str, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let output = std::fs::File::create(destination).map_err(|error| error.to_string())?;
    let mut zip = zip::ZipWriter::new(output);
    let own = format!("{class}.class");
    let nested = format!("{class}$");
    let mut copied = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_owned();
        if name == own || (name.starts_with(&nested) && name.ends_with(".class")) {
            zip.raw_copy_file(entry).map_err(|error| error.to_string())?;
            copied += 1;
        }
    }
    zip.finish().map_err(|error| error.to_string())?;
    if copied == 0 {
        return Err(format!("{own} isn't in {}", jar.display()));
    }
    Ok(())
}
