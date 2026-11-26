//! What `textDocument/codeAction` answers for Java at a cursor, as jdtls
//! answers it to a client without its extended commands: the source
//! actions of the file and the quick assists of the place, each with the
//! edit it makes. jdtls computes every edit up front for such a client and
//! leaves out an action that would change nothing or that fails, so each
//! one here is offered only where it changes something.
//!
//! - Source actions: `Organize imports` (where the imports are not already
//!   organized), `Sort Members for 'File.java'` (where Eclipse's order —
//!   by kind, then visibility, then name — moves a member),
//!   `Change modifiers to final where possible` (JDT's clean-up rules),
//!   `Generate Getters`, `Generate Setters`, `Generate Getters and Setters`
//!   (the last where both kinds are missing), and `Generate toString()`
//!   for a class without instance fields.
//! - Quick assists: `Add Javadoc comment` in a member without one,
//!   `Add final modifier for 'x'` in a declaration that can take it,
//!   `Assign statement to new local variable` / `… new field` in a
//!   statement whose value is not void, `Convert to static import` (and
//!   `(replace all occurrences)`) on a static member named through its
//!   type, `Inline Method` where JDT's inline refactoring succeeds,
//!   `Invert conditions` on a boolean expression where a negation fits and
//!   `Invert equals` on `a.equals(b)`.

use tower_lsp::lsp_types::Url;

use crate::feature::highlight;
use crate::feature::semantic::{
    imports, is_type_declaration, members_of, parameters_of, path_to, type_of_node, Bindings, FileView, Found, Snapshot,
};
use crate::parser::{Position, Syn};

/// One action: its title, its kind and the edits it makes.
pub struct Action {
    pub title: String,
    pub kind: &'static str,
    pub edits: Vec<(Url, Vec<Edit>)>,
    /// Why the action cannot be made here, where it is offered all the same.
    pub disabled: Option<&'static str>,
}

/// Text to put between two places of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: Position,
    pub end: Position,
    pub text: String,
}

pub const SOURCE: &str = "source";
pub const ORGANIZE_IMPORTS: &str = "source.organizeImports";
pub const SORT_MEMBERS: &str = "source.sortMembers";
pub const FINAL_MODIFIERS: &str = "source.generate.finalModifiers";
pub const GENERATE_ACCESSORS: &str = "source.generate.accessors";
pub const GENERATE_TO_STRING: &str = "source.generate.toString";
pub const QUICK_ASSIST: &str = "refactor";
pub const INLINE: &str = "refactor.inline";
pub const ASSIGN: &str = "refactor.assign";

/// The actions at `at` in `file`.
pub fn actions(snapshot: &Snapshot, file: &FileView, at: Position) -> Vec<Action> {
    let mut actions = Vec::new();
    let path = path_to(&file.tree, at);
    actions.extend(add_javadoc(file, &path));
    actions.extend(static_import(snapshot, file, &path));
    actions.extend(inline_method(snapshot, file, &path));
    actions.extend(invert_conditions(snapshot, file, &path));
    actions.extend(invert_equals(snapshot, file, &path));
    actions.extend(add_final_here(snapshot, file, &path));
    actions.extend(assign_to_variable(snapshot, file, &path));
    actions.extend(change_to_final(snapshot, file));
    actions.extend(generate_accessors(snapshot, file, at));
    actions.extend(generate_to_string(snapshot, file, at));
    actions.extend(organize_imports(file));
    actions.extend(sort_members(file));
    actions
}

fn single(file: &FileView, title: impl Into<String>, kind: &'static str, edits: Vec<Edit>) -> Action {
    Action {
        title: title.into(),
        kind,
        edits: vec![(file.uri.clone(), edits)],
        disabled: None,
    }
}

/// The text of `file`'s line `line`.
fn line_text(file: &FileView, line: u32) -> &str {
    file.text.lines().nth(line as usize).unwrap_or_default()
}

/// The leading whitespace of a line.
fn indentation(file: &FileView, line: u32) -> String {
    line_text(file, line)
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect()
}

/// One more level of indentation than `indent`, as the file indents: a
/// tab where it indents with tabs, four spaces else.
fn deeper(indent: &str) -> String {
    if indent.contains('\t') {
        format!("{indent}\t")
    } else {
        format!("{indent}    ")
    }
}

fn line_start(line: u32) -> Position {
    Position { line, column: 0 }
}

// ---------------------------------------------------------------- javadoc

/// The member around the cursor (a method, a constructor, a field, a type),
/// if it has no javadoc: a template above it.
fn add_javadoc(file: &FileView, path: &[&Syn]) -> Option<Action> {
    let (index, member) = path.iter().enumerate().rev().find(|(_, node)| {
        matches!(
            node.kind.as_str(),
            "method_declaration" | "constructor_declaration" | "field_declaration"
        ) || is_type_declaration(node)
    })?;
    let parent = index.checked_sub(1).map(|at| path[at])?;
    let position = parent.children.iter().position(|child| std::ptr::eq(child, *member))?;
    let documented = parent.children[..position]
        .iter()
        .rev()
        .take_while(|previous| matches!(previous.kind.as_str(), "block_comment" | "line_comment"))
        .any(|previous| previous.text.starts_with("/**"));
    if documented {
        return None;
    }
    let indent = indentation(file, member.start.line);
    let mut lines = vec![format!("{indent}/**"), format!("{indent} * ")];
    if matches!(member.kind.as_str(), "method_declaration" | "constructor_declaration") {
        for parameter in member.child("type_parameters").into_iter().flat_map(|parameters| parameters.all("type_parameter")) {
            if let Some(name) = parameter.first("type_identifier") {
                lines.push(format!("{indent} * @param <{}>", name.text));
            }
        }
        for parameter in parameters_of(member) {
            let name = parameter
                .child("name")
                .or_else(|| parameter.first("variable_declarator").and_then(|declarator| declarator.child("name")));
            if let Some(name) = name {
                lines.push(format!("{indent} * @param {}", name.text));
            }
        }
        let returns_void = member.child("type").is_none_or(|written| written.text == "void");
        if member.kind == "method_declaration" && !returns_void {
            lines.push(format!("{indent} * @return"));
        }
        if let Some(throws) = member.first("throws") {
            for thrown in throws.children.iter().filter(|child| child.kind.ends_with("type_identifier") || child.kind == "generic_type") {
                lines.push(format!("{indent} * @throws {}", thrown.text));
            }
        }
    }
    lines.push(format!("{indent} */"));
    let text = lines.join("\n") + "\n";
    Some(single(
        file,
        "Add Javadoc comment",
        QUICK_ASSIST,
        vec![Edit {
            start: line_start(member.start.line),
            end: line_start(member.start.line),
            text,
        }],
    ))
}

// ---------------------------------------------------------- static import

/// `Validate.isTrue(…)` at `isTrue`: imported statically, the qualifier
/// dropped here or everywhere in the file.
fn static_import(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Vec<Action> {
    let Some(name) = path.last().filter(|node| node.kind == "identifier") else {
        return Vec::new();
    };
    let Some(access) = path.iter().rev().nth(1).filter(|node| {
        matches!(node.kind.as_str(), "method_invocation" | "field_access")
            && node
                .child(if node.kind == "method_invocation" { "name" } else { "field" })
                .is_some_and(|named| named.start == name.start)
    }) else {
        return Vec::new();
    };
    let Some(qualifier) = access.child("object") else {
        return Vec::new();
    };
    let Some(owner) = qualifier_type(snapshot, file, qualifier) else {
        return Vec::new();
    };
    let Some(member) = snapshot.declaration_at(&file.uri, name.start) else {
        return Vec::new();
    };
    let is_static = member
        .node
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "static"))
        || matches!(member.node.kind.as_str(), "enum_constant" | "constant_declaration");
    if !is_static {
        return Vec::new();
    }
    let qualified = snapshot.qualified_type_name(owner.file, owner.node);
    let import = format!("{qualified}.{}", name.text);
    let already = imports(file)
        .iter()
        .any(|existing| existing.is_static && (existing.name == import || existing.wildcard && existing.name == qualified));
    let import_edit = (!already).then(|| added_import(file, &format!("import static {import};")));
    // Each `Owner.member` this access names.
    let drop_qualifier = |access: &Syn| -> Option<Edit> {
        let qualifier = access.child("object")?;
        let named = access.child(if access.kind == "method_invocation" { "name" } else { "field" })?;
        Some(Edit {
            start: qualifier.start,
            end: named.start,
            text: String::new(),
        })
    };
    let mut here: Vec<Edit> = import_edit.iter().cloned().collect();
    here.extend(drop_qualifier(access));
    let mut everywhere: Vec<Edit> = import_edit.into_iter().collect();
    for node in file.tree.walk() {
        let matches = matches!(node.kind.as_str(), "method_invocation" | "field_access")
            && node
                .child(if node.kind == "method_invocation" { "name" } else { "field" })
                .is_some_and(|named| {
                    named.text == name.text
                        && snapshot
                            .declaration_at(&file.uri, named.start)
                            .is_some_and(|found| found.file.uri == member.file.uri && found.selection() == member.selection())
                })
            && node
                .child("object")
                .and_then(|object| qualifier_type(snapshot, file, object))
                .is_some_and(|other| other.file.uri == owner.file.uri && other.name.start == owner.name.start);
        if matches {
            everywhere.extend(drop_qualifier(node));
        }
    }
    vec![
        single(file, "Convert to static import", QUICK_ASSIST, here),
        single(file, "Convert to static import (replace all occurrences)", QUICK_ASSIST, everywhere),
    ]
}

/// The type a qualifier names, where it names one rather than a value.
fn qualifier_type<'a>(snapshot: &'a Snapshot, file: &'a FileView, qualifier: &Syn) -> Option<Found<'a>> {
    let found = snapshot.declaration_at(&file.uri, qualifier.end)?;
    (is_type_declaration(found.node) && found.name.text == last_name(&qualifier.text)).then_some(found)
}

fn last_name(dotted: &str) -> &str {
    dotted.rsplit('.').next().unwrap_or(dotted).trim()
}

/// An import added where it sorts among the file's imports, or after the
/// package declaration where there are none.
fn added_import(file: &FileView, line: &str) -> Edit {
    let declarations: Vec<&Syn> = file.tree.all("import_declaration").collect();
    let before = declarations.iter().find(|existing| import_order_key(&existing.text) > import_order_key(line));
    match (before, declarations.last()) {
        (Some(before), _) => Edit {
            start: line_start(before.start.line),
            end: line_start(before.start.line),
            text: format!("{line}\n"),
        },
        (None, Some(last)) => Edit {
            start: last.end,
            end: last.end,
            text: format!("\n{line}"),
        },
        (None, None) => {
            let after = file.tree.first("package_declaration").map_or(Position { line: 0, column: 0 }, |package| package.end);
            Edit {
                start: after,
                end: after,
                text: format!("\n\n{line}"),
            }
        }
    }
}

// ---------------------------------------------------------------- inline

/// `Inline Method` on a call of a method with a body whose only `return`
/// is its last statement: the call replaced by the body.
fn inline_method(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Option<Action> {
    let name = path.last().filter(|node| node.kind == "identifier")?;
    let call = path
        .iter()
        .rev()
        .find(|node| node.kind == "method_invocation" && node.child("name").is_some_and(|named| named.start == name.start))?;
    let method = snapshot.invoked(call, file)?;
    if method.node.kind != "method_declaration" {
        return None;
    }
    let body = method.node.child("body")?;
    let statements: Vec<&Syn> = body
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "{" | "}" | "line_comment" | "block_comment"))
        .collect();
    // JDT's conditions on where the call is. A `return` before the last
    // statement interrupts the flow: only a call that is itself returned
    // takes such a body. Inside an expression, a body of more than its
    // `return` fits only an assignment, a variable's only initializer or
    // a lambda's body.
    let interrupted = statements.iter().enumerate().any(|(index, statement)| {
        let last = index + 1 == statements.len();
        statement.walk().into_iter().any(|node| {
            node.kind == "return_statement" && !(last && std::ptr::eq(node, *statement)) && !inside_lambda_or_class(statement, node)
        })
    });
    let parent = path
        .iter()
        .rposition(|node| std::ptr::eq(*node, *call))
        .and_then(|index| index.checked_sub(1))
        .map(|index| path[index]);
    let parent_kind = parent.map_or("", |parent| parent.kind.as_str());
    let simple_function = statements.is_empty() || (statements.len() == 1 && statements[0].kind == "return_statement");
    // A call through `super` is checked only as a statement of its own.
    let through_super = call.child("object").is_some_and(|object| object.kind == "super");
    match parent_kind {
        "expression_statement" if interrupted => return None,
        _ if through_super => {}
        "return_statement" | "yield_statement" => {}
        "expression_statement" if !interrupted => {}
        "expression_statement" => return None,
        _ if interrupted => return None,
        "assignment_expression" | "lambda_expression" => {}
        "variable_declarator" => {
            let single = path
                .iter()
                .rev()
                .find(|node| matches!(node.kind.as_str(), "local_variable_declaration" | "field_declaration"))
                .is_some_and(|declaration| {
                    declaration.kind == "local_variable_declaration" && declaration.all("variable_declarator").count() == 1
                });
            if !single && !simple_function {
                return None;
            }
        }
        _ if !simple_function => return None,
        _ => {}
    }
    // `Outer.this` cannot be carried to another place.
    let qualified_this = body.walk().into_iter().any(|node| {
        node.kind == "field_access" && node.children.last().is_some_and(|last| last.kind == "this")
    });
    if qualified_this {
        return None;
    }
    let mut tail_returns = Vec::new();
    // A returned call takes the body as it is, its returns kept.
    let into_return = parent_kind == "return_statement" && interrupted;
    if !into_return && !returns_in_tail(&statements, &mut tail_returns) {
        return None;
    }
    // Recursion cannot be inlined.
    let recursive = body.walk().into_iter().any(|node| {
        node.kind == "method_invocation"
            && node.child("name").is_some_and(|named| named.text == method.name.text)
            && snapshot.invoked(node, method.file).is_some_and(|called| called.node.start == method.node.start)
    });
    if recursive {
        return None;
    }
    // Where the body uses a member the call's place cannot see, the
    // refactoring fails, and jdtls then offers nothing.
    let uses_super = body.walk().into_iter().any(|node| node.kind == "super");
    let through_this = call.child("object").is_some_and(|object| object.kind == "this");
    let super_elsewhere = uses_super
        && !through_super
        && (through_this
            || method.file.uri != file.uri
            || snapshot.enclosing(file, call.start).is_none_or(|around| Some(around.node.start) != crate::feature::type_hierarchy::owner(&method).map(|owner| owner.node.start)));
    // JDT checks what the body may see only for a method of the
    // workspace; one read from a library is taken as it is.
    let hidden = !method.file.library && uses_hidden_members(snapshot, file, call, &method, body);
    if super_elsewhere || hidden {
        return None;
    }
    let mut edits = inlined(snapshot, file, call, &method, &statements, &tail_returns, into_return)?;
    for import in imports_for_inlined(snapshot, file, &method, body) {
        edits.push(added_import(file, &format!("import {import};")));
    }
    Some(single(file, "Inline Method", INLINE, edits))
}

/// The types `body` names that the caller's file does not see by their
/// simple name, as the imports it then needs.
fn imports_for_inlined(snapshot: &Snapshot, file: &FileView, method: &Found<'_>, body: &Syn) -> Vec<String> {
    if method.file.uri == file.uri {
        return Vec::new();
    }
    let own_package = snapshot.package(file).unwrap_or_default();
    let existing = imports(file);
    let mut needed: Vec<String> = Vec::new();
    for node in body.walk() {
        if !matches!(node.kind.as_str(), "type_identifier" | "identifier")
            || !node.text.starts_with(|character: char| character.is_uppercase())
        {
            continue;
        }
        // A type as written is looked up as one: `new X()` names X's
        // constructor otherwise.
        let found = if node.kind == "type_identifier" {
            snapshot.type_named(&node.text, method.file)
        } else {
            snapshot.declaration_at(&method.file.uri, node.start)
        };
        let Some(found) = found else {
            continue;
        };
        if !is_type_declaration(found.node) || found.file.uri == file.uri {
            continue;
        }
        let qualified = snapshot.qualified_type_name(found.file, found.node);
        let package = snapshot.package(found.file).unwrap_or_default();
        let top_level = qualified == format!("{package}.{}", node.text) || qualified == node.text;
        let seen = (top_level && (package == own_package || package == "java.lang"))
            || existing.iter().any(|import| {
                if import.wildcard {
                    top_level && import.name == package
                } else {
                    import.name == qualified || import.last_segment() == node.text
                }
            });
        if !seen && !needed.contains(&qualified) {
            needed.push(qualified);
        }
    }
    needed
}

/// Whether `body` names a member the call's place cannot see: a private
/// one from outside its top-level type, a package-private one from
/// another package.
fn uses_hidden_members(snapshot: &Snapshot, file: &FileView, call: &Syn, method: &Found<'_>, body: &Syn) -> bool {
    let top_level = |file: &FileView, at: Position| {
        path_to(&file.tree, at).into_iter().find(|node| is_type_declaration(node)).map(|node| node.start)
    };
    let caller_top = (&file.uri, top_level(file, call.start));
    let caller_package = snapshot.package(file).unwrap_or_default();
    body.walk().into_iter().any(|node| {
        let found = match node.kind.as_str() {
            "type_identifier" => snapshot.type_named(&node.text, method.file),
            "identifier" => snapshot.declaration_at(&method.file.uri, node.start),
            _ => None,
        };
        let Some(found) = found else {
            return false;
        };
        let member = matches!(
            found.node.kind.as_str(),
            "field_declaration" | "method_declaration" | "constructor_declaration"
        ) || is_type_declaration(found.node);
        if !member {
            return false;
        }
        let modifiers = found.node.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
        let says = |word: &str| modifiers.split_whitespace().any(|written| written == word);
        if says("private") {
            return (&found.file.uri, top_level(found.file, found.node.start)) != caller_top;
        }
        if says("public") {
            return false;
        }
        // A protected one only to its package without a subtype check;
        // the call's place is taken as not one.
        if says("protected") {
            return snapshot.package(found.file).unwrap_or_default() != caller_package;
        }
        // An interface's members are public without saying so.
        let in_interface = path_to(&found.file.tree, found.node.start)
            .into_iter()
            .rev()
            .filter(|around| !std::ptr::eq(*around, found.node))
            .find(|around| is_type_declaration(around))
            .is_some_and(|owner| matches!(owner.kind.as_str(), "interface_declaration" | "annotation_type_declaration"));
        !in_interface && snapshot.package(found.file).unwrap_or_default() != caller_package
    })
}

/// The names used in the callable around `call`, which inlined locals
/// must not take.
fn names_around(file: &FileView, call: &Syn) -> std::collections::HashSet<String> {
    let path = path_to(&file.tree, call.start);
    // The member of a class body the call is in.
    let callable = path
        .windows(2)
        .rev()
        .find(|pair| matches!(pair[0].kind.as_str(), "class_body" | "enum_body_declarations" | "interface_body"))
        .map(|pair| pair[1]);
    callable
        .map(|callable| {
            callable
                .walk()
                .into_iter()
                .filter(|node| node.kind == "identifier")
                .map(|node| node.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// `name`, or `name` with the lowest number after it that is not taken;
/// taken from then on.
fn fresh(name: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let mut chosen = name.to_owned();
    let mut number = 1;
    while taken.contains(&chosen) {
        number += 1;
        chosen = format!("{name}{number}");
    }
    taken.insert(chosen.clone());
    chosen
}

/// The locals a statement declares, catch and loop variables too.
fn declared_locals(statement: &Syn) -> Vec<String> {
    let mut names = Vec::new();
    for node in statement.walk() {
        match node.kind.as_str() {
            "local_variable_declaration" => names.extend(
                node.children
                    .iter()
                    .filter(|child| child.kind == "variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .map(|name| name.text.clone()),
            ),
            "catch_formal_parameter" | "enhanced_for_statement" | "resource" => {
                names.extend(node.child("name").map(|name| name.text.clone()));
            }
            _ => {}
        }
    }
    names
}

/// Whether every `return` of `statements` ends them or ends a branch that
/// does (an `if`'s, a block's, a `try`'s, a `synchronized`'s), gathering
/// those returns.
fn returns_in_tail<'a>(statements: &[&'a Syn], returns: &mut Vec<&'a Syn>) -> bool {
    for (index, statement) in statements.iter().enumerate() {
        let last = index + 1 == statements.len();
        if !last {
            let returns_early = statement
                .walk()
                .into_iter()
                .any(|node| node.kind == "return_statement" && !inside_lambda_or_class(statement, node));
            if returns_early {
                return false;
            }
            continue;
        }
        if !tail(statement, returns) {
            return false;
        }
    }
    true
}

/// [`returns_in_tail`] of the statement that ends a body.
fn tail<'a>(statement: &'a Syn, returns: &mut Vec<&'a Syn>) -> bool {
    let statements_of = |block: &'a Syn| -> Vec<&'a Syn> {
        block
            .children
            .iter()
            .filter(|child| !matches!(child.kind.as_str(), "{" | "}" | "line_comment" | "block_comment"))
            .collect()
    };
    match statement.kind.as_str() {
        "return_statement" => {
            returns.push(statement);
            true
        }
        "block" => returns_in_tail(&statements_of(statement), returns),
        "if_statement" => {
            let branches = [statement.child("consequence"), statement.child("alternative")];
            branches.into_iter().flatten().all(|branch| tail(branch, returns))
        }
        "try_statement" | "synchronized_statement" => statement
            .children
            .iter()
            .filter(|child| child.kind == "block")
            .take(1)
            .all(|block| tail(block, returns))
            && statement
                .children
                .iter()
                .filter(|child| child.kind == "catch_clause")
                .all(|clause| clause.child("body").is_none_or(|body| tail(body, returns))),
        _ => !statement
            .walk()
            .into_iter()
            .any(|node| node.kind == "return_statement" && !inside_lambda_or_class(statement, node)),
    }
}

fn inside_lambda_or_class(statement: &Syn, node: &Syn) -> bool {
    path_to(statement, node.start)
        .iter()
        .any(|between| matches!(between.kind.as_str(), "lambda_expression" | "class_body"))
}

/// The edits that put `method`'s body where `call` is.
fn inlined(
    snapshot: &Snapshot,
    file: &FileView,
    call: &Syn,
    method: &Found<'_>,
    statements: &[&Syn],
    tail_returns: &[&Syn],
    into_return: bool,
) -> Option<Vec<Edit>> {
    let arguments: Vec<&Syn> = call
        .child("arguments")
        .map(|arguments| {
            arguments
                .children
                .iter()
                .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
                .collect()
        })
        .unwrap_or_default();
    let parameters = parameters_of(method.node);
    let varargs = parameters.last().is_some_and(|parameter| parameter.kind == "spread_parameter");
    let fixed = if varargs { parameters.len() - 1 } else { parameters.len() };
    if if varargs { arguments.len() < fixed } else { arguments.len() != fixed } {
        return None;
    }
    // The arguments a varargs parameter takes, passed as its array.
    let mut arguments: Vec<(String, &str)> = arguments
        .iter()
        .take(fixed)
        .map(|argument| (argument.text.clone(), argument.kind.as_str()))
        .collect();
    if varargs {
        let rest: Vec<&str> = call
            .child("arguments")
            .map(|all| {
                all.children
                    .iter()
                    .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
                    .skip(fixed)
                    .map(|child| child.text.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let element = parameters.last().and_then(|parameter| {
            parameter.children.iter().find(|child| child.kind.ends_with("type") || child.kind == "type_identifier")
        });
        let element = written_type(element);
        arguments.push((format!("new {element}[]{{{}}}", rest.join(", ")), "array_creation_expression"));
    }
    let receiver = call.child("object").map(|object| object.text.clone());
    let statement = path_to(&file.tree, call.start)
        .into_iter()
        .rev()
        .find(|node| node.kind.ends_with("_statement") || node.kind == "local_variable_declaration")?;
    let indent = indentation(file, statement.start.line);
    // Each parameter becomes its argument, or a local holding it where
    // the argument is not a plain name or literal.
    let mut prelude = Vec::new();
    let mut replacements: Vec<(String, String)> = Vec::new();
    let mut taken = names_around(file, call);
    for (parameter, (argument, kind)) in parameters.iter().zip(&arguments) {
        let name = parameter
            .child("name")
            .or_else(|| parameter.first("variable_declarator").and_then(|declarator| declarator.child("name")))?;
        let simple = matches!(*kind, "identifier" | "this") || kind.ends_with("_literal") || matches!(*kind, "true" | "false");
        if simple {
            replacements.push((name.text.clone(), argument.clone()));
        } else {
            let written = match parameter.child("type") {
                Some(written) => type_of_node(written).written(),
                None => {
                    let element = parameter.children.iter().find(|child| child.kind.ends_with("type") || child.kind == "type_identifier");
                    format!("{}[]", written_type(element))
                }
            };
            let local = fresh(&name.text, &mut taken);
            if local != name.text {
                replacements.push((name.text.clone(), local.clone()));
            }
            prelude.push(format!("{written} {local} = {argument};"));
        }
    }
    // The body's own locals keep their names unless the caller has them.
    let mut renamed: Vec<(String, String)> = Vec::new();
    for statement in statements {
        for local in declared_locals(statement) {
            let name = fresh(&local, &mut taken);
            if name != local && !renamed.iter().any(|(from, _)| *from == local) {
                renamed.push((local, name));
            }
        }
    }
    if into_return {
        // `return call(…);` becomes the body itself, returns and all.
        let mut lines = prelude;
        for body_statement in statements {
            let text = substituted(snapshot, method, body_statement, &replacements, &renamed, receiver.as_deref(), None);
            lines.extend(text.lines().map(str::to_owned));
        }
        let base = statements
            .first()
            .map(|first| indentation(method.file, first.start.line))
            .unwrap_or_default();
        let text = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let stripped = line.strip_prefix(base.as_str()).unwrap_or(line.trim_start());
                if index == 0 { stripped.to_owned() } else { format!("{indent}{stripped}") }
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Some(vec![Edit {
            start: statement.start,
            end: statement.end,
            text,
        }]);
    }
    // Returns inside branches give their value to a local the call then
    // stands for; a lone return at the end is the call itself.
    let only_last_returns = tail_returns.len() <= 1
        && tail_returns
            .first()
            .is_none_or(|only| statements.last().is_some_and(|last| std::ptr::eq(*last, *only)));
    let returns_value = method.node.child("type").is_some_and(|written| written.text != "void");
    let result_local =
        (!only_last_returns && returns_value).then(|| fresh(&format!("{}Result", method.name.text), &mut taken));
    if let Some(local) = &result_local {
        let written = method.node.child("type").map(type_of_node).map(|written| written.written())?;
        prelude.push(format!("{written} {local};"));
    }
    let mut lines: Vec<String> = prelude;
    let mut result: Option<String> = result_local.clone();
    for (index, body_statement) in statements.iter().enumerate() {
        let text = substituted(
            snapshot,
            method,
            body_statement,
            &replacements,
            &renamed,
            receiver.as_deref(),
            (!only_last_returns).then_some((tail_returns, result_local.as_deref())),
        );
        if only_last_returns && index + 1 == statements.len() && body_statement.kind == "return_statement" {
            let returned = text.trim().trim_start_matches("return").trim().trim_end_matches(';').trim().to_owned();
            if !returned.is_empty() {
                result = Some(returned);
            }
            continue;
        }
        lines.extend(text.lines().map(str::to_owned));
    }
    // Re-indented to the statement's place.
    let base = statements
        .first()
        .map(|first| indentation(method.file, first.start.line))
        .unwrap_or_default();
    let reindented: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let stripped = line.strip_prefix(base.as_str()).unwrap_or(line.trim_start());
            if index == 0 { stripped.to_owned() } else { format!("{indent}{stripped}") }
        })
        .collect();
    let mut edits = Vec::new();
    let standalone = statement.kind == "expression_statement"
        && statement.children.first().is_some_and(|expression| expression.start == call.start && expression.end == call.end);
    if standalone {
        let mut text = reindented.join("\n");
        if let Some(result) = result.filter(|result| is_statement_expression(result)) {
            if !text.is_empty() {
                text.push('\n');
                text.push_str(&indent);
            }
            text.push_str(&format!("{result};"));
        }
        edits.push(Edit {
            start: statement.start,
            end: statement.end,
            text,
        });
        return Some(edits);
    }
    let result = result?;
    if !reindented.is_empty() {
        edits.push(Edit {
            start: statement.start,
            end: statement.start,
            text: format!("{}\n{indent}", reindented.join("\n")),
        });
    }
    let simple = result.chars().all(|character| character.is_alphanumeric() || "_.()".contains(character));
    edits.push(Edit {
        start: call.start,
        end: call.end,
        text: if simple { result } else { format!("({result})") },
    });
    Some(edits)
}

/// Whether an expression can stand as a statement: a call, an
/// assignment, an increment, a creation.
fn is_statement_expression(expression: &str) -> bool {
    let expression = expression.trim();
    expression.ends_with(')') && !expression.starts_with('(')
        || expression.contains('=') && !expression.contains("==")
        || expression.ends_with("++")
        || expression.ends_with("--")
}

/// A statement of the inlined method's body as the caller writes it: its
/// parameters as the arguments passed, its members through the call's
/// receiver.
fn substituted(
    snapshot: &Snapshot,
    method: &Found<'_>,
    statement: &Syn,
    replacements: &[(String, String)],
    renamed: &[(String, String)],
    receiver: Option<&str>,
    returning: Option<(&[&Syn], Option<&str>)>,
) -> String {
    let mut pieces: Vec<(Position, Position, String)> = Vec::new();
    // `return x;` in a branch: `result = x;`, or nothing for a void one.
    if let Some((returns, local)) = returning {
        for returned in returns {
            let inside = statement.start <= returned.start && returned.end <= statement.end;
            if !inside {
                continue;
            }
            let value = returned.children.iter().find(|child| !matches!(child.kind.as_str(), "return" | ";"));
            match (local, value) {
                (Some(local), Some(value)) => {
                    pieces.push((returned.start, value.start, format!("{local} = ")));
                }
                _ => pieces.push((returned.start, returned.end, String::new())),
            }
        }
    }
    for node in statement.walk() {
        // `this` is the object the method was called on.
        if node.kind == "this" {
            if let Some(receiver) = receiver.filter(|receiver| !matches!(*receiver, "this" | "super")) {
                let simple = receiver.chars().all(|character| character.is_alphanumeric() || "_.".contains(character));
                let receiver = if simple { receiver.to_owned() } else { format!("({receiver})") };
                pieces.push((node.start, node.end, receiver));
            }
            continue;
        }
        if node.kind != "identifier" {
            continue;
        }
        let Some(found) = snapshot.declaration_at(&method.file.uri, node.start) else {
            continue;
        };
        let local_of_body = found.file.uri == method.file.uri
            && method.node.start <= found.node.start
            && found.node.end <= method.node.end;
        if local_of_body
            && !matches!(found.node.kind.as_str(), "formal_parameter" | "spread_parameter")
            && let Some((_, name)) = renamed.iter().find(|(from, _)| *from == node.text)
        {
            pieces.push((node.start, node.end, name.clone()));
            continue;
        }
        let is_parameter = matches!(found.node.kind.as_str(), "formal_parameter" | "spread_parameter")
            && path_to(&method.file.tree, found.node.start)
                .iter()
                .any(|around| std::ptr::eq(*around, method.node));
        if is_parameter {
            if let Some((_, argument)) = replacements.iter().find(|(name, _)| *name == node.text) {
                pieces.push((node.start, node.end, argument.clone()));
            }
            continue;
        }
        // An unqualified member of the method's type: through the receiver.
        let Some(receiver) = receiver else {
            continue;
        };
        let parent = path_to(statement, node.start)
            .into_iter()
            .rev()
            .nth(1);
        let qualified = parent.is_some_and(|parent| {
            matches!(parent.kind.as_str(), "field_access" | "method_invocation")
                && parent.child("object").is_some_and(|object| object.end <= node.start)
        });
        let is_member = matches!(
            found.node.kind.as_str(),
            "field_declaration" | "method_declaration" | "constant_declaration"
        ) && found.file.uri == method.file.uri;
        if is_member && !qualified {
            pieces.push((node.start, node.start, format!("{receiver}.")));
        }
    }
    apply(&statement.text, statement.start, pieces)
}

/// `text`, which starts at `origin`, with each piece's range replaced.
fn apply(text: &str, origin: Position, mut pieces: Vec<(Position, Position, String)>) -> String {
    pieces.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let offset = |at: Position| -> usize {
        let mut line = origin.line;
        let mut column = origin.column;
        for (index, character) in text.char_indices() {
            if line == at.line && column >= at.column {
                return index;
            }
            if character == '\n' {
                line += 1;
                column = 0;
            } else {
                column += character.len_utf16() as u32;
            }
        }
        text.len()
    };
    let mut out = text.to_owned();
    for (start, end, replacement) in pieces {
        let (start, end) = (offset(start), offset(end));
        if start <= end && end <= out.len() {
            out.replace_range(start..end, &replacement);
        }
    }
    out
}

// ------------------------------------------------------ assign statement

/// `Assign statement to new local variable` and `… new field` in an
/// expression statement whose value is not void and not an assignment.
fn assign_to_variable(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Vec<Action> {
    let Some(statement) = path.iter().rev().find(|node| is_statement_or_member(&node.kind)) else {
        return Vec::new();
    };
    if statement.kind != "expression_statement" {
        return Vec::new();
    }
    let Some(expression) = statement.children.iter().find(|child| !matches!(child.kind.as_str(), ";") && !child.kind.ends_with("comment")) else {
        return Vec::new();
    };
    // The type of the value as written, and a name for it.
    let (written, suggested) = match expression.kind.as_str() {
        "object_creation_expression" => {
            let Some(created) = expression.child("type") else {
                return Vec::new();
            };
            let simple = created.text.split('<').next().unwrap_or_default().rsplit('.').next().unwrap_or_default().to_owned();
            (created.text.clone(), decapitalized(&simple))
        }
        "update_expression" => ("int".to_owned(), "i".to_owned()),
        "method_invocation" => {
            let Some(method) = snapshot.invoked(expression, file) else {
                return Vec::new();
            };
            let Some(returned) = method.node.child("type").filter(|written| written.text != "void") else {
                return Vec::new();
            };
            let name = method.name.text.as_str();
            let base = ["get", "is", "to"]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix).filter(|rest| rest.starts_with(char::is_uppercase)))
                .map_or_else(|| name.to_owned(), decapitalized);
            (returned.text.clone(), base)
        }
        _ => return Vec::new(),
    };
    let mut taken = names_around(file, expression);
    let local = fresh(&suggested, &mut taken);
    let assign_local = vec![Edit {
        start: expression.start,
        end: expression.start,
        text: format!("{written} {local} = "),
    }];
    let mut assign_field = vec![Edit {
        start: expression.start,
        end: expression.start,
        text: format!("{local} = "),
    }];
    if let Some(body) = path.iter().rev().find(|node| matches!(node.kind.as_str(), "class_body" | "enum_body" | "interface_body")) {
        let open = Position {
            line: body.start.line,
            column: body.start.column + 1,
        };
        let member = body.children.iter().find(|child| !matches!(child.kind.as_str(), "{" | "}"));
        let indent = member.map_or_else(|| "    ".to_owned(), |member| indentation(file, member.start.line));
        assign_field.push(Edit {
            start: open,
            end: open,
            text: format!("\n{indent}private {written} {local};"),
        });
    }
    vec![
        single(file, "Assign statement to new local variable", ASSIGN, assign_local),
        single(file, "Assign statement to new field", ASSIGN, assign_field),
    ]
}

fn decapitalized(name: &str) -> String {
    let mut characters = name.chars();
    characters
        .next()
        .map(|first| first.to_lowercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
}

// ------------------------------------------------------- invert condition

/// `Invert conditions` on the outermost boolean expression around the
/// cursor that stands where a negation fits, as jdtls finds it: an
/// operand, a condition, an argument, a returned or assigned value.
fn invert_conditions(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Option<Action> {
    let mut found: Option<&Syn> = None;
    for index in (0..path.len()).rev() {
        let node = path[index];
        // An argument's list is no node of JDT's: the call is next.
        if node.kind == "argument_list" || is_condition_parentheses(path, index) {
            continue;
        }
        if !is_expression(node) {
            break;
        }
        // Past the parentheses around it; a condition's own are syntax.
        let mut at = index;
        while at > 0 && path[at - 1].kind == "parenthesized_expression" && !is_condition_parentheses(path, at - 1) {
            at -= 1;
        }
        let expression = path[at];
        let parent = at.checked_sub(1).map(|parent| path[parent]);
        let parent = parent.map(|parent| {
            if is_condition_parentheses(path, at - 1) { path[at - 2] } else { parent }
        });
        if negatable_place(expression, parent) && is_boolean(snapshot, file, expression) {
            found = Some(expression);
        }
    }
    let found = found?;
    Some(single(
        file,
        "Invert conditions",
        QUICK_ASSIST,
        vec![Edit {
            start: found.start,
            end: found.end,
            text: inverted(found),
        }],
    ))
}

/// `Invert equals` on `a.equals(b)` (or `equalsIgnoreCase`), the cursor on
/// the call or a node right below it: `b.equals(a)`.
fn invert_equals(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Option<Action> {
    let last = path.len().checked_sub(1)?;
    let call = if path[last].kind == "method_invocation" {
        path[last]
    } else {
        // The node's parent as JDT sees it: an argument's is the call.
        let mut index = last.checked_sub(1)?;
        if path[index].kind == "argument_list" {
            index = index.checked_sub(1)?;
        }
        path[index]
    };
    if call.kind != "method_invocation" {
        return None;
    }
    let name = call.child("name")?;
    if !matches!(name.text.as_str(), "equals" | "equalsIgnoreCase") {
        return None;
    }
    let arguments: Vec<&Syn> = call
        .child("arguments")?
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
        .collect();
    let [right] = arguments.as_slice() else {
        return None;
    };
    // Not with `null` or a primitive value as the argument.
    let primitive = |written: &str| matches!(written, "int" | "long" | "short" | "byte" | "char" | "float" | "double" | "boolean");
    let not_an_object = match right.kind.as_str() {
        "null_literal" | "true" | "false" | "character_literal" | "decimal_integer_literal" | "hex_integer_literal"
        | "octal_integer_literal" | "binary_integer_literal" | "decimal_floating_point_literal" | "hex_floating_point_literal" => true,
        "binary_expression" => !is_boolean(snapshot, file, right) && !right.text.contains('"'),
        "identifier" | "field_access" | "method_invocation" | "array_access" => snapshot
            .type_of_expression(right, file)
            .is_some_and(|written| written.dimensions == 0 && primitive(&written.name)),
        _ => false,
    } || (right.kind == "binary_expression" && is_boolean(snapshot, file, right));
    if not_an_object {
        return None;
    }
    let left = call.child("object").map_or("this", |object| object.text.as_str());
    let right_text = if matches!(right.kind.as_str(), "identifier" | "field_access" | "method_invocation" | "string_literal" | "this" | "parenthesized_expression") {
        right.text.clone()
    } else {
        format!("({})", right.text)
    };
    Some(single(
        file,
        "Invert equals",
        QUICK_ASSIST,
        vec![Edit {
            start: call.start,
            end: call.end,
            text: format!("{right_text}.{}({left})", name.text),
        }],
    ))
}

/// The parentheses an `if`, `while` or `do` writes its condition in, which
/// are not an expression of their own in JDT's tree.
fn is_condition_parentheses(path: &[&Syn], index: usize) -> bool {
    path[index].kind == "parenthesized_expression"
        && index > 0
        && matches!(path[index - 1].kind.as_str(), "if_statement" | "while_statement" | "do_statement" | "switch_expression" | "switch_statement" | "synchronized_statement")
}

fn is_expression(node: &Syn) -> bool {
    node.kind.ends_with("_expression")
        || node.kind.ends_with("_literal")
        || matches!(
            node.kind.as_str(),
            "identifier" | "field_access" | "method_invocation" | "array_access" | "this" | "true" | "false" | "null_literal" | "method_reference"
        )
}

/// Whether `expression`, below `parent`, is where JDT negates one.
fn negatable_place(expression: &Syn, parent: Option<&Syn>) -> bool {
    let Some(parent) = parent else {
        return false;
    };
    let is_field = |field: &str| parent.child(field).is_some_and(|child| std::ptr::eq(child, expression));
    match parent.kind.as_str() {
        "binary_expression" | "argument_list" | "return_statement" | "assert_statement" | "if_statement" | "while_statement" | "do_statement" => true,
        "assignment_expression" => is_field("right"),
        "for_statement" => is_field("condition"),
        "ternary_expression" => is_field("condition"),
        "unary_expression" => true,
        _ => false,
    }
}

/// Whether an expression is a `boolean` or a `Boolean`.
fn is_boolean(snapshot: &Snapshot, file: &FileView, expression: &Syn) -> bool {
    match expression.kind.as_str() {
        "true" | "false" | "instanceof_expression" => true,
        "binary_expression" => expression.child("operator").is_some_and(|operator| {
            matches!(operator.kind.as_str(), "&&" | "||" | "==" | "!=" | "<" | "<=" | ">" | ">=")
                || (matches!(operator.kind.as_str(), "&" | "|" | "^")
                    && expression.child("left").is_some_and(|left| is_boolean(snapshot, file, left)))
        }),
        "unary_expression" => expression.child("operator").is_some_and(|operator| operator.kind == "!"),
        "parenthesized_expression" => expression
            .children
            .iter()
            .find(|child| !matches!(child.kind.as_str(), "(" | ")"))
            .is_some_and(|inner| is_boolean(snapshot, file, inner)),
        "method_invocation" => snapshot
            .invoked(expression, file)
            .and_then(|method| method.node.child("type").map(|written| written.text.clone()))
            .is_some_and(|written| matches!(written.as_str(), "boolean" | "Boolean" | "java.lang.Boolean")),
        "identifier" | "field_access" | "ternary_expression" | "array_access" | "cast_expression" => snapshot
            .type_of_expression(expression, file)
            .is_some_and(|written| written.dimensions == 0 && matches!(written.name.as_str(), "boolean" | "Boolean")),
        _ => false,
    }
}

/// `expression` negated, as JDT inverts a condition.
fn inverted(expression: &Syn) -> String {
    match expression.kind.as_str() {
        "binary_expression" => {
            let (Some(left), Some(right)) = (expression.child("left"), expression.child("right")) else {
                return format!("!({})", expression.text);
            };
            let operator = expression.child("operator").map_or("", |operator| operator.kind.as_str());
            match operator {
                "&&" => format!("{} || {}", inverted(left), inverted(right)),
                "||" => format!("{} && {}", grouped(&inverted(left), left), grouped(&inverted(right), right)),
                "==" => format!("{} != {}", left.text, right.text),
                "!=" => format!("{} == {}", left.text, right.text),
                "<" => format!("{} >= {}", left.text, right.text),
                "<=" => format!("{} > {}", left.text, right.text),
                ">" => format!("{} <= {}", left.text, right.text),
                ">=" => format!("{} < {}", left.text, right.text),
                _ => format!("!({})", expression.text),
            }
        }
        "unary_expression" if expression.child("operator").is_some_and(|operator| operator.kind == "!") => expression
            .child("operand")
            .map_or_else(|| expression.text.clone(), |operand| unparenthesized(operand).to_owned()),
        "parenthesized_expression" => {
            let inner = expression.children.iter().find(|child| !matches!(child.kind.as_str(), "(" | ")"));
            inner.map_or_else(|| format!("!{}", expression.text), |inner| format!("({})", inverted(inner)))
        }
        "true" => "false".to_owned(),
        "false" => "true".to_owned(),
        _ => format!("!{}", expression.text),
    }
}

/// An `||` inverted inside an `&&` keeps its own parentheses.
fn grouped(inverted: &str, original: &Syn) -> String {
    let is_or_of_and = original.kind == "binary_expression"
        && original.child("operator").is_some_and(|operator| operator.kind == "&&");
    if is_or_of_and { format!("({inverted})") } else { inverted.to_owned() }
}

fn unparenthesized(expression: &Syn) -> &str {
    if expression.kind == "parenthesized_expression" {
        let text = expression.text.trim();
        return text.strip_prefix('(').and_then(|text| text.strip_suffix(')')).unwrap_or(text);
    }
    &expression.text
}

// ------------------------------------------------------------ final where possible

/// `final` on each field, parameter and local that is given its value
/// once and never again, as JDT's "final where possible" clean-up finds
/// them; none where there is nothing to change.
fn change_to_final(snapshot: &Snapshot, file: &FileView) -> Option<Action> {
    let edits = one_per_declaration(final_candidates(snapshot, file, None));
    (!edits.is_empty()).then(|| single(file, "Change modifiers to final where possible", FINAL_MODIFIERS, edits))
}

/// `Add final modifier for 'x'` in the declaration of a variable that can
/// be final: the declaration the cursor is in, up to its statement or
/// member. A declaration of several names is `Change modifiers to final`.
fn add_final_here(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Option<Action> {
    // The declaration around the cursor, looked for up to the statement or
    // member it is in: a variable first, else a field's or a statement's.
    let mut declaration: Option<(&Syn, Vec<String>)> = None;
    for node in path.iter().rev() {
        match node.kind.as_str() {
            "variable_declarator" => {
                let name = node.child("name")?;
                // A field's or a local's declarator stands for that one name.
                declaration = Some((node, vec![name.text.clone()]));
                break;
            }
            "formal_parameter" | "catch_formal_parameter" | "spread_parameter" => {
                let name = node.child("name").or_else(|| node.first("variable_declarator").and_then(|declarator| declarator.child("name")))?;
                declaration = Some((node, vec![name.text.clone()]));
                break;
            }
            "field_declaration" | "local_variable_declaration" => {
                let names = node
                    .all("variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .map(|name| name.text.clone())
                    .collect();
                declaration = Some((node, names));
                break;
            }
            kind if is_statement_or_member(kind) => break,
            _ => {}
        }
    }
    let (declaration, names) = declaration?;
    let edits = one_per_declaration(
        final_candidates(snapshot, file, Some(declaration))
            .into_iter()
            .filter(|candidate| declaration.start <= candidate.name && candidate.name < declaration.end)
            .collect(),
    );
    if edits.is_empty() {
        return None;
    }
    let title = match names.as_slice() {
        [only] => format!("Add final modifier for '{only}'"),
        _ => "Change modifiers to final".to_owned(),
    };
    Some(single(file, title, QUICK_ASSIST, edits))
}

/// A statement or a member of a type: where the search for the
/// declaration around the cursor stops.
fn is_statement_or_member(kind: &str) -> bool {
    kind.ends_with("_statement")
        || matches!(
            kind,
            "block"
                | "local_variable_declaration"
                | "field_declaration"
                | "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "static_initializer"
                | "enum_constant"
                | "constant_declaration"
                | "annotation_type_element_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
                | "explicit_constructor_invocation"
                | "switch_block_statement_group"
        )
}

/// A variable that can be made final, and the edit that makes it so.
struct FinalCandidate {
    name: Position,
    edit: Edit,
}

/// Every variable of `file` that can be final, or only those declared in
/// `within`.
fn final_candidates(snapshot: &Snapshot, file: &FileView, within: Option<&Syn>) -> Vec<FinalCandidate> {
    let mut candidates = Vec::new();
    let says = |node: &Syn, word: &str| {
        node.first("modifiers")
            .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|written| written == word))
    };
    // Where a name is written after its declaration.
    let writes_of = |name: &Syn| -> Option<Vec<Position>> {
        let found = snapshot.declaration_at(&file.uri, name.start)?;
        Some(
            snapshot
                .uses_in(&found, Some(&file.uri))
                .into_iter()
                .filter(|(_, at)| *at != name.start)
                .filter(|(_, at)| highlight::shown(file, *at, false).is_some_and(|shown| shown.write))
                .map(|(_, at)| at)
                .collect(),
        )
    };
    let nodes = match within {
        // The declaration, or the one it is a declarator of.
        Some(within) => path_to(&file.tree, within.start)
            .into_iter()
            .filter(|node| node.start <= within.start && within.end <= node.end)
            .rev()
            .take(3)
            .chain(within.walk())
            .collect::<Vec<_>>(),
        None => file.tree.walk().into_iter().collect(),
    };
    for node in nodes {
        let excluded = says(node, "final") || says(node, "volatile") || says(node, "transient");
        let (names, type_node): (Vec<&Syn>, Option<&Syn>) = match node.kind.as_str() {
            "field_declaration" | "local_variable_declaration" => (
                node.all("variable_declarator").filter_map(|declarator| declarator.child("name")).collect(),
                node.child("type"),
            ),
            "formal_parameter" | "catch_formal_parameter" => (
                node.child("name")
                    .or_else(|| node.first("variable_declarator").and_then(|declarator| declarator.child("name")))
                    .into_iter()
                    .collect(),
                node.child("type").or_else(|| node.first("catch_type")),
            ),
            "enhanced_for_statement" => (node.child("name").into_iter().collect(), node.child("type")),
            _ => continue,
        };
        if excluded || names.is_empty() {
            continue;
        }
        let Some(type_node) = type_node else {
            continue;
        };
        // `var` would need its type written out to take a modifier the
        // same way; JDT still adds `final` before it.
        let parent = path_to(&file.tree, node.start).into_iter().rev().nth(1);
        let parent_kind = parent.map(|parent| parent.kind.as_str()).unwrap_or_default();
        if node.kind == "formal_parameter" {
            // A method's parameter only where the method has a body.
            if parent_kind == "formal_parameters" {
                let callable = path_to(&file.tree, node.start).into_iter().rev().nth(2);
                let bodiless = callable.is_some_and(|callable| {
                    matches!(callable.kind.as_str(), "method_declaration" | "constructor_declaration")
                        && callable.child("body").is_none()
                });
                if bodiless {
                    continue;
                }
            } else if parent_kind != "inferred_parameters" {
                // A record's components are not variables of a body.
                if parent_kind == "record_declaration" {
                    continue;
                }
            }
        }
        // `for (int i = 0, j = 0; …)` cannot take it either way.
        if node.kind == "local_variable_declaration" && parent_kind == "for_statement" && names.len() > 1 {
            continue;
        }
        if node.kind == "field_declaration" {
            if !says(node, "private") || says(node, "static") {
                continue;
            }
            let in_record_or_interface = parent.is_some_and(|parent| parent.kind == "interface_body");
            if in_record_or_interface {
                continue;
            }
        }
        for name in names {
            let Some(writes) = writes_of(name) else {
                continue;
            };
            let can = if node.kind == "field_declaration" {
                let initialized = name_initializer(node, name).is_some();
                if writes.is_empty() {
                    initialized
                } else {
                    !initialized && field_written_once_per_constructor(file, node, name, &writes)
                }
            } else {
                writes.is_empty()
            };
            if can {
                candidates.push(FinalCandidate {
                    name: name.start,
                    edit: Edit {
                        start: type_node.start,
                        end: type_node.start,
                        text: "final ".to_owned(),
                    },
                });
            }
        }
    }
    candidates
}

/// One `final` per declaration, however many of its names can take it.
fn one_per_declaration(candidates: Vec<FinalCandidate>) -> Vec<Edit> {
    let mut edits: Vec<Edit> = Vec::new();
    for candidate in candidates {
        if !edits.iter().any(|edit| edit.start == candidate.edit.start) {
            edits.push(candidate.edit);
        }
    }
    edits
}

/// Whether a field without an initializer is given its value once in each
/// constructor of its class, and never elsewhere: each write a statement
/// of its own at the top of a constructor without a `return`, not read
/// before it there, each constructor writing it or calling one that does.
fn field_written_once_per_constructor(file: &FileView, field: &Syn, name: &Syn, writes: &[Position]) -> bool {
    let Some(class_body) = path_to(&file.tree, field.start).into_iter().rev().nth(1) else {
        return false;
    };
    let constructors: Vec<&Syn> = class_body.children.iter().filter(|child| child.kind == "constructor_declaration").collect();
    let mut writing: Vec<&Syn> = Vec::new();
    for write in writes {
        let path = path_to(&file.tree, *write);
        // `name = …;` or `this.name = …;` straight in a constructor's body.
        let Some(assignment_index) = path.iter().rposition(|node| node.kind == "assignment_expression") else {
            return false;
        };
        let [.., constructor, body, statement] = &path[..assignment_index] else {
            return false;
        };
        if statement.kind != "expression_statement" || body.kind != "constructor_body" || constructor.kind != "constructor_declaration" {
            return false;
        }
        if !constructors.iter().any(|own| std::ptr::eq(*own, *constructor)) {
            return false;
        }
        if writing.iter().any(|seen| std::ptr::eq(*seen, *constructor)) {
            return false;
        }
        if constructor.walk().into_iter().any(|node| node.kind == "return_statement") {
            return false;
        }
        let read_before = constructor
            .walk()
            .into_iter()
            .any(|node| node.kind == "identifier" && node.text == name.text && node.start < *write);
        if read_before {
            return false;
        }
        writing.push(constructor);
    }
    // `this(…)` as the first statement: the constructor it calls.
    let delegate = |constructor: &Syn| -> Option<usize> {
        let body = constructor.child("body")?;
        let first = body.children.iter().find(|child| !matches!(child.kind.as_str(), "{" | "}" | "line_comment" | "block_comment"))?;
        if first.kind != "explicit_constructor_invocation" || !first.children.first().is_some_and(|called| called.kind == "this") {
            return None;
        }
        let arguments = first
            .child("arguments")
            .map_or(0, |arguments| arguments.children.iter().filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",")).count());
        // Overloads told apart by their number of parameters.
        constructors.iter().position(|other| parameters_of(other).len() == arguments)
    };
    let calls_writing = |start: &Syn| -> bool {
        let mut current = start;
        let mut visited = vec![current.start];
        while let Some(next) = delegate(current) {
            let next = constructors[next];
            if writing.iter().any(|seen| std::ptr::eq(*seen, next)) {
                return true;
            }
            if visited.contains(&next.start) {
                return false;
            }
            visited.push(next.start);
            current = next;
        }
        false
    };
    if writing.iter().any(|constructor| calls_writing(constructor)) {
        return false;
    }
    let all_covered = constructors
        .iter()
        .all(|constructor| writing.iter().any(|seen| std::ptr::eq(*seen, *constructor)) || calls_writing(constructor));
    // A lambda in a field's initializer that names it would read it unset.
    let lambda_uses = class_body.children.iter().filter(|child| child.kind == "field_declaration").any(|declaration| {
        declaration.walk().into_iter().any(|node| {
            node.kind == "lambda_expression"
                && node.walk().into_iter().any(|inner| inner.kind == "identifier" && inner.text == name.text)
        })
    });
    all_covered && !lambda_uses
}

fn name_initializer<'a>(declaration: &'a Syn, name: &Syn) -> Option<&'a Syn> {
    declaration
        .all("variable_declarator")
        .find(|declarator| declarator.child("name").is_some_and(|named| named.start == name.start))?
        .child("value")
}

// -------------------------------------------------------------- accessors

/// Getters and setters for the fields of the cursor's class that have
/// none, at the end of the class.
fn generate_accessors(snapshot: &Snapshot, file: &FileView, at: Position) -> Vec<Action> {
    let Some(class) = selection_type(snapshot, file, at) else {
        return Vec::new();
    };
    if !matches!(class.node.kind.as_str(), "class_declaration" | "enum_declaration") {
        return Vec::new();
    }
    let members = members_of(class.node);
    // Each method by name, with the types of its parameters as written.
    let methods: Vec<(&str, Vec<String>)> = members
        .iter()
        .filter(|(member, _)| member.kind == "method_declaration")
        .map(|(member, named)| {
            let parameters = parameters_of(member)
                .iter()
                .map(|parameter| written_type(parameter.child("type")))
                .collect();
            (named.text.as_str(), parameters)
        })
        .collect();
    let has_method = |name: &str, parameters: &[&str]| {
        methods
            .iter()
            .any(|(named, written)| *named == name && written.iter().map(String::as_str).eq(parameters.iter().copied()))
    };
    let Some(body) = class.node.children.iter().find(|child| matches!(child.kind.as_str(), "class_body" | "enum_body")) else {
        return Vec::new();
    };
    let indent = deeper(&indentation(file, class.node.start.line));
    let inner = deeper(&indent);
    let mut getters = Vec::new();
    let mut setters = Vec::new();
    for (member, named) in &members {
        if member.kind != "field_declaration" {
            continue;
        }
        let modifiers = member.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
        let is_static = modifiers.split_whitespace().any(|word| word == "static");
        let is_final = modifiers.split_whitespace().any(|word| word == "final");
        let written = written_type(member.child("type"));
        if written.is_empty() {
            continue;
        }
        let names = AccessorNames::of(&named.text, written == "boolean", is_static && is_final);
        let statically = if is_static { "static " } else { "" };
        let holder = if is_static { class.name.text.clone() } else { "this".to_owned() };
        // A boolean's getter is found under `is…`, else under `get…`.
        let has_getter = has_method(&names.getter, &[]) || (written == "boolean" && has_method(&names.get, &[]));
        let generate_getter = !has_getter;
        let generate_setter = !is_final && !has_method(&names.setter, &[written.as_str()]);
        if generate_getter {
            getters.push(format!(
                "{indent}public {statically}{written} {}() {{\n{inner}return {};\n{indent}}}",
                names.getter, named.text
            ));
        }
        if generate_setter {
            setters.push(format!(
                "{indent}public {statically}void {}({written} {name}) {{\n{inner}{holder}.{name} = {name};\n{indent}}}",
                names.setter,
                name = named.text
            ));
        }
    }
    let insert = |methods: &[String]| -> Vec<Edit> {
        let close = body.end;
        let at = Position {
            line: close.line,
            column: close.column.saturating_sub(1),
        };
        vec![Edit {
            start: at,
            end: at,
            text: format!("\n{}\n", methods.iter().map(|method| format!("\n{method}")).collect::<String>().trim_start_matches('\n')),
        }]
    };
    let mut actions = Vec::new();
    if !getters.is_empty() {
        actions.push(single(file, "Generate Getters", GENERATE_ACCESSORS, insert(&getters)));
    }
    if !setters.is_empty() {
        actions.push(single(file, "Generate Setters", GENERATE_ACCESSORS, insert(&setters)));
    }
    if !getters.is_empty() && !setters.is_empty() {
        let both: Vec<String> = getters.iter().chain(&setters).cloned().collect();
        actions.push(single(file, "Generate Getters and Setters", GENERATE_ACCESSORS, insert(&both)));
    }
    actions
}

/// A field's type as written, its spaces gone.
fn written_type(written: Option<&Syn>) -> String {
    written.map(|written| written.text.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// The accessors JDT names for a field: `getName`/`setName`, `isDone`
/// for a boolean (`isDone` again for one already named so), and a
/// constant's words joined (`MAX_SIZE` → `getMaxSize`).
struct AccessorNames {
    getter: String,
    /// A boolean's getter under `get…`, which JDT also takes for one.
    get: String,
    setter: String,
}

impl AccessorNames {
    fn of(field: &str, boolean: bool, constant: bool) -> Self {
        let base = if constant {
            field
                .split('_')
                .filter(|word| !word.is_empty())
                .map(|word| capitalized(&word.to_lowercase()))
                .collect::<String>()
        } else {
            let stripped = field
                .strip_prefix("is")
                .filter(|rest| boolean && rest.starts_with(|character: char| character.is_uppercase()));
            capitalized(stripped.unwrap_or(field))
        };
        AccessorNames {
            getter: if boolean { format!("is{base}") } else { format!("get{base}") },
            get: format!("get{base}"),
            setter: format!("set{base}"),
        }
    }
}

// -------------------------------------------------------------- toString

/// `Generate toString()` for a class without instance fields: jdtls makes
/// it at once, where a class with fields needs a prompt this client lacks.
fn generate_to_string(snapshot: &Snapshot, file: &FileView, at: Position) -> Option<Action> {
    let class = selection_type(snapshot, file, at)?;
    if class.node.kind != "class_declaration" {
        return None;
    }
    let has_instance_fields = members_of(class.node).iter().any(|(member, _)| {
        member.kind == "field_declaration"
            && !member
                .first("modifiers")
                .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "static"))
    });
    if has_instance_fields {
        return None;
    }
    let body = class.node.child("body")?;
    let indent = deeper(&indentation(file, class.node.start.line));
    let inner = deeper(&indent);
    let at = Position {
        line: body.end.line,
        column: body.end.column.saturating_sub(1),
    };
    let text = format!(
        "\n{indent}@Override\n{indent}public String toString() {{\n{inner}return \"{} []\";\n{indent}}}\n",
        class.name.text
    );
    Some(single(
        file,
        "Generate toString()",
        GENERATE_TO_STRING,
        vec![Edit { start: at, end: at, text }],
    ))
}

/// The type jdtls generates code into: the innermost one around the
/// cursor, else the file's first.
fn selection_type<'a>(snapshot: &'a Snapshot, file: &'a FileView, at: Position) -> Option<Found<'a>> {
    snapshot.enclosing(file, at).or_else(|| {
        let first = file.tree.children.iter().find(|node| is_type_declaration(node))?;
        Some(Found {
            file,
            node: first,
            name: first.child("name")?,
            bindings: Bindings::new(),
        })
    })
}

fn capitalized(name: &str) -> String {
    let mut characters = name.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
}

// --------------------------------------------------------- organize imports

/// The imports the file needs, each once, unused ones gone, in jdtls's
/// order: static imports, then `java`, `javax`, `org`, `com` and the rest,
/// a blank line between groups.
fn organize_imports(file: &FileView) -> Option<Action> {
    let declarations: Vec<&Syn> = file.tree.all("import_declaration").collect();
    let (Some(first), Some(last)) = (declarations.first(), declarations.last()) else {
        return None;
    };
    let used = used_names(file);
    let mut kept: Vec<String> = Vec::new();
    for import in imports(file) {
        let line = if import.is_static {
            format!("import static {}{};", import.name, if import.wildcard { ".*" } else { "" })
        } else {
            format!("import {}{};", import.name, if import.wildcard { ".*" } else { "" })
        };
        let needed = import.wildcard || used.contains(import.last_segment());
        if needed && !kept.contains(&line) {
            kept.push(line);
        }
    }
    kept.sort_by_key(|line| import_order_key(line));
    let mut text = String::new();
    let mut group = None;
    for line in &kept {
        let this_group = import_order_key(line).0;
        if group.is_some_and(|group| group != this_group) {
            text.push('\n');
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
        group = Some(this_group);
    }
    // Offered only where it changes something, as jdtls offers it: a
    // wildcard import is always spelled out into the names it brings.
    let wildcard = imports(file).iter().any(|import| import.wildcard);
    if !wildcard && text == file.text_between(first.start, last.end) {
        return None;
    }
    Some(single(
        file,
        "Organize imports",
        ORGANIZE_IMPORTS,
        vec![Edit {
            start: first.start,
            end: last.end,
            text,
        }],
    ))
}

/// Where an import line sorts: its group (static, `java`, `javax`, `org`,
/// `com`, the rest), then its name.
fn import_order_key(line: &str) -> (u8, String) {
    let name = line
        .trim()
        .trim_start_matches("import")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_owned();
    if let Some(name) = name.strip_prefix("static ") {
        return (0, name.trim().to_owned());
    }
    let group = match name.split('.').next().unwrap_or_default() {
        "java" => 1,
        "javax" => 2,
        "org" => 3,
        "com" => 4,
        _ => 5,
    };
    (group, name)
}

/// The simple names a file's code and javadoc write, outside its imports
/// and package declaration.
fn used_names(file: &FileView) -> std::collections::HashSet<String> {
    let mut used = std::collections::HashSet::new();
    for node in file.tree.walk() {
        let in_header = matches!(node.kind.as_str(), "import_declaration" | "package_declaration");
        if in_header {
            continue;
        }
        if matches!(node.kind.as_str(), "identifier" | "type_identifier") {
            let in_import = path_to(&file.tree, node.start)
                .iter()
                .any(|around| matches!(around.kind.as_str(), "import_declaration" | "package_declaration"));
            if !in_import {
                used.insert(node.text.clone());
            }
        }
        if node.kind == "block_comment" && node.text.starts_with("/**") {
            for word in node.text.split(|character: char| !(character.is_alphanumeric() || character == '_')) {
                if !word.is_empty() {
                    used.insert(word.to_owned());
                }
            }
        }
    }
    used
}

// ------------------------------------------------------------ sort members

/// The members of each type in Eclipse's default order: types, static
/// fields, static initializers, static methods, fields, initializers,
/// constructors, methods — fields, enum constants and initializers kept
/// in their order, the rest by name.
fn sort_members(file: &FileView) -> Option<Action> {
    let title = format!(
        "Sort Members for '{}'",
        file.uri.path().rsplit('/').next().unwrap_or_default()
    );
    let mut edits = Vec::new();
    for body in file.tree.walk().into_iter().filter(|node| {
        matches!(node.kind.as_str(), "class_body" | "interface_body" | "enum_body_declarations" | "annotation_type_body")
    }) {
        edits.extend(sorted_body(file, body));
    }
    (!edits.is_empty()).then(|| single(file, title, SORT_MEMBERS, edits))
}

fn sorted_body(file: &FileView, body: &Syn) -> Option<Edit> {
    // An interface's fields are static without saying so.
    let in_interface = matches!(body.kind.as_str(), "interface_body" | "annotation_type_body");
    // Each member with the comments that lead it, as one chunk.
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut pending_start: Option<Position> = None;
    let mut index = 0;
    for child in &body.children {
        if matches!(child.kind.as_str(), "{" | "}" | ";" | ",") {
            continue;
        }
        if matches!(child.kind.as_str(), "line_comment" | "block_comment") {
            pending_start.get_or_insert(child.start);
            continue;
        }
        let start = pending_start.take().unwrap_or(child.start);
        let modifiers = child.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
        let is_static = modifiers.split_whitespace().any(|word| word == "static");
        // Eclipse's default order: types, static fields and initializers,
        // static methods, fields and initializers, constructors, methods.
        // Fields and initializers keep their order among themselves.
        let category = match child.kind.as_str() {
            kind if is_type_declaration(child) || kind == "annotation_type_declaration" => 0,
            "field_declaration" | "constant_declaration" | "static_initializer"
                if is_static || in_interface || child.kind != "field_declaration" =>
            {
                1
            }
            "method_declaration" if is_static => 3,
            "field_declaration" | "block" => 4,
            "constructor_declaration" | "compact_constructor_declaration" => 6,
            "method_declaration" | "annotation_type_element_declaration" => 7,
            _ => return None,
        };
        let sorts_by_name = !matches!(category, 1 | 4);
        let name = if sorts_by_name {
            child.child("name").map(|named| named.text.clone()).unwrap_or_default()
        } else {
            String::new()
        };
        let parameters = if sorts_by_name {
            parameters_of(child)
                .iter()
                .map(|parameter| {
                    parameter
                        .child("type")
                        .map(|written| written.text.split_whitespace().collect::<String>())
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        };
        // jdtls sorts by visibility within a category: public, protected,
        // package, private. An interface's members are public unless
        // private.
        let says = |word: &str| modifiers.split_whitespace().any(|written| written == word);
        let visibility = if says("public") || (in_interface && !says("private")) {
            0
        } else if says("protected") {
            1
        } else if says("private") {
            3
        } else {
            2
        };
        chunks.push(Chunk {
            category,
            visibility: if sorts_by_name { visibility } else { 0 },
            name,
            parameters,
            index,
            start,
            end: child.end,
        });
        index += 1;
    }
    if chunks.len() < 2 {
        return None;
    }
    let mut sorted = chunks.clone();
    sorted.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.visibility.cmp(&right.visibility))
            .then_with(|| collated(&left.name, &right.name))
            .then_with(|| {
                left.parameters
                    .iter()
                    .zip(&right.parameters)
                    .map(|(left, right)| collated(left, right))
                    .find(|order| order.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.parameters.len().cmp(&right.parameters.len()))
            .then_with(|| left.index.cmp(&right.index))
    });
    if sorted.iter().map(|chunk| chunk.index).eq(chunks.iter().map(|chunk| chunk.index)) {
        return None;
    }
    let first = chunks.first()?.start;
    let last = chunks.last()?.end;
    // The gaps between chunks stay where they are; the chunks move.
    let mut text = String::new();
    for (position, chunk) in sorted.iter().enumerate() {
        if position > 0 {
            let previous = &chunks[position - 1];
            text.push_str(&file.text_between(previous.end, chunks[position].start));
        }
        text.push_str(&file.text_between(chunk.start, chunk.end));
    }
    Some(Edit {
        start: first,
        end: last,
        text,
    })
}

/// A member of a type body and where it sorts.
#[derive(Clone)]
struct Chunk {
    category: u8,
    visibility: u8,
    name: String,
    parameters: Vec<String>,
    index: usize,
    start: Position,
    end: Position,
}

/// Two names in the order a `java.text.Collator` puts them: letters
/// alike whatever their case, punctuation before digits before letters,
/// and only then a lowercase letter before its uppercase one.
fn collated(left: &str, right: &str) -> std::cmp::Ordering {
    let primary = |character: char| -> (u8, char) {
        if character.is_alphabetic() {
            (2, character.to_lowercase().next().unwrap_or(character))
        } else if character.is_ascii_digit() {
            (1, character)
        } else {
            (0, character)
        }
    };
    left.chars()
        .map(primary)
        .cmp(right.chars().map(primary))
        .then_with(|| {
            left.chars()
                .zip(right.chars())
                .find(|(left, right)| left != right)
                .map_or(std::cmp::Ordering::Equal, |(left, right)| {
                    right.is_lowercase().cmp(&left.is_lowercase())
                })
        })
}
