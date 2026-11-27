//! The refactorings jdtls offers on statements, local variables and
//! expressions, each with the edit it makes:
//!
//! - `Surround with try/catch` on the statement the cursor is in, or the
//!   statements a selection touches;
//! - on the name of a local variable: `Extract to field`, and for one with
//!   a value `Split variable declaration` and `Change type of 'x' to 'var'`
//!   (where the value has the very type declared);
//! - on statements selected whole, or an expression selected exactly:
//!   `Extract to method`; on such an expression also `Extract to local
//!   variable` (and `(replace all occurrences)`), `Extract to field`,
//!   `Extract to constant` (when it reads no local) and `Introduce
//!   Parameter...`, which asks for a name and is offered disabled.

use std::collections::HashSet;

use super::assists::{needs_input, Selection};
use super::{decapitalized, fresh, indentation, names_around, single, Action, Edit, QUICK_ASSIST};
use crate::feature::semantic::{is_type_declaration, path_to, FileView, Snapshot};
use crate::parser::{Position, Syn};

pub const EXTRACT: &str = "refactor.extract";
const SURROUND: &str = "refactor.surround";

/// Every refactoring of this module the selection allows.
pub fn actions(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    actions.extend(surround_with_try(snapshot, file, selection));
    actions.extend(on_local_name(snapshot, file, selection));
    actions.extend(extractions(snapshot, file, selection));
    actions
}

// ------------------------------------------------------------- helpers

/// Whether a node is a statement a block holds.
fn is_block_statement(node: &Syn) -> bool {
    !matches!(node.kind.as_str(), "{" | "}") && !node.kind.ends_with("comment")
}

/// Whether `node` holds statements: a block or a constructor's body.
fn holds_statements(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "block" | "constructor_body" | "switch_block_statement_group")
}

/// The method, constructor or initializer whose body holds `at`.
fn enclosing_callable<'a>(path: &[&'a Syn]) -> Option<&'a Syn> {
    path.iter()
        .rev()
        .find(|node| {
            matches!(
                node.kind.as_str(),
                "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" | "static_initializer" | "lambda_expression"
            ) || node.kind == "block" && path_parent_is_class_body(path, node)
        })
        .copied()
}

fn path_parent_is_class_body(path: &[&Syn], node: &Syn) -> bool {
    path.iter()
        .position(|around| std::ptr::eq(*around, node))
        .and_then(|index| index.checked_sub(1))
        .is_some_and(|index| path[index].kind == "class_body")
}

/// The statements of one block under the selection: the statement around
/// the cursor, or each one a selection touches.
fn statements_under<'a>(file: &'a FileView, selection: &Selection) -> Option<(&'a Syn, Vec<&'a Syn>)> {
    let path = path_to(&file.tree, selection.start);
    if !selection.is_empty() {
        // The innermost block around the whole selection.
        let end_path = path_to(&file.tree, selection.end);
        let block = path
            .iter()
            .rev()
            .find(|node| holds_statements(node) && end_path.iter().any(|other| std::ptr::eq(*other, **node)))
            .copied()?;
        let touched: Vec<&Syn> = block
            .children
            .iter()
            .filter(|child| is_block_statement(child) && selection.touches(child))
            .collect();
        if !touched.is_empty() {
            return Some((block, touched));
        }
    }
    let index = path.iter().rposition(|node| holds_statements(node) && node.start < selection.start && selection.start < node.end)?;
    let block = path[index];
    let statement = path.get(index + 1).copied().filter(|statement| is_block_statement(statement))?;
    Some((block, vec![statement]))
}

/// `text` with every line after the first indented once more.
fn indented_once(text: &str, unit: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(index, line)| if index == 0 || line.trim().is_empty() { line.to_owned() } else { format!("{unit}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One level of the file's indentation: a tab where it indents with tabs.
fn unit_of(indent: &str) -> &'static str {
    if indent.contains('\t') || indent.is_empty() { "\t" } else { "    " }
}

/// The locals and parameters `node` reads that are declared in
/// `callable` outside of `node`, in the order first read, each with its
/// type as written.
fn outer_locals(snapshot: &Snapshot, file: &FileView, callable: &Syn, node_start: Position, node_end: Position) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for name in file.tree.walk() {
        if name.kind != "identifier" || name.start < node_start || name.end > node_end {
            continue;
        }
        let Some(declaration) = snapshot.declaration_at(&file.uri, name.start) else {
            continue;
        };
        if declaration.file.uri != file.uri
            || declaration.node.start < callable.start
            || declaration.node.end > callable.end
            || (node_start <= declaration.name.start && declaration.name.start < node_end)
            || is_type_declaration(declaration.node)
            || matches!(declaration.node.kind.as_str(), "method_declaration" | "constructor_declaration")
        {
            continue;
        }
        if !seen.insert(declaration.name.text.clone()) {
            continue;
        }
        let written = snapshot
            .type_of_declaration(&declaration)
            .map(|written| written.written())
            .unwrap_or_else(|| "Object".to_owned());
        found.push((written, declaration.name.text.clone()));
    }
    found
}

/// The class body holding `at`, and the member of it the cursor is in.
fn member_around<'a>(path: &[&'a Syn]) -> Option<(&'a Syn, &'a Syn)> {
    let index = path
        .iter()
        .rposition(|node| matches!(node.kind.as_str(), "class_body" | "enum_body" | "enum_body_declarations"))?;
    Some((path[index], *path.get(index + 1)?))
}

/// An edit putting `member` as the first member of `body`.
fn new_first_member(file: &FileView, body: &Syn, member: &str) -> Edit {
    let open = Position {
        line: body.start.line,
        column: body.start.column + 1,
    };
    let first = body.children.iter().find(|child| !matches!(child.kind.as_str(), "{" | "}"));
    let indent = first.map_or_else(|| "\t".to_owned(), |first| indentation(file, first.start.line));
    Edit {
        start: open,
        end: open,
        text: format!("\n{indent}{member}"),
    }
}

/// An edit putting `method` after `member`.
fn after_member(file: &FileView, member: &Syn, method: &str) -> Edit {
    let indent = indentation(file, member.start.line);
    let unit = unit_of(&indent);
    let text = method
        .lines()
        .map(|line| if line.is_empty() { String::new() } else { format!("{indent}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("\t", unit);
    Edit {
        start: member.end,
        end: member.end,
        text: format!("\n\n{text}"),
    }
}

// ------------------------------------------------------------ surround

/// `Surround with try/catch`: the statements in `try { … }`, a
/// `catch (Exception e)` printing its trace after them. A local declared
/// there and read after stays declared before the `try`.
fn surround_with_try(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Option<Action> {
    // JDT surrounds what is selected, and nothing inside a lambda or a
    // method reference of a statement.
    if selection.is_empty() || file.text_between(selection.start, selection.end).trim().is_empty() {
        return None;
    }
    let (block, statements) = statements_under(file, selection)?;
    if let [statement] = statements.as_slice() {
        let path = path_to(statement, selection.start);
        if path.iter().any(|node| matches!(node.kind.as_str(), "lambda_expression" | "method_reference")) {
            return None;
        }
    }
    let first = statements.first()?;
    let last = statements.last()?;
    let indent = indentation(file, first.start.line);
    let unit = unit_of(&indent);
    let inner = format!("{indent}{unit}");
    let mut before = String::new();
    let mut body: Vec<String> = Vec::new();
    for statement in &statements {
        let text = file.text_between(statement.start, statement.end);
        // A declaration read after the `try` is kept outside of it.
        if statement.kind == "local_variable_declaration" {
            let read_after = statement.all("variable_declarator").any(|declarator| {
                declarator.child("name").is_some_and(|name| {
                    block.walk().into_iter().any(|node| node.kind == "identifier" && node.text == name.text && node.start > last.end)
                })
            });
            if read_after
                && let Some(written) = statement.child("type")
            {
                let names: Vec<&str> = statement
                    .all("variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .map(|name| name.text.as_str())
                    .collect();
                before.push_str(&format!("{} {};\n{indent}", written.text, names.join(", ")));
                for declarator in statement.all("variable_declarator") {
                    if declarator.child("value").is_some() {
                        body.push(format!("{};", file.text_between(declarator.start, declarator.end)));
                    }
                }
                continue;
            }
        }
        body.push(indented_once(&text, unit));
    }
    let _ = snapshot;
    let text = format!(
        "{before}try {{\n{inner}{}\n{indent}}} catch (Exception e) {{\n{inner}e.printStackTrace();\n{indent}}}",
        body.join(&format!("\n{inner}"))
    );
    Some(single(file, "Surround with try/catch", SURROUND, vec![Edit { start: first.start, end: last.end, text }]))
}

// ---------------------------------------------------------- local names

/// The local declaration and its declarator whose name the selection is
/// on, nothing more.
fn local_name_at<'a>(file: &'a FileView, selection: &Selection) -> Option<(&'a Syn, &'a Syn, &'a Syn)> {
    let path = path_to(&file.tree, selection.start);
    let index = path.iter().rposition(|node| node.kind == "variable_declarator")?;
    let declarator = path[index];
    let declaration = *path.get(index.checked_sub(1)?)?;
    if declaration.kind != "local_variable_declaration" {
        return None;
    }
    let name = declarator.child("name")?;
    let on_name = name.start <= selection.start && selection.end <= name.end;
    on_name.then_some((declaration, declarator, name))
}

/// `Extract to field`, `Split variable declaration` and `Change type of
/// 'x' to 'var'` on a local's name.
fn on_local_name(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some((declaration, declarator, name)) = local_name_at(file, selection) else {
        return actions;
    };
    let Some(written) = declaration.child("type") else {
        return actions;
    };
    let path = path_to(&file.tree, declaration.start);
    // In a `for`'s header nothing moves.
    let parent = path
        .iter()
        .position(|node| std::ptr::eq(*node, declaration))
        .and_then(|index| index.checked_sub(1))
        .map(|index| path[index]);
    if parent.is_none_or(|parent| !holds_statements(parent)) {
        return actions;
    }
    let declarators: Vec<&Syn> = declaration.all("variable_declarator").collect();
    let value = declarator.child("value");
    let indent = indentation(file, declaration.start.line);
    let modifiers = declaration
        .first("modifiers")
        .map(|modifiers| format!("{} ", modifiers.text))
        .unwrap_or_default();
    // Extract to field: the declaration becomes an assignment, or goes.
    if written.text != "var"
        && let Some((body, _)) = member_around(&path)
    {
        let field = new_first_member(file, body, &format!("private {} {};", written.text, name.text));
        let mut edits = vec![field];
        if declarators.len() == 1 {
            let replacement = match value {
                Some(value) => format!("{} = {};", name.text, file.text_between(value.start, value.end)),
                None => String::new(),
            };
            edits.push(Edit {
                start: declaration.start,
                end: declaration.end,
                text: replacement,
            });
        } else {
            let others: Vec<String> = declarators
                .iter()
                .filter(|other| !std::ptr::eq(**other, declarator))
                .map(|other| file.text_between(other.start, other.end))
                .collect();
            let mut replacement = format!("{modifiers}{} {};", written.text, others.join(", "));
            if let Some(value) = value {
                replacement.push_str(&format!("\n{indent}{} = {};", name.text, file.text_between(value.start, value.end)));
            }
            edits.push(Edit {
                start: declaration.start,
                end: declaration.end,
                text: replacement,
            });
        }
        actions.push(single(file, "Extract to field", EXTRACT, edits));
    }
    let (Some(value), [_]) = (value, declarators.as_slice()) else {
        return actions;
    };
    actions.push(single(
        file,
        "Split variable declaration",
        QUICK_ASSIST,
        vec![Edit {
            start: declaration.start,
            end: declaration.end,
            text: format!(
                "{modifiers}{} {};\n{indent}{} = {};",
                written.text,
                name.text,
                name.text,
                file.text_between(value.start, value.end)
            ),
        }],
    ));
    // `var` where the value has the type declared, no more, no less.
    let same_type = written.text != "var"
        && !matches!(value.kind.as_str(), "array_initializer" | "lambda_expression" | "null_literal" | "method_reference")
        && snapshot
            .type_of_expression(value, file)
            .is_some_and(|given| given.written() == written.text.split_whitespace().collect::<String>());
    if same_type {
        actions.push(single(
            file,
            format!("Change type of '{}' to 'var'", name.text),
            QUICK_ASSIST,
            vec![Edit {
                start: written.start,
                end: written.end,
                text: "var".to_owned(),
            }],
        ));
    }
    actions
}

// ---------------------------------------------------------- extractions

/// What a selection picks out to extract: whole statements of one block,
/// or one expression exactly.
enum Picked<'a> {
    Statements(&'a Syn, Vec<&'a Syn>),
    Expression(&'a Syn),
}

fn is_expression(node: &Syn) -> bool {
    node.kind.ends_with("_expression")
        || node.kind.ends_with("_literal")
        || matches!(node.kind.as_str(), "method_invocation" | "field_access" | "array_access" | "true" | "false" | "this")
}

fn picked<'a>(file: &'a FileView, selection: &Selection) -> Option<Picked<'a>> {
    if selection.is_empty() {
        return None;
    }
    let text = file.text_between(selection.start, selection.end);
    // The selection less the blanks at its ends.
    let leading = text.len() - text.trim_start().len();
    let start = advanced(selection.start, &text[..leading]);
    let end = advanced(selection.start, text.trim_end());
    let path = path_to(&file.tree, start);
    if let Some(expression) = path.iter().rev().find(|node| node.start == start && node.end == end && is_expression(node)) {
        return Some(Picked::Expression(expression));
    }
    let (block, statements) = statements_under(file, &Selection { start, end })?;
    let whole = statements.first()?.start == start && statements.last()?.end == end;
    whole.then_some(Picked::Statements(block, statements))
}

/// The position `text` reaches written from `from`.
fn advanced(from: Position, text: &str) -> Position {
    let mut at = from;
    for character in text.chars() {
        if character == '\n' {
            at.line += 1;
            at.column = 0;
        } else {
            at.column += character.len_utf16() as u32;
        }
    }
    at
}

/// A name for a value of the type written: `HashMap` gives `hashMap`.
fn name_for(written: &str) -> String {
    let simple = written.split('<').next().unwrap_or_default().rsplit('.').next().unwrap_or_default();
    let simple = simple.trim_end_matches("[]");
    if simple.is_empty() || simple.chars().next().is_some_and(char::is_lowercase) {
        return "i".to_owned();
    }
    decapitalized(simple)
}

/// `HASH_MAP` for `hashMap`.
fn constant_name(name: &str) -> String {
    let mut constant = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            constant.push('_');
        }
        constant.extend(character.to_uppercase());
    }
    constant
}

fn extractions(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(picked) = picked(file, selection) else {
        return actions;
    };
    let anchor = match &picked {
        Picked::Statements(_, statements) => statements[0].start,
        Picked::Expression(expression) => expression.start,
    };
    let path = path_to(&file.tree, anchor);
    let Some(callable) = enclosing_callable(&path) else {
        return actions;
    };
    let Some((_, member)) = member_around(&path) else {
        return actions;
    };
    let is_static = member
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "static"));
    let statically = if is_static { "static " } else { "" };
    let mut taken = names_around(file, callable);
    match picked {
        Picked::Statements(block, statements) => {
            let first = statements[0];
            let last = *statements.last().expect("one statement at least");
            let parameters = outer_locals(snapshot, file, callable, first.start, last.end);
            // A local declared here and read after is what the method returns.
            let mut returned: Vec<(String, String)> = Vec::new();
            for statement in &statements {
                if statement.kind != "local_variable_declaration" {
                    continue;
                }
                let Some(written) = statement.child("type") else {
                    continue;
                };
                for declarator in statement.all("variable_declarator") {
                    let Some(name) = declarator.child("name") else {
                        continue;
                    };
                    let read_after = block
                        .walk()
                        .into_iter()
                        .any(|node| node.kind == "identifier" && node.text == name.text && node.start > last.end);
                    if read_after {
                        returned.push((written.text.clone(), name.text.clone()));
                    }
                }
            }
            if returned.len() > 1 || statements.iter().any(|statement| statement.kind == "return_statement") {
                return actions;
            }
            let method_name = fresh("extracted", &mut taken);
            let arguments: Vec<&str> = parameters.iter().map(|(_, name)| name.as_str()).collect();
            let declared: Vec<String> = parameters.iter().map(|(written, name)| format!("{written} {name}")).collect();
            let indent = indentation(file, first.start.line);
            let body_lines: Vec<String> = statements
                .iter()
                .map(|statement| {
                    let text = file.text_between(statement.start, statement.end);
                    text.lines()
                        .enumerate()
                        .map(|(index, line)| {
                            let line = if index == 0 { line } else { line.strip_prefix(indent.as_str()).unwrap_or(line) };
                            format!("\t{line}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect();
            let (returns, call, tail) = match returned.first() {
                Some((written, name)) => (
                    written.clone(),
                    format!("{written} {name} = {method_name}({});", arguments.join(", ")),
                    format!("\n\treturn {name};"),
                ),
                None => ("void".to_owned(), format!("{method_name}({});", arguments.join(", ")), String::new()),
            };
            let method = format!(
                "private {statically}{returns} {method_name}({}) {{\n{}{tail}\n}}",
                declared.join(", "),
                body_lines.join("\n")
            );
            actions.push(single(
                file,
                "Extract to method",
                EXTRACT,
                vec![Edit { start: first.start, end: last.end, text: call }, after_member(file, member, &method)],
            ));
        }
        Picked::Expression(expression) => {
            let Some(written) = snapshot.type_of_expression(expression, file).map(|written| written.written()) else {
                return actions;
            };
            if written == "void" {
                return actions;
            }
            let Some(statement) = path
                .iter()
                .rposition(|node| holds_statements(node))
                .and_then(|index| path.get(index + 1))
                .copied()
            else {
                return actions;
            };
            let text = file.text_between(expression.start, expression.end);
            let parameters = outer_locals(snapshot, file, callable, expression.start, expression.end);
            let local = fresh(&name_for(&written), &mut taken);
            let indent = indentation(file, statement.start.line);
            let declare = Edit {
                start: statement.start,
                end: statement.start,
                text: format!("{written} {local} = {text};\n{indent}"),
            };
            let replace = |by: &str| Edit {
                start: expression.start,
                end: expression.end,
                text: by.to_owned(),
            };
            actions.push(single(file, "Extract to local variable", EXTRACT, vec![declare.clone(), replace(&local)]));
            // Every occurrence of the same text in the body.
            let mut all = vec![declare];
            for node in callable.walk() {
                if node.text == text && is_expression(node) && node.kind == expression.kind {
                    all.push(Edit {
                        start: node.start,
                        end: node.end,
                        text: local.clone(),
                    });
                }
            }
            actions.push(single(file, "Extract to local variable (replace all occurrences)", EXTRACT, all));
            let method_name = fresh("extracted", &mut taken);
            let arguments: Vec<&str> = parameters.iter().map(|(_, name)| name.as_str()).collect();
            let declared: Vec<String> = parameters.iter().map(|(written, name)| format!("{written} {name}")).collect();
            let method = format!("private {statically}{written} {method_name}({}) {{\n\treturn {text};\n}}", declared.join(", "));
            actions.push(single(
                file,
                "Extract to method",
                EXTRACT,
                vec![replace(&format!("{method_name}({})", arguments.join(", "))), after_member(file, member, &method)],
            ));
            if let Some((body, _)) = member_around(&path) {
                let field = fresh(&local, &mut names_around(file, callable));
                if parameters.is_empty() {
                    actions.push(single(
                        file,
                        "Extract to field",
                        EXTRACT,
                        vec![
                            new_first_member(file, body, &format!("private {statically}{written} {field} = {text};")),
                            replace(&field),
                        ],
                    ));
                    let constant = constant_name(&local);
                    actions.push(single(
                        file,
                        "Extract to constant",
                        EXTRACT,
                        vec![
                            new_first_member(file, body, &format!("private static final {written} {constant} = {text};")),
                            replace(&constant),
                        ],
                    ));
                } else {
                    actions.push(single(
                        file,
                        "Extract to field",
                        EXTRACT,
                        vec![
                            new_first_member(file, body, &format!("private {statically}{written} {field};")),
                            Edit {
                                start: statement.start,
                                end: statement.start,
                                text: format!("{field} = {text};\n{indent}"),
                            },
                            replace(&field),
                        ],
                    ));
                }
            }
            actions.push(needs_input(file, "Introduce Parameter...", EXTRACT));
        }
    }
    actions
}
