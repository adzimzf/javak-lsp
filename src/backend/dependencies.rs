//! Dependency sync, run for the server: the libraries of the workspace's
//! last manifest are indexed at once, then the build tool is asked again
//! in the background whenever the build files may have changed — on
//! startup, when a build file is saved or changes on disk, and after a
//! back-off when some sources failed to download.
//!
//! Settings come from the client's `initializationOptions`:
//! `{"dependencySync": "auto" | "off", "indexingOptions": {"decompileLibraries": bool}}`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tower_lsp::lsp_types::MessageType;
use tower_lsp::Client;

use crate::dependency_sync::{
    self, is_build_file_name, load_manifest, sources_retry_due, sync_fingerprint, SyncMode,
    SyncOptions, SyncOutcome, RETRY_BACK_OFF,
};
use crate::indexer::Indexer;

#[derive(Default)]
pub struct DependencySync {
    settings: Mutex<(SyncMode, SyncOptions)>,
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
    /// Nothing happens for a workspace without a root on disk.
    pub fn start(self: &Arc<Self>, indexer: Arc<Indexer>, client: Client) {
        if indexer.workspace_pinned.load(Ordering::Relaxed) {
            return;
        }
        let Some(root) = indexer.workspace_root.get().cloned() else {
            return;
        };
        if !root.is_dir() {
            return;
        }
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
        index_manifest(root.to_path_buf(), Arc::clone(indexer), client).await;
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
                let indexed = index_manifest(root.to_path_buf(), Arc::clone(indexer), client).await;
                client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "javak-lsp: dependencies synced — {dependencies} libraries, \
                             {sources} sources jars, {decompiled} decompiled; \
                             {indexed} library source files indexed"
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

/// Index the sources jars of `root`'s saved manifest. Returns how many
/// library source files were listed. The client is told while it runs, so
/// it can show that answers may still be incomplete.
async fn index_manifest(root: PathBuf, indexer: Arc<Indexer>, client: &Client) -> usize {
    log::info!("dependency sync: indexing libraries of {}", root.display());
    client
        .log_message(
            MessageType::INFO,
            "javak-lsp: indexing libraries\u{2026}".to_string(),
        )
        .await;
    let indexed = tokio::task::spawn_blocking(move || {
        let Some(manifest) = load_manifest(&root) else {
            return 0;
        };
        let jars: Vec<PathBuf> = manifest
            .sources_jars_by_artifact()
            .into_iter()
            .map(|(_, _, jar)| jar)
            .collect();
        log::info!("dependency sync: indexing {} sources jars", jars.len());
        indexer.libraries.replace(&jars)
    })
    .await
    .unwrap_or(0);
    log::info!("dependency sync: library indexing done, {indexed} source files");
    client
        .log_message(
            MessageType::INFO,
            format!("javak-lsp: library indexing done, {indexed} source files"),
        )
        .await;
    indexed
}
