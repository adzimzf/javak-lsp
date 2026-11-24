use super::*;

/// A two-module Gradle workspace: `app` (runnable) depends on `core`.
fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    std::fs::write(path.join("settings.gradle.kts"), "include(\":app\", \":core\")").unwrap();
    for module in ["app", "core"] {
        std::fs::create_dir_all(path.join(module).join("src/main/kotlin")).unwrap();
        std::fs::write(path.join(module).join("build.gradle.kts"), "").unwrap();
        std::fs::create_dir_all(path.join(module).join("build/classes/kotlin/main")).unwrap();
        std::fs::create_dir_all(path.join(module).join("build/classes/kotlin/test")).unwrap();
    }
    // Generated output is never a module.
    std::fs::create_dir_all(path.join("app/build/generated")).unwrap();
    std::fs::write(path.join("app/build/generated/build.gradle.kts"), "").unwrap();
    root
}

fn read(indexer: &Indexer, path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    assert!(indexer.reread(&Url::from_file_path(path).unwrap()));
}

fn indexer_at(root: &Path) -> Indexer {
    let indexer = Indexer::new();
    indexer.workspace_root.set(root.to_path_buf()).unwrap();
    indexer
}

#[test]
fn the_module_of_a_source_file_is_its_nearest_build_directory() {
    let root = workspace();
    let file = root.path().join("app/src/main/kotlin/Main.kt");
    assert_eq!(module_dir_of(&file, root.path()), root.path().join("app"));
    assert_eq!(project_name(&root.path().join("app"), root.path()), "app");
    let root_name = root.path().file_name().unwrap().to_string_lossy();
    assert_eq!(project_name(root.path(), root.path()), root_name);
}

#[test]
fn modules_are_not_looked_for_in_build_output() {
    let root = workspace();
    assert_eq!(
        module_dirs(root.path()),
        vec![root.path().to_path_buf(), root.path().join("app"), root.path().join("core")]
    );
}

#[test]
fn main_classes_carry_their_module_as_project_name() {
    let root = workspace();
    let indexer = indexer_at(root.path());
    read(
        &indexer,
        &root.path().join("app/src/main/kotlin/Main.kt"),
        "package com.example\n\nfun main() {\n    println(1)\n}\n",
    );
    read(
        &indexer,
        &root.path().join("core/src/main/kotlin/Util.kt"),
        "package com.example.core\n\nfun util() = 1\n",
    );

    let found = resolve_main_classes(&indexer, None);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].main_class, "com.example.MainKt");
    assert_eq!(found[0].project_name, "app");
    assert!(resolve_main_classes(&indexer, Some(&root.path().join("core"))).is_empty());
}

#[test]
fn the_classpath_puts_the_launched_module_first() {
    let root = workspace();
    let indexer = indexer_at(root.path());
    read(&indexer, &root.path().join("app/src/main/kotlin/Main.kt"), "package com.example\n\nfun main() {}\n");

    let runtime = resolve_classpath(&indexer, "com.example.MainKt", None, ClasspathScope::Runtime);
    let app = root.path().join("app/build/classes/kotlin/main");
    let core = root.path().join("core/build/classes/kotlin/main");
    assert_eq!(runtime, vec![app.to_string_lossy().into_owned(), core.to_string_lossy().into_owned()]);

    let test = resolve_classpath(&indexer, "com.example.MainKt", Some(":app"), ClasspathScope::Test);
    assert!(test.contains(&root.path().join("app/build/classes/kotlin/test").to_string_lossy().into_owned()));
    assert!(!test.contains(&root.path().join("core/build/classes/kotlin/test").to_string_lossy().into_owned()));
}

#[test]
fn a_configured_java_home_comes_first() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("bin")).unwrap();
    let name = if cfg!(windows) { "java.exe" } else { "java" };
    std::fs::write(home.path().join("bin").join(name), "").unwrap();
    assert_eq!(java_executable(Some(home.path())), Some(home.path().join("bin").join(name)));
}
