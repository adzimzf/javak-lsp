use super::*;

fn parsed(language: Language, text: &str) -> Syn {
    syntax(language).tree(text)
}

fn breakpoint(language: Language, file_name: &str, text: &str, line: u32) -> Option<String> {
    breakpoint_class(language, file_name, text, &parsed(language, text), line)
}

fn mains(language: Language, file_name: &str, text: &str) -> Vec<String> {
    main_classes(language, file_name, text, &parsed(language, text))
}

const JAVA: &str = "\
package com.demo;

import java.util.List;

public class Main {
    static class Inner {
        void run() {
            Runnable task = () -> System.out.println();
        }
    }

    public static void main(String[] args) {
        new Inner().run();
    }
}

class Helper {
    void help() {}
}
";

#[test]
fn a_java_breakpoint_goes_in_the_outermost_class_at_its_line() {
    let at = |line| breakpoint(Language::Java, "/w/Main.java", JAVA, line);
    assert_eq!(at(7).as_deref(), Some("com.demo.Main"), "a lambda in a nested class");
    assert_eq!(at(12).as_deref(), Some("com.demo.Main"));
    assert_eq!(at(17).as_deref(), Some("com.demo.Helper"));
    assert_eq!(at(2), None, "an import holds no code");
    assert_eq!(at(15), None, "between two classes");
}

#[test]
fn a_kotlin_line_outside_every_class_is_in_the_file_facade() {
    let text = "package com.demo\n\nfun main() {\n    println(Greeter().greet())\n}\n\nclass Greeter {\n    fun greet() = \"hi\"\n}\n";
    let at = |line| breakpoint(Language::Kotlin, "/w/app/main.kt", text, line);
    assert_eq!(at(3).as_deref(), Some("com.demo.MainKt"));
    assert_eq!(at(7).as_deref(), Some("com.demo.Greeter"));
}

#[test]
fn a_kotlin_facade_takes_the_file_jvm_name() {
    let text = "@file:JvmName(\"Starter\")\npackage com.demo\n\nfun main() {}\n";
    assert_eq!(breakpoint(Language::Kotlin, "Main.kt", text, 3).as_deref(), Some("com.demo.Starter"));
    assert_eq!(mains(Language::Kotlin, "Main.kt", text), ["com.demo.Starter"]);
}

#[test]
fn facade_names_capitalize_the_file_stem() {
    let tree = parsed(Language::Kotlin, "");
    assert_eq!(kotlin_facade_name("/w/src/main.kt", &tree), "MainKt");
    assert_eq!(kotlin_facade_name("app-utils.kt", &tree), "App_utilsKt");
}

#[test]
fn java_main_classes_are_the_ones_with_a_static_main_taking_strings() {
    let text = "\
package com.demo;
public class Main {
    static class Inner { public static void main(String... args) {} }
    public static void main(String[] args) {}
}
class NotStatic { public void main(String[] args) {} }
class WrongParameters { public static void main(int[] args) {} }
class TwoDimensions { public static void main(String[][] args) {} }
";
    assert_eq!(mains(Language::Java, "Main.java", text), ["com.demo.Main", "com.demo.Main$Inner"]);
}

#[test]
fn kotlin_main_classes_are_the_facade_and_jvm_static_mains() {
    let text = "\
package com.demo

class Greeter {
    companion object {
        @JvmStatic fun main(args: Array<String>) {}
    }
}

object Tool {
    @JvmStatic
    fun main(args: Array<String>) {}
}

object NotStatic {
    fun main(args: Array<String>) {}
}

fun main() {}
";
    assert_eq!(
        mains(Language::Kotlin, "App.kt", text),
        ["com.demo.AppKt", "com.demo.Greeter", "com.demo.Tool"]
    );
}

#[test]
fn a_main_method_is_reported_at_its_name() {
    let text = "class A {\n    public static void main(String[] args) {}\n}\n";
    let found = main_methods(Language::Java, "A.java", text, &parsed(Language::Java, text));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].class_name, "A");
    assert_eq!(found[0].name_start, Position { line: 1, column: 23 });
    assert_eq!(found[0].name_end, Position { line: 1, column: 27 });
}

#[test]
fn source_paths_match_by_file_name_and_package() {
    let source = SourcePath::parse("com/demo/Main.kt").unwrap();
    assert!(source.matches("/w/src/whatever/Main.kt", Some("com.demo")));
    assert!(!source.matches("/w/src/com/demo/Main.kt", Some("com.other")));
    assert!(SourcePath::parse("Main.java").unwrap().matches("/w/Main.java", None));
    assert_eq!(
        SourcePath::of_class("com.demo.Outer$Inner$1", "java"),
        SourcePath { package: "com.demo".into(), file_name: "Outer.java".into() }
    );
}
