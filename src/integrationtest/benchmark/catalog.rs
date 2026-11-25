//! The projects the comparison is run on.
//!
//! Each is a real Java project, cloned at a fixed tag where it has one, so
//! two runs ask the same questions of the same code. The list is short on
//! purpose: jdtls imports each one before it answers anything, and that
//! is most of a run's time.

/// The build tool that fetches a project's libraries before either server
/// starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Build {
    Maven,
    Gradle,
}

pub(super) struct Repo {
    /// What the report calls it, and what `JDTLS_COMPAT_REPOS` selects.
    pub(super) name: &'static str,
    pub(super) url: &'static str,
    /// A tag or a branch: `git clone --branch` takes either.
    pub(super) rev: &'static str,
    pub(super) build: Build,
    /// Files removed after the clone, below the project root: a project
    /// with both a `pom.xml` and a `build.gradle` is made one of the two,
    /// so both servers import it the same way.
    pub(super) remove: &'static [&'static str],
}

pub(super) const REPOS: &[Repo] = &[
    // Repo {
    //     name: "javapoet",
    //     url: "https://github.com/square/javapoet",
    //     rev: "javapoet-1.13.0",
    //     build: Build::Maven,
    //     remove: &[],
    // },
    // Repo {
    //     name: "commons-cli",
    //     url: "https://github.com/apache/commons-cli",
    //     rev: "rel/commons-cli-1.9.0",
    //     build: Build::Maven,
    //     remove: &[],
    // },
    Repo {
        name: "jsoup",
        url: "https://github.com/jhy/jsoup",
        rev: "jsoup-1.17.2",
        build: Build::Maven,
        remove: &[],
    },
    // Repo {
    //     name: "spring-petclinic",
    //     url: "https://github.com/spring-projects/spring-petclinic",
    //     rev: "main",
    //     build: Build::Gradle,
    //     remove: &["pom.xml"],
    // },
];
