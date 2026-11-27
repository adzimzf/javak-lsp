use super::*;

/// Compile `sources` (`(path, text)`) with the machine's `javac` into a
/// jar, keeping parameter names only where `parameters` asks; `None`
/// where there is no `javac`.
fn compiled(directory: &Path, sources: &[(&str, &str)], options: &[&str]) -> Option<std::path::PathBuf> {
    let source_dir = directory.join("src");
    let class_dir = directory.join("classes");
    std::fs::create_dir_all(&class_dir).unwrap();
    let mut files = Vec::new();
    for (path, text) in sources {
        let file = source_dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        files.push(file);
    }
    let status = std::process::Command::new("javac")
        .args(options)
        .arg("-d")
        .arg(&class_dir)
        .args(&files)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    assert!(status.success(), "javac failed");
    let jar = directory.join("lib.jar");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
    let mut stack = vec![class_dir.clone()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(&class_dir).unwrap().to_string_lossy().replace('\\', "/");
                zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
                zip.write_all(&std::fs::read(&path).unwrap()).unwrap();
            }
        }
    }
    zip.finish().unwrap();
    Some(jar)
}

fn stubs_of(jar: &Path) -> Vec<(String, String)> {
    let classes = read_classes(jar, None).unwrap().unwrap();
    render_all(&classes)
}

#[test]
fn a_class_is_written_with_its_generic_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jar) = compiled(
        dir.path(),
        &[(
            "com/acme/Box.java",
            "package com.acme;\n\
             import java.util.*;\n\
             public class Box<T extends Comparable<T>> extends AbstractList<T> implements java.io.Serializable {\n\
             \x20   public static final int LIMIT = 10;\n\
             \x20   public static final String NAME = \"box\\n\";\n\
             \x20   protected List<? super T> items;\n\
             \x20   private int hidden;\n\
             \x20   public Box(int capacity) {}\n\
             \x20   public T get(int index) { return null; }\n\
             \x20   public int size() { return 0; }\n\
             \x20   public <K, V extends Number> Map<K, List<V>> group(K key, V... values) throws java.io.IOException { return null; }\n\
             \x20   @Deprecated public static native void poke();\n\
             \x20   public class Inner { public Inner(String label) {} }\n\
             \x20   public static class Nested { public Map.Entry<String, Box<?>> entry() { return null; } }\n\
             \x20   private void secret() {}\n\
             }\n",
        )],
        &["-parameters"],
    ) else {
        return;
    };
    let stubs = stubs_of(&jar);
    assert_eq!(stubs.len(), 1, "one file per top-level class: {stubs:?}");
    let (entry, text) = &stubs[0];
    assert_eq!(entry, "com/acme/Box.java");
    let expected = [
        "package com.acme;",
        "",
        "import java.io.IOException;",
        "import java.io.Serializable;",
        "import java.util.AbstractList;",
        "import java.util.List;",
        "import java.util.Map;",
        "",
        "public class Box<T extends Comparable<T>> extends AbstractList<T> implements Serializable {",
        "",
        "    public static final int LIMIT = 10;",
        "",
        "    public static final String NAME = \"box\\n\";",
        "",
        "    protected List<? super T> items;",
        "",
        "    public Box(int capacity) { /* compiled code */ }",
        "",
        "    public T get(int index) { /* compiled code */ }",
        "",
        "    public int size() { /* compiled code */ }",
        "",
        "    public <K, V extends Number> Map<K, List<V>> group(K key, V... values) throws IOException { /* compiled code */ }",
        "",
        "    @Deprecated",
        "    public static native void poke();",
        "",
        "    public static class Nested {",
        "",
        "        public Nested() { /* compiled code */ }",
        "",
        "        public Map.Entry<String, Box<?>> entry() { /* compiled code */ }",
        "    }",
        "",
        "    public class Inner {",
        "",
        "        public Inner(String label) { /* compiled code */ }",
        "    }",
        "}",
        "",
    ]
    .join("\n");
    assert_eq!(*text, expected);
    // The stub is Java the parser reads without errors.
    assert!(crate::parser::syntax(crate::language::Language::Java).syntax_errors(text).is_empty(), "{text}");
}

#[test]
fn enums_records_interfaces_and_annotations_keep_their_shape() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jar) = compiled(
        dir.path(),
        &[
            ("p/Color.java", "package p;\npublic enum Color { RED, GREEN; public int code() { return 1; } }\n"),
            ("p/Point.java", "package p;\npublic record Point(int x, java.util.List<String> tags) { public Point { } public static Point origin() { return null; } }\n"),
            (
                "p/Shape.java",
                "package p;\npublic interface Shape extends Comparable<Shape> { double area(); default String label() { return \"\"; } static Shape unit() { return null; } }\n",
            ),
            (
                "p/Marker.java",
                "package p;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target({ElementType.TYPE, ElementType.METHOD})\npublic @interface Marker { String value() default \"x\"; int[] counts() default {1, 2}; Class<?> kind() default Object.class; }\n",
            ),
        ],
        &[],
    ) else {
        return;
    };
    let stubs: HashMap<String, String> = stubs_of(&jar).into_iter().collect();
    let color = &stubs["p/Color.java"];
    assert!(color.contains("public enum Color {\n    RED,\n    GREEN;\n"), "{color}");
    assert!(color.contains("public int code()"), "{color}");
    assert!(!color.contains("values()") && !color.contains("valueOf"), "{color}");
    let point = &stubs["p/Point.java"];
    assert!(point.contains("public record Point(int x, List<String> tags) {"), "{point}");
    assert!(point.contains("public static Point origin()"), "{point}");
    assert!(point.contains("public int x()"), "{point}");
    let shape = &stubs["p/Shape.java"];
    assert!(shape.contains("public interface Shape extends Comparable<Shape> {"), "{shape}");
    assert!(shape.contains("    double area();"), "{shape}");
    assert!(shape.contains("    default String label() { /* compiled code */ }"), "{shape}");
    assert!(shape.contains("    static Shape unit() { /* compiled code */ }"), "{shape}");
    let marker = &stubs["p/Marker.java"];
    assert!(marker.contains("@Retention(RetentionPolicy.RUNTIME)"), "{marker}");
    assert!(marker.contains("@Target({ElementType.TYPE, ElementType.METHOD})"), "{marker}");
    assert!(marker.contains("public @interface Marker {"), "{marker}");
    assert!(marker.contains("String value() default \"x\";"), "{marker}");
    assert!(marker.contains("int[] counts() default {1, 2};"), "{marker}");
    assert!(marker.contains("Class<?> kind() default Object.class;"), "{marker}");
    for (entry, text) in &stubs {
        assert!(crate::parser::syntax(crate::language::Language::Java).syntax_errors(text).is_empty(), "{entry}:\n{text}");
    }
}

#[test]
fn parameter_names_come_from_the_local_variable_table_or_are_numbered() {
    let dir = tempfile::tempdir().unwrap();
    let source = [("q/Calc.java", "package q;\npublic class Calc { public long add(long left, int right) { return left + right; } public abstract static class Op { public abstract int apply(int value); } }\n")];
    let Some(with_debug) = compiled(&dir.path().join("g"), &source, &["-g"]) else {
        return;
    };
    let text = &stubs_of(&with_debug)[0].1;
    assert!(text.contains("public long add(long left, int right)"), "{text}");
    // An abstract method has no code, so no table: numbered, as jdtls does.
    assert!(text.contains("public abstract int apply(int arg0);"), "{text}");
    let Some(without) = compiled(&dir.path().join("none"), &source, &["-g:none"]) else {
        return;
    };
    let text = &stubs_of(&without)[0].1;
    assert!(text.contains("public long add(long arg0, int arg1)"), "{text}");
}

#[test]
fn a_type_named_like_another_is_written_qualified() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jar) = compiled(
        dir.path(),
        &[(
            "r/Lists.java",
            "package r;\npublic class Lists { public java.util.List<java.lang.String> one() { return null; } public java.util.List<String> mine() { return null; } public java.awt.List two() { return null; } public static class String {} }\n",
        )],
        &[],
    ) else {
        return;
    };
    let text = &stubs_of(&jar)[0].1;
    // `String` is the nested class here, so `java.lang.String` is written
    // out; the two `List`s are told apart.
    assert!(text.contains("public java.util.List<java.lang.String> one()"), "{text}");
    assert!(text.contains("public java.util.List<Lists.String> mine()"), "{text}");
    assert!(text.contains("public java.awt.List two()"), "{text}");
}

#[test]
fn string_constants_are_escaped() {
    assert_eq!(constant_literal(&Constant::String("a\"b\\\u{0}é😀".to_owned()), "Ljava/lang/String;"), "\"a\\\"b\\\\\\u0000é😀\"");
    assert_eq!(constant_literal(&Constant::Int(39), "C"), "'\\''");
    assert_eq!(constant_literal(&Constant::Float(f32::NEG_INFINITY), "F"), "-1.0f / 0.0f");
    assert_eq!(constant_literal(&Constant::Long(-5), "J"), "-5L");
    assert_eq!(constant_literal(&Constant::Double(1e300), "D"), "1e300");
}

/// `JAVAK_STUB_JARS=a.jar:b.jar cargo test --release --lib stubs_of_real_jars -- --ignored --nocapture`:
/// how long the stubs of real jars take, and whether each parses.
#[test]
#[ignore]
fn stubs_of_real_jars() {
    let Some(jars) = std::env::var_os("JAVAK_STUB_JARS") else {
        return;
    };
    for jar in std::env::split_paths(&jars).filter(|jar| jar.is_file()) {
        let started = std::time::Instant::now();
        let Some(classes) = read_classes(&jar, None).unwrap() else {
            eprintln!("{}: Kotlin", jar.display());
            continue;
        };
        let files = render_all(&classes);
        let elapsed = started.elapsed();
        let bytes: usize = files.iter().map(|(_, text)| text.len()).sum();
        let broken: Vec<&String> = files
            .iter()
            .filter(|(_, text)| !crate::parser::syntax(crate::language::Language::Java).syntax_errors(text).is_empty())
            .map(|(entry, _)| entry)
            .collect();
        eprintln!(
            "{}: {} classes → {} files, {} KB, {elapsed:?}; {} with syntax errors {:?}",
            jar.file_name().unwrap().to_string_lossy(),
            classes.len(),
            files.len(),
            bytes / 1024,
            broken.len(),
            &broken[..broken.len().min(5)]
        );
    }
}

/// `JAVAK_STUB_JAR=x.jar JAVAK_STUB_ENTRY=a/B.java cargo test --lib dump_stub -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump_stub() {
    let (Some(jar), Ok(entry)) = (std::env::var_os("JAVAK_STUB_JAR"), std::env::var("JAVAK_STUB_ENTRY")) else {
        return;
    };
    let classes = read_classes(Path::new(&jar), None).unwrap().unwrap();
    for (name, text) in render_all(&classes) {
        if name == entry {
            println!("{text}");
        }
    }
}
