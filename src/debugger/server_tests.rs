use super::*;

fn workspace_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Indexer) {
    let root = tempfile::tempdir().unwrap();
    let indexer = Indexer::new();
    indexer.workspace_root.set(root.path().to_path_buf()).unwrap();
    for (path, text) in files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        assert!(indexer.reread(&Url::from_file_path(&path).unwrap()));
    }
    (root, indexer)
}

#[test]
fn breakpoint_lines_are_one_based() {
    let (root, indexer) = workspace_with(&[(
        "src/com/demo/Main.java",
        "package com.demo;\n\npublic class Main {\n    void run() {}\n}\n",
    )]);
    let uri = Url::from_file_path(root.path().join("src/com/demo/Main.java")).unwrap();
    assert_eq!(
        breakpoint_classes(&indexer, uri.as_str(), &[4, 1]),
        vec![Some("com.demo.Main".to_owned()), None]
    );
    // A path works as well as a URI.
    let path = root.path().join("src/com/demo/Main.java");
    assert_eq!(breakpoint_classes(&indexer, &path.to_string_lossy(), &[4]), vec![Some("com.demo.Main".to_owned())]);
}

#[test]
fn a_file_the_index_has_not_read_is_read_from_disk() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Tool.kt");
    std::fs::write(&path, "package tools\n\nfun main() {\n    println()\n}\n").unwrap();
    let classes = breakpoint_classes(&Indexer::new(), &path.to_string_lossy(), &[4]);
    assert_eq!(classes, vec![Some("tools.ToolKt".to_owned())]);
}

#[test]
fn a_class_is_found_by_its_source_path_or_its_name() {
    let (root, indexer) = workspace_with(&[
        ("src/main/kotlin/app.kt", "package com.demo\n\nfun main() {}\n"),
        ("src/main/java/com/demo/Greeter.java", "package com.demo;\n\nclass Greeter {}\n"),
    ]);
    let kotlin = Url::from_file_path(root.path().join("src/main/kotlin/app.kt")).unwrap();
    let java = Url::from_file_path(root.path().join("src/main/java/com/demo/Greeter.java")).unwrap();
    assert_eq!(source_uri(&indexer, "com.demo.AppKt", Some("com/demo/app.kt")), Some(kotlin.to_string()));
    assert_eq!(source_uri(&indexer, "com.demo.Greeter$1", None), Some(java.to_string()));
    assert_eq!(source_uri(&indexer, "com.other.Greeter", None), None);
}

#[test]
fn the_control_channel_answers_in_its_own_shape() {
    let (root, indexer) = workspace_with(&[("A.java", "class A {\n  void f() {}\n}\n")]);
    let uri = Url::from_file_path(root.path().join("A.java")).unwrap();
    let answer_for = answer(
        &indexer,
        "breakpointClasses",
        &serde_json::json!({ "uri": uri.as_str(), "lines": [2] }),
    );
    assert_eq!(answer_for, serde_json::json!({ "classNames": ["A"] }));
    assert_eq!(answer(&indexer, "unknown", &serde_json::Value::Null), serde_json::Value::Null);
}
