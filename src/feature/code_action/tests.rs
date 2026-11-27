//! The edits of the quick assists and refactorings, applied: what the
//! integration cases cannot see, since they compare titles alone.

use std::sync::Arc;

use tower_lsp::lsp_types::Url;

use super::{actions, Edit};
use crate::feature::semantic::{FileView, Snapshot};
use crate::language::Language;
use crate::parser::{syntax, Position};

/// The position of `marker` in `text`, and `text` without it.
fn take_marker(text: &str, marker: &str) -> (String, Option<Position>) {
    let Some(offset) = text.find(marker) else {
        return (text.to_owned(), None);
    };
    let at = crate::parser::position_of(text, offset);
    (format!("{}{}", &text[..offset], &text[offset + marker.len()..]), Some(at))
}

/// `source` with the edits of its action `title` applied, the selection
/// from `$0` to `$1`.
fn applied(source: &str, title: &str) -> String {
    let (text, start) = take_marker(source, "$0");
    let start = start.expect("a `$0` cursor");
    let (text, end) = take_marker(&text, "$1");
    let end = end.unwrap_or(start);
    let file = FileView {
        uri: Url::parse("file:///demo/A.java").expect("a uri"),
        language: Language::Java,
        tree: Arc::new(syntax(Language::Java).tree(&text)),
        text: Arc::new(text.clone()),
        library: false,
    };
    let snapshot = Snapshot::new(vec![file]);
    let file = &snapshot.files[0];
    let offered = actions(&snapshot, file, start, end);
    let titles: Vec<&str> = offered.iter().map(|action| action.title.as_str()).collect();
    let action = offered
        .iter()
        .find(|action| action.title == title)
        .unwrap_or_else(|| panic!("`{title}` is offered among {titles:?}"));
    let mut edits: Vec<&Edit> = action.edits.iter().flat_map(|(_, edits)| edits).collect();
    edits.sort_by_key(|edit| std::cmp::Reverse((edit.start, edit.end)));
    let mut result = text;
    for edit in edits {
        let offset = |at: Position| -> usize {
            let line_start: usize = result.split_inclusive('\n').take(at.line as usize).map(str::len).sum();
            line_start + at.column as usize
        };
        let (from, to) = (offset(edit.start), offset(edit.end));
        result.replace_range(from..to, &edit.text);
    }
    result
}

#[test]
fn surround_with_try_catch_wraps_the_statement() {
    let source = "class A {\n\tvoid f() {\n\t\tString $0b$1 = \"x\";\n\t}\n}\n";
    assert_eq!(
        applied(source, "Surround with try/catch"),
        "class A {\n\tvoid f() {\n\t\ttry {\n\t\t\tString b = \"x\";\n\t\t} catch (Exception e) {\n\t\t\te.printStackTrace();\n\t\t}\n\t}\n}\n"
    );
}

#[test]
fn split_variable_declaration_assigns_on_its_own_line() {
    let source = "class A {\n\tvoid f() {\n\t\tString $0b$1 = \"x\";\n\t}\n}\n";
    assert_eq!(
        applied(source, "Split variable declaration"),
        "class A {\n\tvoid f() {\n\t\tString b;\n\t\tb = \"x\";\n\t}\n}\n"
    );
}

#[test]
fn change_type_to_var_replaces_the_type() {
    let source = "class A {\n\tvoid f() {\n\t\tString $0b$1 = \"x\";\n\t}\n}\n";
    assert_eq!(
        applied(source, "Change type of 'b' to 'var'"),
        "class A {\n\tvoid f() {\n\t\tvar b = \"x\";\n\t}\n}\n"
    );
}

#[test]
fn extract_local_to_field_leaves_an_assignment() {
    let source = "class A {\n\tvoid f() {\n\t\tString $0b$1 = \"x\";\n\t}\n}\n";
    assert_eq!(
        applied(source, "Extract to field"),
        "class A {\n\tprivate String b;\n\tvoid f() {\n\t\tb = \"x\";\n\t}\n}\n"
    );
}

#[test]
fn extract_to_method_passes_the_locals_it_reads() {
    let source = "class A {\n\tvoid f(String a) {\n\t\t$0String c = a;$1\n\t}\n}\n";
    assert_eq!(
        applied(source, "Extract to method"),
        "class A {\n\tvoid f(String a) {\n\t\textracted(a);\n\t}\n\n\tprivate void extracted(String a) {\n\t\tString c = a;\n\t}\n}\n"
    );
}

#[test]
fn generate_getter_for_one_field() {
    let source = "class A {\n\tprivate $0String name$1;\n}\n";
    assert_eq!(
        applied(source, "Generate Getter for 'name'"),
        // As `Generate Getters` writes it: a class at the margin gets
        // four spaces.
        "class A {\n\tprivate String name;\n\n    public String getName() {\n        return name;\n    }\n}\n"
    );
}

#[test]
fn assign_parameter_to_new_field() {
    let source = "class A {\n\tA(int $0count$1) {\n\t}\n}\n";
    assert_eq!(
        applied(source, "Assign parameter to new field"),
        "class A {\n\tprivate int count;\n\tA(int count) {\n\t\tthis.count = count;\n\t}\n}\n"
    );
}

#[test]
fn make_static_adds_the_modifier() {
    let source = "class A {\n\tvoid $0f$1() {\n\t}\n}\n";
    assert_eq!(applied(source, "Make Static"), "class A {\n\tstatic void f() {\n\t}\n}\n");
}

#[test]
fn anonymous_class_to_lambda() {
    let source = "class A {\n\tRunnable r = new Runnable() {\n\t\tpublic void $0run() {\n\t\t\tf();\n\t\t}\n\t};\n\tvoid f() {}\n}\ninterface Runnable { void run(); }\n";
    assert_eq!(
        applied(source, "Convert to lambda expression"),
        "class A {\n\tRunnable r = () -> f();\n\tvoid f() {}\n}\ninterface Runnable { void run(); }\n"
    );
}

#[test]
fn qualified_type_gets_an_import() {
    let source = "package p;\nclass A {\n\tjava$0.util.List list;\n}\n";
    assert_eq!(applied(source, "Add import"), "package p;\n\nimport java.util.List;\nclass A {\n\tList list;\n}\n");
}

#[test]
fn sort_selected_members_moves_only_those() {
    let source = "class A {\n\tint b;\n\t$0void m() {}\n\tint a;$1\n}\n";
    assert_eq!(applied(source, "Sort Selected Members"), "class A {\n\tint b;\n\tint a;\n\tvoid m() {}\n}\n");
}
