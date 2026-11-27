use super::*;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

// ── detection ────────────────────────────────────────────────────────────────

fn launcher_name(tool: Option<BuildTool>) -> Option<String> {
    let launcher = match tool? {
        BuildTool::Gradle { launcher } | BuildTool::Maven { launcher } => launcher,
    };
    Some(launcher.file_name()?.to_string_lossy().into_owned())
}

#[test]
fn detects_gradle_and_prefers_its_wrapper() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "settings.gradle.kts", "");
    let detected = BuildTool::detect(dir.path());
    assert!(
        matches!(detected, Some(BuildTool::Gradle { .. })),
        "{detected:?}"
    );
    // Without a wrapper: an installed `gradle` (or the bare name, when none is found).
    assert!(
        launcher_name(detected).is_some_and(|name| name.starts_with("gradle")),
        "launcher must be gradle"
    );
    write(dir.path(), "gradlew", "#!/bin/sh\n");
    if !cfg!(windows) {
        assert_eq!(
            BuildTool::detect(dir.path()),
            Some(BuildTool::Gradle {
                launcher: dir.path().join("gradlew")
            })
        );
    }
}

#[test]
fn detects_maven_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(BuildTool::detect(dir.path()), None);
    write(dir.path(), "src/Main.kt", "fun main() {}");
    assert_eq!(BuildTool::detect(dir.path()), None);
    write(dir.path(), "pom.xml", "<project/>");
    let detected = BuildTool::detect(dir.path());
    assert!(
        matches!(detected, Some(BuildTool::Maven { .. })),
        "{detected:?}"
    );
    assert!(launcher_name(detected).is_some_and(|name| name.starts_with("mvn")));
}

#[cfg(unix)]
#[test]
fn installed_tool_is_found_outside_path_and_must_be_executable() {
    use std::os::unix::fs::PermissionsExt;
    let not_executable = tempfile::tempdir().unwrap();
    let installed = tempfile::tempdir().unwrap();
    write(not_executable.path(), "mvn", "#!/bin/sh\n");
    write(installed.path(), "mvn", "#!/bin/sh\n");
    std::fs::set_permissions(
        installed.path().join("mvn"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let directories = vec![
        PathBuf::from("/definitely/not/here"),
        not_executable.path().to_path_buf(),
        installed.path().to_path_buf(),
    ];
    assert_eq!(
        installed_tool("mvn", &directories),
        Some(installed.path().join("mvn"))
    );
    assert_eq!(installed_tool("gradle", &directories), None);
}

#[test]
fn search_dirs_cover_sdkman_and_package_managers() {
    let directories = search_dirs(&MAVEN_INSTALL);
    let has = |suffix: &str| {
        directories
            .iter()
            .any(|directory| directory.ends_with(suffix))
    };
    assert!(
        has(".sdkman/candidates/maven/current/bin"),
        "{directories:?}"
    );
    assert!(has("/opt/homebrew/bin") || cfg!(windows));
    assert!(has("/usr/share/maven/bin"));
}

#[test]
fn missing_launcher_is_reported_as_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.txt");
    let failure = maven::resolve(dir.path(), Path::new("/definitely/not/here/mvn"), &output)
        .expect_err("a missing launcher can't succeed");
    assert!(
        matches!(&failure, BuildToolFailure::NotFound(program) if program.ends_with("mvn")),
        "{failure:?}"
    );
}

#[test]
fn sync_without_a_build_tool_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/Main.kt", "fun main() {}");
    assert_eq!(
        sync(dir.path(), false, SyncOptions::default()),
        SyncOutcome::NoBuildTool
    );
}

#[test]
fn sync_mode_parses_case_insensitively() {
    assert_eq!(SyncMode::parse("AUTO"), Some(SyncMode::Auto));
    assert_eq!(SyncMode::parse("off"), Some(SyncMode::Off));
    assert_eq!(SyncMode::parse("sometimes"), None);
}

// ── fingerprint ──────────────────────────────────────────────────────────────

#[test]
fn fingerprint_follows_build_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "settings.gradle.kts", "include(\":app\")\n");
    write(root, "app/build.gradle.kts", "dependencies {}\n");
    write(root, "app/src/main/kotlin/Main.kt", "fun main() {}\n");
    let original = build_fingerprint(root).unwrap();

    write(
        root,
        "app/src/main/kotlin/Main.kt",
        "fun main() { println() }\n",
    );
    write(
        root,
        "app/build/generated/build.gradle.kts",
        "ignored output\n",
    );
    assert_eq!(
        build_fingerprint(root).unwrap(),
        original,
        "sources and build output don't change the dependencies"
    );

    write(
        root,
        "gradle/libs.versions.toml",
        "[versions]\nkotlin = \"2.1.0\"\n",
    );
    let with_catalog = build_fingerprint(root).unwrap();
    assert_ne!(with_catalog, original, "a version catalog does");

    write(
        root,
        "app/build.gradle.kts",
        "dependencies { implementation(\"a:b:1\") }\n",
    );
    assert_ne!(
        build_fingerprint(root).unwrap(),
        with_catalog,
        "a build script does"
    );
}

#[test]
fn build_file_names_cover_gradle_maven_and_catalogs() {
    for name in [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle.kts",
        "gradle.properties",
        "gradle-wrapper.properties",
        "libs.versions.toml",
        "pom.xml",
    ] {
        assert!(is_build_file_name(name), "{name}");
    }
    for name in ["Main.kt", "build.xml", "versions.txt"] {
        assert!(!is_build_file_name(name), "{name}");
    }
}

// ── tool output ──────────────────────────────────────────────────────────────

#[test]
fn gradle_output_lines_become_dependencies() {
    let parsed = gradle::parse_output(concat!(
        "{\"coordinate\":\"com.acme:textkit:1.0.0\",\"jar\":\"/c/textkit-1.0.0.jar\",\"sources\":\"/c/textkit-1.0.0-sources.jar\"}\n",
        "{\"coordinate\":\"org.x:y:2\",\"jar\":\"/c/y-2.jar\",\"sources\":null}\n",
        "not json\n",
    ));
    assert!(!parsed.incomplete_sources);
    assert_eq!(
        parsed.dependencies,
        vec![
            ResolvedDependency {
                coordinate: "com.acme:textkit:1.0.0".to_owned(),
                jar: Some(PathBuf::from("/c/textkit-1.0.0.jar")),
                sources: Some(PathBuf::from("/c/textkit-1.0.0-sources.jar")),
                decompiled: None,
            },
            ResolvedDependency {
                coordinate: "org.x:y:2".to_owned(),
                jar: Some(PathBuf::from("/c/y-2.jar")),
                sources: None,
                decompiled: None,
            },
        ]
    );
}

#[test]
fn maven_output_pairs_jars_with_their_sources() {
    // Real `dependency:sources` + `dependency:list` output (both goals write
    // to the same file, with differently ordered fields).
    let parsed = maven::parse_output(concat!(
        "\n",
        "The following files have been resolved:\n",
        "   com.acme:textkit:jar:sources:1.0.0:/m2/com/acme/textkit/1.0.0/textkit-1.0.0-sources.jar -- module textkit (auto)\n",
        "\n",
        "The following files have NOT been resolved:\n",
        "   com.acme:textkit:jar:1.0.0:compile:/m2/com/acme/textkit/1.0.0/textkit-1.0.0.jar -- module textkit (auto)\n",
        "The following files have been resolved:\n",
        "   com.acme:textkit:jar:1.0.0:compile:/m2/com/acme/textkit/1.0.0/textkit-1.0.0.jar -- module textkit (auto)\n",
        "   org.jetbrains:annotations:jar:13.0:compile:/m2/org/jetbrains/annotations/13.0/annotations-13.0.jar\n",
        "   org.example:native:jar:linux:1.2:runtime:/m2/org/example/native/1.2/native-1.2-linux.jar\n",
    ));
    let textkit = parsed
        .iter()
        .find(|dependency| dependency.coordinate == "com.acme:textkit:1.0.0")
        .unwrap();
    assert_eq!(
        textkit.jar.as_deref(),
        Some(Path::new("/m2/com/acme/textkit/1.0.0/textkit-1.0.0.jar"))
    );
    assert_eq!(
        textkit.sources.as_deref(),
        Some(Path::new(
            "/m2/com/acme/textkit/1.0.0/textkit-1.0.0-sources.jar"
        ))
    );
    let annotations = parsed
        .iter()
        .find(|dependency| dependency.coordinate == "org.jetbrains:annotations:13.0")
        .unwrap();
    assert_eq!(annotations.sources, None);
    assert!(
        parsed
            .iter()
            .any(|dependency| dependency.coordinate == "org.example:native:1.2"),
        "a classifier jar keeps its real version: {parsed:?}"
    );
}

#[test]
fn maven_failure_in_one_module_keeps_the_other_modules_dependencies() {
    // gson's shape: `gson` resolves; `test-jpms` depends on the
    // never-installed `gson` SNAPSHOT and fails the whole run.
    let listed = maven::parse_output(concat!(
        "The following files have been resolved:\n",
        "   com.google.truth:truth:jar:1.4.5:test:/m2/com/google/truth/truth/1.4.5/truth-1.4.5.jar -- module truth (auto)\n",
        "   junit:junit:jar:4.13.2:test:/m2/junit/junit/4.13.2/junit-4.13.2.jar -- module junit [auto]\n",
    ));
    let failure =
        "[ERROR] Failed to execute goal on project test-jpms: Could not resolve dependencies";
    let resolved = maven::resolved_from(
        listed,
        maven::ModulesResolved::Some {
            failure: failure.to_owned(),
        },
    )
    .expect("the modules that resolved still count");
    let coordinates: Vec<&str> = resolved
        .dependencies
        .iter()
        .map(|dependency| dependency.coordinate.as_str())
        .collect();
    assert_eq!(
        coordinates,
        ["com.google.truth:truth:1.4.5", "junit:junit:4.13.2"]
    );
    assert!(
        resolved.incomplete_sources,
        "a partial result is retried later"
    );

    let nothing_listed = maven::resolved_from(
        Vec::new(),
        maven::ModulesResolved::Some {
            failure: failure.to_owned(),
        },
    );
    match nothing_listed {
        Err(BuildToolFailure::Failed(message)) => assert_eq!(message, failure),
        other => panic!("a failed run that listed nothing is a failure: {other:?}"),
    }

    let complete = maven::resolved_from(Vec::new(), maven::ModulesResolved::All).unwrap();
    assert!(!complete.incomplete_sources);
}

#[test]
fn maven_output_accepts_windows_paths() {
    let parsed = maven::parse_output(
        "   com.acme:textkit:jar:1.0.0:compile:C:\\Users\\me\\.m2\\repository\\com\\acme\\textkit\\1.0.0\\textkit-1.0.0.jar\n",
    );
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    assert_eq!(parsed[0].coordinate, "com.acme:textkit:1.0.0");
}

#[test]
fn gradle_marker_line_flags_incomplete_sources() {
    let parsed = gradle::parse_output(concat!(
        "{\"coordinate\":\"a:b:1\",\"jar\":\"/c/b-1.jar\",\"sources\":null}\n",
        "{\"incompleteSources\":true}\n",
    ));
    assert!(parsed.incomplete_sources);
    assert_eq!(parsed.dependencies.len(), 1);
}

#[test]
fn incomplete_manifest_is_retried_only_after_the_back_off() {
    let manifest = DependencyManifest {
        format: MANIFEST_FORMAT,
        fingerprint: "f".to_owned(),
        build_tool: "gradle".to_owned(),
        dependencies: vec![],
        incomplete_sources: true,
        saved_at: 1_000,
        modules: Vec::new(),
        sources_retries: 0,
    };
    assert!(!manifest.is_due_for_retry(1_000 + 60));
    assert!(manifest.is_due_for_retry(1_000 + INCOMPLETE_RETRY_AFTER.as_secs()));
    let complete = DependencyManifest {
        incomplete_sources: false,
        ..manifest
    };
    assert!(!complete.is_due_for_retry(u64::MAX / 2));
}

#[test]
fn every_retry_that_fails_again_doubles_the_back_off() {
    let first = INCOMPLETE_RETRY_AFTER.as_secs();
    let manifest = |sources_retries| DependencyManifest {
        format: MANIFEST_FORMAT,
        fingerprint: "f".to_owned(),
        build_tool: "gradle".to_owned(),
        dependencies: vec![],
        incomplete_sources: true,
        saved_at: 1_000,
        modules: Vec::new(),
        sources_retries,
    };
    assert!(manifest(0).is_due_for_retry(1_000 + first));
    assert!(!manifest(1).is_due_for_retry(1_000 + first));
    assert!(manifest(1).is_due_for_retry(1_000 + 2 * first));
    assert!(!manifest(3).is_due_for_retry(1_000 + 7 * first));
    // Never longer than a day, however many retries failed.
    assert!(manifest(u32::MAX).is_due_for_retry(1_000 + MAX_INCOMPLETE_RETRY_AFTER.as_secs()));
}

#[test]
fn login_shell_output_is_read_between_markers_only() {
    let output = format!(
        "Welcome to zsh!\n{SHELL_PATH_MARKER}/opt/zerobrew/bin:/usr/bin::/bin{SHELL_PATH_MARKER}/opt/zerobrew/opt/openjdk{SHELL_PATH_MARKER}\nbye"
    );
    let environment = parse_login_shell_output(&output);
    assert_eq!(
        environment.path,
        vec![
            PathBuf::from("/opt/zerobrew/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ]
    );
    assert_eq!(
        environment.java_home.as_deref(),
        Some("/opt/zerobrew/opt/openjdk")
    );

    let no_java_home = parse_login_shell_output(&format!(
        "{SHELL_PATH_MARKER}/usr/bin{SHELL_PATH_MARKER}{SHELL_PATH_MARKER}"
    ));
    assert_eq!(no_java_home.java_home, None);
    assert_eq!(
        parse_login_shell_output("no markers here"),
        LoginShellEnvironment::default()
    );
}

#[test]
fn failure_details_pick_the_error_lines() {
    let maven = "[INFO] Scanning for projects...\n\
        [ERROR] Failed to execute goal org.apache.maven.plugins:maven-dependency-plugin:3.8.1:sources: \
        Could not resolve dependencies: com.acme:missing:jar:9 was not found\n\
        [ERROR] \n\
        [ERROR] To see the full stack trace of the errors, re-run Maven with the -e switch.\n\
        [ERROR] -> [Help 1]\n";
    let details = failure_details(maven);
    assert!(
        details.starts_with("[ERROR] Failed to execute goal"),
        "{details}"
    );
    assert!(!details.contains("stack trace"), "{details}");

    let no_java = "Picked up JAVA_TOOL_OPTIONS: -Dx\nThe JAVA_HOME environment variable is not defined correctly,\n\
        this environment variable is needed to run this program.\n";
    assert!(failure_details(no_java).contains("JAVA_HOME"));
    assert!(!failure_details(no_java).contains("Picked up"));

    let plain = "line one\nline two\n";
    assert_eq!(failure_details(plain), "line one\nline two");
}

#[test]
fn zerobrew_prefix_is_a_known_location() {
    let directories = search_dirs(&MAVEN_INSTALL);
    assert!(directories
        .iter()
        .any(|directory| directory.ends_with("/opt/zerobrew/bin")));
}

#[test]
fn gradle_decompiler_line_is_read() {
    let parsed = gradle::parse_output(concat!(
        "{\"coordinate\":\"a:b:1\",\"jar\":\"/c/b-1.jar\",\"sources\":null}\n",
        "{\"decompiler\":\"/c/vineflower-1.11.1.jar\"}\n",
    ));
    assert_eq!(
        parsed.decompiler.as_deref(),
        Some(Path::new("/c/vineflower-1.11.1.jar"))
    );
    assert_eq!(parsed.dependencies.len(), 1);
}

#[test]
fn gradle_module_lines_say_where_each_project_keeps_its_sources() {
    let parsed = gradle::parse_output(concat!(
        "{\"coordinate\":\"a:b:1\",\"jar\":\"/c/b-1.jar\",\"sources\":null}\n",
        "{\"module\":\":app\",\"dir\":\"/w/app\",\"java\":[\"/w/app/src/main/java\"],",
        "\"kotlin\":[\"/w/app/src/main/kotlin\"],\"javaLevel\":\"17\"}\n",
        "{\"module\":\":\",\"dir\":\"/w\",\"java\":[],\"kotlin\":[],\"javaLevel\":null}\n",
    ));
    assert_eq!(parsed.dependencies.len(), 1);
    assert_eq!(
        parsed.modules,
        [
            ReportedModule {
                dir: PathBuf::from("/w/app"),
                java: vec![PathBuf::from("/w/app/src/main/java")],
                kotlin: vec![PathBuf::from("/w/app/src/main/kotlin")],
                java_level: Some("17".to_owned()),
            },
            ReportedModule {
                dir: PathBuf::from("/w"),
                ..ReportedModule::default()
            },
        ]
    );
}

#[test]
fn decompile_option_is_part_of_the_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pom.xml", "<project/>");
    let on = sync_fingerprint(dir.path(), SyncOptions { decompile: DecompileMode::OnDemand }).unwrap();
    let off = sync_fingerprint(dir.path(), SyncOptions { decompile: DecompileMode::Off }).unwrap();
    assert_ne!(on, off, "toggling decompileLibraries must re-sync");
}

#[test]
fn decompiled_sources_stand_in_for_missing_sources() {
    let dir = tempfile::tempdir().unwrap();
    let decompiled = dir.path().join("greeter-decompiled-sources.jar");
    std::fs::write(&decompiled, b"zip").unwrap();
    let manifest = DependencyManifest {
        format: MANIFEST_FORMAT,
        fingerprint: "f".to_owned(),
        build_tool: "maven".to_owned(),
        dependencies: vec![ResolvedDependency {
            coordinate: "com.acme:greeter:1.0.0".to_owned(),
            jar: None,
            sources: None,
            decompiled: Some(decompiled.clone()),
        }],
        incomplete_sources: false,
        saved_at: 0,
        modules: Vec::new(),
        sources_retries: 0,
    };
    assert_eq!(
        manifest.sources_jars_by_artifact(),
        vec![("com.acme".to_owned(), "greeter".to_owned(), decompiled)]
    );
}

#[test]
fn decompiled_files_are_packaged_as_a_sources_jar() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "out/com/acme/Greeter.java",
        "package com.acme;\nclass Greeter {}\n",
    );
    write(
        dir.path(),
        "out/com/acme/Util.kt",
        "package com.acme\nfun util() {}\n",
    );
    write(
        dir.path(),
        "out/META-INF/MANIFEST.MF",
        "Manifest-Version: 1.0\n",
    );
    let jar = dir.path().join("greeter-decompiled-sources.jar");
    decompile::package_sources(&dir.path().join("out"), &jar).unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut names: Vec<String> = (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["com/acme/Greeter.java", "com/acme/Util.kt"]);
}

/// `covers_whole_classpath` decides whether the shared Gradle cache is
/// crawled at all, so each arm it can take is pinned here.
fn manifest_of(build_tool: &str, jar: Option<PathBuf>) -> DependencyManifest {
    DependencyManifest {
        format: MANIFEST_FORMAT,
        fingerprint: "f".to_owned(),
        build_tool: build_tool.to_owned(),
        dependencies: vec![ResolvedDependency {
            coordinate: "com.acme:greeter:1.0.0".to_owned(),
            jar,
            sources: None,
            decompiled: None,
        }],
        incomplete_sources: false,
        saved_at: 0,
        modules: Vec::new(),
        sources_retries: 0,
    }
}

#[test]
fn a_maven_manifest_covers_the_whole_classpath() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("greeter-1.0.0.jar");
    std::fs::write(&jar, b"jar").unwrap();

    assert!(
        manifest_of(BuildTool::MAVEN_NAME, Some(jar.clone())).covers_whole_classpath(),
        "`dependency:list` reports each module's fully-resolved classpath — \
         the shared Gradle cache can only add decoys on top of it"
    );
    assert!(
        !manifest_of(BuildTool::GRADLE_NAME, Some(jar)).covers_whole_classpath(),
        "a Gradle manifest misses buildSrc/plugin classpaths, which the \
         cache crawl covers"
    );
}

#[test]
fn a_manifest_whose_jars_are_gone_covers_nothing() {
    let manifest = manifest_of(
        BuildTool::MAVEN_NAME,
        Some(PathBuf::from("/nonexistent/greeter-1.0.0.jar")),
    );
    assert!(
        !manifest.covers_whole_classpath(),
        "replacing the cache crawl with a list of deleted jars would index \
         no libraries at all"
    );
    assert!(!manifest_of(BuildTool::MAVEN_NAME, None).covers_whole_classpath());
}

#[test]
fn maven_reactor_retry_runs_the_compile_phase_without_compiling() {
    use maven::SiblingModules;
    let output = Path::new("/tmp/deps.txt");

    let plain = maven::dependency_goal_arguments(output, SiblingModules::FromRepository);
    let reactor = maven::dependency_goal_arguments(output, SiblingModules::FromReactor);

    assert!(!plain.iter().any(|argument| argument == "compile"));
    // The phase must come before the dependency goals, so the reactor has
    // every sibling's output in place when they resolve.
    let compile = reactor.iter().position(|argument| argument == "compile");
    let list = reactor
        .iter()
        .position(|argument| argument.ends_with(":list"));
    assert!(compile.is_some() && compile < list, "{reactor:?}");
    assert!(reactor
        .iter()
        .any(|argument| argument == "-Dmaven.main.skip=true"));
    assert!(reactor.iter().any(|argument| argument == "--fail-at-end"));
}

#[test]
fn the_declared_java_version_is_read_from_the_root_build_file() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pom.xml",
        "<project><properties><maven.compiler.release>17</maven.compiler.release>\
         </properties></project>",
    );
    assert_eq!(declared_java_version(dir.path()).as_deref(), Some("17"));

    let gradle = tempfile::tempdir().unwrap();
    write(
        gradle.path(),
        "build.gradle.kts",
        "java {\n  toolchain { languageVersion = JavaLanguageVersion.of(21) }\n}\n",
    );
    assert_eq!(declared_java_version(gradle.path()).as_deref(), Some("21"));

    let old = tempfile::tempdir().unwrap();
    // `1.8` is Java 8.
    write(old.path(), "build.gradle", "sourceCompatibility = 1.8\n");
    assert_eq!(declared_java_version(old.path()).as_deref(), Some("8"));

    // A key whose value is on the next line names no version.
    let empty = tempfile::tempdir().unwrap();
    write(empty.path(), "build.gradle", "sourceCompatibility =\n\n17\n");
    assert_eq!(declared_java_version(empty.path()), None);
}

#[test]
fn maven_compiler_plugin_executions_name_the_level_too() {
    // jsoup declares nothing in `<properties>`: the levels are inside the
    // compiler plugin's executions, and the highest one is the project's.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pom.xml",
        "<project><properties><project.build.sourceEncoding>UTF-8\
         </project.build.sourceEncoding></properties><build><plugins><plugin>\
         <artifactId>maven-compiler-plugin</artifactId><executions>\
         <execution><configuration><source>8</source><target>8</target>\
         </configuration></execution>\
         <execution><configuration><release>8</release></configuration></execution>\
         <execution><configuration><release>11</release></configuration></execution>\
         <execution><configuration><source>1.8</source></configuration></execution>\
         </executions></plugin><plugin><configuration><sources><source>\
         ${project.basedir}/src/test/java11</source></sources></configuration>\
         </plugin></plugins></build></project>",
    );
    assert_eq!(declared_java_version(dir.path()).as_deref(), Some("11"));
}

fn zip_of(path: &Path, entries: &[&str]) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for entry in entries {
        zip.start_file(*entry, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn the_packages_a_sources_jar_lacks_are_missing() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("assertj.jar");
    let sources = dir.path().join("assertj-sources.jar");
    zip_of(
        &jar,
        &[
            "META-INF/MANIFEST.MF",
            "META-INF/versions/9/module-info.class",
            "org/assertj/core/api/Assertions.class",
            "org/assertj/core/api/Assertions$1.class",
            "org/assertj/core/internal/bytebuddy/ByteBuddy.class",
            "org/assertj/core/internal/bytebuddy/ByteBuddy$Inner.class",
            "org/assertj/core/util/package-info.class",
        ],
    );
    zip_of(&sources, &["org/assertj/core/api/Assertions.java"]);
    assert_eq!(
        decompile::missing_packages(&jar, &sources).unwrap(),
        vec!["org/assertj/core/internal/bytebuddy".to_owned()]
    );
    let dependency = ResolvedDependency {
        coordinate: "org.assertj:assertj-core:3.22.0".to_owned(),
        jar: Some(jar.clone()),
        sources: Some(sources.clone()),
        decompiled: None,
    };
    assert_eq!(
        decompile::missing_sources(&dependency),
        Some(decompile::Missing::Packages(vec!["org/assertj/core/internal/bytebuddy".to_owned()]))
    );
    // A Kotlin file's `FooKt` class is in the package of `Foo.kt`.
    zip_of(&jar, &["demo/UtilKt.class"]);
    zip_of(&sources, &["demo/Util.kt"]);
    assert_eq!(decompile::missing_sources(&ResolvedDependency { jar: Some(jar), sources: Some(sources), ..dependency.clone() }), None);
    assert_eq!(
        decompile::missing_sources(&ResolvedDependency { sources: None, ..dependency }),
        Some(decompile::Missing::Everything)
    );
}

#[test]
fn a_jar_with_sources_and_its_decompiled_rest_gives_both() {
    let dir = tempfile::tempdir().unwrap();
    let sources = dir.path().join("assertj-sources.jar");
    let rest = dir.path().join("assertj-missing-sources.jar");
    std::fs::write(&sources, b"zip").unwrap();
    std::fs::write(&rest, b"zip").unwrap();
    let manifest = DependencyManifest {
        dependencies: vec![ResolvedDependency {
            coordinate: "org.assertj:assertj-core:3.22.0".to_owned(),
            jar: None,
            sources: Some(sources.clone()),
            decompiled: Some(rest.clone()),
        }],
        ..manifest_of("maven", None)
    };
    let jars: Vec<PathBuf> = manifest.sources_jars_by_artifact().into_iter().map(|(_, _, jar)| jar).collect();
    assert_eq!(jars, vec![sources, rest]);
}

#[test]
fn a_kotlin_sources_jar_laid_out_by_source_set_lacks_no_package() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("kotlinx-coroutines-core-jvm.jar");
    let sources = dir.path().join("kotlinx-coroutines-core-jvm-sources.jar");
    zip_of(
        &jar,
        &[
            "kotlinx/coroutines/Job.class",
            "kotlinx/coroutines/flow/Flow.class",
            "kotlinx/coroutines/flow/FlowKt.class",
            "kotlinx/coroutines/internal/shaded/Hidden.class",
            "kotlinx/coroutines/unsigned/UJobKt.class",
        ],
    );
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&sources).unwrap());
    for (entry, text) in [
        ("commonMain/Job.kt", "/* licence */\n@file:JvmName(\"JobKt\")\npackage kotlinx.coroutines\n\ninterface Job\n"),
        ("commonMain/flow/Flow.kt", "package kotlinx.coroutines.flow\n\ninterface Flow<T>\n"),
        (
            "jvmMain/UJob.kt",
            "@file:kotlin.jvm.JvmPackageName(\"kotlinx.coroutines.unsigned\")\npackage kotlinx.coroutines\n",
        ),
    ] {
        zip.start_file(entry, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    // Only the package no file declares is missing, not every package.
    assert_eq!(
        decompile::missing_packages(&jar, &sources).unwrap(),
        vec!["kotlinx/coroutines/internal/shaded".to_owned()]
    );
}

#[test]
fn the_decompile_setting_takes_a_mode_or_a_bool() {
    use serde_json::json;
    assert_eq!(DecompileMode::default(), DecompileMode::OnDemand);
    assert_eq!(DecompileMode::parse(&json!("onDemand")), Some(DecompileMode::OnDemand));
    assert_eq!(DecompileMode::parse(&json!("EAGER")), Some(DecompileMode::Eager));
    assert_eq!(DecompileMode::parse(&json!("off")), Some(DecompileMode::Off));
    assert_eq!(DecompileMode::parse(&json!(true)), Some(DecompileMode::OnDemand));
    assert_eq!(DecompileMode::parse(&json!(false)), Some(DecompileMode::Off));
    assert_eq!(DecompileMode::parse(&json!("sometimes")), None);
}

#[test]
fn switching_between_the_modes_that_decompile_needs_no_new_sync() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
    let fingerprint = |decompile| sync_fingerprint(dir.path(), SyncOptions { decompile }).unwrap();
    assert_eq!(fingerprint(DecompileMode::OnDemand), fingerprint(DecompileMode::Eager));
    assert_ne!(fingerprint(DecompileMode::OnDemand), fingerprint(DecompileMode::Off));
}
