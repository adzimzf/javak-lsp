//! Dependency sync, run for the server: the libraries of the workspace's
//! last manifest are indexed at once, then the build tool is asked again
//! in the background whenever the build files may have changed — on
//! startup, when a build file is saved or changes on disk, and after a
//! back-off when some sources failed to download.
//!
//! The platform's own sources — the JDK's `src.zip` and the Kotlin standard
//! library's sources jar ([`platform`]) — are indexed with them, build
//! tool or none.
//!
//! Settings come from the client's `initializationOptions`:
//! `{"dependencySync": "auto" | "off", "javaHome": "/path/to/jdk",
//!   "indexingOptions": {"decompileLibraries": bool}}`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::dependency_sync::platform::{self, KOTLIN_STDLIB};
use crate::dependency_sync::{
    self, is_build_file_name, load_manifest, sources_retry_due, sync_fingerprint,
    DependencyManifest, SyncMode, SyncOptions, SyncOutcome, RETRY_BACK_OFF,
};
use crate::indexer::Indexer;

#[derive(Default)]
pub struct DependencySync {
    settings: Mutex<(SyncMode, SyncOptions)>,
    /// The JDK whose sources are read (`javaHome`); found on the machine
    /// when the client names none.
    java_home: Mutex<Option<PathBuf>>,
    /// A sync is running; a request meanwhile runs one more after it.
    running: AtomicBool,
    again: AtomicBool,
}

impl DependencySync {
    /// Reads the client's settings.
    pub fn configure(&self, options: Option<&Value>) {
        let Some(options) = options else {
            return;
        };
        let mut settings = self.settings.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mode) = options
            .get("dependencySync")
            .and_then(Value::as_str)
            .and_then(SyncMode::parse)
        {
            settings.0 = mode;
        }
        if let Some(decompile) = options
            .pointer("/indexingOptions/decompileLibraries")
            .and_then(Value::as_bool)
        {
            settings.1.decompile = decompile;
        }
        drop(settings);
        if let Some(home) = options.get("javaHome").and_then(Value::as_str) {
            let home = home.trim();
            if !home.is_empty() {
                *self.java_home.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(PathBuf::from(home));
            }
        }
    }

    fn java_home(&self) -> Option<PathBuf> {
        self.java_home
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn settings(&self) -> (SyncMode, SyncOptions) {
        *self.settings.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether a changed file can change the dependencies.
    pub fn is_build_file(path: &Path) -> bool {
        path.file_name()
            .is_some_and(|name| is_build_file_name(&name.to_string_lossy()))
    }

    /// Index the last manifest's libraries, then sync in the background.
    /// A workspace without a root on disk has only the platform's sources
    /// indexed.
    pub fn start(self: &Arc<Self>, indexer: Arc<Indexer>, client: Client) {
        if indexer.workspace_pinned.load(Ordering::Relaxed) {
            return;
        }
        let root = indexer.workspace_root.get().cloned().filter(|root| root.is_dir());
        let Some(root) = root else {
            let java_home = self.java_home();
            tokio::spawn(async move {
                let indexed = index_libraries(None, java_home, indexer, &client).await;
                indexed.report(&client).await;
            });
            return;
        };
        if self.running.swap(true, Ordering::AcqRel) {
            self.again.store(true, Ordering::Release);
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                this.again.store(false, Ordering::Release);
                this.run(&root, &indexer, &client).await;
                if !this.again.load(Ordering::Acquire) {
                    break;
                }
            }
            this.running.store(false, Ordering::Release);
        });
    }

    async fn run(self: &Arc<Self>, root: &Path, indexer: &Arc<Indexer>, client: &Client) {
        let (mode, options) = self.settings();
        let indexed =
            index_libraries(Some(root.to_path_buf()), self.java_home(), Arc::clone(indexer), client)
                .await;
        indexed.report(client).await;
        if mode == SyncMode::Off {
            return;
        }
        let blocking_root = root.to_path_buf();
        let outcome = tokio::task::spawn_blocking(move || {
            dependency_sync::sync(&blocking_root, false, options)
        })
        .await
        .unwrap_or_else(|error| SyncOutcome::Failed(error.to_string()));
        match outcome {
            SyncOutcome::NoBuildTool | SyncOutcome::UpToDate => {}
            SyncOutcome::Synced {
                dependencies,
                sources,
                decompiled,
            } => {
                let indexed = index_libraries(
                    Some(root.to_path_buf()),
                    self.java_home(),
                    Arc::clone(indexer),
                    client,
                )
                .await;
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "javak-lsp: dependencies synced — {dependencies} libraries, \
                             {sources} sources jars, {decompiled} decompiled; \
                             {} library types indexed",
                            indexed.types
                        ),
                    )
                    .await;
            }
            SyncOutcome::Failed(message) => {
                client
                    .show_message(
                        MessageType::WARNING,
                        format!("javak-lsp: dependency sync failed: {message}"),
                    )
                    .await;
            }
        }
        self.retry_missing_sources(root, options, indexer, client);
    }

    /// Some sources failed to download for a transient reason: ask again
    /// once the back-off has passed.
    fn retry_missing_sources(
        self: &Arc<Self>,
        root: &Path,
        options: SyncOptions,
        indexer: &Arc<Indexer>,
        client: &Client,
    ) {
        let incomplete = load_manifest(root).is_some_and(|manifest| manifest.incomplete_sources);
        if !incomplete {
            return;
        }
        let this = Arc::clone(self);
        let root = root.to_path_buf();
        let indexer = Arc::clone(indexer);
        let client = client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RETRY_BACK_OFF).await;
            let due = sync_fingerprint(&root, options)
                .is_some_and(|fingerprint| sources_retry_due(&root, &fingerprint));
            if due {
                this.start(indexer, client);
            }
        });
    }
}

/// What [`index_libraries`] indexed.
#[derive(Default)]
struct Indexed {
    types: usize,
    /// The Java level the project's build files declare, and the Kotlin
    /// version its manifest depends on.
    java_version: Option<String>,
    kotlin_version: Option<String>,
    jdk: Option<PathBuf>,
    kotlin_stdlib: Option<PathBuf>,
    /// Whether the workspace has Kotlin sources, so the standard library
    /// was looked for.
    wants_kotlin: bool,
}

impl Indexed {
    async fn report(&self, client: &Client) {
        client
            .log_message(
                MessageType::INFO,
                format!("javak-lsp: library indexing done, {} library types", self.types),
            )
            .await;
        client
            .log_message(
                MessageType::INFO,
                format!(
                    "javak-lsp: project Java {}, Kotlin {}",
                    self.java_version.as_deref().unwrap_or("not declared (build tool default)"),
                    self.kotlin_version.as_deref().unwrap_or("none")
                ),
            )
            .await;
        match &self.jdk {
            Some(jdk) => {
                client
                    .log_message(
                        MessageType::INFO,
                        format!("javak-lsp: JDK sources {}", jdk.display()),
                    )
                    .await;
            }
            None => {
                client
                    .log_message(
                        MessageType::WARNING,
                        "javak-lsp: no JDK sources (src.zip) found — set JAVA_HOME or the \
                         `javaHome` setting to a JDK that ships them (on Debian/Ubuntu: \
                         the openjdk-<version>-source package)",
                    )
                    .await;
            }
        }
        if let Some(stdlib) = &self.kotlin_stdlib {
            client
                .log_message(
                    MessageType::INFO,
                    format!("javak-lsp: Kotlin standard library sources {}", stdlib.display()),
                )
                .await;
        } else if self.wants_kotlin {
            client
                .log_message(
                    MessageType::WARNING,
                    "javak-lsp: no Kotlin standard library sources found in the Gradle or \
                     Maven cache",
                )
                .await;
        }
    }
}

/// Index the sources jars of `root`'s saved manifest and the platform's
/// sources: the JDK's always, the Kotlin standard library's when the
/// project has Kotlin and its manifest brings no sources of it. The client
/// is told while it runs, so it can show that answers may still be
/// incomplete.
async fn index_libraries(
    root: Option<PathBuf>,
    java_home: Option<PathBuf>,
    indexer: Arc<Indexer>,
    client: &Client,
) -> Indexed {
    match &root {
        Some(root) => log::info!("dependency sync: indexing libraries of {}", root.display()),
        None => log::info!("dependency sync: indexing the platform's libraries"),
    }
    client
        .log_message(
            MessageType::INFO,
            "javak-lsp: indexing libraries\u{2026}".to_string(),
        )
        .await;
    let indexed = tokio::task::spawn_blocking(move || {
        let manifest = root.as_deref().and_then(load_manifest);
        if let Some(manifest) = &manifest {
            indexer.apply_reported(&manifest.modules);
        }
        let (mut jars, stdlib) = manifest_sources(manifest.as_ref());
        let mut indexed = Indexed {
            wants_kotlin: stdlib.listed || root.as_deref().is_none_or(has_kotlin_sources),
            java_version: root.as_deref().and_then(dependency_sync::declared_java_version),
            kotlin_version: stdlib.version.clone(),
            ..Indexed::default()
        };
        if indexed.wants_kotlin {
            indexed.kotlin_stdlib = match stdlib.sources {
                Some(sources) => Some(sources),
                None => {
                    let found = platform::kotlin_stdlib_sources(stdlib.version.as_deref());
                    jars.extend(found.clone());
                    found
                }
            };
        }
        indexed.jdk = platform::jdk_sources(java_home.as_deref());
        jars.extend(indexed.jdk.clone());
        log::info!("dependency sync: indexing {} sources jars", jars.len());
        indexed.types = indexer.libraries.replace(&jars);
        // Parse the library files the open files name now, rather than on
        // the first request.
        drop(indexer.snapshot());
        indexed
    })
    .await
    .unwrap_or_default();
    log::info!("dependency sync: library indexing done, {} library types", indexed.types);
    indexed
}

/// What a manifest says of the Kotlin standard library.
#[derive(Default)]
struct StdlibInManifest {
    /// The project depends on it.
    listed: bool,
    version: Option<String>,
    /// Its own sources jar, when the build tool downloaded one.
    sources: Option<PathBuf>,
}

/// The sources jars of a manifest, and what it says of the Kotlin
/// standard library. A decompiled standard library is left out: its
/// sources are looked for in the caches instead.
fn manifest_sources(manifest: Option<&DependencyManifest>) -> (Vec<PathBuf>, StdlibInManifest) {
    let mut stdlib = StdlibInManifest::default();
    let Some(manifest) = manifest else {
        return (Vec::new(), stdlib);
    };
    for dependency in &manifest.dependencies {
        let mut parts = dependency.coordinate.split(':');
        let (group, artifact) = (parts.next(), parts.next());
        if (group, artifact) == (Some(KOTLIN_STDLIB.0), Some(KOTLIN_STDLIB.1)) {
            stdlib.listed = true;
            stdlib.version = parts.next().map(str::to_owned);
            stdlib.sources = dependency.sources.clone().filter(|path| path.is_file());
        }
    }
    let jars = manifest
        .sources_jars_by_artifact()
        .into_iter()
        .filter(|(group, artifact, jar)| {
            let is_stdlib = (group.as_str(), artifact.as_str()) == KOTLIN_STDLIB;
            !is_stdlib || stdlib.sources.as_ref() == Some(jar)
        })
        .map(|(_, _, jar)| jar)
        .collect();
    (jars, stdlib)
}

/// Whether a `.kt` file sits below `root`, looking at a few thousand
/// files at most.
fn has_kotlin_sources(root: &Path) -> bool {
    let mut budget = 5_000usize;
    let mut found = false;
    crate::indexer::walk_sources(root, |path| {
        found = path.extension().is_some_and(|extension| extension == "kt");
        budget = budget.saturating_sub(1);
        if found || budget == 0 {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kotlin_source_is_found_below_the_root() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("build.gradle.kts"), "").unwrap();
        std::fs::create_dir_all(directory.path().join("build/generated")).unwrap();
        std::fs::write(directory.path().join("build/generated/A.kt"), "").unwrap();
        assert!(!has_kotlin_sources(directory.path()));
        let sources = directory.path().join("src/main/kotlin/demo");
        std::fs::create_dir_all(&sources).unwrap();
        std::fs::write(sources.join("Main.kt"), "").unwrap();
        assert!(has_kotlin_sources(directory.path()));
    }

    #[test]
    fn a_decompiled_standard_library_is_left_for_its_sources() {
        let directory = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = directory.path().join(name);
            std::fs::write(&path, "").unwrap();
            path
        };
        let dependency = |coordinate: &str, sources: Option<PathBuf>, decompiled: Option<PathBuf>| {
            crate::dependency_sync::ResolvedDependency {
                coordinate: coordinate.to_owned(),
                jar: None,
                sources,
                decompiled,
            }
        };
        let manifest = DependencyManifest {
            format: crate::dependency_sync::MANIFEST_FORMAT,
            fingerprint: String::new(),
            build_tool: "gradle".to_owned(),
            dependencies: vec![
                dependency("com.acme:greeter:1.0", Some(file("greeter-sources.jar")), None),
                dependency(
                    "org.jetbrains.kotlin:kotlin-stdlib:2.2.20",
                    None,
                    Some(file("kotlin-stdlib-decompiled.jar")),
                ),
            ],
            incomplete_sources: false,
            saved_at: 0,
            modules: Vec::new(),
        };
        let (jars, stdlib) = manifest_sources(Some(&manifest));
        assert_eq!(jars, vec![directory.path().join("greeter-sources.jar")]);
        assert!(stdlib.listed);
        assert_eq!(stdlib.version.as_deref(), Some("2.2.20"));
        assert_eq!(stdlib.sources, None);
    }
}
