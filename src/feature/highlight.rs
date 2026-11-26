//! What `textDocument/documentHighlight` points at, as jdtls's
//! `Highlight.java` does: besides the uses of a name, it shows where a
//! method exits, where a declared exception is thrown, the loop a `break`
//! or `continue` goes to, and the methods implementing an interface or
//! overriding a superclass. Each place is a range, read or written.

use crate::feature::semantic::{
    Bindings, FileView, Found, Snapshot, is_identifier, members_of, parameters_of, path_to,
};
use crate::parser::{Position, Syn};

/// One highlighted range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Highlight {
    pub start: Position,
    pub end: Position,
    /// Whether the place writes the variable it names.
    pub write: bool,
}

impl Highlight {
    fn read(node: &Syn) -> Self {
        Self {
            start: node.start,
            end: node.end,
            write: false,
        }
    }

    /// The first `length` characters of `node`.
    fn word(node: &Syn, length: u32) -> Self {
        Self {
            start: node.start,
            end: Position {
                line: node.start.line,
                column: node.start.column + length,
            },
            write: false,
        }
    }

    /// The `}` a block ends with.
    fn last_brace(block: &Syn) -> Self {
        Self {
            start: Position {
                line: block.end.line,
                column: block.end.column.saturating_sub(1),
            },
            end: block.end,
            write: false,
        }
    }
}

/// Where the members Lombok generates use `found`, as jdtls highlights
/// them: at the annotation that generates them, a field read (a getter)
/// or written (a setter, a constructor), and a type where a generated
/// member repeats a field's type.
pub fn generated(snapshot: &Snapshot, file: &FileView, found: &Found<'_>, names_a_type: bool) -> Vec<Highlight> {
    use crate::parser::java::lombok::{uses, Access};
    if !file.text.contains("lombok") {
        return Vec::new();
    }
    let is_field = found.file.uri == file.uri;
    let wanted = (&found.file.uri, found.selection());
    let mut places = Vec::new();
    for used in uses(&file.tree) {
        let at = Highlight {
            start: used.start,
            end: used.end,
            write: used.access == Access::Write,
        };
        match used.access {
            Access::Read | Access::Write if is_field && used.field == found.name.start => places.push(at),
            Access::Type if names_a_type => {
                let path = path_to(&file.tree, used.field);
                let Some(written) = path
                    .iter()
                    .rev()
                    .find(|node| node.kind == "field_declaration")
                    .and_then(|field| field.child("type"))
                else {
                    continue;
                };
                let names_it = written
                    .walk()
                    .into_iter()
                    .filter(|node| node.kind == "type_identifier")
                    .filter_map(|node| snapshot.declaration_at(&file.uri, node.start))
                    .any(|declaration| (&declaration.file.uri, declaration.selection()) == wanted);
                if names_it {
                    places.push(at);
                }
            }
            _ => {}
        }
    }
    places
}

/// The places jdtls highlights for a cursor, or `None` where the cursor is
/// on a plain name and the uses of that name are the answer.
pub fn special(snapshot: &Snapshot, file: &FileView, at: Position) -> Option<Vec<Highlight>> {
    let path = path_to(&file.tree, at);
    let last = path.last()?;
    // `this` and `super` name no variable jdtls highlights.
    if matches!(last.kind.as_str(), "this" | "super") {
        return Some(Vec::new());
    }
    if let Some(places) = package(&path, file) {
        return Some(places);
    }
    if let Some(places) = jump_target(&path, at) {
        return Some(places);
    }
    if let Some(places) = exits(&path, at) {
        return Some(places);
    }
    if let Some(places) = thrown(&path, at) {
        return Some(places);
    }
    implementations(snapshot, &path, file, at)
}

/// A use of a name as jdtls highlights it: the name — the whole qualified
/// name where it names a type written qualified (`Outer.Inner`) — written
/// where it is declared as a variable, assigned or incremented.
pub fn shown(file: &FileView, at: Position, names_a_type: bool) -> Option<Highlight> {
    let path = path_to(&file.tree, at);
    let (index, name) = path
        .iter()
        .enumerate()
        .rev()
        .find(|(_, node)| node.start == at && is_identifier(node))?;
    let mut start = name.start;
    if names_a_type {
        let mut inner = *name;
        for outer in path[..index].iter().rev() {
            let qualified = matches!(
                outer.kind.as_str(),
                "scoped_type_identifier" | "scoped_identifier"
            );
            let named_last = outer
                .children
                .iter()
                .rev()
                .find(|child| is_identifier(child))
                .is_some_and(|last| std::ptr::eq(last, inner));
            if !(qualified && named_last) {
                break;
            }
            start = outer.start;
            inner = outer;
        }
    }
    Some(Highlight {
        start,
        end: name.end,
        write: is_written(&path[..index], name),
    })
}

/// Whether `name`, under `ancestors`, declares a variable or gives one a
/// value.
fn is_written(ancestors: &[&Syn], name: &Syn) -> bool {
    let Some(parent) = ancestors.last() else {
        return false;
    };
    let named_by_parent = parent
        .child("name")
        .is_some_and(|named| std::ptr::eq(named, name));
    match parent.kind.as_str() {
        "variable_declarator"
        | "formal_parameter"
        | "catch_formal_parameter"
        | "resource"
        | "type_pattern"
        | "record_pattern_component"
        | "enhanced_for_statement"
        | "instanceof_expression" => {
            return named_by_parent
                || (parent.kind == "record_pattern_component" || parent.kind == "type_pattern")
                    && parent
                        .children
                        .last()
                        .is_some_and(|last| std::ptr::eq(last, name));
        }
        _ => {}
    }
    // `count = …`, `this.count = …`, `count++`.
    let (target, holder) = if parent.kind == "field_access"
        && parent
            .child("field")
            .is_some_and(|field| std::ptr::eq(field, name))
    {
        let Some(holder) = ancestors.len().checked_sub(2).map(|index| ancestors[index]) else {
            return false;
        };
        (*parent, holder)
    } else {
        (name, *parent)
    };
    match holder.kind.as_str() {
        "assignment_expression" => holder
            .child("left")
            .is_some_and(|left| std::ptr::eq(left, target)),
        "update_expression" => true,
        _ => false,
    }
}

/// A `break` or `continue`: the loop or switch it goes to — its label, or
/// its keyword — and for a `break` the `}` that ends it.
fn jump_target(path: &[&Syn], at: Position) -> Option<Vec<Highlight>> {
    let (index, jump) =
        path.iter().enumerate().rev().find(|(_, node)| {
            matches!(node.kind.as_str(), "break_statement" | "continue_statement")
        })?;
    let label = jump.children.iter().find(|child| is_identifier(child));
    // The word itself or its label; not a `break` somewhere inside it.
    let on_the_jump = jump.children.first().is_some_and(|word| word.covers(at))
        || label.is_some_and(|label| label.covers(at));
    if !on_the_jump {
        return None;
    }
    let enclosing = &path[..index];
    let (target, mut places) = match label {
        Some(label) => {
            let labelled = enclosing.iter().rev().find(|node| {
                node.kind == "labeled_statement"
                    && node
                        .children
                        .first()
                        .is_some_and(|named| named.text == label.text)
            })?;
            let named = labelled.children.first()?;
            let statement = labelled
                .children
                .iter()
                .rev()
                .find(|child| child.kind.ends_with("statement"))?;
            (statement, vec![Highlight::read(named)])
        }
        None => {
            let wanted =
                |node: &&&Syn| is_loop(node) || (jump.kind == "break_statement" && is_switch(node));
            let target: &Syn = enclosing.iter().rev().find(wanted)?;
            let keyword = target.children.first()?;
            (target, vec![Highlight::read(keyword)])
        }
    };
    if jump.kind == "break_statement"
        && let Some(body) = target.child("body")
        && body.text.ends_with('}')
    {
        places.push(Highlight::last_brace(body));
    }
    Some(places)
}

fn is_loop(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement"
    )
}

fn is_switch(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "switch_expression" | "switch_statement")
}

/// What a method's exits are found in: its body, not the lambdas and
/// classes written in it.
fn is_another_body(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "lambda_expression"
            | "class_body"
            | "class_declaration"
            | "record_declaration"
            | "enum_declaration"
            | "interface_declaration"
    )
}

/// Every node under `node` that `wanted` picks, not looking into lambdas
/// or classes.
fn in_the_same_body<'a>(node: &'a Syn, wanted: &dyn Fn(&Syn) -> bool, found: &mut Vec<&'a Syn>) {
    for child in &node.children {
        if wanted(child) {
            found.push(child);
        }
        if !is_another_body(child) {
            in_the_same_body(child, wanted, found);
        }
    }
}

/// The cursor on a method's return type or on a `return`: every way the
/// method leaves — its return type, its `return`s, its `throw`s and, where
/// the method returns nothing, its last `}`.
fn exits(path: &[&Syn], at: Position) -> Option<Vec<Highlight>> {
    let (index, method) = path
        .iter()
        .enumerate()
        .rev()
        .find(|(_, node)| node.kind == "method_declaration" || is_another_body(node))?;
    if method.kind != "method_declaration" {
        return None;
    }
    let returns = method.child("type")?;
    // The cursor is on the type or on the `return` word itself, not
    // merely somewhere in what a `return` returns.
    let on_an_exit = returns.covers(at)
        || path[index..]
            .iter()
            .filter(|node| node.kind == "return_statement")
            .any(|statement| {
                statement
                    .first("return")
                    .is_some_and(|word| word.covers(at))
            });
    if !on_an_exit {
        return None;
    }
    let Some(body) = method.child("body") else {
        return Some(Vec::new());
    };
    let mut places = vec![Highlight::read(returns)];
    let mut found = Vec::new();
    in_the_same_body(
        body,
        &|node| matches!(node.kind.as_str(), "return_statement" | "throw_statement"),
        &mut found,
    );
    places.extend(found.into_iter().map(|node| {
        if node.kind == "throw_statement" {
            Highlight::word(node, "throw".len() as u32)
        } else {
            Highlight::read(node)
        }
    }));
    // jdtls marks a `void` method's last `}` as an exit whatever its last
    // statement is, and never another method's.
    if returns.kind == "void_type" {
        places.push(Highlight::last_brace(body));
    }
    Some(places)
}

/// The cursor on an exception a method declares it throws: the name, and
/// every `throw` of that exception in the method.
fn thrown(path: &[&Syn], at: Position) -> Option<Vec<Highlight>> {
    let throws = path.iter().rev().find(|node| node.kind == "throws")?;
    let named = throws
        .children
        .iter()
        .find(|node| node.covers(at) && node.kind != "throws")?;
    let name = named
        .walk()
        .into_iter()
        .rev()
        .find(|node| is_identifier(node))?;
    let method = path.iter().rev().find(|node| {
        matches!(
            node.kind.as_str(),
            "method_declaration" | "constructor_declaration"
        )
    })?;
    let mut places = vec![Highlight::read(named)];
    let mut found = Vec::new();
    in_the_same_body(method, &|node| node.kind == "throw_statement", &mut found);
    places.extend(
        found
            .into_iter()
            .filter(|node| throws_the_same(node, &name.text))
            .map(|node| Highlight::word(node, "throw".len() as u32)),
    );
    Some(places)
}

/// Whether a `throw` throws a new something called `name`.
fn throws_the_same(statement: &Syn, name: &str) -> bool {
    statement
        .walk()
        .into_iter()
        .filter(|node| node.kind == "object_creation_expression")
        .any(|created| {
            created.child("type").is_some_and(|written| {
                written
                    .walk()
                    .into_iter()
                    .rev()
                    .find(|node| is_identifier(node))
                    .is_some_and(|last| last.text == name)
            })
        })
}

/// The cursor on an interface a class implements, or the class it extends:
/// the type, and the methods of the class that implement or override that
/// type's.
fn implementations(
    snapshot: &Snapshot,
    path: &[&Syn],
    file: &FileView,
    at: Position,
) -> Option<Vec<Highlight>> {
    let list = path
        .iter()
        .rev()
        .find(|node| matches!(node.kind.as_str(), "super_interfaces" | "superclass"))?;
    // The type the list names, not one of its type arguments.
    let written = list
        .walk()
        .into_iter()
        .filter(|node| node.covers(at))
        .find(|node| {
            matches!(
                node.kind.as_str(),
                "generic_type" | "type_identifier" | "scoped_type_identifier"
            )
        })?;
    let head = if written.kind == "generic_type" {
        written.children.first()?
    } else {
        written
    };
    let name = head
        .walk()
        .into_iter()
        .rev()
        .find(|node| is_identifier(node))
        .filter(|name| name.covers(at) || name.end == at)?;
    let class = path.iter().rev().find(|node| {
        matches!(
            node.kind.as_str(),
            "class_declaration" | "enum_declaration" | "record_declaration"
        )
    })?;
    let implemented = snapshot.type_named(&name.text, file)?;
    let wanted: Vec<(String, usize)> = members_of(implemented.node)
        .into_iter()
        .filter(|(member, _)| member.kind == "method_declaration")
        .map(|(member, named)| (named.text.clone(), parameters_of(member).len()))
        .collect();
    let mut places = vec![Highlight::read(written)];
    places.extend(
        members_of(class)
            .into_iter()
            .filter(|(member, named)| {
                member.kind == "method_declaration"
                    && wanted.contains(&(named.text.clone(), parameters_of(member).len()))
            })
            .map(|(_, named)| Highlight::read(named)),
    );
    Some(places)
}

/// The cursor on a package's name in the `package` declaration or an
/// import: that package's name wherever a declaration writes it.
fn package(path: &[&Syn], file: &FileView) -> Option<Vec<Highlight>> {
    let declaration = path.iter().find(|node| {
        matches!(
            node.kind.as_str(),
            "package_declaration" | "import_declaration"
        )
    })?;
    let at = path.last()?.start;
    let name = declaration
        .walk()
        .into_iter()
        .find(|node| node.kind == "identifier" && node.start <= at && at <= node.end)?;
    // A package is lower case; `Outer` in `import a.Outer.Inner` is a type.
    let names_a_package = declaration.kind == "package_declaration"
        || name.text.chars().next().is_some_and(char::is_lowercase);
    if !names_a_package {
        return None;
    }
    // The dotted name that ends at the cursor's name: `com.demo` for `demo`.
    let named = path
        .iter()
        .rev()
        .find(|node| {
            node.kind == "scoped_identifier"
                && node
                    .child("name")
                    .is_some_and(|last| last.start == name.start)
        })
        .copied()
        .unwrap_or(name);
    let wanted: String = named.text.split_whitespace().collect();
    let in_a_declaration = |node: &Syn| {
        matches!(
            node.kind.as_str(),
            "package_declaration" | "import_declaration"
        )
    };
    // jdtls knows a package that holds types: the file's own, or one an
    // import names a type of. `java` alone holds none.
    let holds_types = file
        .tree
        .children
        .iter()
        .filter(|node| in_a_declaration(node))
        .any(|written| {
            let full: String = written
                .children
                .iter()
                .filter(|child| matches!(child.kind.as_str(), "scoped_identifier" | "identifier"))
                .map(|child| child.text.split_whitespace().collect::<String>())
                .next()
                .unwrap_or_default();
            let wildcard = written
                .children
                .iter()
                .any(|child| child.kind == "asterisk");
            if written.kind == "package_declaration" || wildcard {
                return full == wanted;
            }
            full.strip_prefix(&wanted)
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|rest| rest.chars().next().is_some_and(char::is_uppercase))
        });
    if !holds_types {
        return Some(Vec::new());
    }
    let mut places = Vec::new();
    for written in file
        .tree
        .children
        .iter()
        .filter(|node| in_a_declaration(node))
    {
        places.extend(
            written
                .walk()
                .into_iter()
                .filter(|node| matches!(node.kind.as_str(), "scoped_identifier" | "identifier"))
                .filter(|node| node.text.split_whitespace().collect::<String>() == wanted)
                .map(Highlight::read),
        );
    }
    Some(places)
}

/// The uses of a record component, as jdtls tells them apart: its
/// declaration stands alone, the compact constructor reads a parameter of
/// the same name, and the methods read the field — not the accessor.
pub fn of_a_component(
    file: &FileView,
    declared: &Syn,
    at: Position,
    uses: Vec<Position>,
) -> Vec<Position> {
    let path = path_to(&file.tree, declared.start);
    let is_component = path.iter().any(|node| {
        node.kind == "record_declaration"
            && node
                .child("parameters")
                .is_some_and(|header| header.covers(declared.start))
    });
    if !is_component {
        return uses;
    }
    if declared.covers(at) || declared.end == at {
        return vec![declared.start];
    }
    // Where the name at the cursor starts, the cursor being anywhere on it.
    let at = file
        .tree
        .walk()
        .into_iter()
        .find(|node| is_identifier(node) && node.start <= at && at <= node.end)
        .map_or(at, |name| name.start);
    let compact = |at: Position| {
        path_to(&file.tree, at)
            .into_iter()
            .find(|node| node.kind == "compact_constructor_declaration")
            .map(|constructor| constructor.start)
    };
    let called = |at: Position| {
        let path = path_to(&file.tree, at);
        path.len() >= 2
            && path[path.len() - 2].kind == "method_invocation"
            && path[path.len() - 2]
                .child("name")
                .is_some_and(|name| name.start == at)
    };
    // `x()`: the accessor a record has without writing it.
    if called(at) {
        return uses.into_iter().filter(|place| called(*place)).collect();
    }
    let region = compact(at);
    uses.into_iter()
        .filter(|place| *place != declared.start && !called(*place) && compact(*place) == region)
        .collect()
}

/// The cursor on `var`: the type it stands for, which jdtls highlights.
pub fn var_at<'a>(snapshot: &'a Snapshot, file: &'a FileView, at: Position) -> Option<Found<'a>> {
    let path = path_to(&file.tree, at);
    let written = path
        .last()
        .filter(|node| node.kind == "type_identifier" && node.text == "var")?;
    let declaration = path
        .iter()
        .rev()
        .nth(1)
        .filter(|node| node.kind == "local_variable_declaration")?;
    if !declaration
        .child("type")
        .is_some_and(|type_| std::ptr::eq(type_, *written))
    {
        return None;
    }
    inferred_class(snapshot, file, declaration)
}

/// The class a `var` declaration's value makes it.
fn inferred_class<'a>(
    snapshot: &'a Snapshot,
    file: &'a FileView,
    declaration: &'a Syn,
) -> Option<Found<'a>> {
    let name = declaration
        .first("variable_declarator")
        .and_then(|declarator| declarator.child("name"))?;
    let local = Found {
        file,
        node: declaration,
        name,
        bindings: Bindings::new(),
    };
    let written = snapshot.type_of_declaration(&local)?;
    snapshot.of_type(&written, file)
}

/// Every `var` in `file` that stands for the type `found`.
pub fn vars_of(snapshot: &Snapshot, file: &FileView, found: &Found<'_>) -> Vec<Position> {
    file.tree
        .walk()
        .into_iter()
        .filter(|node| node.kind == "local_variable_declaration")
        .filter_map(|declaration| {
            let written = declaration
                .child("type")
                .filter(|written| written.text == "var")?;
            let class = inferred_class(snapshot, file, declaration)?;
            let same = class.file.uri == found.file.uri && class.selection() == found.selection();
            same.then_some(written.start)
        })
        .collect()
}
