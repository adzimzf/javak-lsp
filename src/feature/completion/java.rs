//! What a Java file is offered, as jdtls offers it.
//!
//! The cursor's surroundings decide the kind of name wanted, and with it
//! what is offered and in which order:
//!
//! - between declarations of the file, keywords only, and only those that
//!   may come next: `package` before anything else, not after;
//! - where a member starts, the file's types, the modifiers and the
//!   methods of the supertypes to override;
//! - after `new`, types;
//! - in an argument, the method or constructor being called, then the
//!   names whose type its parameter takes;
//! - in a statement or an expression, the names in scope (locals, then
//!   fields, then methods), then types, then keywords. A value of a
//!   declared type puts that type first, and leaves out the variable
//!   being declared and what returns `void`.
//!
//! A type is offered by name only once a prefix is typed, except the file's
//! own: an empty prefix would otherwise offer every type of the JDK.

use super::{
    Offer, PROBE, Wanted, annotation_elements, enclosing_kind, members, receiver_of,
    starts_a_member, switch_constants,
};
use crate::feature::semantic::{
    FileView, Found, Snapshot, Type, imports, is_identifier, is_type_declaration, members_of,
    package_of, parameters_of, path_to, type_of_node, types_of,
};
use crate::parser::{Position, Syn};

/// The order offers are sorted in: each group by name.
const EXPECTED: char = '0';
const LOCAL: char = '1';
const FIELD: char = '2';
const METHOD: char = '3';
const TYPE: char = '5';
const KEYWORD: char = '9';

/// What may start a compilation unit.
const UNIT_KEYWORDS: &[&str] = &[
    "abstract",
    "class",
    "enum",
    "final",
    "import",
    "interface",
    "non-sealed",
    "package",
    "public",
    "record",
    "sealed",
];

/// What declares another type once one is declared.
const TYPE_KEYWORDS: &[&str] = &["class", "enum", "interface", "record"];

/// What may start a member of a type.
const MEMBER_KEYWORDS: &[&str] = &[
    "abstract",
    "class",
    "default",
    "enum",
    "final",
    "interface",
    "native",
    "private",
    "protected",
    "public",
    "static",
    "synchronized",
    "transient",
    "void",
    "volatile",
];

/// What may start a statement or an expression.
const STATEMENT_KEYWORDS: &[&str] = &[
    "assert",
    "boolean",
    "break",
    "byte",
    "char",
    "class",
    "continue",
    "do",
    "double",
    "false",
    "final",
    "float",
    "for",
    "if",
    "int",
    "long",
    "new",
    "null",
    "return",
    "short",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "true",
    "try",
    "var",
    "while",
];

/// What may start an expression inside another.
const EXPRESSION_KEYWORDS: &[&str] = &["false", "new", "null", "super", "this", "true"];

/// Primitive types and their boxes: a value of one of these is never a
/// type name, so only what converts to it is offered.
const PRIMITIVE_LIKE: &[&str] = &[
    "boolean",
    "byte",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "Boolean",
    "Byte",
    "Character",
    "Short",
    "Integer",
    "Long",
    "Float",
    "Double",
];

/// The cursor's surroundings: the file read with the placeholder in it,
/// and the file as written.
pub(super) struct Context<'a> {
    pub snapshot: &'a Snapshot,
    pub file: &'a FileView,
    pub written: &'a FileView,
    pub path: Vec<&'a Syn>,
    pub name: &'a Syn,
    pub prefix: &'a str,
    pub dotted: Option<&'a str>,
}

pub(super) fn contextual(context: &Context<'_>) -> Vec<Offer> {
    let Context {
        snapshot,
        file,
        path,
        name,
        prefix,
        ..
    } = context;
    if enclosing_kind(path, "import_declaration").is_some() {
        return import_offers(context);
    }
    if let Some(receiver) = receiver_of(path, name) {
        return members(snapshot, file, receiver, prefix);
    }
    if enclosing_kind(path, "switch_label").is_some() {
        return switch_constants(snapshot, file, name.start);
    }
    if enclosing_kind(path, "annotation_argument_list").is_some() {
        if !is_element_value(path, name) {
            return annotation_elements(snapshot, file, path);
        }
        return element_values(context);
    }
    if is_declared_name(path, name) {
        return Vec::new();
    }
    if wants_a_type(context) {
        return types(context);
    }
    if !in_a_body(path) {
        return unit_keywords(context);
    }
    if let Some(offers) = argument_offers(context) {
        return offers;
    }
    let expected = expected_type(context);
    if after_new(context) {
        let mut offers = types(context);
        if let Some(expected) = &expected {
            offers.push(expected_offer(expected));
        }
        return offers;
    }
    if starts_a_member(path, name) || starts_a_recovered_member(path, name) {
        let mut offers = types(context);
        offers.extend(overridable(context));
        offers.extend(keywords(MEMBER_KEYWORDS));
        return offers;
    }
    let mut offers = Vec::new();
    let only_values = expected
        .as_ref()
        .is_some_and(|expected| PRIMITIVE_LIKE.contains(&expected.name.as_str()));
    offers.extend(
        in_scope(context)
            .into_iter()
            .filter(|value| match &expected {
                Some(expected) => {
                    !value.is_void && (!only_values || value.converts_to(context, expected))
                }
                None => true,
            })
            .map(|value| value.offer),
    );
    offers.extend(statically_imported(context));
    if !only_values {
        offers.extend(types(context));
    }
    if let Some(expected) = &expected {
        offers.push(expected_offer(expected));
    }
    if !prefix.is_empty() {
        if starts_a_statement(context) {
            offers.extend(keywords(STATEMENT_KEYWORDS));
        } else {
            offers.extend(keywords(EXPRESSION_KEYWORDS));
        }
    }
    offers
}

/// The value of an annotation's element: what has the element's type. A
/// text is a literal or a constant, never a type's name.
fn element_values(context: &Context<'_>) -> Vec<Offer> {
    let wanted = element_type(context);
    let is_text = wanted
        .as_ref()
        .is_some_and(|wanted| wanted.name == "String");
    let mut offers: Vec<Offer> = in_scope(context)
        .into_iter()
        .filter(|value| !value.is_void)
        .filter(|value| match &wanted {
            Some(wanted) if is_text => value.converts_to(
                context,
                &Type {
                    dimensions: 0,
                    ..wanted.clone()
                },
            ),
            _ => true,
        })
        .map(|value| value.offer)
        .collect();
    if !is_text {
        offers.extend(types(context));
    }
    offers
}

/// The type of the annotation element the placeholder is the value of.
fn element_type(context: &Context<'_>) -> Option<Type> {
    let pair = context
        .path
        .iter()
        .rev()
        .find(|node| node.kind == "element_value_pair")?;
    let key = pair.child("key")?;
    let annotation = context
        .path
        .iter()
        .rev()
        .find(|node| node.kind == "annotation")?;
    let declaration = context
        .snapshot
        .type_named(&annotation.child("name")?.text, context.file)?;
    members_of(declaration.node)
        .into_iter()
        .find(|(_, named)| named.text == key.text)
        .and_then(|(member, _)| member.child("type"))
        .map(type_of_node)
}

/// Where only a type can be written: after `extends`, `implements` or
/// `throws`.
fn wants_a_type(context: &Context<'_>) -> bool {
    let in_a_clause = context.path.iter().any(|node| {
        matches!(
            node.kind.as_str(),
            "superclass" | "super_interfaces" | "extends_interfaces" | "throws" | "type_bound"
        )
    });
    in_a_clause
        || ["extends", "implements", "throws"]
            .iter()
            .any(|word| ends_with_word(&text_before(context), word))
}

/// Whether `text` ends with the word `word`.
fn ends_with_word(text: &str, word: &str) -> bool {
    let text = text.trim_end();
    text.ends_with(word)
        && !text[..text.len() - word.len()]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
}

/// The text written before the name being typed, from the start of the
/// file.
fn text_before(context: &Context<'_>) -> String {
    let text = &context.written.text;
    offset_of(text, context.name.start)
        .map(|offset| text[..offset].to_owned())
        .unwrap_or_default()
}

/// Whether the name starts a statement, where `if` or `while` may be
/// written, rather than being inside an expression.
fn starts_a_statement(context: &Context<'_>) -> bool {
    let before = text_before(context);
    let before = before.trim_end();
    before.is_empty() || before.ends_with([';', '{', '}', ':'])
}

/// Where a member starts in a type the parser could not close: the
/// placeholder stands among the pieces of `class Name { …`.
fn starts_a_recovered_member(path: &[&Syn], name: &Syn) -> bool {
    parent_of(path, name)
        .is_some_and(|parent| parent.kind == "ERROR" && opens_a_body(parent, name.start))
}

/// Whether a node the parser could not read holds `class Name {` (or an
/// interface's, enum's or record's) before `at`.
fn opens_a_body(node: &Syn, at: Position) -> bool {
    let children: Vec<&Syn> = node
        .children
        .iter()
        .filter(|child| child.end <= at)
        .collect();
    children.windows(2).any(|pair| {
        matches!(
            pair[0].kind.as_str(),
            "class" | "interface" | "enum" | "record"
        ) && is_identifier(pair[1])
    }) && children.iter().any(|child| child.kind == "{")
}

fn keywords(words: &[&str]) -> Vec<Offer> {
    words
        .iter()
        .map(|word| Offer {
            name: (*word).to_owned(),
            rank: format!("{KEYWORD}{word}"),
        })
        .collect()
}

fn expected_offer(expected: &Type) -> Offer {
    Offer {
        name: expected.name.clone(),
        rank: format!("{EXPECTED}{}", expected.name),
    }
}

/// Whether the cursor is inside a type's body, rather than between the
/// declarations of the file.
fn in_a_body(path: &[&Syn]) -> bool {
    let at = path.last().map(|node| node.start);
    path.iter().any(|node| {
        matches!(
            node.kind.as_str(),
            "class_body" | "interface_body" | "enum_body" | "annotation_type_body"
        ) || node.kind == "ERROR" && at.is_some_and(|at| opens_a_body(node, at))
    })
}

/// Between the declarations of the file: what may come next. Before
/// anything is declared, everything a unit may start with; once a type is
/// declared, another type. With a prefix typed, any keyword but those the
/// file is past.
fn unit_keywords(context: &Context<'_>) -> Vec<Offer> {
    let at = context.name.start;
    let tree = &context.file.tree;
    let before = |kind: &str| {
        tree.children
            .iter()
            .any(|child| child.kind == kind && child.end <= at)
    };
    let has_package = before("package_declaration");
    let has_import = before("import_declaration");
    let has_type = tree
        .children
        .iter()
        .any(|child| is_type_declaration(child) && child.end <= at);
    let past = |word: &&str| match *word {
        "package" => has_package || has_import || has_type,
        "import" => has_type,
        _ => false,
    };
    let starts = !has_package && !has_import && !has_type;
    if starts || !context.prefix.is_empty() {
        let words: Vec<&str> = UNIT_KEYWORDS
            .iter()
            .copied()
            .filter(|word| !past(word))
            .collect();
        return keywords(&words);
    }
    let mut words: Vec<&str> = TYPE_KEYWORDS.to_vec();
    if !has_type {
        words.push("import");
    }
    keywords(&words)
}

/// Whether the placeholder is the name a declaration declares: a variable
/// is being named, and nothing that exists is wanted there.
fn is_declared_name(path: &[&Syn], name: &Syn) -> bool {
    let Some(parent) = parent_of(path, name) else {
        return false;
    };
    let names = |node: &Syn| {
        node.child("name")
            .is_some_and(|named| named.start == name.start && named.kind == name.kind)
    };
    match parent.kind.as_str() {
        "variable_declarator" => {
            names(parent)
                && path
                    .iter()
                    .any(|node| node.kind == "local_variable_declaration")
        }
        "formal_parameter" | "catch_formal_parameter" => names(parent),
        _ => false,
    }
}

fn parent_of<'a>(path: &[&'a Syn], name: &Syn) -> Option<&'a Syn> {
    path.iter()
        .rev()
        .find(|node| {
            node.children
                .iter()
                .any(|child| child.start == name.start && child.kind == name.kind)
        })
        .copied()
}

/// Whether the placeholder is the value of `element = value` in an
/// annotation, rather than the element's name.
fn is_element_value(path: &[&Syn], name: &Syn) -> bool {
    path.iter().any(|node| {
        node.kind == "element_value_pair"
            && node
                .child("key")
                .is_some_and(|key| !(key.start == name.start && key.kind == name.kind))
    })
}

/// Right after `new`: a type is being created.
fn after_new(context: &Context<'_>) -> bool {
    let line = context
        .written
        .text
        .lines()
        .nth(context.name.start.line as usize)
        .unwrap_or_default();
    let before: String = line
        .chars()
        .take(context.name.start.column as usize)
        .collect();
    let before = before.trim_end();
    before.ends_with("new")
        && !before[..before.len() - 3]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
}

/// The types a name may stand for: with no prefix, the file's own; with
/// one, every type of the workspace and its libraries starting with it.
fn types(context: &Context<'_>) -> Vec<Offer> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    };
    if context.prefix.is_empty() {
        for (name, _) in types_of(context.file) {
            add(name);
        }
        for name in recovered_types(&context.file.tree) {
            add(&name);
        }
    } else {
        for file in &context.snapshot.files {
            for (name, _) in types_of(file) {
                if starts_with(name, context.prefix) {
                    add(name);
                }
            }
        }
        if let Some(libraries) = context.snapshot.libraries() {
            for name in libraries.types_starting(context.prefix) {
                add(&name);
            }
        }
    }
    names
        .into_iter()
        .map(|name| Offer {
            rank: format!("{TYPE}{name}"),
            name,
        })
        .collect()
}

/// The types a file declares where the parser could not read the
/// declaration: `class Name {` left open.
fn recovered_types(tree: &Syn) -> Vec<String> {
    let mut names = Vec::new();
    for node in tree.walk() {
        if node.kind != "ERROR" {
            continue;
        }
        for pair in node.children.windows(2) {
            if matches!(
                pair[0].kind.as_str(),
                "class" | "interface" | "enum" | "record"
            ) && is_identifier(&pair[1])
                && pair[1].text != PROBE
            {
                names.push(pair[1].text.clone());
            }
        }
    }
    names
}

/// Whether `name` starts with `prefix`, whatever the case.
pub(super) fn starts_with(name: &str, prefix: &str) -> bool {
    name.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// Whether an offer matches what is typed, as jdtls matches it: a type
/// whatever the case (`ru` finds `Runnable`), anything else with its
/// first letter as typed (`A` does not find `args`).
pub(super) fn matches(offer: &Offer, prefix: &str) -> bool {
    let is_a_type = offer.rank.starts_with([TYPE, EXPECTED]);
    let first_letter = |text: &str| text.chars().next();
    starts_with(&offer.name, prefix)
        && (is_a_type || first_letter(&offer.name) == first_letter(prefix))
}

/// A name in scope, with what is known of its type.
struct Value<'a> {
    offer: Offer,
    is_void: bool,
    found: Option<Found<'a>>,
}

impl Value<'_> {
    fn converts_to(&self, context: &Context<'_>, expected: &Type) -> bool {
        let Some(found) = &self.found else {
            return false;
        };
        context
            .snapshot
            .type_of_declaration(found)
            .is_some_and(|written| assignable(context, &written, expected))
    }
}

/// Whether a value of type `from` may be given where `to` is wanted.
fn assignable(context: &Context<'_>, from: &Type, to: &Type) -> bool {
    if from.dimensions != to.dimensions {
        return to.name == "Object" && to.dimensions == 0;
    }
    if from.name == to.name || boxed(&from.name) == boxed(&to.name) {
        return true;
    }
    if PRIMITIVE_LIKE.contains(&from.name.as_str()) {
        return to.name == "Object" || to.name == "Number" && !is_primitive(&from.name);
    }
    if to.name == "Object" {
        return true;
    }
    let Some(declaration) = context.snapshot.of_type(from, context.file) else {
        return false;
    };
    context
        .snapshot
        .with_supertypes(&declaration)
        .iter()
        .any(|above| above.name.text == to.name)
}

fn is_primitive(name: &str) -> bool {
    name.starts_with(|character: char| character.is_lowercase())
}

/// A primitive's box, any other type itself.
fn boxed(name: &str) -> &str {
    match name {
        "boolean" => "Boolean",
        "byte" => "Byte",
        "char" => "Character",
        "short" => "Short",
        "int" => "Integer",
        "long" => "Long",
        "float" => "Float",
        "double" => "Double",
        other => other,
    }
}

/// The locals and parameters in scope, then the members of the classes
/// around the cursor: fields, then methods. The variable whose value is
/// being written is not offered.
fn in_scope<'a>(context: &Context<'a>) -> Vec<Value<'a>> {
    let Context {
        snapshot,
        file,
        name,
        ..
    } = context;
    let at = name.start;
    let declaring = declaring(context);
    let mut values = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut locals = snapshot.names_in_scope(file, at);
    locals.extend(recovered_locals(context));
    for local in locals {
        if declaring.as_deref() == Some(local.as_str()) || seen.contains(&local) {
            continue;
        }
        seen.push(local.clone());
        values.push(Value {
            found: snapshot.name_in_scope(&local, file, at),
            offer: Offer {
                rank: format!("{LOCAL}{local}"),
                name: local,
            },
            is_void: false,
        });
    }
    let enclosing = path_to(&file.tree, at)
        .into_iter()
        .rev()
        .filter(|node| is_type_declaration(node))
        .filter_map(|node| snapshot.type_named(&node.child("name")?.text, file))
        .collect::<Vec<_>>();
    for owner in enclosing {
        for (member, named, owner) in own_members(context, &owner, Wanted::Everything) {
            if seen.contains(&named.text) {
                continue;
            }
            seen.push(named.text.clone());
            let is_method = member.kind == "method_declaration";
            values.push(Value {
                offer: Offer {
                    name: named.text.clone(),
                    rank: format!("{}{}", if is_method { METHOD } else { FIELD }, named.text),
                },
                is_void: is_method
                    && member
                        .child("type")
                        .is_some_and(|written| written.text == "void"),
                found: Some(Found {
                    file: owner.file,
                    node: member,
                    name: named,
                    bindings: owner.bindings.clone(),
                }),
            });
        }
    }
    values
}

/// Locals the parser could not read as declarations: `String s` before
/// the placeholder in a statement it gave up on.
fn recovered_locals(context: &Context<'_>) -> Vec<String> {
    let at = context.name.start;
    let mut names = Vec::new();
    for node in context.path.iter().filter(|node| node.kind == "ERROR") {
        for pair in node.children.windows(2) {
            let is_type = matches!(
                pair[0].kind.as_str(),
                "type_identifier"
                    | "generic_type"
                    | "integral_type"
                    | "floating_point_type"
                    | "boolean_type"
                    | "array_type"
                    | "scoped_type_identifier"
            );
            if is_type && pair[1].kind == "identifier" && pair[1].end <= at && pair[1].text != PROBE
            {
                names.push(pair[1].text.clone());
            }
        }
    }
    names
}

/// The variable whose initializer the placeholder is in.
fn declaring(context: &Context<'_>) -> Option<String> {
    context
        .path
        .iter()
        .rev()
        .find(|node| {
            node.kind == "variable_declarator"
                && node
                    .child("value")
                    .is_some_and(|value| value.covers(context.name.start))
        })
        .and_then(|declarator| declarator.child("name"))
        .map(|named| named.text.clone())
        .or_else(|| written_declaration(context).map(|(_, name)| name))
}

/// `Type name = ` written right before the placeholder, in a statement
/// the parser could not read, possibly across a line comment:
/// `Object variable = |//`.
fn written_declaration(context: &Context<'_>) -> Option<(Type, String)> {
    let before = text_before(context);
    let statement = before
        .rsplit([';', '{', '}'])
        .next()?
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    let mut statement = statement.trim_end();
    if ends_with_word(statement, "new") {
        statement = statement[..statement.len() - 3].trim_end();
    }
    let statement = statement.strip_suffix('=')?.trim_end();
    let name_start = statement
        .rfind(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '$')
        })
        .map_or(0, |at| at + 1);
    let name = &statement[name_start..];
    let written = statement[..name_start].trim();
    let written = written
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "final" | "static" | "private" | "public" | "protected"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let is_type = !written.is_empty()
        && written
            .chars()
            .all(|character| character.is_alphanumeric() || "_$.<>[], ?".contains(character));
    if name.is_empty() || !is_type || written.contains(' ') && !written.contains('<') {
        return None;
    }
    Some((written_type(&written), name.to_owned()))
}

/// A type as it is written: `java.util.List<String>[]` is a `List` with a
/// dimension.
fn written_type(written: &str) -> Type {
    let dimensions = written.matches("[]").count();
    let raw = written
        .split('<')
        .next()
        .unwrap_or(written)
        .trim_end_matches("[]")
        .trim();
    Type {
        name: raw.rsplit('.').next().unwrap_or(raw).to_owned(),
        dimensions,
        ..Type::default()
    }
}

/// The members a type and its supertypes declare that the file may name,
/// without those of the types nested in them, nearest first.
fn own_members<'a>(
    context: &Context<'a>,
    of: &Found<'a>,
    wanted: Wanted,
) -> Vec<(&'a Syn, &'a Syn, Found<'a>)> {
    let mut found = Vec::new();
    for owner in context.snapshot.with_supertypes(of) {
        let nested: Vec<(Position, Position)> = body_of(owner.node)
            .map(|body| {
                body.walk()
                    .into_iter()
                    .skip(1)
                    .filter(|node| {
                        matches!(
                            node.kind.as_str(),
                            "class_body" | "interface_body" | "enum_body" | "annotation_type_body"
                        )
                    })
                    .map(|node| (node.start, node.end))
                    .collect()
            })
            .unwrap_or_default();
        for (member, named) in members_of(owner.node) {
            if nested
                .iter()
                .any(|(start, end)| *start <= member.start && member.end <= *end)
            {
                continue;
            }
            if matches!(
                member.kind.as_str(),
                "constructor_declaration" | "compact_constructor_declaration"
            ) {
                continue;
            }
            let is_static = member.text.contains("static") || member.kind == "enum_constant";
            let offered = match wanted {
                Wanted::Static => is_static,
                Wanted::Instance => member.kind != "enum_constant",
                Wanted::Everything => true,
            };
            if offered && super::reachable(context.snapshot, context.file, &owner, member) {
                found.push((member, named, owner.clone()));
            }
        }
    }
    found
}

fn body_of(declaration: &Syn) -> Option<&Syn> {
    declaration.children.iter().find(|child| {
        matches!(
            child.kind.as_str(),
            "class_body" | "interface_body" | "enum_body" | "annotation_type_body"
        )
    })
}

/// The type the value being written is wanted as: what the variable it
/// initializes or is assigned to is declared as.
fn expected_type(context: &Context<'_>) -> Option<Type> {
    let is_the_value =
        |value: &&Syn| value.start == context.name.start && value.end == context.name.end;
    for node in context.path.iter().rev() {
        match node.kind.as_str() {
            "variable_declarator" => {
                // A placeholder the parser left beside the value, rather
                // than in it, is read from the text below.
                let value = node.child("value");
                if !value.is_some_and(|value| value.covers(context.name.start)) {
                    break;
                }
                value.filter(is_the_value)?;
                let declaration = context.path.iter().rev().find(|node| {
                    matches!(
                        node.kind.as_str(),
                        "local_variable_declaration" | "field_declaration"
                    )
                })?;
                return declaration
                    .child("type")
                    .map(type_of_node)
                    .filter(|written| written.name != "var");
            }
            "assignment_expression" => {
                node.child("right").filter(is_the_value)?;
                let left = node.child("left")?;
                return context.snapshot.type_of_expression(left, context.file);
            }
            "block" | "class_body" | "lambda_expression" | "argument_list" => break,
            _ => {}
        }
    }
    written_declaration(context).map(|(written, _)| written)
}

/// In the arguments of a call: the method or constructor called, then
/// the names whose type one of its parameters at this position takes.
/// Right after another name, only the call. The call is read from the text,
/// which a half-written argument list leaves to the parser to guess at.
fn argument_offers(context: &Context<'_>) -> Option<Vec<Offer>> {
    let call = open_call(context)?;
    let mut offers = vec![Offer {
        rank: format!("{EXPECTED}{}", call.name),
        name: call.name.clone(),
    }];
    let before = text_before(context);
    let before = before.trim_end();
    if !(before.ends_with('(') || before.ends_with(',')) {
        return Some(offers);
    }
    let wanted = parameter_types(context, &call);
    offers.extend(
        in_scope(context)
            .into_iter()
            .filter(|value| !value.is_void)
            .filter(|value| {
                wanted
                    .iter()
                    .any(|expected| value.converts_to(context, expected))
            })
            .map(|value| value.offer),
    );
    Some(offers)
}

/// A call whose argument list the placeholder is in.
struct OpenCall {
    name: String,
    /// `new Name(…)` rather than a method's call.
    creates: bool,
    /// Which argument the placeholder is.
    index: usize,
    /// Where the `(` is.
    open: Position,
}

/// The call around the placeholder: the nearest `(` not closed before it,
/// with a name in front that is not a keyword (`if (`).
fn open_call(context: &Context<'_>) -> Option<OpenCall> {
    let before = text_before(context);
    let characters: Vec<char> = before.chars().collect();
    let mut depth = 0usize;
    let mut index = 0;
    let mut open = None;
    let mut in_text = false;
    for at in (0..characters.len()).rev() {
        let character = characters[at];
        if character == '"' {
            in_text = !in_text;
            continue;
        }
        if in_text {
            continue;
        }
        match character {
            ')' | ']' => depth += 1,
            '(' | '[' if depth > 0 => depth -= 1,
            '(' => {
                open = Some(at);
                break;
            }
            ',' if depth == 0 => index += 1,
            ';' | '{' | '}' if depth == 0 => return None,
            _ => {}
        }
    }
    let open = open?;
    let mut end = open;
    while end > 0 && characters[end - 1].is_whitespace() {
        end -= 1;
    }
    // `new ArrayList<>(`: the type arguments are not the name.
    if end > 0 && characters[end - 1] == '>' {
        let mut depth = 0;
        while end > 0 {
            end -= 1;
            match characters[end] {
                '>' => depth += 1,
                '<' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let mut start = end;
    while start > 0 && (characters[start - 1].is_alphanumeric() || characters[start - 1] == '_') {
        start -= 1;
    }
    let name: String = characters[start..end].iter().collect();
    if name.is_empty()
        || super::JAVA_KEYWORDS.contains(&name.as_str())
        || name.starts_with(char::is_numeric)
    {
        return None;
    }
    let mut qualifier_start = start;
    while qualifier_start > 0
        && (characters[qualifier_start - 1].is_alphanumeric()
            || matches!(characters[qualifier_start - 1], '_' | '.'))
    {
        qualifier_start -= 1;
    }
    let ahead: String = characters[..qualifier_start].iter().collect();
    let creates = ends_with_word(&ahead, "new");
    let line = characters[..open]
        .iter()
        .filter(|character| **character == '\n')
        .count();
    let column = characters[..open]
        .iter()
        .rev()
        .take_while(|character| **character != '\n')
        .count();
    Some(OpenCall {
        name,
        creates,
        index,
        open: Position {
            line: line as u32,
            column: column as u32,
        },
    })
}

fn offset_of(text: &str, at: Position) -> Option<usize> {
    let mut offset = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index == at.line as usize {
            let column: usize = line
                .chars()
                .take(at.column as usize)
                .map(char::len_utf8)
                .sum();
            return Some(offset + column);
        }
        offset += line.len();
    }
    None
}

/// The types the parameter at the placeholder's position of every
/// overload of the call takes.
fn parameter_types(context: &Context<'_>, call: &OpenCall) -> Vec<Type> {
    let snapshot = context.snapshot;
    let file = context.file;
    let mut overloads: Vec<&Syn> = Vec::new();
    if call.creates {
        if let Some(owner) = snapshot.type_named(&call.name, file) {
            for (member, _) in members_of(owner.node) {
                if member.kind == "constructor_declaration" {
                    overloads.push(member);
                }
            }
        }
    } else {
        // The invocation the parser made of it, for its receiver.
        let invocation = context.path.iter().rev().find(|node| {
            node.kind == "method_invocation"
                && node
                    .child("arguments")
                    .is_some_and(|arguments| arguments.start == call.open)
        });
        let owner = match invocation.and_then(|invocation| invocation.child("object")) {
            Some(object) => super::type_of_receiver(snapshot, file, object)
                .and_then(|written| snapshot.of_type(&written, file))
                .or_else(|| snapshot.type_named(object.text.trim(), file)),
            None if invocation.is_some() => snapshot.enclosing(file, context.name.start),
            None => None,
        };
        let mut owners: Vec<Found<'_>> = owner
            .map(|owner| snapshot.with_supertypes(&owner))
            .unwrap_or_default();
        // Every class is an `Object`, written or not.
        if let Some(object) = snapshot.type_named("Object", file)
            && !owners.is_empty()
            && !owners.iter().any(|owner| owner.name.text == "Object")
        {
            owners.push(object);
        }
        for above in owners {
            for (member, named) in members_of(above.node) {
                if member.kind == "method_declaration" && named.text == call.name {
                    overloads.push(member);
                }
            }
        }
    }
    let index = call.index;
    overloads
        .into_iter()
        .filter_map(|member| {
            let parameters = parameters_of(member);
            let parameter = parameters.get(index).or_else(|| {
                parameters
                    .last()
                    .filter(|last| last.kind == "spread_parameter")
            })?;
            let mut written = parameter.child("type").map(type_of_node)?;
            if parameter.kind == "spread_parameter" && parameters.len() <= index {
                written.dimensions = written.dimensions.saturating_sub(1);
            }
            // A type parameter takes whatever it is bound to: anything.
            let generic = member
                .child("type_parameters")
                .is_some_and(|parameters| parameters.text.contains(&written.name));
            if generic || written.name.len() == 1 {
                written = Type {
                    name: "Object".to_owned(),
                    dimensions: written.dimensions,
                    ..Type::default()
                };
            }
            Some(written)
        })
        .collect()
}

/// The methods of the supertypes a member written here may override,
/// those the class does not declare already.
fn overridable(context: &Context<'_>) -> Vec<Offer> {
    let Some(owner) = context.snapshot.enclosing(context.file, context.name.start) else {
        return Vec::new();
    };
    let declared: Vec<&str> = members_of(owner.node)
        .into_iter()
        .map(|(_, named)| named.text.as_str())
        .collect();
    let mut offers: Vec<Offer> = Vec::new();
    for above in context.snapshot.with_supertypes(&owner).into_iter().skip(1) {
        for (member, named) in members_of(above.node) {
            if member.kind != "method_declaration" || declared.contains(&named.text.as_str()) {
                continue;
            }
            let modifiers = member
                .children
                .iter()
                .find(|child| child.kind == "modifiers")
                .map(|modifiers| modifiers.text.as_str())
                .unwrap_or_default();
            if ["private", "static", "final"]
                .iter()
                .any(|word| modifiers.split_whitespace().any(|written| written == *word))
            {
                continue;
            }
            if offers.iter().any(|offer| offer.name == named.text) {
                continue;
            }
            offers.push(Offer {
                name: named.text.clone(),
                rank: format!("{METHOD}{}", named.text),
            });
        }
    }
    offers
}

/// With a prefix typed, the static members of the types the file imports
/// static members of: `asList` beside `import static java.util.Arrays.sort`.
fn statically_imported(context: &Context<'_>) -> Vec<Offer> {
    if context.prefix.is_empty() {
        return Vec::new();
    }
    let mut offers: Vec<Offer> = Vec::new();
    for import in imports(context.file)
        .into_iter()
        .filter(|import| import.is_static)
    {
        let owner = if import.wildcard {
            import.name.clone()
        } else {
            import.qualifier().to_owned()
        };
        let Some(declaration) = context.snapshot.type_named(&owner, context.file) else {
            continue;
        };
        for (member, named, _) in own_members(context, &declaration, Wanted::Static) {
            if offers.iter().any(|offer| offer.name == named.text) {
                continue;
            }
            let group = if member.kind == "method_declaration" {
                METHOD
            } else {
                FIELD
            };
            offers.push(Offer {
                name: named.text.clone(),
                rank: format!("{group}{}", named.text),
            });
        }
    }
    offers
}

/// In an `import`: the packages and types below the dotted name written so
/// far, or the static members of the type a static import names. What the
/// file imports already is not offered again.
fn import_offers(context: &Context<'_>) -> Vec<Offer> {
    let Some(declaration) = enclosing_kind(&context.path, "import_declaration") else {
        return Vec::new();
    };
    let written = declaration.text.trim_start_matches("import").trim_start();
    let is_static = written.starts_with("static");
    let qualifier = context
        .dotted
        .and_then(|dotted| dotted.rsplit_once('.'))
        .map(|(qualifier, _)| qualifier.to_owned())
        .unwrap_or_default();
    let snapshot = context.snapshot;
    let mut offers = Vec::new();
    if qualifier.is_empty() {
        return super::packages(snapshot, context.prefix);
    }
    if let Some(owner) = snapshot
        .type_named(&qualifier, context.file)
        .filter(|found| found.file.uri != context.file.uri)
    {
        if is_static {
            for (_, named, _) in own_members(context, &owner, Wanted::Static) {
                offers.push(Offer {
                    name: named.text.clone(),
                    rank: format!("{FIELD}{}", named.text),
                });
            }
        }
        return offers;
    }
    offers.extend(
        super::packages(snapshot, &format!("{qualifier}.{}", context.prefix))
            .into_iter()
            .filter(|offer| offer.name != qualifier),
    );
    let imported: Vec<String> = imported_already(context);
    for name in package_types(snapshot, &qualifier) {
        if starts_with(&name, context.prefix) && !imported.contains(&name) {
            offers.push(Offer {
                rank: format!("{TYPE}{name}"),
                name,
            });
        }
    }
    offers
}

/// The simple names of the types the file as written imports by name,
/// the import being typed counted only if the cursor is not at its end.
fn imported_already(context: &Context<'_>) -> Vec<String> {
    let line = context.name.start.line;
    imports(context.written)
        .into_iter()
        .zip(context.written.tree.all("import_declaration"))
        .filter(|(import, declaration)| {
            !import.wildcard
                && !import.is_static
                && !(declaration.start.line <= line
                    && line <= declaration.end.line
                    && context
                        .written
                        .text
                        .lines()
                        .nth(line as usize)
                        .is_some_and(|text| {
                            let rest: String = text
                                .chars()
                                .skip(
                                    context.name.start.column as usize
                                        + context.prefix.chars().count(),
                                )
                                .collect();
                            !rest.starts_with(|character: char| character.is_alphanumeric())
                        }))
        })
        .map(|(import, _)| import.last_segment().to_owned())
        .collect()
}

/// The types a package declares, in the workspace and in the libraries.
fn package_types(snapshot: &Snapshot, package: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for file in &snapshot.files {
        if package_of(snapshot, file).as_deref() != Some(package) {
            continue;
        }
        for (name, declaration) in types_of(file) {
            let top_level = file
                .tree
                .children
                .iter()
                .any(|child| std::ptr::eq(child, declaration));
            if top_level && !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
    }
    if let Some(libraries) = snapshot.libraries() {
        for name in libraries.types_in_package(package) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}
