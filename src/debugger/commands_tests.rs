use super::*;

#[tokio::test]
async fn other_commands_are_left_to_the_caller() {
    let commands = DebugCommands::new(Arc::new(Indexer::new()));
    let outcome = commands.execute("javak-lsp/reindex", &[]).await;
    assert!(matches!(outcome, CommandOutcome::NotDebugCommand));
}

#[tokio::test]
async fn resolve_classpath_answers_in_jdtls_shape() {
    let commands = DebugCommands::new(Arc::new(Indexer::new()));
    let outcome = commands
        .execute(RESOLVE_CLASSPATH, &[Value::from("my.module/com.example.MainKt")])
        .await;
    let CommandOutcome::Answered(value) = outcome else {
        panic!("resolveClasspath should answer");
    };
    // `[modulepaths, classpaths]`, both arrays, even without a workspace.
    assert_eq!(value, serde_json::json!([[], []]));
}

#[tokio::test]
async fn resolve_classpath_without_a_main_class_fails() {
    let commands = DebugCommands::new(Arc::new(Indexer::new()));
    let outcome = commands.execute(RESOLVE_CLASSPATH, &[]).await;
    assert!(matches!(outcome, CommandOutcome::Failed(_)));
}

#[test]
fn the_module_prefix_is_stripped_from_a_main_class() {
    assert_eq!(strip_module_prefix("my.module/com.example.Main"), "com.example.Main");
    assert_eq!(strip_module_prefix("com.example.Main"), "com.example.Main");
}

#[tokio::test]
async fn resolve_main_method_lists_the_mains_of_a_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Main.java");
    std::fs::write(&path, "class Main {\n    public static void main(String[] args) {}\n}\n").unwrap();
    let indexer = Indexer::new();
    indexer.workspace_root.set(root.path().to_path_buf()).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    assert!(indexer.reread(&uri));
    let commands = DebugCommands::new(Arc::new(indexer));
    let CommandOutcome::Answered(value) = commands.execute(RESOLVE_MAIN_METHOD, &[Value::from(uri.as_str())]).await
    else {
        panic!("resolveMainMethod should answer");
    };
    assert_eq!(value[0]["mainClass"], "Main");
    assert_eq!(value[0]["range"]["start"], serde_json::json!({ "line": 1, "character": 23 }));
}

#[test]
fn every_offered_command_is_answered() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let commands = DebugCommands::new(Arc::new(Indexer::new()));
    for command in COMMANDS {
        // Starting the debug server needs the jar and a JVM: the end-to-end
        // test covers it.
        if *command == START_DEBUG_SESSION {
            continue;
        }
        let outcome = runtime.block_on(commands.execute(command, &[Value::from("com.example.Main")]));
        assert!(
            !matches!(outcome, CommandOutcome::NotDebugCommand),
            "{command} is offered but not answered"
        );
    }
}
