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
const PACKAGE: char = '6';
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

/// What a statement the parser could not finish wants of the expression
/// at `receiver`, read from the text before it: the declared type of
/// `int value = |`, the return type of `return |`.
fn expected_from_text(file: &FileView, path: &[&Syn], receiver: &Syn) -> Option<String> {
    let line = file.text.lines().nth(receiver.start.line as usize)?;
    let before: String = line.chars().take(receiver.start.column as usize).collect();
    let before = before.trim();
    if before == "return" {
        let method = path.iter().rev().find(|node| node.kind == "method_declaration")?;
        return method
            .child("type")
            .map(|written| crate::feature::semantic::type_of_node(written).written());
    }
    let declared = before.strip_suffix('=')?.trim_end();
    let (written, name) = declared.rsplit_once(char::is_whitespace)?;
    let is_name = name.chars().all(|character| character.is_alphanumeric() || character == '_');
    let written = written.trim();
    let written = written.rsplit(char::is_whitespace).next().unwrap_or(written);
    let is_type = !written.is_empty()
        && written
            .chars()
            .all(|character| character.is_alphanumeric() || "_$.<>[], ?".contains(character));
    (is_name && is_type && written != "return").then(|| written.to_owned())
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
        // `shape.ar|(…)`: a call, and jdtls offers only methods for it —
        // once a name is begun; at its start, anything.
        let calling = !prefix.is_empty() && path.iter().rev().any(|node| {
            node.kind == "method_invocation"
                && node.child("name").is_some_and(|called| called.start == name.start)
        });
        // What the place wants (`int value = Units.|`) ranks first.
        let expected = path
            .iter()
            .rev()
            .find(|node| node.children.iter().any(|child| child.start == name.start && child.kind == name.kind))
            .and_then(|access| snapshot.expected_type_of(access, file))
            .map(|expected| expected.written())
            .or_else(|| expected_from_text(file, path, receiver));
        let offers = members(snapshot, file, receiver, prefix, calling, expected.as_deref());
        // `new com.demo.data.ArrayL|`: the constructors of the types.
        let start = receiver_chain_start(path, receiver);
        if creates_an_object(context) || written_after_new(context, start) {
            return creations(context, offers, Some(start));
        }
        // `java.util.List|`: the type, written as qualified as it is.
        let mut offers = offers;
        for offer in &mut offers {
            if offer.kind == super::OfferKind::Type
                && let Some(qualified) = &offer.qualified
                && enclosing_kind(path, "import_declaration").is_none()
            {
                offer.shown.insert = Some(qualified.clone());
                offer.shown.replaces = Some(start);
            }
        }
        return offers;
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
            offers.push(expected_offer(context, expected));
        }
        return creations(context, offers, None);
    }
    if starts_a_member(path, name) || starts_a_recovered_member(path, name) {
        let mut offers = types(context);
        offers.extend(overridable(context));
        // In a type's body jdtls writes a keyword with the space after it.
        offers.extend(keywords(MEMBER_KEYWORDS).into_iter().map(|mut offer| {
            offer.shown.insert = Some(format!("{} ", offer.name));
            offer
        }));
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
        offers.push(expected_offer(context, expected));
    }
    // A name begun may be a package's: `foo|` offers `foo`.
    if !prefix.is_empty() && !only_values {
        offers.extend(super::packages(snapshot, prefix).into_iter().map(|mut offer| {
            offer.rank = format!("{PACKAGE}{}", offer.name);
            offer.shown = super::shown::package(&offer.name);
            offer
        }));
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
            kind: super::OfferKind::Keyword,
            ..Offer::default()
        })
        .collect()
}

/// The type the place wants, as jdtls offers it first: with its type
/// arguments (`List<java.lang.String>`).
fn expected_offer(context: &Context<'_>, expected: &Type) -> Offer {
    let qualified = context
        .snapshot
        .of_type(expected, context.file)
        .filter(|found| is_type_declaration(found.node))
        .map(|found| context.snapshot.qualified_type_name(found.file, found.node));
    let with_arguments = !expected.arguments.is_empty();
    Offer {
        name: expected.name.clone(),
        rank: format!("{EXPECTED}{}", expected.name),
        kind: super::OfferKind::Type,
        shown: super::Shown {
            label: with_arguments.then(|| super::shown::jdt_type(context.snapshot, context.file, expected)),
            detail: with_arguments.then(|| super::shown::qualified(context.snapshot, context.file, expected)),
            ..super::Shown::default()
        },
        qualified,
        ..Offer::default()
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

/// Whether the name is the type of a `new` expression.
fn creates_an_object(context: &Context<'_>) -> bool {
    context.path.iter().any(|node| {
        node.kind == "object_creation_expression"
            && node.child("type").is_some_and(|written| written.covers(context.name.start))
    })
}

/// Whether `new` is written right before `start`.
fn written_after_new(context: &Context<'_>, start: Position) -> bool {
    let Some(offset) = offset_of(&context.written.text, start) else {
        return false;
    };
    let before = context.written.text[..offset].trim_end();
    before.ends_with("new")
        && !before[..before.len() - 3]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
}

/// Where the qualified name the receiver is part of starts.
fn receiver_chain_start(path: &[&Syn], receiver: &Syn) -> Position {
    path.iter()
        .filter(|node| matches!(node.kind.as_str(), "scoped_type_identifier" | "scoped_identifier" | "field_access"))
        .find(|node| node.covers(receiver.start))
        .map_or(receiver.start, |node| node.start)
}

/// What `new` offers of the types `offers` names, as jdtls does: a class's
/// constructors, each its own proposal (the default one where it declares
/// none); an interface's or abstract class's anonymous class. With no
/// prefix the types themselves stay beside them. `qualified_from` is where
/// a qualified name being written starts: the constructor writes it whole.
fn creations(context: &Context<'_>, offers: Vec<Offer>, qualified_from: Option<Position>) -> Vec<Offer> {
    let snapshot = context.snapshot;
    let file = context.file;
    let mut created = Vec::new();
    for offer in offers {
        let Some(found) = offer
            .qualified
            .as_deref()
            .and_then(|qualified| snapshot.type_named(qualified, file))
            .filter(|found| is_type_declaration(found.node))
        else {
            created.push(offer);
            continue;
        };
        let qualified = snapshot.qualified_type_name(found.file, found.node);
        let abstract_type = matches!(found.node.kind.as_str(), "interface_declaration" | "annotation_type_declaration")
            || found
                .node
                .first("modifiers")
                .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "abstract"));
        if found.node.kind == "enum_declaration" {
            created.push(offer);
            continue;
        }
        // A type the file cannot name by its simple name (another type of
        // that name is imported) is written qualified.
        let hidden = imports(context.written).into_iter().any(|import| {
            !import.wildcard && !import.is_static && import.last_segment() == found.name.text && import.name != qualified
        });
        let written_name = if hidden || qualified_from.is_some() {
            qualified.clone()
        } else {
            found.name.text.clone()
        };
        if abstract_type {
            let package = snapshot.package(found.file).filter(|package| !package.is_empty());
            let nested = package
                .as_deref()
                .and_then(|package| qualified.strip_prefix(&format!("{package}.")))
                .unwrap_or(&qualified)
                .to_owned();
            let end = if ends_the_statement(context) { ";" } else { "" };
            created.push(Offer {
                name: found.name.text.clone(),
                rank: format!("{EXPECTED}{}", found.name.text),
                kind: super::OfferKind::Type,
                constructs: true,
                shown: super::Shown {
                    kind: Some(super::ItemKind::Constructor),
                    label: Some(nested),
                    description: Some("Anonymous Inner Type".to_owned()),
                    detail: (!context.prefix.is_empty()).then(|| qualified.clone()),
                    insert: Some(format!("{written_name}() {{\n\t${{0}}\n}}{end}")),
                    snippet: true,
                    replaces: qualified_from,
                    ..super::Shown::default()
                },
                qualified: offer.qualified.clone(),
                declarations: vec![super::Proposed { parameters: Some(Vec::new()), ..super::Proposed::default() }],
                ..Offer::default()
            });
        } else {
            let constructors: Vec<Found<'_>> = members_of(found.node)
                .into_iter()
                .filter(|(member, _)| member.kind == "constructor_declaration")
                .map(|(member, named)| Found { file: found.file, node: member, name: named, bindings: found.bindings.clone() })
                .collect();
            let declarations: Vec<super::Proposed> = if constructors.is_empty() {
                vec![super::Proposed {
                    parameters: Some(Vec::new()),
                    shown: constructor_shown(&qualified, &found.name.text, &[], &written_name, &[], qualified_from),
                    ..super::Proposed::default()
                }]
            } else {
                let mut constructors = constructors;
                // jdtls lists them by what their label writes.
                constructors.sort_by_cached_key(|constructor| snapshot.parameters_written(constructor).join(", "));
                constructors
                    .iter()
                    .enumerate()
                    .map(|(index, constructor)| {
                        let written = snapshot.parameters_written(constructor);
                        let names: Vec<String> = crate::feature::semantic::parameters(constructor.node, &constructor.bindings)
                            .into_iter()
                            .map(|parameter| parameter.name)
                            .collect();
                        super::Proposed {
                            shown: constructor_shown(&qualified, &found.name.text, &written, &written_name, &names, qualified_from),
                            parameters: Some(written),
                            names,
                            deprecated: super::shown::deprecated(constructor.node),
                            index,
                            ..super::Proposed::default()
                        }
                    })
                    .collect()
            };
            created.push(Offer {
                name: found.name.text.clone(),
                rank: offer.rank.clone(),
                kind: super::OfferKind::Type,
                constructs: true,
                qualified: offer.qualified.clone(),
                declarations,
                ..Offer::default()
            });
        }
        // A class stays offered as a type beside its constructors.
        if context.prefix.is_empty() && qualified_from.is_none() && !abstract_type {
            created.push(offer);
        }
    }
    created
}

/// A constructor as jdtls shows it after `new`: `Name(String text)`, its
/// qualified name as detail, the call it writes.
fn constructor_shown(
    qualified: &str,
    name: &str,
    written: &[String],
    written_name: &str,
    names: &[String],
    replaces: Option<Position>,
) -> super::Shown {
    let arguments: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(index, name)| format!("${{{}:{name}}}", index + 1))
        .collect();
    super::Shown {
        kind: Some(super::ItemKind::Constructor),
        detail: Some(format!("{qualified}.{name}({})", written.join(", "))),
        insert: Some(format!("{written_name}({})", arguments.join(", "))),
        snippet: !arguments.is_empty(),
        replaces,
        ..super::Shown::default()
    }
}

/// Whether nothing but `;` (or the end of the line) follows the cursor,
/// in a declaration or a statement: what is written there ends it.
fn ends_the_statement(context: &Context<'_>) -> bool {
    let line = context.written.text.lines().nth(context.name.start.line as usize).unwrap_or_default();
    let rest: String = line.chars().skip(context.name.start.column as usize + context.prefix.chars().count()).collect();
    let rest = rest.trim();
    rest.is_empty() || rest == ";"
}

/// The types a name may stand for: with no prefix, the file's own; with
/// one, every type of the workspace and its libraries starting with it.
fn types(context: &Context<'_>) -> Vec<Offer> {
    // Each name with the qualified name of the type it is, where known:
    // types of one name in different packages are offered each.
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    let mut add = |name: &str, qualified: Option<String>| {
        let known = names.iter().any(|(known, known_qualified)| {
            known == name && (known_qualified.is_none() || qualified.is_none() || *known_qualified == qualified)
        });
        if !known {
            names.push((name.to_owned(), qualified));
        }
    };
    let snapshot = context.snapshot;
    // Ranked as jdtls lists them, by label: an inner class of a generic
    // type the cursor is in is `Shapes<T>.Inner`, beside `Shapes<T>`.
    let mut ranks: Vec<(String, String)> = Vec::new();
    let mut parameters: Vec<String> = Vec::new();
    if context.prefix.is_empty() {
        for (name, declaration) in types_of(context.file) {
            add(name, Some(snapshot.qualified_type_name(context.file, declaration)));
            if let Some(outer) = generic_outer(context, declaration) {
                ranks.push((name.to_owned(), format!("{outer}.{name}")));
            }
        }
        for name in recovered_types(&context.file.tree) {
            add(&name, None);
        }
        // The type parameters in scope are types too.
        for node in &context.path {
            let Some(declared) = node.child("type_parameters") else { continue };
            for parameter in declared.all("type_parameter") {
                if let Some(named) = parameter.children.iter().find(|child| is_identifier(child))
                    && !parameters.contains(&named.text)
                {
                    parameters.push(named.text.clone());
                }
            }
        }
    } else {
        for file in &snapshot.files {
            for (name, declaration) in types_of(file) {
                if starts_with(name, context.prefix) {
                    add(name, Some(snapshot.qualified_type_name(file, declaration)));
                }
            }
        }
        if let Some(libraries) = snapshot.libraries() {
            for (name, packages) in libraries.types_starting_in_packages(context.prefix) {
                for package in packages {
                    let qualified = if package.is_empty() { name.clone() } else { format!("{package}.{name}") };
                    add(&name, Some(qualified));
                }
            }
        }
    }
    let mut offers: Vec<Offer> = names
        .into_iter()
        .map(|(name, qualified)| {
            let ranked = ranks.iter().find(|(known, _)| *known == name).map_or(name.clone(), |(_, rank)| rank.clone());
            Offer {
                rank: format!("{TYPE}{ranked}"),
                name,
                kind: super::OfferKind::Type,
                qualified,
                ..Offer::default()
            }
        })
        .collect();
    offers.extend(parameters.into_iter().map(|name| Offer {
        rank: format!("{TYPE}{name}"),
        kind: super::OfferKind::Type,
        shown: super::Shown {
            kind: Some(super::ItemKind::Class),
            detail: Some(name.clone()),
            ..super::Shown::default()
        },
        name,
        ..Offer::default()
    }));
    offers
}

/// The name of the generic class around an inner class `declaration`
/// the cursor is in, as jdtls labels the inner one by it.
fn generic_outer(context: &Context<'_>, declaration: &Syn) -> Option<String> {
    if declaration.kind != "class_declaration"
        || declaration.first("modifiers").is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "static"))
    {
        return None;
    }
    let outer = path_to(&context.file.tree, declaration.start)
        .into_iter()
        .rev()
        .find(|node| node.start < declaration.start && is_type_declaration(node))?;
    let at = context.name.start;
    (outer.kind == "class_declaration" && outer.child("type_parameters").is_some() && outer.start <= at && at <= outer.end)
        .then(|| outer.child("name").map(|named| named.text.clone()))
        .flatten()
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
    let mut locals: Vec<(String, Option<String>)> =
        snapshot.names_in_scope(file, at).into_iter().map(|name| (name, None)).collect();
    locals.extend(recovered_locals(context).into_iter().map(|(name, written)| (name, Some(written))));
    for (local, recovered_type) in locals {
        if declaring.as_deref() == Some(local.as_str()) || seen.contains(&local) {
            continue;
        }
        seen.push(local.clone());
        let found = snapshot.name_in_scope(&local, file, at);
        let shown = found.as_ref().map_or_else(
            || super::Shown {
                kind: Some(super::ItemKind::Variable),
                description: recovered_type.clone(),
                ..super::Shown::default()
            },
            |found| super::shown::variable(snapshot, file, found),
        );
        values.push(Value {
            found,
            offer: Offer {
                rank: format!("{LOCAL}{local}"),
                name: local,
                kind: super::OfferKind::Local,
                shown,
                ..Offer::default()
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
            let found = Found {
                file: owner.file,
                node: member,
                name: named,
                bindings: owner.bindings.clone(),
            };
            values.push(Value {
                offer: member_offer(context, &owner, &found, if is_method { METHOD } else { FIELD }),
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

/// A member of `owner` offered by name, every overload of a method its
/// own proposal, as jdtls offers it.
fn member_offer(context: &Context<'_>, owner: &Found<'_>, found: &Found<'_>, group: char) -> Offer {
    let snapshot = context.snapshot;
    let named = &found.name.text;
    let is_method = found.node.kind == "method_declaration";
    let declarations = if is_method {
        let mut overloads: Vec<super::Proposed> = Vec::new();
        let mut signatures: Vec<Vec<String>> = Vec::new();
        for (index, (member, other)) in members_of(owner.node).into_iter().enumerate() {
            if member.kind != "method_declaration" || other.text != *named {
                continue;
            }
            let overload = Found { file: owner.file, node: member, name: other, bindings: owner.bindings.clone() };
            let signature = snapshot.erased_parameter_types(&overload);
            if signatures.contains(&signature) {
                continue;
            }
            signatures.push(signature);
            overloads.push(proposed_method(context, owner, &overload, index));
        }
        overloads
    } else {
        vec![super::Proposed {
            shown: super::shown::field(snapshot, context.file, owner, found),
            deprecated: super::shown::deprecated(found.node),
            ..super::Proposed::default()
        }]
    };
    Offer {
        name: named.clone(),
        rank: format!("{group}{named}"),
        kind: if is_method { super::OfferKind::Method } else { super::OfferKind::Field },
        declarations,
        ..Offer::default()
    }
}

/// One overload of a method, as a proposal shows it.
fn proposed_method(context: &Context<'_>, owner: &Found<'_>, found: &Found<'_>, index: usize) -> super::Proposed {
    let written = context.snapshot.parameters_written(found);
    let (shown, names) = super::shown::method(context.snapshot, owner, found, &written);
    super::Proposed {
        returns: context.snapshot.return_written(found),
        parameters: Some(written),
        names,
        shown,
        deprecated: super::shown::deprecated(found.node),
        index,
        ..super::Proposed::default()
    }
}

/// Locals the parser could not read as declarations: `String s` before
/// the placeholder in a statement it gave up on.
fn recovered_locals(context: &Context<'_>) -> Vec<(String, String)> {
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
                names.push((pair[1].text.clone(), pair[0].text.clone()));
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
    // The call itself, each overload, as jdtls shows what it is filling
    // in: it writes nothing.
    let declarations: Vec<super::Proposed> = call_overloads(context, &call)
        .iter()
        .enumerate()
        .map(|(index, (owner, member))| {
            let mut proposed = proposed_method(context, owner, member, index);
            if call.creates {
                proposed.shown.detail = Some(format!(
                    "{}.{}({})",
                    owner.name.text,
                    member.name.text,
                    proposed.parameters.as_deref().unwrap_or_default().join(", ")
                ));
                proposed.shown.description = Some("void".to_owned());
            }
            proposed.shown.insert = Some(String::new());
            proposed
        })
        .collect();
    let mut offers = vec![Offer {
        rank: format!("{EXPECTED}{}", call.name),
        name: call.name.clone(),
        kind: super::OfferKind::Method,
        declarations,
        ..Offer::default()
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

pub(super) fn offset_of(text: &str, at: Position) -> Option<usize> {
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
    let overloads: Vec<&Syn> = call_overloads(context, call)
        .into_iter()
        .map(|(_, member)| member.node)
        .collect();
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

/// The constructors or methods a call may be calling, each with the type
/// declaring it, nearest type first.
fn call_overloads<'a>(context: &Context<'a>, call: &OpenCall) -> Vec<(Found<'a>, Found<'a>)> {
    let snapshot = context.snapshot;
    let file = context.file;
    let mut overloads: Vec<(Found<'a>, Found<'a>)> = Vec::new();
    if call.creates {
        if let Some(owner) = snapshot.type_named(&call.name, file) {
            for (member, named) in members_of(owner.node) {
                if member.kind == "constructor_declaration" {
                    let constructor = Found { file: owner.file, node: member, name: named, bindings: owner.bindings.clone() };
                    overloads.push((owner.clone(), constructor));
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
        let mut signatures: Vec<Vec<String>> = Vec::new();
        for above in owners {
            for (member, named) in members_of(above.node) {
                if member.kind == "method_declaration" && named.text == call.name {
                    let method = Found { file: above.file, node: member, name: named, bindings: above.bindings.clone() };
                    // An override is the method it overrides: offered once.
                    let signature = snapshot.erased_parameter_types(&method);
                    if signatures.contains(&signature) {
                        continue;
                    }
                    signatures.push(signature);
                    overloads.push((above.clone(), method));
                }
            }
        }
    }
    overloads
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
                ..Offer::default()
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
        let qualified = context.snapshot.qualified_type_name(declaration.file, declaration.node);
        for (member, named, owner) in own_members(context, &declaration, Wanted::Static) {
            if offers.iter().any(|offer| offer.name == named.text) {
                continue;
            }
            let group = if member.kind == "method_declaration" {
                METHOD
            } else {
                FIELD
            };
            let found = Found { file: owner.file, node: member, name: named, bindings: owner.bindings.clone() };
            let mut offer = member_offer(context, &owner, &found, group);
            // Named as jdtls names a member of a type the file imports
            // members of: qualified, its import added unless one covers it.
            let covered = imports(context.file).into_iter().any(|import| {
                import.is_static && (import.wildcard || import.last_segment() == named.text)
            });
            for proposed in &mut offer.declarations {
                if let Some(detail) = &proposed.shown.detail {
                    proposed.shown.detail = Some(format!("{qualified}{}", &detail[owner.name.text.len()..]));
                }
                if !covered {
                    proposed.shown.import = Some(format!("import static {qualified}.{};\n", named.text));
                }
            }
            offers.push(offer);
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
                    ..Offer::default()
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
                qualified: Some(format!("{qualifier}.{name}")),
                name,
                kind: super::OfferKind::Type,
                ..Offer::default()
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
