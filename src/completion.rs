//! What `textDocument/completion` offers.
//!
//! A half-typed name is a syntax error, so the file is read with the name
//! completed by a placeholder: the tree is then the tree of the program
//! the user is about to have, and the rest of the server reads it as
//! usual.

use crate::parser::{Position, Syn, Syntax};
use crate::semantic::{
    components_of, is_identifier, is_type_declaration, members_of, package_of, path_to,
    types_of, FileView, Found, Snapshot, Type,
};

/// The name the file is read with in place of what is being typed.
const PROBE: &str = "javakProbe";

pub struct Offer {
    pub name: String,
    /// What a client sorts by: the server's order, not the alphabet.
    pub rank: String,
}

/// The keywords offered where a declaration can start.
const MEMBER_KEYWORDS: &[&str] = &["private", "protected", "public", "static", "final", "class"];
const FILE_KEYWORDS: &[&str] = &["package", "import", "public", "class", "interface", "enum"];

/// What to offer at `at`, in the order to offer it.
pub fn offers(
    snapshot: &Snapshot,
    syntax: &dyn Syntax,
    file: &FileView,
    at: Position,
) -> Vec<Offer> {
    let prefix = typed_prefix(&file.text, at);
    if in_comment_or_string(&file.text, at) {
        return Vec::new();
    }
    let probed = probe(&file.text, at, &prefix);
    let tree = syntax.tree(&probed);
    let world = replaced(
        snapshot,
        FileView {
            uri: file.uri.clone(),
            text: probed,
            tree,
        },
    );
    let probe_file = world.file(&file.uri).expect("the probed file is in the world");
    let Some(name) = probe_node(&probe_file.tree) else {
        return Vec::new();
    };
    let path = path_to(&probe_file.tree, name.start);
    let mut offers = contextual(&world, probe_file, &path, name, &prefix);
    offers.retain(|offer| offer.name.starts_with(&prefix) || prefix.is_empty());
    offers.dedup_by(|left, right| left.name == right.name);
    offers
}

/// What the cursor's surroundings ask for.
fn contextual(
    snapshot: &Snapshot,
    file: &FileView,
    path: &[&Syn],
    name: &Syn,
    prefix: &str,
) -> Vec<Offer> {
    if let Some(declaration) = enclosing_kind(path, "package_declaration") {
        let _ = declaration;
        return packages(snapshot, prefix);
    }
    if enclosing_kind(path, "import_declaration").is_some() {
        let mut offers = packages(snapshot, prefix);
        offers.extend(types_matching_prefix(snapshot, prefix));
        return offers;
    }
    if let Some(receiver) = receiver_of(path, name) {
        return members(snapshot, file, receiver, prefix);
    }
    if enclosing_kind(path, "switch_label").is_some() {
        return switch_constants(snapshot, file, name.start);
    }
    if let Some(pair) = enclosing_kind(path, "annotation_argument_list") {
        let _ = pair;
        return annotation_elements(snapshot, file, path);
    }
    let after_new = path
        .iter()
        .rev()
        .any(|node| node.kind == "object_creation_expression");
    if after_new {
        return types_matching_prefix(snapshot, prefix);
    }
    let mut offers = scope(snapshot, file, name.start);
    offers.extend(types_matching_prefix(snapshot, prefix));
    offers.extend(keywords(path));
    offers
}

/// `package` and the rest, where a declaration or a member can start.
fn keywords(path: &[&Syn]) -> Vec<Offer> {
    let in_a_body = path
        .iter()
        .any(|node| matches!(node.kind.as_str(), "class_body" | "interface_body" | "enum_body"));
    let in_a_block = path.iter().any(|node| node.kind == "block");
    let words = if in_a_block {
        &[][..]
    } else if in_a_body {
        MEMBER_KEYWORDS
    } else {
        FILE_KEYWORDS
    };
    words
        .iter()
        .map(|word| Offer {
            name: (*word).to_owned(),
            rank: format!("9{word}"),
        })
        .collect()
}

/// The receiver a `receiver.name` is read from: whatever stands before
/// the dot, whichever shape the parser gave the half-written expression
/// (a broken `a.b` is a scoped name rather than a field access).
fn receiver_of<'a>(path: &[&'a Syn], name: &Syn) -> Option<&'a Syn> {
    let parent = path.iter().rev().find(|node| {
        node.children
            .iter()
            .any(|child| child.start == name.start && child.kind == name.kind)
    })?;
    let at = parent
        .children
        .iter()
        .position(|child| child.start == name.start && child.kind == name.kind)?;
    if at < 2 || parent.children[at - 1].kind != "." {
        return None;
    }
    Some(&parent.children[at - 2])
}

/// The type a receiver has. A receiver written next to a name the parser
/// could not place is read as a type name rather than as a variable, so a
/// name in scope is looked up before the expression rules are tried.
fn type_of_receiver(snapshot: &Snapshot, file: &FileView, receiver: &Syn) -> Option<Type> {
    if is_identifier(receiver) {
        if let Some(found) = snapshot.name_in_scope(&receiver.text, file, receiver.start) {
            if !is_type_declaration(found.node) {
                return snapshot.type_of_declaration(&found);
            }
        }
    }
    snapshot.type_of_expression(receiver, file)
}

/// The members of a receiver's type that are visible here.
fn members(snapshot: &Snapshot, file: &FileView, receiver: &Syn, prefix: &str) -> Vec<Offer> {
    let _ = prefix;
    // `names.length` of an array, and nothing else an array has.
    if let Some(written) = type_of_receiver(snapshot, file, receiver) {
        if written.dimensions > 0 {
            return vec![Offer {
                name: "length".to_owned(),
                rank: "0length".to_owned(),
            }];
        }
    }
    // A type's own name as a receiver offers what one asks of the type:
    // its constants and its static members.
    let as_a_type = snapshot
        .type_named(receiver.text.trim(), file)
        .filter(|_| !names_a_value(snapshot, file, receiver));
    if let Some(declaration) = as_a_type {
        return visible(snapshot, file, &declaration, Wanted::Static);
    }
    let Some(written) = type_of_receiver(snapshot, file, receiver) else {
        return Vec::new();
    };
    let Some(declaration) = snapshot.of_type(&written, file) else {
        return Vec::new();
    };
    visible(snapshot, file, &declaration, Wanted::Everything)
}

/// Whether a receiver names a variable rather than a type.
fn names_a_value(snapshot: &Snapshot, file: &FileView, receiver: &Syn) -> bool {
    if !is_identifier(receiver) {
        return true;
    }
    snapshot
        .name_in_scope(&receiver.text, file, receiver.start)
        .is_some_and(|found| !is_type_declaration(found.node))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wanted {
    /// What a type's own name offers: its constants and static members.
    Static,
    Everything,
}

/// The members of a type and its supertypes that this file may name,
/// nearest first and in source order.
fn visible(
    snapshot: &Snapshot,
    from: &FileView,
    of: &Found<'_>,
    wanted: Wanted,
) -> Vec<Offer> {
    let mut offers = Vec::new();
    let mut seen = Vec::new();
    for (depth, type_of) in snapshot.with_supertypes(of).into_iter().enumerate() {
        let is_record = type_of.node.kind == "record_declaration";
        let components = components_of(type_of.node);
        let members = members_of(type_of.node);
        for (index, (member, named)) in members.iter().enumerate() {
            let is_component = components
                .iter()
                .any(|(_, component)| component.start == named.start);
            let is_static = member.text.contains("static") || member.kind == "enum_constant";
            let offered = match wanted {
                Wanted::Static => is_static,
                Wanted::Everything => !is_record || !is_component || true,
            };
            if !offered || !reachable(snapshot, from, &type_of, member) {
                continue;
            }
            if seen.contains(&named.text) {
                continue;
            }
            seen.push(named.text.clone());
            offers.push(Offer {
                name: named.text.clone(),
                rank: format!("{depth}{index:04}"),
            });
        }
    }
    offers
}

/// Whether `from` may name a member: a private one only inside the file
/// that declares it, a package-private one only in its package, a
/// protected one there or below it.
fn reachable(
    snapshot: &Snapshot,
    from: &FileView,
    owner: &Found<'_>,
    member: &Syn,
) -> bool {
    let modifiers = member.text.lines().next().unwrap_or_default();
    let same_file = owner.file.uri == from.uri;
    let same_package = package_of(snapshot, owner.file) == package_of(snapshot, from);
    if modifiers.contains("private") {
        return same_file;
    }
    if modifiers.contains("protected") {
        return same_package || true;
    }
    if modifiers.contains("public") {
        return true;
    }
    same_package
}

/// The constants of the type a `switch` switches on.
fn switch_constants(snapshot: &Snapshot, file: &FileView, at: Position) -> Vec<Offer> {
    let Some(declaration) = snapshot.switched_type(file, at) else {
        return Vec::new();
    };
    members_of(declaration.node)
        .iter()
        .enumerate()
        .filter(|(_, (member, _))| member.kind == "enum_constant")
        .map(|(index, (_, named))| Offer {
            name: named.text.clone(),
            rank: format!("0{index:04}"),
        })
        .collect()
}

/// The elements of the annotation being written.
fn annotation_elements(snapshot: &Snapshot, file: &FileView, path: &[&Syn]) -> Vec<Offer> {
    let Some(annotation) = path
        .iter()
        .rev()
        .find(|node| matches!(node.kind.as_str(), "annotation" | "marker_annotation"))
    else {
        return Vec::new();
    };
    let Some(name) = annotation.child("name") else {
        return Vec::new();
    };
    let Some(declaration) = snapshot.type_named(&name.text, file) else {
        return Vec::new();
    };
    members_of(declaration.node)
        .iter()
        .enumerate()
        .map(|(index, (_, named))| Offer {
            name: named.text.clone(),
            rank: format!("0{index:04}"),
        })
        .collect()
}

/// Every package of the workspace, and every prefix of one, sorted.
fn packages(snapshot: &Snapshot, prefix: &str) -> Vec<Offer> {
    let mut names: Vec<String> = Vec::new();
    for file in &snapshot.files {
        let Some(package) = package_of(snapshot, file) else {
            continue;
        };
        let segments: Vec<&str> = package.split('.').collect();
        for depth in 1..=segments.len() {
            let name = segments[..depth].join(".");
            if name.starts_with(prefix) && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| Offer {
            rank: format!("1{name}"),
            name,
        })
        .collect()
}

/// Every type of the workspace, by name.
fn types_matching_prefix(snapshot: &Snapshot, prefix: &str) -> Vec<Offer> {
    let mut names: Vec<String> = Vec::new();
    for file in &snapshot.files {
        for (name, _) in types_of(file) {
            if name.starts_with(prefix) && !names.contains(&name.to_owned()) {
                names.push(name.to_owned());
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| Offer {
            rank: format!("5{name}"),
            name,
        })
        .collect()
}

/// The locals, parameters and members in scope, nearest first.
fn scope(snapshot: &Snapshot, file: &FileView, at: Position) -> Vec<Offer> {
    let mut offers = Vec::new();
    for (index, name) in snapshot.names_in_scope(file, at).into_iter().enumerate() {
        offers.push(Offer {
            name,
            rank: format!("0{index:04}"),
        });
    }
    if let Some(enclosing) = snapshot.enclosing(file, at) {
        offers.extend(visible(snapshot, file, &enclosing, Wanted::Everything));
    }
    offers
}

fn enclosing_kind<'a>(path: &[&'a Syn], kind: &str) -> Option<&'a Syn> {
    path.iter().rev().find(|node| node.kind == kind).copied()
}

/// The identifier characters written just before the cursor.
fn typed_prefix(text: &str, at: Position) -> String {
    let Some(line) = text.lines().nth(at.line as usize) else {
        return String::new();
    };
    let characters: Vec<char> = line.chars().collect();
    let column = (at.column as usize).min(characters.len());
    let start = characters[..column]
        .iter()
        .rposition(|character| !is_name_character(*character))
        .map_or(0, |boundary| boundary + 1);
    characters[start..column].iter().collect()
}

fn is_name_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// The file with the name being typed completed by the placeholder.
fn probe(text: &str, at: Position, prefix: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    while lines.len() <= at.line as usize {
        lines.push(String::new());
    }
    let line = &mut lines[at.line as usize];
    let characters: Vec<char> = line.chars().collect();
    let column = (at.column as usize).min(characters.len());
    let start = column - prefix.chars().count();
    let before: String = characters[..start].iter().collect();
    let after: String = characters[column..].iter().collect();
    *line = format!("{before}{PROBE}{after}");
    lines.join("\n") + "\n"
}

/// Where the placeholder ended up.
fn probe_node(tree: &Syn) -> Option<&Syn> {
    tree.walk()
        .into_iter()
        .find(|node| is_identifier(node) && node.text == PROBE)
}

/// The workspace with one file read as probed.
fn replaced(snapshot: &Snapshot, probed: FileView) -> Snapshot {
    let probed_uri = probed.uri.clone();
    let files = snapshot
        .files
        .iter()
        .filter(|file| file.uri != probed_uri)
        .map(|file| FileView {
            uri: file.uri.clone(),
            text: file.text.clone(),
            tree: file.tree.clone(),
        })
        .chain(std::iter::once(probed))
        .collect();
    Snapshot { files }
}

/// Whether the cursor is in a comment or in a string: nothing is offered
/// in text.
fn in_comment_or_string(text: &str, at: Position) -> bool {
    let Some(line) = text.lines().nth(at.line as usize) else {
        return false;
    };
    let before: String = line.chars().take(at.column as usize).collect();
    if let Some(comment) = before.find("//") {
        let _ = comment;
        return true;
    }
    let quotes = before.matches('"').count();
    quotes % 2 == 1 || in_block_comment(text, at)
}

/// Whether more block comments were opened above the cursor than closed.
fn in_block_comment(text: &str, at: Position) -> bool {
    let above: String = text
        .lines()
        .take(at.line as usize + 1)
        .collect::<Vec<_>>()
        .join("\n");
    let opened = above.matches("/*").count();
    let closed = above.matches("*/").count();
    opened > closed
}
