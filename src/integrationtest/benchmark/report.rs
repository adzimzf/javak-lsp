//! The two recordings of a project side by side: only the questions the
//! servers answered differently are shown, each as a diff from jdtls's
//! answer (`-`) to javak-lsp's (`+`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

use super::record::{Answer, Recording};

/// Marks the pull request comment this report owns, so a later run
/// replaces it instead of adding another.
const COMMENT_MARK: &str = "<!-- javak-lsp-jdtls-compat -->";
/// GitHub refuses a comment longer than 65536 characters.
const COMMENT_LIMIT: usize = 60_000;
/// How many diff lines one question shows.
const TERMINAL_LINES: usize = 40;
const COMMENT_LINES: usize = 20;

pub(super) struct RepoReport {
    pub(super) repo: String,
    pub(super) commit: String,
    pub(super) total: usize,
    /// Per request: how many answers agree, of how many asked.
    pub(super) by_method: BTreeMap<String, (usize, usize)>,
    pub(super) differences: Vec<Difference>,
    /// The questions jdtls failed on (an internal error, not an answer),
    /// with what it said: left out of `total` and of the counts.
    pub(super) jdtls_failed: Vec<(String, String)>,
    pub(super) jdtls: Timing,
    pub(super) javak: Timing,
}

pub(super) struct Timing {
    pub(super) ready: bool,
    pub(super) import_seconds: f64,
    pub(super) answer_seconds: f64,
}

impl From<&Recording> for Timing {
    fn from(recording: &Recording) -> Self {
        Self {
            ready: recording.ready,
            import_seconds: recording.import_seconds,
            answer_seconds: recording.answer_seconds,
        }
    }
}

pub(super) struct Difference {
    pub(super) id: String,
    /// `-`, `+` and `@@` lines.
    pub(super) diff: Vec<String>,
}

impl RepoReport {
    pub(super) fn agreed(&self) -> usize {
        self.total - self.differences.len()
    }
}

pub(super) fn compare(jdtls: &Recording, javak: &Recording) -> RepoReport {
    let theirs: BTreeMap<&str, &Answer> = javak
        .answers
        .iter()
        .map(|answer| (answer.probe.id.as_str(), answer))
        .collect();
    let mut by_method: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut differences = Vec::new();
    let mut jdtls_failed = Vec::new();
    for expected in &jdtls.answers {
        // A question jdtls failed on has no answer to agree with.
        if let Some(said) = &expected.failed {
            jdtls_failed.push((expected.probe.id.clone(), said.clone()));
            continue;
        }
        let method = expected.probe.method.name();
        let counts = by_method.entry(method).or_default();
        counts.1 += 1;
        let actual = theirs
            .get(expected.probe.id.as_str())
            .map_or_else(|| vec!["(not asked)".to_owned()], |answer| answer.lines.clone());
        if actual == expected.lines {
            counts.0 += 1;
            continue;
        }
        differences.push(Difference {
            id: expected.probe.id.clone(),
            diff: diff(&expected.lines, &actual),
        });
    }
    RepoReport {
        repo: jdtls.repo.clone(),
        commit: jdtls.commit.clone(),
        total: jdtls.answers.len() - jdtls_failed.len(),
        by_method,
        differences,
        jdtls_failed,
        jdtls: jdtls.into(),
        javak: javak.into(),
    }
}

/// The lines only in `old` (`-`) and only in `new` (`+`), in order, with a
/// `@@ line N` marker where a run of changes starts.
pub(super) fn diff(old: &[String], new: &[String]) -> Vec<String> {
    let (n, m) = (old.len(), new.len());
    if n.saturating_mul(m) > 4_000_000 {
        // Too large to align: what each has that the other has not.
        let lines = old
            .iter()
            .filter(|line| !new.contains(line))
            .map(|line| format!("- {line}"))
            .chain(new.iter().filter(|line| !old.contains(line)).map(|line| format!("+ {line}")))
            .collect();
        return lines;
    }
    // The longest common subsequence, from the end.
    let mut common = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[at(i, j)] = if old[i] == new[j] {
                common[at(i + 1, j + 1)] + 1
            } else {
                common[at(i + 1, j)].max(common[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut lines = Vec::new();
    let mut in_change = false;
    let mark = |lines: &mut Vec<String>, in_change: &mut bool, line: usize| {
        if !*in_change && n + m > 2 {
            lines.push(format!("@@ line {}", line + 1));
        }
        *in_change = true;
    };
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            in_change = false;
            i += 1;
            j += 1;
        } else if i < n && (j == m || common[at(i + 1, j)] >= common[at(i, j + 1)]) {
            mark(&mut lines, &mut in_change, i);
            lines.push(format!("- {}", old[i]));
            i += 1;
        } else {
            mark(&mut lines, &mut in_change, i);
            lines.push(format!("+ {}", new[j]));
            j += 1;
        }
    }
    lines
}

/// The report as the terminal shows it.
pub(super) fn print(reports: &[RepoReport]) {
    let color = std::io::stderr().is_terminal();
    let paint = |code: &str, text: &str| {
        if color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    };
    for report in reports {
        eprintln!(
            "\n{}",
            paint(
                "1",
                &format!(
                    "━━ {} @ {} — {}/{} agree with jdtls",
                    report.repo,
                    short(&report.commit),
                    report.agreed(),
                    report.total
                )
            )
        );
        for (id, said) in &report.jdtls_failed {
            eprintln!("\n{} {} — jdtls failed, not compared: {said}", paint("90", "∅"), id);
        }
        for difference in &report.differences {
            eprintln!("\n{} {}", paint("33", "✗"), paint("1", &difference.id));
            for line in difference.diff.iter().take(TERMINAL_LINES) {
                let line = match line.chars().next() {
                    Some('-') => paint("31", line),
                    Some('+') => paint("32", line),
                    _ => paint("36", line),
                };
                eprintln!("    {line}");
            }
            if difference.diff.len() > TERMINAL_LINES {
                eprintln!("    … {} more lines", difference.diff.len() - TERMINAL_LINES);
            }
        }
    }
    eprintln!("\n{}", paint("1", "Summary (- jdtls, + javak-lsp)"));
    eprintln!("{}", summary_table(reports));
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(10)]
}

fn summary_table(reports: &[RepoReport]) -> String {
    let mut table = String::new();
    let _ = writeln!(
        table,
        "| project | agree | jdtls import | javak import | jdtls answers | javak answers |"
    );
    let _ = writeln!(table, "| --- | --- | --- | --- | --- | --- |");
    for report in reports {
        let ready = |timing: &Timing| {
            format!(
                "{:.1}s{}",
                timing.import_seconds,
                if timing.ready { "" } else { " (not ready)" }
            )
        };
        let _ = writeln!(
            table,
            "| {} | {}/{} | {} | {} | {:.1}s | {:.1}s |",
            report.repo,
            report.agreed(),
            report.total,
            ready(&report.jdtls),
            ready(&report.javak),
            report.jdtls.answer_seconds,
            report.javak.answer_seconds,
        );
    }
    let mut methods: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for report in reports {
        for (method, (agree, total)) in &report.by_method {
            let entry = methods.entry(method).or_default();
            entry.0 += agree;
            entry.1 += total;
        }
    }
    let _ = writeln!(table, "\n| request | agree |");
    let _ = writeln!(table, "| --- | --- |");
    for (method, (agree, total)) in methods {
        let mark = if agree == total { "✅" } else { "❌" };
        let _ = writeln!(table, "| {method} | {mark} {agree}/{total} |");
    }
    table
}

/// The report as a pull request comment: the summary, then each project's
/// differences folded away.
/// The report as Markdown. `seed`, when the questions were random, is
/// shown under the heading with the command that asks them again.
pub(super) fn markdown(reports: &[RepoReport], seed: Option<u64>) -> String {
    let agreed: usize = reports.iter().map(RepoReport::agreed).sum();
    let total: usize = reports.iter().map(|report| report.total).sum();
    let questions = match seed {
        Some(seed) => format!(
            "Random questions, seed **`{seed}`**. Replay: `just benchmark \"\" {seed}` \
             (`JDTLS_COMPAT_SEED={seed}`), or run this workflow with seed `{seed}`.\n\n"
        ),
        None => "Evenly spaced questions.\n\n".to_owned(),
    };
    let failed: usize = reports.iter().map(|report| report.jdtls_failed.len()).sum();
    let left_out = if failed == 0 {
        String::new()
    } else {
        format!(" {failed} more questions were not compared: jdtls failed on them (below).")
    };
    let mut body = format!(
        "{COMMENT_MARK}\n## javak-lsp vs jdtls\n\n{questions}**{agreed}/{total}** answers agree with jdtls.{left_out}\n\n{}",
        summary_table(reports)
    );
    if failed > 0 {
        body.push_str("\n<details><summary>Not compared: jdtls failed</summary>\n\n");
        for report in reports {
            for (id, said) in &report.jdtls_failed {
                let _ = writeln!(body, "- `{id}`: {said}");
            }
        }
        body.push_str("\n</details>\n");
    }
    let mut truncated = false;
    for report in reports {
        if report.differences.is_empty() {
            continue;
        }
        let mut section = format!(
            "\n<details><summary><b>{}</b> @ <code>{}</code>: {} differ</summary>\n\n",
            report.repo,
            short(&report.commit),
            report.differences.len()
        );
        for difference in &report.differences {
            let mut block = format!("`{}`\n```diff\n", difference.id);
            for line in difference.diff.iter().take(COMMENT_LINES) {
                block.push_str(line);
                block.push('\n');
            }
            if difference.diff.len() > COMMENT_LINES {
                let _ = writeln!(block, "… {} more lines", difference.diff.len() - COMMENT_LINES);
            }
            block.push_str("```\n");
            if body.len() + section.len() + block.len() > COMMENT_LIMIT {
                truncated = true;
                break;
            }
            section.push_str(&block);
        }
        section.push_str("\n</details>\n");
        body.push_str(&section);
        if truncated {
            break;
        }
    }
    if truncated {
        body.push_str("\n_The rest is cut; the full report is in the workflow's artifacts._\n");
    }
    body
}

/// Adds the report to the pull request of this branch, or replaces the one
/// an earlier run added. Needs `gh` and `GITHUB_REPOSITORY`; the pull
/// request is `JDTLS_COMPAT_PR`, else the open one whose head is this
/// branch.
pub(super) fn comment_on_pull_request(body_file: &Path) {
    let Ok(repository) = std::env::var("GITHUB_REPOSITORY") else {
        eprintln!("not commenting: GITHUB_REPOSITORY is not set");
        return;
    };
    let pull_request = std::env::var("JDTLS_COMPAT_PR")
        .ok()
        .filter(|number| !number.is_empty())
        .or_else(|| {
            let branch = std::env::var("GITHUB_HEAD_REF")
                .ok()
                .filter(|branch| !branch.is_empty())
                .or_else(|| std::env::var("GITHUB_REF_NAME").ok())?;
            gh(&[
                "pr", "list", "--repo", &repository, "--head", &branch, "--state", "open",
                "--json", "number", "--jq", ".[0].number",
            ])
        });
    let Some(pull_request) = pull_request else {
        eprintln!("not commenting: no open pull request for this branch");
        return;
    };
    let existing = gh(&[
        "api",
        "--paginate",
        &format!("repos/{repository}/issues/{pull_request}/comments"),
        "--jq",
        &format!(".[] | select(.body | contains(\"{COMMENT_MARK}\")) | .id"),
    ])
    .and_then(|ids| ids.lines().next().map(str::to_owned));
    let body = format!("body=@{}", body_file.display());
    let outcome = match &existing {
        Some(id) => gh(&[
            "api", "-X", "PATCH", &format!("repos/{repository}/issues/comments/{id}"), "-F", &body,
            "--jq", ".html_url",
        ]),
        None => gh(&[
            "api", "-X", "POST", &format!("repos/{repository}/issues/{pull_request}/comments"),
            "-F", &body, "--jq", ".html_url",
        ]),
    };
    match outcome {
        Some(url) => eprintln!(
            "{} the comment on #{pull_request}: {url}",
            if existing.is_some() { "updated" } else { "added" }
        ),
        None => eprintln!("commenting on #{pull_request} failed"),
    }
}

/// What `gh args…` prints, trimmed; `None` when it fails or prints nothing.
fn gh(arguments: &[&str]) -> Option<String> {
    let output = Command::new("gh").args(arguments).output().ok()?;
    if !output.status.success() {
        eprintln!("gh {}: {}", arguments[0], String::from_utf8_lossy(&output.stderr).trim());
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    fn recording(server: &str, answers: Vec<(&str, &str, Option<&str>)>) -> Recording {
        let answers = answers
            .into_iter()
            .map(|(id, line, failed)| Answer {
                probe: serde_json::from_value(serde_json::json!({
                    "id": id, "file": "A.java", "method": "References",
                    "line": 0, "character": 0, "word": "a", "edit": null,
                }))
                .expect("a probe"),
                lines: vec![line.to_owned()],
                millis: 0,
                failed: failed.map(str::to_owned),
            })
            .collect();
        Recording {
            server: server.to_owned(),
            repo: "demo".to_owned(),
            commit: "0".to_owned(),
            ready: true,
            import_seconds: 0.0,
            answer_seconds: 0.0,
            answers,
        }
    }

    #[test]
    fn a_question_jdtls_failed_on_is_not_compared() {
        let jdtls = recording(
            "jdtls",
            vec![("one", "x", None), ("two", "(failed: NullPointerException)", Some("NullPointerException"))],
        );
        let javak = recording("javak-lsp", vec![("one", "x", None), ("two", "y", None)]);
        let report = compare(&jdtls, &javak);
        assert_eq!((report.agreed(), report.total), (1, 1));
        assert!(report.differences.is_empty());
        assert_eq!(report.jdtls_failed, [("two".to_owned(), "NullPointerException".to_owned())]);
    }

    #[test]
    fn a_question_javak_failed_on_differs() {
        let jdtls = recording("jdtls", vec![("one", "x", None)]);
        let javak = recording("javak-lsp", vec![("one", "(failed: boom)", Some("boom"))]);
        let report = compare(&jdtls, &javak);
        assert_eq!((report.agreed(), report.total), (0, 1));
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let diff = diff(&lines("a b c d"), &lines("a x c d e"));
        assert_eq!(diff, ["@@ line 2", "- b", "+ x", "@@ line 5", "+ e"]);
    }

    #[test]
    fn a_one_line_answer_has_no_marker() {
        assert_eq!(diff(&lines("a"), &lines("b")), ["- a", "+ b"]);
    }
}
