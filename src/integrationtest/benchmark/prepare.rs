//! Getting a project and the two servers ready: the clone, its libraries,
//! and where jdtls and javak-lsp are.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::catalog::{Build, Repo};

/// How long the build tool may take to fetch a project's libraries before
/// it is stopped and the servers import what is there.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The jdtls milestone downloaded when none is installed.
const JDTLS_VERSION: &str = "1.63.0";

/// A server: the program and the arguments it starts with.
#[derive(Clone)]
pub(super) struct ServerCommand {
    pub(super) program: String,
    pub(super) arguments: Vec<String>,
}

impl ServerCommand {
    fn parse(command: &str) -> Self {
        let mut words = command.split_whitespace().map(str::to_owned);
        let program = words.next().expect("a command is not empty");
        Self {
            program,
            arguments: words.collect(),
        }
    }
}

/// Clones `repo` into `destination` at its revision, one commit deep, and
/// returns the commit it got.
pub(super) fn clone(repo: &Repo, destination: &Path) -> String {
    run(
        Command::new("git")
            .args(["-c", "advice.detachedHead=false", "clone", "--quiet", "--depth", "1"])
            .args(["--branch", repo.rev, repo.url])
            .arg(destination),
        "git clone",
    )
    .unwrap_or_else(|error| panic!("{}: {error}", repo.name));
    for file in repo.remove {
        let _ = std::fs::remove_file(destination.join(file));
    }
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(destination)
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&head.stdout).trim().to_owned()
}

/// Asks the project's build tool for its libraries and their sources, so
/// neither server is the one that pays for the download. A failure is
/// reported and not fatal: each server still imports what it can.
pub(super) fn fetch_dependencies(repo: &Repo, project: &Path) {
    let started = std::time::Instant::now();
    let outcome = match repo.build {
        Build::Maven => {
            eprintln!("  fetching libraries with maven…");
            run_logged(
                Command::new(maven(project))
                    .args(["-B", "-DskipTests"])
                    // Maven waits 30 minutes for a stalled download by
                    // default; give up on one sooner and try it again.
                    .args([
                        "-Daether.connector.connectTimeout=15000",
                        "-Daether.connector.requestTimeout=60000",
                        "-Daether.connector.http.retryHandler.count=3",
                    ])
                    // In a multi-module project a module's libraries
                    // include its sibling modules, which no repository
                    // has: Maven hands them over from the reactor only
                    // once their `compile` phase has run. Running the
                    // phase with the compiler skipped is enough, and
                    // works where compiling would not (an annotation
                    // processor too old for the JDK, such as Lombok).
                    .args(["-Dmaven.main.skip=true", "compile"])
                    .args(["dependency:resolve", "dependency:sources"])
                    .current_dir(project),
                "maven",
            )
        }
        Build::Gradle => {
            eprintln!("  fetching libraries with gradle…");
            run_logged(
                Command::new(gradle(project))
                    .args(["--no-daemon", "--console=plain", "dependencies"])
                    .current_dir(project),
                "gradle",
            )
        }
    };
    match outcome {
        Ok(()) => eprintln!("  libraries fetched in {:.1}s", started.elapsed().as_secs_f64()),
        Err(error) => eprintln!("{}: fetching libraries failed, going on: {error}", repo.name),
    }
}

fn maven(project: &Path) -> PathBuf {
    let wrapper = project.join("mvnw");
    if wrapper.is_file() {
        make_executable(&wrapper);
        return wrapper;
    }
    PathBuf::from("mvn")
}

fn gradle(project: &Path) -> PathBuf {
    let wrapper = project.join("gradlew");
    if wrapper.is_file() {
        make_executable(&wrapper);
        return wrapper;
    }
    PathBuf::from("gradle")
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// A copy of `from` at `to`, so each server reads a project nobody else
/// has touched: jdtls leaves `.project`, `.classpath` and `.settings`
/// behind, and javak-lsp must not see them.
pub(super) fn copy_project(from: &Path, to: &Path) {
    run(Command::new("cp").arg("-R").arg(from).arg(to), "cp -R")
        .unwrap_or_else(|error| panic!("copying {}: {error}", from.display()));
}

/// jdtls: `JDTLS_CMD`, else the `jdtls` on the `PATH`, else a milestone
/// downloaded once into the cache.
pub(super) fn jdtls() -> ServerCommand {
    if let Ok(command) = std::env::var("JDTLS_CMD") {
        return ServerCommand::parse(&command);
    }
    if let Some(found) = on_path("jdtls") {
        return ServerCommand::parse(&found.to_string_lossy());
    }
    let launcher = download_jdtls();
    ServerCommand {
        program: "python3".to_owned(),
        arguments: vec![launcher.to_string_lossy().into_owned()],
    }
}

/// javak-lsp: `JAVAK_LSP_CMD`, else this checkout's release build, else
/// its debug build. `just benchmark` builds the release one first.
pub(super) fn javak() -> ServerCommand {
    if let Ok(command) = std::env::var("JAVAK_LSP_CMD") {
        return ServerCommand::parse(&command);
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let binary = ["release", "debug"]
        .iter()
        .map(|profile| target.join(profile).join("javak-lsp"))
        .find(|binary| binary.is_file())
        .unwrap_or_else(|| {
            panic!("no javak-lsp binary: run `cargo build --release` or set JAVAK_LSP_CMD")
        });
    ServerCommand::parse(&binary.to_string_lossy())
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// The launcher of the jdtls milestone in `~/.cache/javak-lsp/jdtls`,
/// downloaded and unpacked when it is not there yet.
fn download_jdtls() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is set");
    let directory = Path::new(&home)
        .join(".cache/javak-lsp/jdtls")
        .join(JDTLS_VERSION);
    let launcher = directory.join("bin/jdtls");
    if launcher.is_file() {
        return launcher;
    }
    let base = format!("https://download.eclipse.org/jdtls/milestones/{JDTLS_VERSION}");
    let latest = Command::new("curl")
        .args(["-fsSL", &format!("{base}/latest.txt")])
        .output()
        .expect("curl runs");
    assert!(latest.status.success(), "jdtls {JDTLS_VERSION}: latest.txt not found");
    let archive = String::from_utf8_lossy(&latest.stdout).trim().to_owned();
    std::fs::create_dir_all(&directory).expect("the cache is writable");
    eprintln!("downloading jdtls {JDTLS_VERSION} ({archive})");
    let tarball = directory.join("jdtls.tar.gz");
    run(
        Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&tarball)
            .arg(format!("{base}/{archive}")),
        "curl jdtls",
    )
    .expect("jdtls downloads");
    run(
        Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&directory),
        "tar",
    )
    .expect("jdtls unpacks");
    let _ = std::fs::remove_file(&tarball);
    assert!(launcher.is_file(), "{} is in the archive", launcher.display());
    launcher
}

/// Runs `command`, printing each line it writes as `[what] line`, so a
/// long download is seen to be moving. The last lines are kept for the
/// error when it fails.
fn run_logged(command: &mut Command, what: &str) -> Result<(), String> {
    use std::io::BufRead;
    use std::sync::{Arc, Mutex};
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("{what} does not start: {error}"))?;
    let tail = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let readers: Vec<_> = [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|pipe| {
        let tail = Arc::clone(&tail);
        let what = what.to_owned();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                eprintln!("    [{what}] {line}");
                let mut tail = tail.lock().expect("the tail is not poisoned");
                if tail.len() == 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
    })
    .collect();
    let deadline = std::time::Instant::now() + FETCH_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("{what} is lost: {error}"))?
        {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            for reader in readers {
                let _ = reader.join();
            }
            return Err(format!(
                "{what} stopped after {}s without finishing",
                FETCH_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    for reader in readers {
        let _ = reader.join();
    }
    if status.success() {
        return Ok(());
    }
    let tail = tail.lock().expect("the tail is not poisoned");
    Err(format!(
        "{what} failed ({status}):\n{}",
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    ))
}

/// Runs `command`, its output kept back unless it fails.
fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("{what} does not start: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tail: Vec<&str> = stderr.lines().chain(stdout.lines()).collect();
    let tail = tail[tail.len().saturating_sub(20)..].join("\n");
    Err(format!("{what} failed ({}):\n{tail}", output.status))
}
