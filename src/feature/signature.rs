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
    if let Some(help) = in_javadoc(snapshot, file, at) {
        return Some(help);
    }
    let path = path_to(&file.tree, at);
    // A call the parser could not close (`println( }`) is read as it reads
    // once closed where the cursor is, as JDT's recovery reads it.
    let unclosed = !path.iter().any(|node| node.kind == "argument_list" && node.start < at && at < node.end);
    if unclosed && file.language == crate::language::Language::Java && open_parenthesis_before(&file.text, at).is_some() {
        // Closed where the text does not close it, else given an argument
        // where the one being typed is still missing (`f("t", )`).
        let patch = if commas_after(&file.text, at).is_some() { "null" } else { ");" };
        if let Some(closed) = closed_at(file, at, patch) {
            let closed_path = path_to(&closed.tree, at);
            if closed_path.iter().any(|node| node.kind == "argument_list" && node.start < at && at < node.end) {
                if let Some(help) = help_in(snapshot, &closed, at, &closed_path) {
                    return Some(help);
                }
            }
        }
    }
    help_in(snapshot, file, at, &path)
}

/// The commas of the call's own level between `at` and the parenthesis
/// that closes it; `None` where the statement ends before one does.
fn commas_after(text: &str, at: Position) -> Option<usize> {
    let offset = offset_of(text, at)?;
    let mut depth = 0usize;
    let mut commas = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in text[offset..].chars() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open || character == '\n' {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' => depth += 1,
            ')' if depth == 0 => return Some(commas),
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => commas += 1,
            ';' | '{' | '}' => return None,
            _ => {}
        }
    }
    None
}

/// `file` with `patch` written at `at`.
fn closed_at(file: &FileView, at: Position, patch: &str) -> Option<FileView> {
    let offset = offset_of(&file.text, at)?;
    let mut text = file.text.to_string();
    text.insert_str(offset, patch);
    let tree = crate::parser::syntax(file.language).tree(&text);
    Some(FileView {
        uri: file.uri.clone(),
        language: file.language,
        text: std::sync::Arc::new(text),
        tree: std::sync::Arc::new(tree),
        library: file.library,
    })
}

/// The byte offset of a position, columns counted in UTF-16 units.
fn offset_of(text: &str, at: Position) -> Option<usize> {
    let mut start = 0;
    for _ in 0..at.line {
        start += text[start..].find('\n')? + 1;
    }
    let mut units = 0;
    for (index, character) in text[start..].char_indices() {
        if units >= at.column as usize || character == '\n' {
            return Some(start + index);
        }
        units += character.len_utf16();
    }
    Some(text.len())
}

fn help_in(snapshot: &Snapshot, file: &FileView, at: Position, path: &[&Syn]) -> Option<Help> {
    let java = file.language == crate::language::Language::Java;
    let mut help = help_on_path(snapshot, file, at, path)?;
    if java {
        help.parameters = help.parameters.iter().map(|parameter| jdt_written(parameter)).collect();
    }
    Some(help)
}

fn help_on_path(snapshot: &Snapshot, file: &FileView, at: Position, path: &[&Syn]) -> Option<Help> {
    match being_typed(path, file, at) {
        Some((call, index)) => {
            let candidates = candidates_of(snapshot, file, call, at)?;
            // The commas after the cursor count too: `String($0,)` has two.
            let written = commas_after(&file.text, at).map_or(0, |after| if index + after > 0 { index + after + 1 } else { 0 });
            let chosen = chosen(snapshot, file, &candidates, call, index, written)?;
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
            let call = on_a_call_name(path, at)?;
            let candidates = candidates_of(snapshot, file, call, at)?;
            let chosen = chosen(snapshot, file, &candidates, call, 0, 0)?;
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

/// Inside a javadoc's `{@link String#substring(…)}` or `@see Type#name(`:
/// the member it names, at the parameter the cursor is at.
fn in_javadoc(snapshot: &Snapshot, file: &FileView, at: Position) -> Option<Help> {
    let comment = path_to(&file.tree, at)
        .into_iter()
        .rev()
        .find(|node| node.kind.ends_with("comment") && node.text.starts_with("/**") && node.start < at && at < node.end)?;
    let before = file.text_between(comment.start, at);
    let open = before.rfind('(')?;
    let inside = &before[open + 1..];
    if inside.contains(')') || inside.contains('\n') {
        return None;
    }
    let head = &before[..open];
    let name_start = head
        .rfind(|character: char| !(character.is_alphanumeric() || character == '_' || character == '$'))
        .map_or(0, |at| at + 1);
    let name = &head[name_start..];
    let reference = head[..name_start].strip_suffix('#')?;
    let type_start = reference
        .rfind(|character: char| !(character.is_alphanumeric() || matches!(character, '_' | '$' | '.')))
        .map_or(0, |at| at + 1);
    let type_name = &reference[type_start..];
    if name.is_empty() {
        return None;
    }
    let owner = if type_name.is_empty() {
        snapshot.enclosing(file, comment.end)?
    } else {
        snapshot.type_named(type_name, file)?
    };
    let candidates = snapshot.members_called(&owner, name);
    let index = inside.matches(',').count();
    let mut fitting: Vec<&Found<'_>> = candidates
        .iter()
        .filter(|candidate| parameters_of(candidate.node).len() > index)
        .collect();
    fitting.sort_by_key(|candidate| parameters_of(candidate.node).len());
    let chosen = fitting.first().copied().or_else(|| candidates.first())?;
    let parameters = substituted_parameters(snapshot, chosen);
    let active = active_parameter(index, &parameters, chosen.node);
    let (overloads, active_overload) = overloads(snapshot, &candidates, chosen);
    Some(Help {
        name: called_name(chosen),
        parameters: parameters.iter().map(|parameter| jdt_written(parameter)).collect(),
        active,
        overloads,
        active_overload,
    })
}

/// A parameter as JDT writes its type: type arguments joined by a comma
/// alone (`Function<String,String> f`).
fn jdt_written(parameter: &str) -> String {
    let mut written = String::new();
    let mut depth = 0usize;
    let mut after_comma = false;
    for character in parameter.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if after_comma && character == ' ' {
            continue;
        }
        after_comma = depth > 0 && character == ',';
        written.push(character);
    }
    written
}

/// Whether the parser had to recover anywhere below `node`.
fn has_error(node: &Syn) -> bool {
    node.walk().into_iter().any(|below| below.kind == "ERROR" || below.text.is_empty())
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
    // The parameters of a declaration are no call.
    let declares = path.iter().any(|node| {
        node.kind == "formal_parameters" && node.start.line == open.line && node.start.column + 1 == open.column
    });
    if declares {
        return None;
    }
    let mut index = arguments_before(&file.text, open, at);
    let arguments = path
        .iter()
        .rev()
        .find(|node| node.kind == "argument_list" && node.start < at && at < node.end);
    if let Some(arguments) = arguments {
        // A list the parser read whole says where its arguments part: a
        // comma between type arguments (`new HashMap<String, String>()`) is
        // none of its own.
        let is_this_list = arguments.start.line == open.line && arguments.start.column + 1 == open.column;
        if is_this_list && !has_error(arguments) {
            index = arguments
                .children
                .iter()
                .filter(|child| child.kind == "," && child.start < at)
                .count();
        }
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
            '"' if text_block_ahead(&characters) => {
                // A text block: up to the closing `"""`.
                step(character, &mut line, &mut column);
                for _ in 0..2 {
                    if let Some(quote) = characters.next() {
                        step(quote, &mut line, &mut column);
                    }
                }
                let mut quotes = 0;
                while let Some(inner) = characters.next() {
                    if (Position { line, column }) >= at {
                        break;
                    }
                    step(inner, &mut line, &mut column);
                    if inner == '\\' {
                        if let Some(escaped) = characters.next() {
                            step(escaped, &mut line, &mut column);
                        }
                        quotes = 0;
                        continue;
                    }
                    quotes = if inner == '"' { quotes + 1 } else { 0 };
                    if quotes == 3 {
                        break;
                    }
                }
                continue;
            }
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
                step(character, &mut line, &mut column);
                while let Some(inner) = characters.next() {
                    step(inner, &mut line, &mut column);
                    if inner == '\n' {
                        break;
                    }
                }
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

/// Whether the `"` just read opens a text block: two more follow it.
fn text_block_ahead(characters: &std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut ahead = characters.clone();
    ahead.next() == Some('"') && ahead.next() == Some('"')
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
            // `E.super.equals(…)`: the supertype of the type named.
            let through_super = call.children.iter().any(|child| child.kind == "super" && child.field.is_none());
            let owner = match call.child("object") {
                Some(object) if through_super => {
                    let named = snapshot.type_named(&object.text, file)?;
                    snapshot.supertype_of(&named)?
                }
                Some(object) => {
                    let receiver = snapshot.type_of_expression(object, file)?;
                    snapshot.of_type(&receiver, file)?
                }
                None => snapshot.enclosing(file, at)?,
            };
            let mut called = snapshot.members_called(&owner, &name.text);
            // Every class is an `Object`, whether or not it says so.
            if called.is_empty()
                && file.language == crate::language::Language::Java
                && let Some(object) = snapshot.type_named("java.lang.Object", file)
            {
                called = snapshot.members_called(&object, &name.text);
            }
            Some(called)
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
    written_in_text: usize,
) -> Option<&'a Found<'a>> {
    // The arguments written, an empty one between commas too:
    // `String(, x)` has two.
    let written = call
        .children
        .iter()
        .find(|child| child.kind == "argument_list")
        .map(|arguments| {
            let given = arguments
                .children
                .iter()
                .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
                .count();
            let commas = arguments.children.iter().filter(|child| child.kind == ",").count();
            if commas > 0 { given.max(commas + 1) } else { given }
        })
        .unwrap_or(0)
        .max(written_in_text);
    // An empty argument list means the overload without parameters, where
    // there is one, as it does to jdtls.
    let wanted = if written == 0
        && candidates.iter().any(|candidate| {
            parameters_of(candidate.node).is_empty()
                && matches!(candidate.node.kind.as_str(), "method_declaration" | "constructor_declaration")
        }) {
        0
    } else {
        written.max(index + 1)
    };
    // jdtls looks through the overloads as it lists them: of one count,
    // the last declared first.
    let listed: Vec<&Found<'a>> = candidates.iter().rev().collect();
    let with_room: Vec<&Found<'a>> = listed
        .iter()
        .copied()
        .filter(|candidate| {
            let arity = parameters_of(candidate.node).len();
            arity == wanted || (is_varargs(candidate.node) && wanted >= arity)
        })
        .collect();
    // None of that count: the fewest parameters above it.
    let mut longer: Vec<&Found<'a>> = listed
        .iter()
        .copied()
        .filter(|candidate| parameters_of(candidate.node).len() > wanted)
        .collect();
    longer.sort_by_key(|candidate| parameters_of(candidate.node).len());
    let considered = if !with_room.is_empty() {
        with_room
    } else if !longer.is_empty() {
        longer
    } else {
        listed
    };
    // The one the arguments written fit best: an argument of the very
    // type a parameter wants counts above one it merely fits.
    let mut best: Option<(&Found<'a>, usize)> = None;
    for candidate in &considered {
        if let Some(score) = fit_score(snapshot, file, candidate, call)
            && best.is_none_or(|(_, best_score)| score > best_score)
        {
            best = Some((candidate, score));
        }
    }
    best.map(|(candidate, _)| candidate).or_else(|| considered.first().copied())
}

/// How well the arguments already written fit a candidate's parameters:
/// `None` where one does not fit, else two for each of the very type a
/// parameter wants and one for each it fits otherwise.
fn fit_score(snapshot: &Snapshot, file: &FileView, candidate: &Found<'_>, call: &Syn) -> Option<usize> {
    let Some(arguments) = call.children.iter().find(|child| child.kind == "argument_list") else {
        return Some(0);
    };
    let passed: Vec<&Syn> = arguments
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
        .collect();
    let parameters = parameters_of(candidate.node);
    let mut score = 0;
    for (at, argument) in passed.iter().enumerate() {
        let spread = is_varargs(candidate.node) && at + 1 >= parameters.len();
        let Some(parameter) = parameters.get(at.min(parameters.len().saturating_sub(1))) else {
            return None;
        };
        if at >= parameters.len() && !spread {
            return None;
        }
        let written = parameter
            .child("type")
            .or_else(|| {
                parameter
                    .children
                    .iter()
                    .find(|child| child.kind.ends_with("_type") || child.kind == "type_identifier")
            })
            .map(crate::feature::semantic::type_of_node);
        let given = snapshot.type_of_expression(argument, file);
        match (written, given) {
            (Some(mut wanted), Some(given)) => {
                // A varargs parameter takes its element, or the array.
                if spread && given.dimensions == wanted.dimensions + 1 {
                    wanted.dimensions += 1;
                }
                if wanted.name == given.name && wanted.dimensions == given.dimensions {
                    score += 2;
                } else if wanted.name == "Object" && wanted.dimensions <= given.dimensions || wanted.name == given.name {
                    score += 1;
                } else {
                    return None;
                }
            }
            // An argument whose type the sources do not say fits anything.
            _ => score += 1,
        }
    }
    Some(score)
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
            parameters: substituted_parameters(snapshot, candidate)
                .iter()
                .map(|parameter| {
                    if candidate.file.language == crate::language::Language::Java {
                        jdt_written(parameter)
                    } else {
                        parameter.clone()
                    }
                })
                .collect(),
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
    let written = snapshot.parameters_written(chosen);
    // A library method of a type with its arguments given is JDT's
    // parameterized binding, whose parameters it names `arg0`, `arg1`.
    if chosen.file.library && chosen.file.language == crate::language::Language::Java && !chosen.bindings.is_empty() {
        return written
            .iter()
            .enumerate()
            .map(|(index, parameter)| match parameter.rsplit_once(' ') {
                Some((written_type, _)) => format!("{written_type} arg{index}"),
                None => parameter.clone(),
            })
            .collect();
    }
    written
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
