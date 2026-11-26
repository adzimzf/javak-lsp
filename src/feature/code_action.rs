//! What `textDocument/codeAction` answers for Java at a cursor, as jdtls
//! answers it to a client without its extended commands: the source
//! actions of the file and the quick assists of the place, each with the
//! edit it makes.
//!
//! - Source actions: `Organize imports`, `Sort Members for 'File.java'`,
//!   `Change modifiers to final where possible`, and `Generate Getters`,
//!   `Generate Setters`, `Generate Getters and Setters` where a class has
//!   fields without them.
//! - Quick assists: `Add Javadoc comment` in a member without one,
//!   `Convert to static import` (and `(replace all occurrences)`) on a
//!   static member named through its type, `Inline Method` on a call of a
//!   method with a body whose only `return` ends it, `Invert conditions`
//!   in the condition of an `if`.

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
pub const QUICK_ASSIST: &str = "refactor";
pub const INLINE: &str = "refactor.inline";

/// The actions at `at` in `file`.
pub fn actions(snapshot: &Snapshot, file: &FileView, at: Position) -> Vec<Action> {
    let mut actions = Vec::new();
    let path = path_to(&file.tree, at);
    actions.extend(add_javadoc(file, &path));
    actions.extend(static_import(snapshot, file, &path));
    actions.extend(inline_method(snapshot, file, &path));
    actions.extend(invert_conditions(file, &path));
    actions.extend(change_to_final(snapshot, file));
    actions.extend(generate_accessors(snapshot, file, at));
    actions.push(organize_imports(file));
    actions.push(sort_members(file));
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
    // A `return` the body goes on after leaves the inlined code with
    // nowhere to go: each must end the body, or end a branch that does.
    let mut tail_returns = Vec::new();
    if !returns_in_tail(&statements, &mut tail_returns) {
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
    // jdtls offers it, and refuses once asked, where the body uses a
    // private member the call's place cannot see.
    let super_elsewhere = body.walk().into_iter().any(|node| node.kind == "super")
        && (method.file.uri != file.uri
            || snapshot.enclosing(file, call.start).is_none_or(|around| Some(around.node.start) != crate::feature::type_hierarchy::owner(&method).map(|owner| owner.node.start)));
    if super_elsewhere || uses_hidden_members(snapshot, file, call, &method, body) {
        return Some(Action {
            disabled: Some("The method's body uses members not visible at the call"),
            ..single(file, "Inline Method", INLINE, Vec::new())
        });
    }
    let mut edits = inlined(snapshot, file, call, &method, &statements, &tail_returns)?;
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
    if parameters.iter().any(|parameter| parameter.kind == "spread_parameter") || parameters.len() != arguments.len() {
        return None;
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
    for (parameter, argument) in parameters.iter().zip(&arguments) {
        let name = parameter.child("name")?;
        let simple = matches!(argument.kind.as_str(), "identifier" | "this")
            || argument.kind.ends_with("_literal")
            || matches!(argument.kind.as_str(), "true" | "false");
        if simple {
            replacements.push((name.text.clone(), argument.text.clone()));
        } else {
            let written = parameter.child("type").map(type_of_node).map(|written| written.written())?;
            let local = fresh(&name.text, &mut taken);
            if local != name.text {
                replacements.push((name.text.clone(), local.clone()));
            }
            prelude.push(format!("{written} {local} = {};", argument.text));
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

// ------------------------------------------------------- invert condition

/// In the condition of an `if`: the condition inverted, the negation
/// pushed in (`!a || !b` for `a && b`).
fn invert_conditions(file: &FileView, path: &[&Syn]) -> Option<Action> {
    let statement = path.iter().rev().find(|node| node.kind == "if_statement")?;
    let condition = statement.child("condition")?;
    let cursor = path.last()?.start;
    if !(condition.start <= cursor && cursor <= condition.end) {
        return None;
    }
    let inner = condition.children.iter().find(|child| !matches!(child.kind.as_str(), "(" | ")"))?;
    Some(single(
        file,
        "Invert conditions",
        QUICK_ASSIST,
        vec![Edit {
            start: inner.start,
            end: inner.end,
            text: inverted(inner),
        }],
    ))
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
/// once and never again.
fn change_to_final(snapshot: &Snapshot, file: &FileView) -> Option<Action> {
    let mut edits = Vec::new();
    for node in file.tree.walk() {
        let (declarator_names, type_node): (Vec<&Syn>, Option<&Syn>) = match node.kind.as_str() {
            "field_declaration" | "local_variable_declaration" => (
                node.all("variable_declarator").filter_map(|declarator| declarator.child("name")).collect(),
                node.child("type"),
            ),
            "formal_parameter" => (node.child("name").into_iter().collect(), node.child("type")),
            _ => continue,
        };
        let modifiers = node.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
        let says = |word: &str| modifiers.split_whitespace().any(|written| written == word);
        if says("final") || declarator_names.is_empty() {
            continue;
        }
        let Some(type_node) = type_node else {
            continue;
        };
        // A lambda's or a catch's parameter, a resource, a `for` header
        // are left as they are; a field only where it is private.
        let parent_kind = path_to(&file.tree, node.start)
            .into_iter()
            .rev()
            .nth(1)
            .map(|parent| parent.kind.clone())
            .unwrap_or_default();
        if node.kind == "formal_parameter" && parent_kind != "formal_parameters" {
            continue;
        }
        if node.kind == "local_variable_declaration" && parent_kind == "for_statement" {
            continue;
        }
        if node.kind == "field_declaration" && !says("private") {
            continue;
        }
        let assigned_once = declarator_names.iter().all(|name| {
            let initialized = node.kind == "formal_parameter"
                || name_initializer(node, name).is_some();
            let Some(found) = snapshot.declaration_at(&file.uri, name.start) else {
                return false;
            };
            let writes = snapshot
                .uses_in(&found, Some(&file.uri))
                .into_iter()
                .filter(|(_, at)| *at != name.start)
                .filter(|(_, at)| highlight::shown(file, *at, false).is_some_and(|shown| shown.write))
                .count();
            initialized && writes == 0
        });
        if assigned_once {
            edits.push(Edit {
                start: type_node.start,
                end: type_node.start,
                text: "final ".to_owned(),
            });
        }
    }
    Some(single(file, "Change modifiers to final where possible", FINAL_MODIFIERS, edits))
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
    let Some(class) = snapshot.enclosing(file, at).or_else(|| {
        let first = file.tree.children.iter().find(|node| is_type_declaration(node))?;
        Some(Found {
            file,
            node: first,
            name: first.child("name")?,
            bindings: Bindings::new(),
        })
    }) else {
        return Vec::new();
    };
    if !matches!(class.node.kind.as_str(), "class_declaration" | "enum_declaration") {
        return Vec::new();
    }
    let members = members_of(class.node);
    let methods: Vec<&str> = members
        .iter()
        .filter(|(member, _)| member.kind == "method_declaration")
        .map(|(_, named)| named.text.as_str())
        .collect();
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
        let Some(written) = member.child("type") else {
            continue;
        };
        let written = written.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let capital = capitalized(&named.text);
        let getter = if written == "boolean" { format!("is{capital}") } else { format!("get{capital}") };
        let setter = format!("set{capital}");
        let statically = if is_static { "static " } else { "" };
        let holder = if is_static { class.name.text.clone() } else { "this".to_owned() };
        if !methods.contains(&getter.as_str()) {
            getters.push(format!(
                "{indent}public {statically}{written} {getter}() {{\n{inner}return {};\n{indent}}}",
                named.text
            ));
        }
        if !is_final && !methods.contains(&setter.as_str()) {
            setters.push(format!(
                "{indent}public {statically}void {setter}({written} {name}) {{\n{inner}{holder}.{name} = {name};\n{indent}}}",
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
    if !getters.is_empty() || !setters.is_empty() {
        let both: Vec<String> = getters.iter().chain(&setters).cloned().collect();
        actions.push(single(file, "Generate Getters and Setters", GENERATE_ACCESSORS, insert(&both)));
    }
    actions
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
fn organize_imports(file: &FileView) -> Action {
    let declarations: Vec<&Syn> = file.tree.all("import_declaration").collect();
    let (Some(first), Some(last)) = (declarations.first(), declarations.last()) else {
        return single(file, "Organize imports", ORGANIZE_IMPORTS, Vec::new());
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
    let edits = if text == file.text_between(first.start, last.end) {
        Vec::new()
    } else {
        vec![Edit {
            start: first.start,
            end: last.end,
            text,
        }]
    };
    single(file, "Organize imports", ORGANIZE_IMPORTS, edits)
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
fn sort_members(file: &FileView) -> Action {
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
    single(file, title, SORT_MEMBERS, edits)
}

fn sorted_body(file: &FileView, body: &Syn) -> Option<Edit> {
    // Each member with the comments that lead it, as one chunk.
    let mut chunks: Vec<(u8, String, usize, Position, Position)> = Vec::new();
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
        let category = match child.kind.as_str() {
            kind if is_type_declaration(child) || kind == "annotation_type_declaration" => 0,
            "field_declaration" | "constant_declaration" if is_static || child.kind == "constant_declaration" => 1,
            "static_initializer" => 2,
            "method_declaration" if is_static => 3,
            "field_declaration" => 4,
            "block" => 5,
            "constructor_declaration" | "compact_constructor_declaration" => 6,
            "method_declaration" | "annotation_type_element_declaration" => 7,
            _ => return None,
        };
        let sorts_by_name = !matches!(category, 1 | 2 | 4 | 5);
        let name = if sorts_by_name {
            child.child("name").map(|named| named.text.clone()).unwrap_or_default()
        } else {
            String::new()
        };
        chunks.push((category, name, index, start, child.end));
        index += 1;
    }
    if chunks.len() < 2 {
        return None;
    }
    let mut sorted = chunks.clone();
    sorted.sort_by(|left, right| (left.0, &left.1, left.2).cmp(&(right.0, &right.1, right.2)));
    if sorted.iter().map(|chunk| chunk.2).eq(chunks.iter().map(|chunk| chunk.2)) {
        return None;
    }
    let first = chunks.first()?.3;
    let last = chunks.last()?.4;
    // The gaps between chunks stay where they are; the chunks move.
    let mut text = String::new();
    for (position, chunk) in sorted.iter().enumerate() {
        if position > 0 {
            let previous = &chunks[position - 1];
            text.push_str(&file.text_between(previous.4, chunks[position].3));
        }
        text.push_str(&file.text_between(chunk.3, chunk.4));
    }
    Some(Edit {
        start: first,
        end: last,
        text,
    })
}
