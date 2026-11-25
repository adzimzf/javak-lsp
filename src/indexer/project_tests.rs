use std::path::{Path, PathBuf};

use super::*;

fn write(root: &Path, path: &str, text: &str) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn modules(projects: &Projects, root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = projects
        .modules
        .iter()
        .map(|module| relative(&module.dir, root))
        .collect();
    dirs.sort();
    dirs
}

fn relative(path: &Path, root: &Path) -> String {
    let below = path
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    if below.is_empty() {
        ".".to_owned()
    } else {
        below
    }
}

fn root_of(projects: &Projects, root: &Path, file: &str, language: Language) -> Option<String> {
    projects
        .source_root_of(&root.join(file), language)
        .map(|dir| relative(dir, root))
}

const PARENT_POM: &str = r#"<project>
  <properties><maven.compiler.release>17</maven.compiler.release></properties>
  <modules>
    <module>core</module>
    <module>./tools/cli</module>
    <!-- <module>gone</module> -->
  </modules>
  <profiles><profile><modules><module>extra</module></modules></profile></profiles>
</project>"#;

#[test]
fn a_maven_parent_brings_its_modules_their_own_source_roots_and_its_level() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "pom.xml", PARENT_POM);
    write(
        root,
        "core/pom.xml",
        "<project><modules><module>nested</module></modules></project>",
    );
    write(root, "core/nested/pom.xml", "<project/>");
    write(
        root,
        "tools/cli/pom.xml",
        "<project><properties><maven.compiler.release>21</maven.compiler.release></properties>\
         <build><sourceDirectory>${project.basedir}/src/java</sourceDirectory>\
         <testSourceDirectory>tests</testSourceDirectory></build></project>",
    );
    write(root, "extra/pom.xml", "<project/>");
    write(root, "gone/pom.xml", "<project/>");

    let projects = Projects::discover(root);
    assert_eq!(
        modules(&projects, root),
        [".", "core", "core/nested", "extra", "tools/cli"],
        "every module a parent lists, profiles included; not a commented one"
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "core/src/main/java/a/A.java",
            Language::Java
        )
        .as_deref(),
        Some("core/src/main/java")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "core/nested/src/test/java/a/A.java",
            Language::Java
        )
        .as_deref(),
        Some("core/nested/src/test/java"),
        "the innermost module's root, not its parent's"
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "tools/cli/src/java/a/A.java",
            Language::Java
        )
        .as_deref(),
        Some("tools/cli/src/java"),
        "<sourceDirectory>, with ${{project.basedir}}"
    );
    assert_eq!(
        root_of(&projects, root, "tools/cli/tests/a/A.java", Language::Java).as_deref(),
        Some("tools/cli/tests")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "tools/cli/src/main/java/a/A.java",
            Language::Java
        ),
        None,
        "a replaced default is no root"
    );
    let level = |file: &str| projects.java_level_of(&root.join(file));
    assert_eq!(
        level("core/src/main/java/A.java"),
        Some(17),
        "inherited from the parent"
    );
    assert_eq!(
        level("core/nested/src/main/java/A.java"),
        Some(17),
        "through two parents"
    );
    assert_eq!(
        level("tools/cli/src/java/A.java"),
        Some(21),
        "a module's own level"
    );
}

#[test]
fn a_gradle_settings_file_includes_its_projects_where_it_says_they_are() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "settings.gradle.kts",
        r#"rootProject.name = "shop"
// include(":commented")
include(":app", ":services:api")
include(
    ":libs:moved",
)
project(":libs:moved").projectDir = file("third_party/moved")
includeBuild("build-logic")
"#,
    );
    write(
        root,
        "build.gradle.kts",
        "java { sourceCompatibility = JavaVersion.VERSION_11 }",
    );
    write(
        root,
        "services/api/build.gradle.kts",
        "java { toolchain { languageVersion.set(JavaLanguageVersion.of(21)) } }",
    );
    write(
        root,
        "build-logic/settings.gradle",
        "include 'conventions',\n    'checks'\n",
    );
    write(root, "groovy/settings.gradle", "include 'one'");

    let projects = Projects::discover(root);
    assert_eq!(
        modules(&projects, root),
        [
            ".",
            "app",
            "build-logic",
            "build-logic/checks",
            "build-logic/conventions",
            "services/api",
            "third_party/moved",
        ],
        "a build inside a build is its own to describe: groovy/ is not one"
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "services/api/src/main/java/a/A.java",
            Language::Java
        )
        .as_deref(),
        Some("services/api/src/main/java")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "third_party/moved/src/main/java/A.java",
            Language::Java
        )
        .as_deref(),
        Some("third_party/moved/src/main/java")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "app/src/main/kotlin/A.kt",
            Language::Kotlin
        )
        .as_deref(),
        Some("app/src/main/kotlin")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "app/src/main/kotlin/A.java",
            Language::Java
        ),
        None,
        "Java is not compiled from a Kotlin root"
    );
    let level = |file: &str| projects.java_level_of(&root.join(file));
    assert_eq!(
        level("app/src/main/java/A.java"),
        Some(11),
        "the root build's level"
    );
    assert_eq!(level("services/api/src/main/java/A.java"), Some(21));
}

#[test]
fn independent_builds_side_by_side_are_each_found() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "services/billing/pom.xml", "<project/>");
    write(root, "services/search/build.gradle", "");
    write(root, "services/billing/target/pom.xml", "<project/>");
    write(root, "node_modules/x/pom.xml", "<project/>");
    write(root, "a/b/c/d/e/f/pom.xml", "<project/>");

    let projects = Projects::discover(root);
    assert_eq!(
        modules(&projects, root),
        ["services/billing", "services/search"],
        "not build output, node_modules, or deeper than jdtls looks"
    );
    assert_eq!(
        root_of(&projects, root, "src/main/java/A.java", Language::Java),
        None,
        "a file outside every build is on no source root"
    );
}

#[test]
fn a_workspace_without_a_build_is_one_module() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let projects = Projects::discover(root);
    assert_eq!(modules(&projects, root), ["."]);
    assert_eq!(
        root_of(&projects, root, "src/main/java/a/A.java", Language::Java).as_deref(),
        Some("src/main/java")
    );
}

#[test]
fn what_gradle_reports_replaces_what_the_files_suggest() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "settings.gradle", "include ':app'");
    let mut projects = Projects::discover(root);
    let at = |path: &str| -> PathBuf { root.join(path) };
    projects.apply_reported(&[
        ReportedModule {
            dir: at("app"),
            java: vec![at("app/src"), at("app/generated")],
            kotlin: vec![at("app/src"), at("app/kt")],
            java_level: Some("17".to_owned()),
        },
        ReportedModule {
            dir: at("plugin-added"),
            java: vec![at("plugin-added/src/main/java")],
            kotlin: Vec::new(),
            java_level: None,
        },
    ]);
    assert_eq!(
        root_of(&projects, root, "app/src/a/A.java", Language::Java).as_deref(),
        Some("app/src")
    );
    assert_eq!(
        root_of(
            &projects,
            root,
            "app/src/main/java/a/A.java",
            Language::Java
        )
        .as_deref(),
        Some("app/src"),
        "the default is gone: the package starts at app/src"
    );
    assert_eq!(
        root_of(&projects, root, "app/kt/A.java", Language::Java),
        None
    );
    assert_eq!(
        root_of(&projects, root, "app/kt/A.kt", Language::Kotlin).as_deref(),
        Some("app/kt")
    );
    assert_eq!(projects.java_level_of(&at("app/src/A.java")), Some(17));
    assert_eq!(
        root_of(
            &projects,
            root,
            "plugin-added/src/main/java/A.java",
            Language::Java
        )
        .as_deref(),
        Some("plugin-added/src/main/java"),
        "a project the settings file did not show"
    );
}

#[test]
fn the_names_a_settings_file_includes_are_read_in_both_dialects() {
    let settings = settings_without_comments(
        "include 'a', \"b\"\ninclude(\":c:d\") // include ':e'\nincludeFlat 'f'\nincludeBuild '../g'\n/* include 'h' */",
    );
    assert_eq!(included(&settings, "include"), ["a", "b", ":c:d"]);
    assert_eq!(included(&settings, "includeFlat"), ["f"]);
    assert_eq!(included(&settings, "includeBuild"), ["../g"]);
}
