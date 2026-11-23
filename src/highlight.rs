//! What `textDocument/documentHighlight` points at, as jdtls's
//! `Highlight.java` does: besides the uses of a name, it shows where a
//! method exits, where a declared exception is thrown, the loop a `break`
//! leaves, and the methods implementing an interface.

use crate::parser::{Position, Syn};
use crate::semantic::{
    is_identifier, members_of, parameters_of, path_to, type_of_node, FileView, Snapshot,
};

/// The places jdtls highlights for a cursor, or `None` where the cursor is
/// on a plain name and the uses of that name are the answer.
pub fn special(snapshot: &Snapshot, file: &FileView, at: Position) -> Option<Vec<Position>> {
    let path = path_to(&file.tree, at);
    if let Some(places) = loop_left(&path) {
        return Some(places);
    }
    if let Some(places) = exits(&path, at) {
        return Some(places);
    }
    if let Some(places) = thrown(&path, file, at) {
        return Some(places);
    }
    implementations(snapshot, &path, file, at)
}

/// A `break`/`continue` with a label: the loop it leaves, start and end.
fn loop_left(path: &[&Syn]) -> Option<Vec<Position>> {
    let jump = path
        .iter()
        .rev()
        .find(|node| matches!(node.kind.as_str(), "break_statement" | "continue_statement"))?;
    let label = jump.children.iter().find(|child| is_identifier(child))?;
    let labelled = path.iter().find(|node| {
        node.kind == "labeled_statement"
            && node
                .children
                .first()
                .is_some_and(|named| named.text == label.text)
    })?;
    let last_brace = Position {
        line: labelled.end.line,
        column: labelled.end.column.saturating_sub(1),
    };
    Some(vec![labelled.start, last_brace])
}

/// The cursor on a method's return type or on a `return`: every way the
/// method leaves — its return type, its `return`s and its `throw`s.
fn exits(path: &[&Syn], at: Position) -> Option<Vec<Position>> {
    let method = path
        .iter()
        .rev()
        .find(|node| node.kind == "method_declaration")?;
    let returns = method.child("type")?;
    // The cursor is on the type or on the `return` word itself, not
    // merely somewhere in what a `return` returns.
    let on_an_exit = returns.covers(at)
        || path
            .iter()
            .filter(|node| node.kind == "return_statement")
            .any(|statement| statement.first("return").is_some_and(|word| word.covers(at)));
    if !on_an_exit {
        return None;
    }
    let mut places = vec![returns.start];
    places.extend(
        method
            .walk()
            .into_iter()
            .filter(|node| matches!(node.kind.as_str(), "return_statement" | "throw_statement"))
            .map(|node| node.start),
    );
    Some(places)
}

/// The cursor on an exception a method declares it throws: the name, and
/// every `throw` of that exception in the method.
fn thrown(path: &[&Syn], file: &FileView, at: Position) -> Option<Vec<Position>> {
    let throws = path.iter().rev().find(|node| node.kind == "throws")?;
    let name = throws
        .walk()
        .into_iter()
        .find(|node| is_identifier(node) && node.covers(at))?;
    let method = path
        .iter()
        .rev()
        .find(|node| node.kind == "method_declaration")?;
    let mut places = vec![name.start];
    places.extend(
        method
            .walk()
            .into_iter()
            .filter(|node| node.kind == "throw_statement")
            .filter(|node| throws_the_same(node, &name.text))
            .map(|node| node.start),
    );
    let _ = file;
    Some(places)
}

/// Whether a `throw` throws something called `name`.
fn throws_the_same(statement: &Syn, name: &str) -> bool {
    statement
        .walk()
        .into_iter()
        .filter(|node| node.kind == "object_creation_expression")
        .any(|created| {
            created
                .child("type")
                .map(type_of_node)
                .is_some_and(|written| written.name == name)
        })
}

/// The cursor on an interface a class implements: the name, and the
/// methods of the class that implement the interface's.
fn implementations(
    snapshot: &Snapshot,
    path: &[&Syn],
    file: &FileView,
    at: Position,
) -> Option<Vec<Position>> {
    let list = path
        .iter()
        .rev()
        .find(|node| matches!(node.kind.as_str(), "super_interfaces" | "superclass"))?;
    let name = list
        .walk()
        .into_iter()
        .find(|node| is_identifier(node) && node.covers(at))?;
    let class = path
        .iter()
        .rev()
        .find(|node| node.kind == "class_declaration")?;
    let interface = snapshot.type_named(&name.text, file)?;
    let wanted: Vec<(String, usize)> = members_of(interface.node)
        .into_iter()
        .filter(|(member, _)| member.kind == "method_declaration")
        .map(|(member, named)| (named.text.clone(), parameters_of(member).len()))
        .collect();
    let mut places = vec![name.start];
    places.extend(
        members_of(class)
            .into_iter()
            .filter(|(member, named)| {
                member.kind == "method_declaration"
                    && wanted.contains(&(named.text.clone(), parameters_of(member).len()))
            })
            .map(|(_, named)| named.start),
    );
    Some(places)
}
