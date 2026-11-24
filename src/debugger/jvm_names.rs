//! Source position ⇄ JVM class name, from the parsed file alone.
//!
//! The debugger thinks in JVM binary class names (`com.example.Outer$Inner`,
//! `com.example.MainKt`); the editor thinks in files and lines. jdtls answers
//! both directions from the JDT Java model; here the answers come from the
//! file's tree: its package, its type declarations and their spans.

use crate::language::Language;
use crate::parser::{syntax, Position, Syn};

/// Declarations that compile to a class of their own.
const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

/// A runnable entry point, in the shape of jdtls' `ResolutionItem`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MainClass {
    pub main_class: String,
    pub project_name: String,
    pub file_path: String,
}

/// The JVM class java-debug should install a breakpoint on `line`
/// (0-based) in; `None` where no class holds code there.
///
/// This is the *outermost* class at the line, not the innermost: java-debug
/// installs a breakpoint on `className` and every `className$*` class as they
/// load, and searches the nested types of loaded ones, so naming the
/// top-level class reaches nested classes, companion objects, anonymous
/// classes and lambdas without predicting the compiler's name for each.
pub(crate) fn breakpoint_class(
    language: Language,
    file_name: &str,
    text: &str,
    tree: &Syn,
    line: u32,
) -> Option<String> {
    let package = syntax(language).package(text);
    let outermost = tree
        .children
        .iter()
        .filter(|node| is_type_declaration(node))
        .find(|node| node.start.line <= line && line <= node.end.line);
    if let Some(declaration) = outermost {
        return Some(qualify(package.as_deref(), type_name(declaration)?));
    }
    match language {
        // Top-level functions and properties live in the file facade.
        Language::Kotlin => Some(qualify(
            package.as_deref(),
            &kotlin_facade_name(file_name, tree),
        )),
        // Imports, blank lines between types: no code to stop on.
        Language::Java => None,
    }
}

/// A `main` method the `java` launcher can start, and where its name is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainMethod {
    /// The JVM binary name of the class to launch.
    pub class_name: String,
    pub name_start: Position,
    pub name_end: Position,
}

/// JVM binary names of every class in the file the `java` launcher can
/// start: one with a `static void main(String[])` (in Kotlin, a top-level
/// `fun main`, or a `@JvmStatic fun main` in an object or companion).
pub(crate) fn main_classes(language: Language, file_name: &str, text: &str, tree: &Syn) -> Vec<String> {
    let mut names: Vec<String> = main_methods(language, file_name, text, tree)
        .into_iter()
        .map(|method| method.class_name)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Every `main` method of the file the `java` launcher can start, in
/// source order.
pub(crate) fn main_methods(language: Language, file_name: &str, text: &str, tree: &Syn) -> Vec<MainMethod> {
    if !text.contains("main") {
        return Vec::new();
    }
    let package = syntax(language).package(text);
    let mut found: Vec<(String, &Syn)> = Vec::new();
    for node in &tree.children {
        if language == Language::Kotlin && is_kotlin_main(node, false) {
            found.push((kotlin_facade_name(file_name, tree), node));
        }
        if is_type_declaration(node) {
            collect_main_methods(language, node, None, &mut found);
        }
    }
    found
        .into_iter()
        .filter_map(|(class_name, method)| {
            let name = method.child("name")?;
            Some(MainMethod {
                class_name: qualify(package.as_deref(), &class_name),
                name_start: name.start,
                name_end: name.end,
            })
        })
        .collect()
}

fn collect_main_methods<'t>(
    language: Language,
    declaration: &'t Syn,
    outer: Option<&str>,
    found: &mut Vec<(String, &'t Syn)>,
) {
    let Some(name) = type_name(declaration) else {
        return;
    };
    let binary = match outer {
        Some(outer) => format!("{outer}${name}"),
        None => name.to_owned(),
    };
    let Some(body) = declaration.child("body") else {
        return;
    };
    for member in &body.children {
        match language {
            Language::Java if is_java_main(member) => found.push((binary.clone(), member)),
            Language::Kotlin if is_kotlin_object(declaration) && is_kotlin_main(member, true) => {
                found.push((binary.clone(), member));
            }
            // A companion's `@JvmStatic main` is also a static method of the
            // class that holds the companion.
            Language::Kotlin if member.kind == "companion_object" => {
                let mains = member
                    .first("class_body")
                    .into_iter()
                    .flat_map(|body| body.children.iter())
                    .filter(|node| is_kotlin_main(node, true));
                for main in mains {
                    found.push((binary.clone(), main));
                }
            }
            _ => {}
        }
        if is_type_declaration(member) {
            collect_main_methods(language, member, Some(&binary), found);
        }
    }
}

/// `public static void main(String[] args)` or `(String... args)`.
fn is_java_main(member: &Syn) -> bool {
    if member.kind != "method_declaration" || member.child("name").is_none_or(|name| name.text != "main") {
        return false;
    }
    let is_static = member
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.children.iter().any(|modifier| modifier.kind == "static"));
    let returns_void = member.child("type").is_some_and(|returned| returned.kind == "void_type");
    let parameters: Vec<&Syn> = member
        .child("parameters")
        .map(|list| {
            list.children
                .iter()
                .filter(|node| node.kind == "formal_parameter" || node.kind == "spread_parameter")
                .collect()
        })
        .unwrap_or_default();
    let takes_strings = match parameters.as_slice() {
        [parameter] if parameter.kind == "spread_parameter" => {
            parameter.first("type_identifier").is_some_and(|name| is_string(&name.text))
        }
        [parameter] => parameter.child("type").is_some_and(|written| {
            written.kind == "array_type"
                && written.child("element").is_some_and(|element| is_string(&element.text))
                && written.child("dimensions").is_some_and(|dimensions| dimensions.text.matches('[').count() == 1)
        }),
        _ => false,
    };
    is_static && returns_void && takes_strings
}

fn is_string(name: &str) -> bool {
    name == "String" || name == "java.lang.String"
}

/// Kotlin `fun main()` / `fun main(args: Array<String>)`; inside an object
/// only with `@JvmStatic`.
fn is_kotlin_main(member: &Syn, needs_jvm_static: bool) -> bool {
    if member.kind != "method_declaration" || member.child("name").is_none_or(|name| name.text != "main") {
        return false;
    }
    if needs_jvm_static {
        let annotated = member.first("modifiers").is_some_and(|modifiers| {
            modifiers.walk().iter().any(|node| node.kind == "type_identifier" && node.text == "JvmStatic")
        });
        if !annotated {
            return false;
        }
    }
    let parameters: Vec<&Syn> = member
        .child("parameters")
        .map(|list| list.children.iter().filter(|node| node.kind == "formal_parameter").collect())
        .unwrap_or_default();
    match parameters.as_slice() {
        [] => !needs_jvm_static,
        [parameter] => {
            let written = parameter.child("type").map_or("", |written| written.text.as_str());
            let compact: String = written.chars().filter(|c| !c.is_whitespace()).collect();
            let is_vararg = parameter.text.trim_start().starts_with("vararg");
            compact == "Array<String>" || compact == "Array<outString>" || (is_vararg && compact == "String")
        }
        _ => false,
    }
}

fn is_kotlin_object(declaration: &Syn) -> bool {
    declaration.first("object").is_some()
}

fn is_type_declaration(node: &Syn) -> bool {
    TYPE_DECLARATIONS.contains(&node.kind.as_str())
}

fn type_name(declaration: &Syn) -> Option<&str> {
    declaration.child("name").map(|name| name.text.as_str())
}

fn qualify(package: Option<&str>, name: &str) -> String {
    match package {
        Some(package) if !package.is_empty() => format!("{package}.{name}"),
        _ => name.to_owned(),
    }
}

/// The class Kotlin compiles a file's top-level declarations into:
/// `@file:JvmName("Name")`'s, else the file name's stem capitalized with
/// `Kt` after it (`main.kt` → `MainKt`).
pub(crate) fn kotlin_facade_name(file_name: &str, tree: &Syn) -> String {
    let jvm_name = tree
        .children
        .iter()
        .filter(|node| node.kind == "file_annotation")
        .filter(|node| node.text.contains("JvmName"))
        .find_map(|node| node.walk().into_iter().find(|inner| inner.kind == "string_content"))
        .map(|content| content.text.clone());
    if let Some(name) = jvm_name {
        return name;
    }
    let stem = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .trim_end_matches(".kts")
        .trim_end_matches(".kt");
    let identifier: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    let mut characters = identifier.chars();
    let capitalized = match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    };
    format!("{capitalized}Kt")
}

/// What JDI reports a class's source as: `com/example/Main.kt`, the
/// package's directories and the `SourceFile` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePath {
    pub package: String,
    pub file_name: String,
}

impl SourcePath {
    pub(crate) fn parse(source_path: &str) -> Option<Self> {
        let normalized = source_path.replace('\\', "/");
        let (directories, file_name) = match normalized.rsplit_once('/') {
            Some((directories, file_name)) => (directories.to_owned(), file_name.to_owned()),
            None => (String::new(), normalized),
        };
        (!file_name.is_empty()).then(|| Self {
            package: directories.replace('/', "."),
            file_name,
        })
    }

    /// The file a class would be in if it was declared in a file of its
    /// top-level class's name: `com.example.Outer$Inner` → `com/example/Outer.<extension>`.
    pub(crate) fn of_class(class_name: &str, extension: &str) -> Self {
        let top_level = class_name.split('$').next().unwrap_or(class_name);
        let (package, simple) = top_level.rsplit_once('.').unwrap_or(("", top_level));
        Self {
            package: package.to_owned(),
            file_name: format!("{simple}.{extension}"),
        }
    }

    /// Whether a file at `path` declaring `package` is this source.
    pub(crate) fn matches(&self, path: &str, package: Option<&str>) -> bool {
        let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        file_name == self.file_name && package.unwrap_or("") == self.package
    }
}

#[cfg(test)]
#[path = "jvm_names_tests.rs"]
mod tests;
