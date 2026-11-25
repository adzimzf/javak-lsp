//! What `textDocument/signatureHelp` answers: the signature of the call
//! the cursor is in, and which parameter it is at.

use crate::parser::{Position, Syn};
use crate::feature::semantic::{
    is_identifier, members_of, parameters_of, path_to, FileView, Found, Snapshot,
};

pub struct Help {
    /// `print` of `print(String text, int copies)`.
    pub name: String,
    /// `String text`, `int copies`, as written.
    pub parameters: Vec<String>,
    /// The parameter being typed, where the cursor is at one.
    pub active: Option<usize>,
    /// Every overload the call could mean, in the order jdtls lists them,
    /// the chosen one among them.
    pub overloads: Vec<Overload>,
    /// Which of `overloads` is the chosen one.
    pub active_overload: usize,
}

/// One signature of the overloads offered.
pub struct Overload {
    pub name: String,
    pub parameters: Vec<String>,
    /// The return type as written; `None` for a constructor.
    pub returns: Option<String>,
}

/// The call the cursor is in, or `None` where it is in none.
pub fn help_at(snapshot: &Snapshot, file: &FileView, at: Position) -> Option<Help> {
    let path = path_to(&file.tree, at);
    match being_typed(&path, file, at) {
        Some((call, index)) => {
            let candidates = candidates_of(snapshot, file, call, at)?;
            let chosen = chosen(snapshot, file, &candidates, call, index)?;
            let parameters = substituted_parameters(snapshot, chosen);
            let active = active_parameter(index, &parameters, chosen.node);
            let (overloads, active_overload) = overloads(snapshot, &candidates, chosen);
            Some(Help {
                name: called_name(chosen),
                parameters,
                active,
                overloads,
                active_overload,
            })
        }
        None => {
            // The cursor on the name of a call: its signature, no
            // parameter marked.
            let call = on_a_call_name(&path, at)?;
            let candidates = candidates_of(snapshot, file, call, at)?;
            let chosen = chosen(snapshot, file, &candidates, call, 0)?;
            let (overloads, active_overload) = overloads(snapshot, &candidates, chosen);
            Some(Help {
                name: called_name(chosen),
                parameters: substituted_parameters(snapshot, chosen),
                active: None,
                overloads,
                active_overload,
            })
        }
    }
}

/// The argument list the cursor is inside, and which argument it is at.
/// The count is taken from the source between the opening parenthesis and
/// the cursor, not from the tree: a call still being typed is a syntax
/// error, and its commas end up wherever the parser could put them.
fn being_typed<'a>(
    path: &[&'a Syn],
    file: &'a FileView,
    at: Position,
) -> Option<(&'a Syn, usize)> {
    let open = open_parenthesis_before(&file.text, at)?;
    let index = arguments_before(&file.text, open, at);
    let arguments = path
        .iter()
        .rev()
        .find(|node| node.kind == "argument_list" && node.start < at && at < node.end);
    if let Some(arguments) = arguments {
        let call = *path.iter().rev().find(|node| {
            node.children
                .iter()
                .any(|child| child.kind == "argument_list" && child.start == arguments.start)
        })?;
        return Some((call, index));
    }
    // A call the parser could not close: the name in front of the
    // parenthesis says what is being called.
    let name = identifier_ending_at(&file.tree, open)?;
    let call = path
        .iter()
        .rev()
        .find(|node| node.kind == "method_invocation")
        .copied();
    call.map(|call| (call, index)).or(Some((name, index)))
}

/// The parenthesis of the innermost call the cursor is inside, as a
/// position just after it. Parentheses in a string or a comment do not
/// count.
fn open_parenthesis_before(text: &str, at: Position) -> Option<Position> {
    let mut open: Vec<Position> = Vec::new();
    for (character, place) in code_characters(text, at) {
        match character {
            '(' => open.push(Position {
                line: place.line,
                column: place.column + 1,
            }),
            ')' => {
                open.pop();
            }
            _ => {}
        }
    }
    open.pop()
}

/// How many arguments were written between `open` and the cursor: the
/// commas of this call's own level.
fn arguments_before(text: &str, open: Position, at: Position) -> usize {
    let mut depth = 0usize;
    let mut commas = 0usize;
    for (character, place) in code_characters(text, at) {
        if place < open {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas
}

/// The characters of `text` up to `at` that are code: what a string, a
/// character literal or a comment holds is text.
fn code_characters(text: &str, at: Position) -> Vec<(char, Position)> {
    let mut found = Vec::new();
    let mut line = 0u32;
    let mut column = 0u32;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let place = Position { line, column };
        if place >= at {
            break;
        }
        let step = |character: char, line: &mut u32, column: &mut u32| {
            if character == '\n' {
                *line += 1;
                *column = 0;
            } else {
                *column += character.len_utf16() as u32;
            }
        };
        match character {
            '"' | '\'' => {
                step(character, &mut line, &mut column);
                while let Some(inner) = characters.next() {
                    step(inner, &mut line, &mut column);
                    if inner == '\\' {
                        if let Some(escaped) = characters.next() {
                            step(escaped, &mut line, &mut column);
                        }
                        continue;
                    }
                    if inner == character || inner == '\n' {
                        break;
                    }
                }
                continue;
            }
            '/' if characters.peek() == Some(&'/') => {
                while let Some(inner) = characters.next() {
                    step(inner, &mut line, &mut column);
                    if inner == '\n' {
                        break;
                    }
                }
                step(character, &mut line, &mut column);
                continue;
            }
            '/' if characters.peek() == Some(&'*') => {
                let mut previous = ' ';
                step(character, &mut line, &mut column);
                while let Some(inner) = characters.next() {
                    step(inner, &mut line, &mut column);
                    if previous == '*' && inner == '/' {
                        break;
                    }
                    previous = inner;
                }
                continue;
            }
            _ => {}
        }
        found.push((character, place));
        step(character, &mut line, &mut column);
    }
    found
}

/// The identifier that ends right before `at`, skipping spaces.
fn identifier_ending_at(tree: &Syn, at: Position) -> Option<&Syn> {
    tree.walk()
        .into_iter()
        .filter(|node| is_identifier(node))
        .filter(|node| node.end.line == at.line && node.end.column < at.column)
        .max_by_key(|node| node.end.column)
}

/// The call whose name the cursor is on.
fn on_a_call_name<'a>(path: &[&'a Syn], at: Position) -> Option<&'a Syn> {
    path.iter()
        .rev()
        .find(|node| {
            node.kind == "method_invocation"
                && node
                    .child("name")
                    .is_some_and(|name| name.start <= at && at <= name.end)
        })
        .copied()
}

/// Every declaration the call could mean.
fn candidates_of<'a>(
    snapshot: &'a Snapshot,
    file: &'a FileView,
    call: &Syn,
    at: Position,
) -> Option<Vec<Found<'a>>> {
    match call.kind.as_str() {
        "identifier" | "field_identifier" => {
            let owner = snapshot.enclosing(file, at)?;
            Some(snapshot.members_called(&owner, &call.text))
        }
        "method_invocation" => {
            let name = call.child("name")?;
            let owner = match call.child("object") {
                Some(object) => {
                    let receiver = snapshot.type_of_expression(object, file)?;
                    snapshot.of_type(&receiver, file)?
                }
                None => snapshot.enclosing(file, at)?,
            };
            Some(snapshot.members_called(&owner, &name.text))
        }
        "object_creation_expression" => {
            let created = call.child("type")?;
            let written = crate::feature::semantic::type_of_node(created);
            let declaration = snapshot.of_type(&written, file)?;
            let constructors = snapshot.constructors_of(&declaration);
            Some(constructors)
        }
        "explicit_constructor_invocation" => {
            let of = call.child("constructor")?;
            let enclosing = snapshot.enclosing(file, at)?;
            let declaration = if of.kind == "super" {
                snapshot.supertype_of(&enclosing)?
            } else {
                enclosing
            };
            Some(snapshot.constructors_of(&declaration))
        }
        _ => None,
    }
}

/// The candidate a call with this many arguments means: one that has room
/// for the argument being typed, and whose arguments already fit.
fn chosen<'a>(
    snapshot: &Snapshot,
    file: &FileView,
    candidates: &'a [Found<'a>],
    call: &Syn,
    index: usize,
) -> Option<&'a Found<'a>> {
    let written = call
        .children
        .iter()
        .find(|child| child.kind == "argument_list")
        .map(|arguments| {
            arguments
                .children
                .iter()
                .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ","))
                .count()
        })
        .unwrap_or(0);
    let wanted = written.max(index + 1);
    let with_room: Vec<&Found<'a>> = candidates
        .iter()
        .filter(|candidate| {
            let arity = parameters_of(candidate.node).len();
            arity == wanted || (is_varargs(candidate.node) && wanted >= arity)
        })
        .collect();
    let considered = if with_room.is_empty() {
        candidates.iter().collect::<Vec<_>>()
    } else {
        with_room
    };
    let fitting = considered
        .iter()
        .find(|candidate| arguments_fit(snapshot, file, candidate, call));
    fitting.copied().or_else(|| considered.first().copied())
}

/// Whether the arguments already written fit a candidate's parameters.
fn arguments_fit(
    snapshot: &Snapshot,
    file: &FileView,
    candidate: &Found<'_>,
    call: &Syn,
) -> bool {
    let Some(arguments) = call.children.iter().find(|child| child.kind == "argument_list") else {
        return true;
    };
    let passed: Vec<&Syn> = arguments
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ","))
        .collect();
    let parameters = parameters_of(candidate.node);
    passed.iter().enumerate().all(|(at, argument)| {
        let Some(parameter) = parameters.get(at) else {
            return is_varargs(candidate.node);
        };
        let wanted = parameter
            .child("type")
            .or_else(|| parameter.children.first())
            .map(crate::feature::semantic::type_of_node);
        let given = snapshot.type_of_expression(argument, file);
        match (wanted, given) {
            (Some(wanted), Some(given)) => {
                wanted.name == given.name || wanted.name == "Object"
            }
            // An argument whose type the sources do not say fits anything.
            _ => true,
        }
    })
}

/// Which parameter the argument at `index` is: the last one swallows the
/// rest where it is varargs.
fn active_parameter(index: usize, parameters: &[String], declaration: &Syn) -> Option<usize> {
    if parameters.is_empty() {
        return None;
    }
    if index < parameters.len() {
        return Some(index);
    }
    is_varargs(declaration).then(|| parameters.len() - 1)
}

fn is_varargs(declaration: &Syn) -> bool {
    parameters_of(declaration)
        .last()
        .is_some_and(|parameter| parameter.kind == "spread_parameter")
}

/// The overloads of a call as jdtls lists them, and where the chosen one
/// is among them. A method overridden lower in the hierarchy is listed
/// once, as the nearest type declares it. jdtls lists the fewest
/// parameters first, and those of one count last declared first.
fn overloads(snapshot: &Snapshot, candidates: &[Found<'_>], chosen: &Found<'_>) -> (Vec<Overload>, usize) {
    let mut kept: Vec<(&Found<'_>, Vec<String>)> = Vec::new();
    for candidate in candidates {
        // A field of the same name is no overload.
        let is_field = matches!(
            candidate.node.kind.as_str(),
            "field_declaration" | "constant_declaration" | "variable_declarator" | "enum_constant"
        );
        if is_field && !std::ptr::eq(candidate.node, chosen.node) {
            continue;
        }
        let types = snapshot.parameter_types(candidate);
        if kept.iter().any(|(_, seen)| *seen == types) {
            continue;
        }
        kept.push((candidate, types));
    }
    kept.reverse();
    kept.sort_by_key(|(_, types)| types.len());
    let active = kept
        .iter()
        .position(|(candidate, _)| std::ptr::eq(candidate.node, chosen.node))
        .unwrap_or(0);
    let overloads = kept
        .into_iter()
        .map(|(candidate, _)| Overload {
            name: called_name(candidate),
            parameters: substituted_parameters(snapshot, candidate),
            returns: snapshot.return_written(candidate),
        })
        .collect();
    (overloads, active)
}

/// A constructor is named after its class; a method after itself.
#[allow(clippy::needless_pass_by_value)]
fn called_name(found: &Found<'_>) -> String {
    found.name.text.clone()
}

/// The parameters of the chosen candidate, with the type arguments of the
/// receiver filled in.
fn substituted_parameters(snapshot: &Snapshot, chosen: &Found<'_>) -> Vec<String> {
    snapshot.parameters_written(chosen)
}

/// The identifier the cursor is in, if any: a caller uses it to tell a
/// name from a call.
pub fn name_at(file: &FileView, at: Position) -> Option<&Syn> {
    path_to(&file.tree, at)
        .into_iter()
        .rev()
        .find(|node| is_identifier(node) && node.start <= at && at <= node.end)
}

/// A class with no constructor of its own still has one: the default.
pub fn is_default_constructor(found: &Found<'_>) -> bool {
    members_of(found.node)
        .iter()
        .all(|(member, _)| member.kind != "constructor_declaration")
}
