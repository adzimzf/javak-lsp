//! The cases: one file each, under `cases/<language>/<kind>/`.
//!
//! A case file is a fixture ([`super::fixture`]) followed by the answer it
//! expects, separated by a `//= expected` line:
//!
//! ```text
//! //- /com/demo/Greeter.java
//! package com.demo;
//! class Greeter { String greet() { return ""; } }
//! //- /com/demo/Main.java
//! package com.demo;
//! class Main { void run(Greeter greeter) { greeter.gr$0eet(); } }
//! //= expected
//! /com/demo/Greeter.java:2:24  class Greeter { String greet() { return ""; } }
//! ```
//!
//! A `//= <name>` line below it starts another block: `//= jdtls` is what
//! that server was recorded answering ([`super::compat`]). The case is the
//! first block alone, so those are notes a normal run never reads.
//!
//! `//= expected (pending)` marks a case whose answer the server does not
//! give yet: it passes while they differ and fails once it agrees, so the
//! marker comes off the day the check lands.
//!
//! The directory a case sits in says which request is asked of it and which
//! language it is written in, so a new case is a new file and no Rust: the
//! set scales to thousands of them. One `#[test]` per language and kind runs
//! a whole directory, its cases in parallel, and names every file that
//! disagreed.
//!
//! A suite is off unless `INTEGRATION=1` is set: a case is a whole server,
//! and `cargo test` on its own stays a unit-test run.

use std::path::{Path, PathBuf};

use futures::stream::{self, StreamExt};

use super::fixture::Fixture;
use super::request::{Kind, Settles, NOTHING};
use super::server::Session;

pub(super) const EXTENSION: &str = "case";
const SEPARATOR: &str = "//= expected";
/// What starts any answer block: `//= expected` is the one the case asks
/// for, and `//= jdtls` what another server was recorded answering
/// ([`super::compat`]). The first block is the case; the rest are notes,
/// and a run without those servers never looks at them.
pub(super) const ANSWER_HEADER: &str = "//=";
/// A case whose expectation the server does not meet yet, written as
/// `//= expected (pending)`: the answer below is what the case asks for,
/// and the suite passes while the server answers something else. It fails
/// once the server does answer it — the marker has to come off then, so a
/// pending case cannot sit there once it holds.
const PENDING: &str = "(pending)";
const INTEGRATION_VARIABLE: &str = "INTEGRATION";
/// How many cases of one suite are in flight; what runs is capped by the
/// budget they share with every other suite.
const CASES_IN_FLIGHT: usize = 64;

/// One runtime for every suite, and a cap on how many cases run at once.
///
/// A case is a server of its own, and the test harness runs the suites
/// themselves in parallel: without a shared budget a thousand servers start
/// at once, each waits longer than its patience for a machine busy with the
/// others, and cases fail for want of a CPU rather than a wrong answer.
fn shared() -> &'static (tokio::runtime::Runtime, tokio::sync::Semaphore) {
    static SHARED: std::sync::OnceLock<(tokio::runtime::Runtime, tokio::sync::Semaphore)> =
        std::sync::OnceLock::new();
    SHARED.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime");
        let at_once = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        (runtime, tokio::sync::Semaphore::new(at_once))
    })
}

macro_rules! suites {
    ($($language:ident { $($case_test:ident => $kind:ident),* $(,)? })*) => {
        $(mod $language {
            use super::{suite, Kind};
            $(#[test]
            fn $case_test() {
                suite(stringify!($language), stringify!($case_test), Kind::$kind);
            })*
        })*

        /// Every `<language>/<request>` a suite above runs, and the
        /// request it asks.
        pub(super) const RUN_DIRECTORIES: &[(&str, &str, Kind)] = &[
            $($((stringify!($language), stringify!($case_test), Kind::$kind),)*)*
        ];
    };
}

suites! {
    java {
        definition => Definition,
        type_definition => TypeDefinition,
        implementation => Implementation,
        references => References,
        document_highlight => DocumentHighlight,
        completion => Completion,
        hover => Hover,
        signature_help => SignatureHelp,
        prepare_rename => PrepareRename,
        rename => Rename,
        document_symbol => DocumentSymbol,
        folding_range => FoldingRange,
        workspace_symbol => WorkspaceSymbol,
        diagnostics => Diagnostics,
    }
    kotlin {
        definition => Definition,
        type_definition => TypeDefinition,
        references => References,
        document_highlight => DocumentHighlight,
        completion => Completion,
        hover => Hover,
        signature_help => SignatureHelp,
        prepare_rename => PrepareRename,
        document_symbol => DocumentSymbol,
        folding_range => FoldingRange,
        workspace_symbol => WorkspaceSymbol,
    }
}

/// A case directory no suite runs would hold cases nothing asks: the
/// `suites!` list above and the `cases/` tree have to say the same thing.
#[test]
fn every_case_directory_is_run_by_a_suite() {
    let mut unrun = Vec::new();
    for language in read_directory(&cases_root()) {
        for request in read_directory(&language) {
            let pair = (directory_name(&language), directory_name(&request));
            let listed = RUN_DIRECTORIES.iter().any(|(run_language, run_request, _)| {
                (*run_language, *run_request) == (&pair.0[..], &pair.1[..])
            });
            if !listed {
                unrun.push(format!("{}/{}", pair.0, pair.1));
            }
        }
    }
    assert!(
        unrun.is_empty(),
        "no suite asks these case directories: {}\nAdd them to `suites!` in {}.",
        unrun.join(", "),
        file!()
    );
}

/// Asks `kind` of every case in `cases/<language>/<kind_directory>` and
/// fails with the ones whose answer is not what their file expects.
fn suite(language: &str, kind_directory: &str, kind: Kind) {
    if !requested() {
        eprintln!(
            "{language}/{kind_directory}: skipped, \
             set {INTEGRATION_VARIABLE}=1 to ask the cases"
        );
        return;
    }
    let directory = cases_root().join(language).join(kind_directory);
    let files = case_files(&directory);
    assert!(
        !files.is_empty(),
        "{} holds no .{EXTENSION} file",
        directory.display()
    );
    let (runtime, at_once) = shared();
    let disagreements: Vec<String> = runtime.block_on(
        stream::iter(files)
            .map(|file| async move {
                let _running = at_once.acquire().await.expect("the budget is never closed");
                run_case(&file, kind).await
            })
            .buffer_unordered(CASES_IN_FLIGHT)
            .filter_map(|outcome| async move { outcome })
            .collect(),
    );
    assert!(
        disagreements.is_empty(),
        "{} of {language}/{kind_directory} answered differently:\n\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}

/// `None` when the case passes, otherwise what it answered instead.
async fn run_case(file: &Path, kind: Kind) -> Option<String> {
    let case = Case::read(file);
    let fixture = Fixture::parse(&case.fixture);
    let mut session = Session::open(&fixture).await;
    let actual = kind
        .ask(&mut session, &fixture, &case.expected, case.settles())
        .await;
    let agrees = actual == case.expected;
    if agrees != case.pending {
        return None;
    }
    if case.pending {
        return Some(format!(
            "{}\n  answers what it asks for now; remove the `{PENDING}` marker",
            file.display()
        ));
    }
    Some(format!(
        "{}\n  expected:\n{}  answered:\n{}",
        file.display(),
        indented(&case.expected),
        indented(&actual)
    ))
}

/// Whether the cases were asked for: a suite starts a server per case, so
/// it runs only under `INTEGRATION=1` and otherwise says why it did
/// nothing.
fn requested() -> bool {
    asked_for(INTEGRATION_VARIABLE)
}

pub(super) fn asked_for(variable: &str) -> bool {
    std::env::var(variable).is_ok_and(|value| value != "0")
}

/// A case file split at its `//= expected` line.
pub(super) struct Case {
    pub(super) fixture: String,
    pub(super) expected: String,
    /// Marked `(pending)`: the expectation is a wish, not yet the answer.
    pending: bool,
}

impl Case {
    pub(super) fn read(file: &Path) -> Self {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|error| panic!("{} is readable: {error}", file.display()));
        let (fixture, expected) = text.split_once(SEPARATOR).unwrap_or_else(|| {
            panic!("{} has a `{SEPARATOR}` line", file.display());
        });
        let (marker, expected) = expected
            .split_once('\n')
            .map_or(("", expected), |(first, rest)| (first.trim(), rest));
        Self {
            fixture: fixture.to_owned(),
            expected: normalized(first_block(expected)),
            pending: marker == PENDING,
        }
    }

    /// A case whose answer is nothing is asked once; any other waits for
    /// the server to have something to say.
    pub(super) fn settles(&self) -> Settles {
        if self.expected.trim() == NOTHING {
            Settles::Immediately
        } else {
            Settles::WhenAnswered
        }
    }
}

/// The lines of `text` before the next `//=` line: the answer the case
/// asks for, without the recorded answers of other servers that may follow
/// it. A case with none of those is the whole of `text`.
fn first_block(text: &str) -> &str {
    let mut end = text.len();
    for (offset, line) in line_offsets(text) {
        if line.trim_start().starts_with(ANSWER_HEADER) {
            end = offset;
            break;
        }
    }
    &text[..end]
}

/// Each line of `text` with where it starts.
fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = offset;
        offset += line.len();
        (start, line.trim_end_matches('\n'))
    })
}

/// Replaces the `//= <name>` block of `file` with `answer`, adding it at
/// the end when the case has none. The case's own `//= expected` block and
/// every other server's are left alone, so one case file holds what each
/// server answers and `git diff` shows which of them moved.
pub(super) fn write_answer_block(file: &Path, name: &str, answer: &str) {
    let text = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", file.display()));
    let header = format!("{ANSWER_HEADER} {name}");
    let block = format!("{header}\n{answer}");
    let rewritten = match block_span(&text, &header) {
        Some((start, end)) => format!("{}{block}{}", &text[..start], &text[end..]),
        None => format!("{}\n{block}", text.trim_end()),
    };
    std::fs::write(file, rewritten)
        .unwrap_or_else(|error| panic!("{} is writable: {error}", file.display()));
}

/// Where the `header` block of `text` starts and ends: from its header
/// line to the next `//=` line, or the end of the file.
fn block_span(text: &str, header: &str) -> Option<(usize, usize)> {
    let lines: Vec<(usize, &str)> = line_offsets(text).collect();
    let start = lines
        .iter()
        .position(|(_, line)| line.trim() == header)
        .map(|index| lines[index].0)?;
    let end = lines
        .iter()
        .find(|(offset, line)| *offset > start && line.trim_start().starts_with(ANSWER_HEADER))
        .map_or(text.len(), |(offset, _)| *offset);
    Some((start, end))
}

/// An expectation as [`Kind::ask`] renders an answer: lines with no blank
/// ones around them, ending in one newline. An empty one stays empty, so a
/// case file committed without an answer disagrees with every answer.
fn normalized(expected: &str) -> String {
    let body = expected.trim();
    if body.is_empty() {
        return String::new();
    }
    body.to_owned() + "\n"
}

pub(super) fn indented(text: &str) -> String {
    if text.is_empty() {
        return "    (nothing written down yet)\n".to_owned();
    }
    text.lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>()
}

pub(super) fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integrationtest/cases")
}

/// The subdirectories of `directory`, in name order.
fn read_directory(directory: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} is a directory: {error}", directory.display()));
    let mut directories: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let path = entry.expect("a readable directory entry").path();
            path.is_dir().then_some(path)
        })
        .collect();
    directories.sort();
    directories
}

/// A recorded answer is a note, not the case: reading one back gives the
/// `//= expected` block alone, and recording one leaves it and the other
/// servers' blocks where they were.
#[test]
fn a_recorded_answer_is_not_the_case() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = directory.path().join("a.case");
    std::fs::write(
        &file,
        "//- /A.java\nclass A { int f$0; }\n//= expected\n/A.java:1:15\n",
    )
    .expect("the case is writable");

    write_answer_block(&file, "jdtls", "/A.java:1:99\n");
    write_answer_block(&file, "kotlinlsp", "(nothing)\n");
    assert_eq!(Case::read(&file).expected, "/A.java:1:15\n");

    // Recording again replaces that block alone, and keeps the order.
    write_answer_block(&file, "jdtls", "/A.java:1:15\n");
    assert_eq!(Case::read(&file).expected, "/A.java:1:15\n");
    let text = std::fs::read_to_string(&file).expect("the case is readable");
    assert_eq!(
        text,
        "//- /A.java\nclass A { int f$0; }\n//= expected\n/A.java:1:15\n\
         //= jdtls\n/A.java:1:15\n//= kotlinlsp\n(nothing)\n",
        "the recorded blocks stay beside the case, in the order written"
    );

    // A case with no recorded answer is unchanged by reading it.
    let pristine = directory.path().join("b.case");
    std::fs::write(&pristine, "//- /B.java\nclass B$0 {}\n//= expected\n(nothing)\n")
        .expect("the case is writable");
    assert_eq!(Case::read(&pristine).expected, "(nothing)\n");
}

fn directory_name(directory: &Path) -> String {
    directory
        .file_name()
        .expect("a directory has a name")
        .to_string_lossy()
        .into_owned()
}

/// The case files of `directory`, in name order, so a failure list reads
/// the same on every machine.
pub(super) fn case_files(directory: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} is a directory: {error}", directory.display()));
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let path = entry.expect("a readable directory entry").path();
            let is_case = path.extension().is_some_and(|suffix| suffix == EXTENSION);
            is_case.then_some(path)
        })
        .collect();
    files.sort();
    files
}
