//! What another language server answers the cases, recorded beside them.
//!
//! The cases say what this crate's server should answer ([`super::cases`]).
//! The same question put to `jdtls` says whether that answer is the one
//! Java tooling agrees on — but a JVM costs more to start than every case
//! costs to run, so this is not a test that runs with the others: it is a
//! recorder, run by hand. Its output is a reference to compare against and
//! is not committed: where the server should answer what jdtls answers,
//! the case's own `//= expected` block is changed to say so.
//!
//! ```text
//! JDTLS_CMD=jdtls cargo test compat -- --ignored --nocapture
//! git diff src/integrationtest/cases
//! ```
//!
//! The answer is written into the case file itself, under a `//= jdtls`
//! line below the `//= expected` one:
//!
//! ```text
//! //= expected
//! /com/demo/Greeter.java:2:24  class Greeter { String greet() { return ""; } }
//! //= jdtls
//! /com/demo/Greeter.java:2:24  class Greeter { String greet() { return ""; } }
//! ```
//!
//! So one case is one file, `git diff` is the report (strip the `//= jdtls`
//! blocks again before committing), and the normal suite
//! never sees those blocks: it reads the answer up to the next `//=` line
//! and nothing below it ([`super::cases`]). A run without `JDTLS_CMD`
//! touches none of it.
//!
//! One process serves every case of a language. It is loaded and unloaded
//! per case ([`Session::load`], [`Session::unload`]) instead of restarted,
//! and each case's files sit in a directory of their own, so one case's
//! `com.demo.Main` is never another's.

use std::path::{Path, PathBuf};

use super::cases::{
    asked_for, case_files, cases_root, indented, write_answer_block, Case, RUN_DIRECTORIES,
};
use super::fixture::Fixture;
use super::request::Kind;
use super::server::Session;

/// The command to run, and the suffix its answers are recorded under.
const SERVERS: &[(&str, &str, &str)] = &[("JDTLS_CMD", "jdtls", "java")];

/// Records what the server named by its environment variable answers every
/// case of its language.
///
/// Ignored: it starts a JVM and takes minutes. Not a check — nothing here
/// fails because the two servers differ, because that is the thing being
/// looked at. It fails only when the recording itself cannot be made.
#[test]
#[ignore = "starts another language server; run it by hand to record"]
fn compat() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let mut recorded = 0;
    for (variable, suffix, language) in SERVERS {
        if !asked_for(variable) {
            eprintln!("{suffix}: skipped, set {variable} to the command that runs it");
            continue;
        }
        let command = std::env::var(variable).expect("asked_for read it");
        recorded += runtime.block_on(record(&command, suffix, language));
    }
    assert!(
        recorded > 0,
        "no server was asked: set one of {}",
        SERVERS
            .iter()
            .map(|(variable, _, _)| *variable)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Asks `command` every case of `language` and writes each answer to its
/// sidecar. Returns how many cases were recorded.
async fn record(command: &str, suffix: &str, language: &str) -> usize {
    let (program, arguments) = split_command(command);
    let project = tempfile::tempdir().expect("a temporary directory");
    let project_root = project.path().to_owned();
    write_project_marks(&project_root, language);

    let cases = cases_of(language);
    let mut session = Session::spawn(
        &program,
        &arguments,
        &project_root,
        &project_root.join(SOURCE_DIRECTORY),
    )
    .await;

    let mut disagreements = Vec::new();
    for (number, (file, kind)) in cases.iter().enumerate() {
        let case = Case::read(file);
        let fixture = Fixture::parse(&case.fixture);
        // A diagnostics case names its files from the project down, source
        // root included, so it decides itself what is under a source root.
        let spells_its_source_root = matches!(kind, Kind::Diagnostics);
        session.place_at(&if spells_its_source_root {
            project_root.clone()
        } else {
            project_root.join(SOURCE_DIRECTORY)
        });
        session.load(&fixture).await;
        let actual = kind
            .ask(&mut session, &fixture, &case.expected, case.settles())
            .await;
        session.unload(&fixture).await;
        if fixture.files.iter().any(|file| file.path == "pom.xml") {
            // The case brought its own build file and took it away again.
            write_project_marks(&project_root, language);
        }
        write_answer_block(file, suffix, &actual);
        if actual != case.expected {
            disagreements.push(format!(
                "{}\n  the case asks:\n{}  {suffix} answers:\n{}",
                file.display(),
                indented(&case.expected),
                indented(&actual)
            ));
        }
        eprintln!("{suffix}: {}/{} {}", number + 1, cases.len(), file.display());
    }

    eprintln!(
        "\n{suffix}: {} of {} cases answered what the case asks; \
         {} differ:\n\n{}",
        cases.len() - disagreements.len(),
        cases.len(),
        disagreements.len(),
        disagreements.join("\n")
    );
    cases.len()
}

/// Where a fixture's files go inside the project: a source root the server
/// reads packages from, rather than the project directory itself.
const SOURCE_DIRECTORY: &str = "src/main/java";

/// Narrows what is recorded to the cases whose path contains this, so a
/// handshake can be tried on one directory before minutes are spent on
/// every case: `COMPAT_ONLY=hover`.
const ONLY_VARIABLE: &str = "COMPAT_ONLY";

/// Every case of `language`, with the request its directory asks.
fn cases_of(language: &str) -> Vec<(PathBuf, Kind)> {
    let only = std::env::var(ONLY_VARIABLE).unwrap_or_default();
    let mut cases = Vec::new();
    for (case_language, request, kind) in RUN_DIRECTORIES {
        if *case_language != language {
            continue;
        }
        for file in case_files(&cases_root().join(case_language).join(request)) {
            if !file.to_string_lossy().contains(&only) {
                continue;
            }
            cases.push((file, *kind));
        }
    }
    assert!(!cases.is_empty(), "no {language} case matches {only:?}");
    cases
}

/// What makes the temporary directory a project the server will open: a
/// build file naming one source root, so every case's package resolves the
/// same way it does in [`super::server`].
fn write_project_marks(root: &Path, language: &str) {
    assert_eq!(language, "java", "only Java projects are described so far");
    std::fs::create_dir_all(root.join(SOURCE_DIRECTORY)).expect("the source root is writable");
    // ponytail: the smallest Maven project jdtls recognises. A Gradle or
    // multi-module layout would need a real build file; add one when a case
    // needs a dependency, which none does — a case declares every type it
    // uses.
    let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>javak.lsp</groupId>
  <artifactId>compat</artifactId>
  <version>1</version>
  <properties>
    <maven.compiler.source>21</maven.compiler.source>
    <maven.compiler.target>21</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
</project>
"#;
    std::fs::write(root.join("pom.xml"), pom).expect("the build file is writable");
}

/// `cargo test compat -- --ignored` passes the command as one string, so a
/// program with arguments (`java -jar …`) is split on spaces.
fn split_command(command: &str) -> (String, Vec<String>) {
    let mut words = command.split_whitespace().map(str::to_owned);
    let program = words.next().expect("a command is not empty");
    (program, words.collect())
}

