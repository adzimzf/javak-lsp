//! The assists of a method's or a constructor's header, as jdtls offers
//! them there:
//!
//! - `Change signature for 'm'`, which asks for the new signature and is
//!   offered as the command that asks;
//! - `Make Static` on an instance method of a class that reads no member of
//!   its instance;
//! - on a parameter's name: `Assign parameter to new field`, `Assign
//!   parameter to field 'f'` for each field it fits, and `Assign all
//!   parameters to new fields` where there are several;
//! - `Inline Method` on the name of a private method nothing calls, which
//!   then goes.

use super::assists::{needs_input, Selection};
use super::{deeper, fresh, indentation, single, Action, Edit, CHANGE_SIGNATURE, INLINE, QUICK_ASSIST};
use crate::feature::semantic::{is_anonymous_class, is_type_declaration, parameters_of, path_to, FileView, Snapshot};
use crate::parser::{Position, Syn};

pub fn actions(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let path = path_to(&file.tree, selection.start);
    let Some(index) = path
        .iter()
        .rposition(|node| matches!(node.kind.as_str(), "method_declaration" | "constructor_declaration"))
    else {
        return actions;
    };
    let method = path[index];
    // The header: anything of the declaration but its body.
    let body = method.child("body");
    // A selection reaching past the declaration is about the members.
    if body.is_some_and(|body| body.start <= selection.start) || selection.end > method.end {
        return actions;
    }
    let Some(name) = method.child("name") else {
        return actions;
    };
    let owner = path[..index].iter().rev().find(|node| is_type_declaration(node) || is_anonymous_class(node)).copied();
    actions.push(needs_input(file, format!("Change signature for '{}'", name.text), CHANGE_SIGNATURE, "changeSignature"));
    actions.extend(make_static(snapshot, file, method, owner, &path[..index]));
    actions.extend(assign_parameters(file, method, owner, selection));
    actions.extend(inline_unused(snapshot, file, method, name, selection));
    actions
}

fn says(node: &Syn, word: &str) -> bool {
    node.first("modifiers").is_some_and(|modifiers| {
        modifiers
            .children
            .iter()
            .any(|modifier| modifier.kind == word || modifier.child("name").is_some_and(|named| named.text == word))
    })
}

/// `Make Static`: an instance method with a body, of a class that is not
/// an inner one, that overrides nothing and reads no member of its own
/// instance.
fn make_static(snapshot: &Snapshot, file: &FileView, method: &Syn, owner: Option<&Syn>, around: &[&Syn]) -> Option<Action> {
    if method.kind != "method_declaration" || says(method, "static") || says(method, "abstract") || says(method, "Override") {
        return None;
    }
    let body = method.child("body")?;
    let owner = owner.filter(|owner| owner.kind == "class_declaration")?;
    // A class inside another's body must itself be static.
    let nested = around.iter().any(|node| is_type_declaration(node) && !std::ptr::eq(*node, owner));
    if nested && !says(owner, "static") {
        return None;
    }
    for node in body.walk() {
        if matches!(node.kind.as_str(), "this" | "super") {
            return None;
        }
        if node.kind != "identifier" {
            continue;
        }
        let Some(found) = snapshot.declaration_at(&file.uri, node.start) else {
            continue;
        };
        let member = matches!(found.node.kind.as_str(), "method_declaration" | "field_declaration" | "variable_declarator");
        let of_owner = found.file.uri == file.uri && owner.start <= found.node.start && found.node.end <= owner.end;
        let in_method = method.start <= found.node.start && found.node.end <= method.end;
        if !member || !of_owner || in_method {
            continue;
        }
        let declaration = if found.node.kind == "variable_declarator" {
            path_to(&file.tree, found.node.start)
                .into_iter()
                .rev()
                .find(|around| around.kind == "field_declaration")
        } else {
            Some(found.node)
        };
        if declaration.is_some_and(|declaration| !says(declaration, "static")) {
            return None;
        }
    }
    let at = method
        .child("type_parameters")
        .or_else(|| method.child("type"))
        .map_or(method.start, |written| written.start);
    Some(single(file, "Make Static", INLINE, vec![Edit { start: at, end: at, text: "static ".to_owned() }]))
}

/// A parameter's type as written and its name.
fn parameter_parts(parameter: &Syn) -> Option<(String, &Syn)> {
    let name = parameter
        .child("name")
        .or_else(|| parameter.first("variable_declarator").and_then(|declarator| declarator.child("name")))?;
    let written = parameter
        .child("type")
        .or_else(|| parameter.children.iter().find(|child| child.kind.ends_with("_type") || child.kind == "type_identifier"))?;
    let mut written: String = written.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if parameter.kind == "spread_parameter" {
        written.push_str("[]");
    }
    Some((written, name))
}

fn assign_parameters(file: &FileView, method: &Syn, owner: Option<&Syn>, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let (Some(owner), Some(body)) = (owner, method.child("body")) else {
        return actions;
    };
    let Some(class_body) = owner.child("body").or_else(|| owner.first("class_body")) else {
        return actions;
    };
    let parameters: Vec<(String, &Syn)> = parameters_of(method).into_iter().filter_map(parameter_parts).collect();
    let Some((written, name)) = parameters
        .iter()
        .find(|(_, name)| name.start <= selection.start && selection.end <= name.end)
    else {
        return actions;
    };
    let is_static = says(method, "static");
    let statically = if is_static { "static " } else { "" };
    let holder = if is_static {
        owner.child("name").map_or_else(|| "this".to_owned(), |named| named.text.clone())
    } else {
        "this".to_owned()
    };
    // The names the class already has for its members.
    let mut taken: std::collections::HashSet<String> = class_body
        .walk()
        .into_iter()
        .filter(|node| node.kind == "variable_declarator")
        .filter_map(|declarator| declarator.child("name"))
        .map(|named| named.text.clone())
        .collect();
    // Statements go first in the body, after a `super(…)` or `this(…)`.
    let after = body
        .children
        .iter()
        .find(|child| child.kind == "explicit_constructor_invocation")
        .map_or(Position { line: body.start.line, column: body.start.column + 1 }, |call| call.end);
    let inner = deeper(&indentation(file, method.start.line));
    let member_indent = indentation(file, method.start.line);
    let class_open = Position {
        line: class_body.start.line,
        column: class_body.start.column + 1,
    };
    let assign_new = |taken: &mut std::collections::HashSet<String>, written: &str, name: &Syn| -> Vec<Edit> {
        let field = fresh(&name.text, taken);
        vec![
            Edit {
                start: class_open,
                end: class_open,
                text: format!("\n{member_indent}private {statically}{written} {field};"),
            },
            Edit {
                start: after,
                end: after,
                text: format!("\n{inner}{holder}.{field} = {};", name.text),
            },
        ]
    };
    actions.push(single(file, "Assign parameter to new field", QUICK_ASSIST, assign_new(&mut taken.clone(), written, name)));
    // A field of the class the parameter's value fits.
    for field in class_body.children.iter().filter(|member| member.kind == "field_declaration") {
        let field_static = says(field, "static");
        if says(field, "final") || field_static != is_static {
            continue;
        }
        let field_type: String = field.child("type").map(|written| written.text.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
        if field_type != *written {
            continue;
        }
        for declarator in field.all("variable_declarator") {
            let Some(field_name) = declarator.child("name") else {
                continue;
            };
            actions.push(single(
                file,
                format!("Assign parameter to field '{}'", field_name.text),
                QUICK_ASSIST,
                vec![Edit {
                    start: after,
                    end: after,
                    text: format!("\n{inner}{holder}.{} = {};", field_name.text, name.text),
                }],
            ));
        }
    }
    if parameters.len() > 1 {
        let mut fields = String::new();
        let mut assignments = String::new();
        for (written, name) in &parameters {
            let field = fresh(&name.text, &mut taken);
            fields.push_str(&format!("\n{member_indent}private {statically}{written} {field};"));
            assignments.push_str(&format!("\n{inner}{holder}.{field} = {};", name.text));
        }
        actions.push(single(
            file,
            "Assign all parameters to new fields",
            QUICK_ASSIST,
            vec![
                Edit { start: class_open, end: class_open, text: fields },
                Edit { start: after, end: after, text: assignments },
            ],
        ));
    }
    actions
}

/// `Inline Method` on the name of a private method nothing calls: inlined
/// into no call, it is removed.
fn inline_unused(snapshot: &Snapshot, file: &FileView, method: &Syn, name: &Syn, selection: &Selection) -> Option<Action> {
    if method.kind != "method_declaration" || !says(method, "private") || method.child("body").is_none() {
        return None;
    }
    if !(name.start <= selection.start && selection.end <= name.end) {
        return None;
    }
    let found = snapshot.declaration_at(&file.uri, name.start)?;
    let called = snapshot.uses_in(&found, None).into_iter().any(|(uri, at)| *uri != file.uri || at != name.start);
    if called {
        return None;
    }
    // The whole lines the method is on.
    let start = Position { line: method.start.line, column: 0 };
    let end = Position { line: method.end.line + 1, column: 0 };
    Some(single(file, "Inline Method", INLINE, vec![Edit { start, end, text: String::new() }]))
}
