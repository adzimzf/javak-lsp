//! The conversions between anonymous classes, lambdas and method
//! references jdtls offers, each with its edit:
//!
//! - in an anonymous class of a functional interface: `Convert to lambda
//!   expression` and `Convert anonymous to nested class`;
//! - in the parameters of a lambda: `Convert to anonymous class creation`;
//! - on a method reference: `Convert to lambda expression`, and `Add
//!   missing method 'm' to class 'C'` where nothing of its name is there.

use super::assists::Selection;
use super::{indentation, single, Action, Edit, QUICK_ASSIST};
use crate::feature::semantic::{is_anonymous_class, members_of, parameters_of, path_to, type_of_node, FileView, Found, Snapshot};
use crate::parser::{Position, Syn};

pub fn actions(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let path = path_to(&file.tree, selection.start);
    if let Some(anonymous) = path.iter().rev().find(|node| is_anonymous_class(node)) {
        actions.extend(anonymous_to_lambda(snapshot, file, anonymous));
        actions.extend(anonymous_to_nested(snapshot, file, anonymous, &path));
    }
    if let Some(lambda) = path.iter().rev().find(|node| node.kind == "lambda_expression") {
        // In its parameters or at its arrow, not in its body.
        let before_body = lambda.child("body").is_some_and(|body| selection.end <= body.start);
        if before_body {
            actions.extend(lambda_to_anonymous(snapshot, file, lambda));
        }
    }
    if let Some(reference) = path.iter().rev().find(|node| node.kind == "method_reference") {
        actions.extend(reference_actions(snapshot, file, reference, &path));
    }
    actions
}

/// The interface method a lambda or a reference at `node` implements,
/// with its parameter names and types as the interface gives them.
fn implemented<'a>(snapshot: &'a Snapshot, file: &'a FileView, node: &Syn) -> Option<Found<'a>> {
    snapshot.lambda_method(node, file)
}

/// The names of a method's parameters.
fn parameter_names(method: &Syn) -> Vec<String> {
    parameters_of(method)
        .into_iter()
        .filter_map(|parameter| {
            parameter
                .child("name")
                .or_else(|| parameter.first("variable_declarator").and_then(|declarator| declarator.child("name")))
                .map(|name| name.text.clone())
        })
        .collect()
}

/// `(a, b)`, or `a` alone.
fn lambda_parameters(names: &[String]) -> String {
    match names {
        [only] => only.clone(),
        _ => format!("({})", names.join(", ")),
    }
}

/// The single method of an anonymous class of an interface with one
/// abstract method, where the class holds nothing else.
fn single_method<'a>(snapshot: &Snapshot, file: &FileView, anonymous: &'a Syn) -> Option<&'a Syn> {
    let created = snapshot.of_type(&type_of_node(anonymous.child("type")?), file)?;
    if created.node.kind != "interface_declaration" {
        return None;
    }
    let abstract_methods = members_of(created.node)
        .iter()
        .filter(|(member, _)| {
            member.kind == "method_declaration"
                && member.child("body").is_none()
                && !member
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| matches!(word, "static" | "default")))
        })
        .count();
    if abstract_methods != 1 {
        return None;
    }
    let body = anonymous.children.iter().find(|child| child.kind == "class_body")?;
    let members: Vec<&Syn> = body
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "{" | "}") && !child.kind.ends_with("comment"))
        .collect();
    match members.as_slice() {
        [method] if method.kind == "method_declaration" => Some(method),
        _ => None,
    }
}

/// A block as a lambda's body: `{ return x; }` and `{ f(); }` as the
/// expression alone, anything else as the block.
fn lambda_body(file: &FileView, block: &Syn) -> String {
    let statements: Vec<&Syn> = block
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "{" | "}") && !child.kind.ends_with("comment"))
        .collect();
    let has_comment = block.children.iter().any(|child| child.kind.ends_with("comment"));
    if let ([statement], false) = (statements.as_slice(), has_comment) {
        let expression = statement.children.iter().find(|child| !matches!(child.kind.as_str(), "return" | ";"));
        if matches!(statement.kind.as_str(), "return_statement" | "expression_statement")
            && let Some(expression) = expression
        {
            return file.text_between(expression.start, expression.end);
        }
    }
    file.text_between(block.start, block.end)
}

fn anonymous_to_lambda(snapshot: &Snapshot, file: &FileView, anonymous: &Syn) -> Option<Action> {
    let method = single_method(snapshot, file, anonymous)?;
    let body = method.child("body")?;
    let text = format!("{} -> {}", lambda_parameters(&parameter_names(method)), lambda_body(file, body));
    Some(single(
        file,
        "Convert to lambda expression",
        QUICK_ASSIST,
        vec![Edit { start: anonymous.start, end: anonymous.end, text }],
    ))
}

/// The class body member around `path`'s end, and the class body.
fn top_member<'a>(path: &[&'a Syn], inside: &Syn) -> Option<&'a Syn> {
    // The member of the class that holds the anonymous class.
    let index = path
        .iter()
        .rposition(|node| matches!(node.kind.as_str(), "class_body" | "enum_body" | "enum_body_declarations") && node.start < inside.start && inside.end <= node.end && !std::ptr::eq(*node, inside))?;
    path.get(index + 1).copied()
}

fn anonymous_to_nested(snapshot: &Snapshot, file: &FileView, anonymous: &Syn, path: &[&Syn]) -> Option<Action> {
    let created = anonymous.child("type")?;
    let declaration = snapshot.of_type(&type_of_node(created), file)?;
    let relation = if declaration.node.kind == "interface_declaration" { "implements" } else { "extends" };
    let body = anonymous.children.iter().find(|child| child.kind == "class_body")?;
    let member = top_member(path, anonymous)?;
    let is_static = member
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "static"));
    let simple = created.text.split('<').next().unwrap_or_default().rsplit('.').next().unwrap_or_default();
    let name = format!("{simple}Implementation");
    let indent = indentation(file, member.start.line);
    // The anonymous class's body, re-indented to its new place.
    let body_indent = indentation(file, body.start.line);
    let body_text = file
        .text_between(body.start, body.end)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line.to_owned()
            } else {
                format!("{indent}{}", line.strip_prefix(body_indent.as_str()).unwrap_or(line.trim_start()))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let statically = if is_static { "static " } else { "" };
    let class = format!("\n\n{indent}private {statically}final class {name} {relation} {} {body_text}", created.text);
    let arguments = anonymous
        .child("arguments")
        .map_or_else(|| "()".to_owned(), |arguments| file.text_between(arguments.start, arguments.end));
    Some(single(
        file,
        "Convert anonymous to nested class",
        QUICK_ASSIST,
        vec![
            Edit {
                start: anonymous.start,
                end: anonymous.end,
                text: format!("new {name}{arguments}"),
            },
            Edit {
                start: member.end,
                end: member.end,
                text: class,
            },
        ],
    ))
}

fn lambda_to_anonymous(snapshot: &Snapshot, file: &FileView, lambda: &Syn) -> Option<Action> {
    let method = implemented(snapshot, file, lambda)?;
    let interface = snapshot.expected_type_of(lambda, file)?.written();
    let returns = snapshot.return_written(&method).unwrap_or_else(|| "void".to_owned());
    // The lambda's own names for the parameters, the interface's types.
    let written_names: Vec<String> = lambda
        .child("parameters")
        .map(|parameters| {
            if parameters.kind == "identifier" {
                vec![parameters.text.clone()]
            } else {
                parameters
                    .walk()
                    .into_iter()
                    .filter(|node| node.kind == "identifier")
                    .map(|node| node.text.clone())
                    .collect()
            }
        })
        .unwrap_or_default();
    let parameters: Vec<String> = snapshot
        .parameters_written(&method)
        .iter()
        .zip(written_names.iter().map(Some).chain(std::iter::repeat(None)))
        .map(|(declared, own)| match (declared.rsplit_once(' '), own) {
            (Some((written, _)), Some(own)) => format!("{written} {own}"),
            _ => declared.clone(),
        })
        .collect();
    let body = lambda.child("body")?;
    let indent = indentation(file, lambda.start.line);
    let unit = if indent.contains('\t') || indent.is_empty() { "\t" } else { "    " };
    let block = if body.kind == "block" {
        file.text_between(body.start, body.end)
    } else {
        let expression = file.text_between(body.start, body.end);
        let statement = if returns == "void" { format!("{expression};") } else { format!("return {expression};") };
        format!("{{\n{indent}{unit}{unit}{statement}\n{indent}{unit}}}")
    };
    let modifiers = method.node.first("modifiers").map(|modifiers| modifiers.text.clone()).unwrap_or_default();
    let visibility = if modifiers.contains("public") || method_in_interface(&method) { "public " } else { "" };
    let text = format!(
        "new {interface}() {{\n{indent}{unit}@Override\n{indent}{unit}{visibility}{returns} {}({}) {block}\n{indent}}}",
        method.name.text,
        parameters.join(", ")
    );
    Some(single(
        file,
        "Convert to anonymous class creation",
        QUICK_ASSIST,
        vec![Edit { start: lambda.start, end: lambda.end, text }],
    ))
}

fn method_in_interface(method: &Found<'_>) -> bool {
    path_to(&method.file.tree, method.node.start)
        .iter()
        .any(|node| node.kind == "interface_declaration")
}

/// A value of the type written, as a generated method returns it.
fn default_value(written: &str) -> &'static str {
    match written {
        "boolean" => "false",
        "int" | "long" | "short" | "byte" | "char" | "float" | "double" => "0",
        _ => "null",
    }
}

fn reference_actions(snapshot: &Snapshot, file: &FileView, reference: &Syn, path: &[&Syn]) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(method) = implemented(snapshot, file, reference) else {
        return actions;
    };
    let (Some(receiver), Some(name)) = (reference.children.first(), reference.children.last()) else {
        return actions;
    };
    if name.kind != "identifier" {
        return actions;
    }
    let names = parameter_names(method.node);
    let call_on = if receiver.kind == "this" { String::new() } else { format!("{}.", receiver.text) };
    actions.push(single(
        file,
        "Convert to lambda expression",
        QUICK_ASSIST,
        vec![Edit {
            start: reference.start,
            end: reference.end,
            text: format!("{} -> {call_on}{}({})", lambda_parameters(&names), name.text, names.join(", ")),
        }],
    ));
    // `this::action` of a class without an `action`: one that fits.
    let class = if receiver.kind == "this" {
        snapshot.enclosing(file, reference.start)
    } else {
        snapshot.type_named(&receiver.text, file)
    };
    let Some(class) = class.filter(|class| class.file.uri == file.uri) else {
        return actions;
    };
    if !snapshot.members_called(&class, &name.text).is_empty() {
        return actions;
    }
    let Some(member) = top_member(path, reference) else {
        return actions;
    };
    let returns = snapshot.return_written(&method).unwrap_or_else(|| "void".to_owned());
    let indent = indentation(file, member.start.line);
    let unit = if indent.contains('\t') || indent.is_empty() { "\t" } else { "    " };
    let statically = if receiver.kind == "this" { "" } else { "static " };
    let body = if returns == "void" {
        String::new()
    } else {
        format!("\n{indent}{unit}return {};", default_value(&returns))
    };
    let parameters = snapshot.parameters_written(&method).join(", ");
    let at: Position = member.end;
    actions.push(single(
        file,
        format!("Add missing method '{}' to class '{}'", name.text, class.name.text),
        "quickfix",
        vec![Edit {
            start: at,
            end: at,
            text: format!("\n\n{indent}private {statically}{returns} {}({parameters}) {{{body}\n{indent}}}", name.text),
        }],
    ));
    actions
}
