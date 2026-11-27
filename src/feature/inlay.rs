//! What `textDocument/inlayHint` answers for Java, as jdtls answers it by
//! default (`java.inlayHints.parameterNames.enabled: literals`): the name
//! of the parameter before each literal argument of a call.
//!
//! As jdtls's `InlayHintVisitor` does, a call whose arguments do not match
//! its parameters in number (and that is not varargs) gets none; an
//! argument that is a lambda or is named as its parameter is skipped; the
//! varargs parameter reads `...name:`. A library method gets none: JDT reads
//! its parameter names from the class file, which has only `arg0, arg1`,
//! and jdtls hints no such name.

use crate::feature::semantic::{parameters_of, FileView, Found, Snapshot};
use crate::parser::{Position, Syn};

/// One hint: where it goes and what it says.
pub struct Hint {
    pub at: Position,
    pub label: String,
}

/// The hints of the calls in `file` between `from` and `to`.
pub fn hints(snapshot: &Snapshot, file: &FileView, from: Position, to: Position) -> Vec<Hint> {
    let mut hints = Vec::new();
    for node in file.tree.walk() {
        if node.end < from || node.start > to {
            continue;
        }
        // Only a literal is hinted: a call without one is not resolved.
        let has_literal = node
            .child("arguments")
            .is_some_and(|arguments| arguments.children.iter().any(is_literal));
        if !has_literal {
            continue;
        }
        let callee = match node.kind.as_str() {
            "method_invocation" => snapshot.invoked(node, file),
            "object_creation_expression" => snapshot.created(node, file),
            "explicit_constructor_invocation" => snapshot.chained(node, file),
            "enum_constant" if node.child("arguments").is_some() => constant_constructor(snapshot, file, node),
            _ => continue,
        };
        let (Some(callee), Some(arguments)) = (callee, node.child("arguments")) else {
            continue;
        };
        hints.extend(of_call(&callee, arguments));
    }
    hints
}

/// The constructor an enum constant's arguments go to.
fn constant_constructor<'a>(snapshot: &'a Snapshot, file: &'a FileView, constant: &Syn) -> Option<Found<'a>> {
    let owner = snapshot.enclosing(file, constant.start)?;
    snapshot.member(&owner, &owner.name.text, constant.child("arguments").map(|arguments| (arguments, file)))
        .filter(|found| found.node.kind == "constructor_declaration")
}

fn of_call(callee: &Found<'_>, arguments: &Syn) -> Vec<Hint> {
    if callee.file.library {
        return Vec::new();
    }
    let parameters = parameters_of(callee.node);
    if parameters.is_empty() {
        return Vec::new();
    }
    let passed: Vec<&Syn> = arguments
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
        .collect();
    if passed.is_empty() {
        return Vec::new();
    }
    let varargs = parameters.last().is_some_and(|last| last.kind == "spread_parameter");
    if !varargs && passed.len() != parameters.len() {
        return Vec::new();
    }
    let names: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            let declarator = parameter.first("variable_declarator").unwrap_or(parameter);
            declarator
                .child("name")
                .map(|name| name.text.clone())
                .unwrap_or_default()
        })
        .collect();
    if names.iter().any(String::is_empty) {
        return Vec::new();
    }
    passed
        .iter()
        .zip(&names)
        .enumerate()
        .filter(|(_, (argument, name))| is_literal(argument) && argument.text != **name)
        .map(|(index, (argument, name))| {
            let spread = if varargs && index == names.len() - 1 { "..." } else { "" };
            Hint {
                at: argument.start,
                label: format!("{spread}{name}:"),
            }
        })
        .collect()
}

/// A literal as JDT has them: a number, a string, a character, a boolean,
/// `null` or a class literal.
fn is_literal(argument: &Syn) -> bool {
    argument.kind.ends_with("_literal") && argument.kind != "array_literal"
        || matches!(argument.kind.as_str(), "true" | "false" | "text_block")
}
