//! javak-lsp against jdtls on real projects.
//!
//! The cases ([`super::cases`]) pin one rule each on a few lines of code;
//! this asks both servers the same questions about whole projects, cloned
//! from GitHub ([`catalog`]), and shows where their answers differ. For
//! each project:
//!
//! 1. it is cloned one commit deep into a temporary directory, and its
//!    build tool (Maven or Gradle) fetches its libraries;
//! 2. the questions are chosen from its code ([`probe`]): navigation,
//!    hover, references, rename, hierarchies, outline, folding, tokens,
//!    formatting… and completion and diagnostics after an edit;
//! 3. jdtls imports a copy of it and answers them; the questions and the
//!    answers are recorded ([`record`]) in `target/jdtls-compat/<project>`;
//! 4. javak-lsp answers the same questions about another, clean, copy;
//! 5. the answers are compared and only the ones that differ are shown
//!    ([`report`]), in the terminal, in `target/jdtls-compat/report.md`, and
//!    — asked for in CI — as a comment on the branch's pull request, which
//!    a later run updates.
//!
//! It takes minutes and the network, so it runs only when asked:
//!
//! ```text
//! JDTLS_COMPAT=1 cargo test --release --lib integrationtest::benchmark -- --nocapture
//! ```
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `JDTLS_COMPAT=1` | run at all |
//! | `JDTLS_COMPAT_REPOS=jsoup,javapoet` | only these projects |
//! | `JDTLS_COMPAT_FILES`, `_PLACES`, `_EDITS` | how many files, places of each kind per file, and edits per file are asked about (4, 3, 2) |
//! | `JDTLS_COMPAT_SEED` | random questions from this seed, `random` for a new one (printed); the same seed asks the same questions |
//! | `JDTLS_COMPAT_PATIENCE` | seconds one question waits for an answer (10) |
//! | `JDTLS_COMPAT_REUSE=1` | reuse jdtls's recorded answers when the commit and questions are unchanged |
//! | `JDTLS_COMPAT_OUT` | where recordings and the report go (`target/jdtls-compat`) |
//! | `JDTLS_COMPAT_COMMENT=1` | comment on the pull request (CI) |
//! | `JDTLS_COMPAT_PR` | the pull request to comment on, instead of the branch's |
//! | `JDTLS_COMPAT_STRICT=1` | fail when any answer differs |
//! | `JDTLS_CMD`, `JAVAK_LSP_CMD` | the servers (default: `jdtls` on the `PATH` or a downloaded milestone; `target/release/javak-lsp`) |

mod catalog;
mod prepare;
mod probe;
mod record;
mod render;
mod report;

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::cases::asked_for;
use catalog::{Repo, REPOS};
use probe::Budget;
use record::{Recording, Server};
use report::RepoReport;

const RUN_VARIABLE: &str = "JDTLS_COMPAT";

#[test]
fn javak_answers_what_jdtls_answers_on_real_projects() {
    if !asked_for(RUN_VARIABLE) {
        eprintln!(
            "jdtls compat: skipped, set {RUN_VARIABLE}=1 to compare javak-lsp \
             with jdtls on real projects (`just benchmark`)"
        );
        return;
    }
    let selected = selected_repos();
    assert!(!selected.is_empty(), "JDTLS_COMPAT_REPOS selects no project");
    let out = output_directory();
    std::fs::create_dir_all(&out).expect("the output directory is writable");
    let jdtls = prepare::jdtls();
    let javak = prepare::javak();
    eprintln!(
        "jdtls: {} {}\njavak-lsp: {}",
        jdtls.program,
        jdtls.arguments.join(" "),
        javak.program
    );
    let budget = Budget {
        files: number("JDTLS_COMPAT_FILES", 4),
        places: number("JDTLS_COMPAT_PLACES", 3),
        edits: number("JDTLS_COMPAT_EDITS", 2),
        seed: seed(),
    };
    let replay = budget.seed.map(|seed| {
        format!("seed {seed}, replay with `JDTLS_COMPAT_SEED={seed}` (`just benchmark <projects> {seed}`)")
    });
    match &replay {
        Some(replay) => eprintln!("random questions: {replay}"),
        None => eprintln!("evenly spaced questions (set JDTLS_COMPAT_SEED for random ones)"),
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");

    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for repo in selected {
        eprintln!("\n▶ {} ({} {})", repo.name, repo.url, repo.rev);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compare_repo(&runtime, repo, &jdtls, &javak, budget, &out)
        }));
        match outcome {
            Ok(report) => reports.push(report),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_default();
                eprintln!("✗ {}: {message}", repo.name);
                failures.push(format!("{}: {message}", repo.name));
            }
        }
    }

    report::print(&reports);
    if let Some(replay) = &replay {
        eprintln!("random questions: {replay}");
    }
    let mut body = report::markdown(&reports);
    if let Some(replay) = &replay {
        body.push_str(&format!("\nRandom questions: {replay}.\n"));
    }
    if !failures.is_empty() {
        body.push_str("\n**Not compared:**\n");
        for failure in &failures {
            body.push_str(&format!("- {}\n", failure.lines().next().unwrap_or_default()));
        }
    }
    let report_file = out.join("report.md");
    std::fs::write(&report_file, &body).expect("the report is writable");
    eprintln!("report: {}", report_file.display());
    if asked_for("JDTLS_COMPAT_COMMENT") {
        report::comment_on_pull_request(&report_file);
    }

    assert!(failures.is_empty(), "not compared:\n{}", failures.join("\n"));
    if asked_for("JDTLS_COMPAT_STRICT") {
        let differing: usize = reports.iter().map(|report| report.differences.len()).sum();
        assert_eq!(differing, 0, "{differing} answers differ from jdtls's");
    }
}

fn compare_repo(
    runtime: &tokio::runtime::Runtime,
    repo: &Repo,
    jdtls: &prepare::ServerCommand,
    javak: &prepare::ServerCommand,
    budget: Budget,
    out: &Path,
) -> RepoReport {
    let work = tempfile::Builder::new()
        .prefix(&format!("jdtls-compat-{}-", repo.name))
        .tempdir()
        .expect("a temporary directory");
    let pristine = work.path().join("pristine");
    let commit = prepare::clone(repo, &pristine);
    eprintln!("  cloned {commit}");
    prepare::fetch_dependencies(repo, &pristine);
    let probes = probe::probes(&pristine, budget);
    eprintln!("  {} questions", probes.len());
    let repo_out = out.join(repo.name);
    std::fs::create_dir_all(&repo_out).expect("the output directory is writable");

    let jdtls_file = repo_out.join("jdtls.json");
    let reusable = asked_for("JDTLS_COMPAT_REUSE")
        .then(|| Recording::load(&jdtls_file))
        .flatten()
        .filter(|recorded| {
            recorded.commit == commit
                && recorded.answers.len() == probes.len()
                && recorded
                    .answers
                    .iter()
                    .zip(&probes)
                    .all(|(answer, probe)| answer.probe.id == probe.id)
        });
    let jdtls_recording = match reusable {
        Some(recorded) => {
            eprintln!("  jdtls: reusing {}", jdtls_file.display());
            recorded
        }
        None => {
            let project = work.path().join("jdtls");
            prepare::copy_project(&pristine, &project);
            let server = Server {
                name: "jdtls",
                command: jdtls.clone(),
                extra_arguments: vec![
                    "-data".to_owned(),
                    work.path().join("jdtls-data").to_string_lossy().into_owned(),
                ],
                ready_markers: record::JDTLS_READY,
            };
            let recording = runtime.block_on(record::record(
                &server,
                repo.name,
                &commit,
                &project,
                &probes,
                &repo_out.join("jdtls.log"),
            ));
            recording.save(&jdtls_file);
            recording
        }
    };

    let project = work.path().join("javak");
    prepare::copy_project(&pristine, &project);
    let server = Server {
        name: "javak-lsp",
        command: javak.clone(),
        extra_arguments: Vec::new(),
        ready_markers: record::JAVAK_READY,
    };
    let javak_recording = runtime.block_on(record::record(
        &server,
        repo.name,
        &commit,
        &project,
        &probes,
        &repo_out.join("javak.log"),
    ));
    javak_recording.save(&repo_out.join("javak.json"));

    if asked_for("JDTLS_COMPAT_KEEP") {
        let kept = work.keep();
        eprintln!("  kept {}", kept.display());
    }
    // Both servers are gone by now: their sessions were dropped.
    std::thread::sleep(Duration::from_millis(100));
    report::compare(&jdtls_recording, &javak_recording)
}

fn selected_repos() -> Vec<&'static Repo> {
    let wanted = std::env::var("JDTLS_COMPAT_REPOS").unwrap_or_default();
    let wanted: Vec<&str> = wanted
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    REPOS
        .iter()
        .filter(|repo| wanted.is_empty() || wanted.contains(&repo.name))
        .collect()
}

fn output_directory() -> PathBuf {
    std::env::var("JDTLS_COMPAT_OUT").map_or_else(
        |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/jdtls-compat"),
        PathBuf::from,
    )
}

/// `JDTLS_COMPAT_SEED`: a number, `random` for a new one (printed, so the
/// run can be replayed), or nothing for evenly spaced picks.
fn seed() -> Option<u64> {
    let value = std::env::var("JDTLS_COMPAT_SEED").unwrap_or_default();
    match value.trim() {
        "" => None,
        "random" => Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos() as u64 % 1_000_000_000),
        ),
        number => Some(
            number
                .parse()
                .unwrap_or_else(|_| panic!("JDTLS_COMPAT_SEED={number} is not a number or `random`")),
        ),
    }
}

fn number(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
