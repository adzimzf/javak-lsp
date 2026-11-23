//! The platform's own sources, which no build tool lists as a dependency:
//! the JDK's `src.zip` and the Kotlin standard library's sources jar.
//!
//! - **JDK** — `src.zip` of the JDK the client names (`javaHome`), else
//!   `$JAVA_HOME`, the login shell's `JAVA_HOME`, the `java` on the `PATH`,
//!   and the usual install locations (`/usr/lib/jvm`,
//!   `/Library/Java/JavaVirtualMachines`, SDKMAN, Gradle's toolchains, …).
//! - **Kotlin** — `kotlin-stdlib-<version>-sources.jar` from the Gradle or
//!   Maven cache, the version the project depends on when it is there, else
//!   the newest; else the `kotlin-stdlib-sources.jar` of a `kotlinc`
//!   install.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use super::decompile::java_executable;
use super::{
    InstallLocations, installed_tool, login_shell_environment, login_shell_path, search_dirs,
};

/// The group and artifact of the Kotlin standard library.
pub(crate) const KOTLIN_STDLIB: (&str, &str) = ("org.jetbrains.kotlin", "kotlin-stdlib");

const KOTLIN_INSTALL: InstallLocations = InstallLocations {
    home_variables: &["KOTLIN_HOME"],
    sdkman_candidate: "kotlin",
    directories: &[
        "/usr/share/kotlin/bin",
        "/opt/kotlin/bin",
        "/opt/kotlinc/bin",
    ],
};

/// Directories that hold one JDK per entry.
fn jdk_parents() -> Vec<PathBuf> {
    let mut parents: Vec<PathBuf> = [
        "/usr/lib/jvm",
        "/usr/java",
        "/opt/java",
        "/opt/jdk",
        "/Library/Java/JavaVirtualMachines",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = crate::util::home_dir() {
        for below in [
            "Library/Java/JavaVirtualMachines",
            ".sdkman/candidates/java",
            ".gradle/jdks",
            ".jdks",
            ".asdf/installs/java",
            ".local/share/mise/installs/java",
        ] {
            parents.push(home.join(below));
        }
    }
    parents
}

/// The JDK's `src.zip`: of `configured` when the client names a JDK, else
/// of the first JDK found that ships one.
pub(crate) fn jdk_sources(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(home) = configured {
        return src_zip(home);
    }
    let from_environment = std::env::var_os("JAVA_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .and_then(|home| src_zip(&home));
    if from_environment.is_some() {
        return from_environment;
    }
    let from_java = java_executable()
        .and_then(|java| std::fs::canonicalize(java).ok())
        .and_then(|java| Some(java.parent()?.parent()?.to_path_buf()))
        .and_then(|home| src_zip(&home));
    if from_java.is_some() {
        return from_java;
    }
    let from_shell = login_shell_environment()
        .java_home
        .as_ref()
        .and_then(|home| src_zip(Path::new(home)));
    if from_shell.is_some() {
        return from_shell;
    }
    if let Some(found) = macos_java_home().and_then(|home| src_zip(&home)) {
        return Some(found);
    }
    let mut homes: Vec<PathBuf> = jdk_parents()
        .iter()
        .filter_map(|parent| std::fs::read_dir(parent).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    homes.sort_by(|left, right| compare_versions(&file_name(right), &file_name(left)));
    homes.iter().find_map(|home| src_zip(home))
}

/// `src.zip` of a JDK home: `lib/src.zip` since JDK 9, `src.zip` before;
/// a JDK 8 `jre` directory's is one level up, and a macOS bundle's below
/// `Contents/Home`.
pub(crate) fn src_zip(home: &Path) -> Option<PathBuf> {
    let homes = [
        home.to_path_buf(),
        home.join("Contents").join("Home"),
        home.parent().map(Path::to_path_buf).unwrap_or_default(),
    ];
    homes
        .iter()
        .flat_map(|home| [home.join("lib").join("src.zip"), home.join("src.zip")])
        .find(|candidate| candidate.is_file())
}

/// What `/usr/libexec/java_home` answers on macOS.
fn macos_java_home() -> Option<PathBuf> {
    let tool = Path::new("/usr/libexec/java_home");
    if !cfg!(target_os = "macos") || !tool.is_file() {
        return None;
    }
    let output = std::process::Command::new(tool).output().ok()?;
    let home = String::from_utf8(output.stdout).ok()?;
    let home = home.trim();
    (output.status.success() && !home.is_empty()).then(|| PathBuf::from(home))
}

/// The Kotlin standard library's sources jar: of `version` when it is
/// cached, else the newest cached, else a `kotlinc` install's.
pub(crate) fn kotlin_stdlib_sources(version: Option<&str>) -> Option<PathBuf> {
    let mut cached = cached_artifact_sources(KOTLIN_STDLIB.0, KOTLIN_STDLIB.1);
    if let Some(wanted) = version
        && let Some((_, jar)) = cached.iter().find(|(cached, _)| cached == wanted)
    {
        return Some(jar.clone());
    }
    cached.sort_by(|left, right| compare_versions(&right.0, &left.0));
    cached
        .into_iter()
        .map(|(_, jar)| jar)
        .next()
        .or_else(kotlinc_stdlib_sources)
}

/// `(version, sources jar)` of every version of an artifact in the Gradle
/// and Maven caches.
fn cached_artifact_sources(group: &str, artifact: &str) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let gradle_home = std::env::var_os("GRADLE_USER_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::util::home_dir().map(|home| home.join(".gradle")));
    if let Some(gradle_home) = gradle_home {
        let module = gradle_home
            .join("caches/modules-2/files-2.1")
            .join(group)
            .join(artifact);
        for version in subdirectories(&module) {
            let name = file_name(&version);
            let jar_name = format!("{artifact}-{name}-sources.jar");
            let jar = subdirectories(&version)
                .into_iter()
                .map(|hashed| hashed.join(&jar_name))
                .find(|jar| jar.is_file());
            if let Some(jar) = jar {
                found.push((name, jar));
            }
        }
    }
    if let Some(home) = crate::util::home_dir() {
        let module = group
            .split('.')
            .fold(home.join(".m2/repository"), |path, part| path.join(part))
            .join(artifact);
        for version in subdirectories(&module) {
            let name = file_name(&version);
            let jar = version.join(format!("{artifact}-{name}-sources.jar"));
            if jar.is_file() {
                found.push((name, jar));
            }
        }
    }
    found
}

/// `lib/kotlin-stdlib-sources.jar` of the `kotlinc` on the `PATH` or in
/// its usual install locations.
fn kotlinc_stdlib_sources() -> Option<PathBuf> {
    let kotlinc = installed_tool("kotlinc", &search_dirs(&KOTLIN_INSTALL))
        .or_else(|| installed_tool("kotlinc", login_shell_path()))?;
    let home = std::fs::canonicalize(kotlinc)
        .ok()?
        .parent()?
        .parent()?
        .to_path_buf();
    let jar = home.join("lib").join("kotlin-stdlib-sources.jar");
    jar.is_file().then_some(jar)
}

fn subdirectories(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Orders `2.0.21` before `2.2.20`, `21.0.2` before `25`, and a release
/// after its pre-releases (`2.1.0-Beta1` before `2.1.0`). Names without a
/// number (`default-java`) come first.
pub(crate) fn compare_versions(left: &str, right: &str) -> Ordering {
    let numbers = |version: &str| -> Vec<u64> {
        version
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let release = |version: &str| {
        let lower = version.to_ascii_lowercase();
        !["alpha", "beta", "rc", "dev", "snapshot", "-m"]
            .iter()
            .any(|marker| lower.contains(marker))
    };
    let (left_numbers, right_numbers) = (numbers(left), numbers(right));
    let common = left_numbers
        .iter()
        .zip(&right_numbers)
        .map(|(left, right)| left.cmp(right))
        .find(|order| order.is_ne());
    common
        .or_else(|| match left_numbers.len().cmp(&right_numbers.len()) {
            Ordering::Equal => None,
            // `2.1.0` and `2.1.0-Beta1` share their first numbers: the
            // release is newer.
            Ordering::Less if release(left) && !release(right) => Some(Ordering::Greater),
            Ordering::Greater if release(right) && !release(left) => Some(Ordering::Less),
            order => Some(order),
        })
        .unwrap_or_else(|| release(left).cmp(&release(right)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_by_their_numbers() {
        assert_eq!(compare_versions("2.0.21", "2.2.20"), Ordering::Less);
        assert_eq!(
            compare_versions("java-21-openjdk-amd64", "java-8-openjdk-amd64"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("2.1.0-Beta1", "2.1.0"), Ordering::Less);
        assert_eq!(compare_versions("2.1.0", "2.1.0-RC2"), Ordering::Greater);
        assert_eq!(
            compare_versions("default-java", "openjdk-21"),
            Ordering::Less
        );
        assert_eq!(compare_versions("1.9.22", "1.9.22"), Ordering::Equal);
    }

    #[test]
    fn src_zip_is_found_where_each_jdk_keeps_it() {
        let directory = tempfile::tempdir().unwrap();
        let modern = directory.path().join("jdk-21");
        std::fs::create_dir_all(modern.join("lib")).unwrap();
        std::fs::write(modern.join("lib/src.zip"), b"").unwrap();
        assert_eq!(src_zip(&modern), Some(modern.join("lib/src.zip")));

        let old = directory.path().join("jdk1.8.0");
        std::fs::create_dir_all(old.join("jre")).unwrap();
        std::fs::write(old.join("src.zip"), b"").unwrap();
        assert_eq!(src_zip(&old.join("jre")), Some(old.join("src.zip")));

        let bundle = directory.path().join("temurin-17.jdk");
        std::fs::create_dir_all(bundle.join("Contents/Home/lib")).unwrap();
        std::fs::write(bundle.join("Contents/Home/lib/src.zip"), b"").unwrap();
        assert_eq!(
            src_zip(&bundle),
            Some(bundle.join("Contents/Home/lib/src.zip"))
        );

        assert_eq!(src_zip(&directory.path().join("jre-only")), None);
    }
}
