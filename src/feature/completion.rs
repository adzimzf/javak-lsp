//! What `textDocument/completion` offers.
//!
//! A half-typed name is a syntax error, so the file is read with the name
//! completed by a placeholder: the tree is then the tree of the program
//! the user is about to have, and the rest of the server reads it as
//! usual.

use std::sync::Arc;

use crate::parser::{syntax, Position, Syn};
use crate::feature::features;
use crate::feature::semantic::{
    type_of_node, is_identifier, is_type_declaration, members_of, package_of, path_to,
    types_of, FileView, Found, Snapshot, Type,
};

/// The name the file is read with in place of what is being typed.
const PROBE: &str = "javakProbe";

pub struct Offer {
    pub name: String,
    /// What a client sorts by: the server's order, not the alphabet.
    pub rank: String,
}

/// What to offer at `at`, in the order to offer it.
pub fn offers(
    snapshot: &Snapshot,
    file: &FileView,
    at: Position,
    implied_package: Option<&str>,
) -> Vec<Offer> {
    let prefix = typed_prefix(&file.text, at);
    if in_comment_or_string(&file.text, at) {
        return Vec::new();
    }
    let probed = probe(&file.text, at, &prefix);
    let tree = syntax(file.language).tree(&probed);
    let world = replaced(
        snapshot,
        FileView {
            uri: file.uri.clone(),
            language: file.language,
            text: Arc::new(probed),
            tree: Arc::new(tree),
            library: file.library,
        },
    );
    let probe_file = world.file(&file.uri).expect("the probed file is in the world");
    let Some(name) = probe_node(&probe_file.tree) else {
        return Vec::new();
    };
    let path = path_to(&probe_file.tree, name.start);
    let dotted = dotted_prefix(&path, &prefix);
    let mut offers = if enclosing_kind(&path, "package_declaration").is_some() {
        let mut offers = packages(&world, dotted.as_deref().unwrap_or(&prefix));
        let implied = implied_package
            .filter(|implied| implied.starts_with(dotted.as_deref().unwrap_or(&prefix)));
        if let Some(implied) = implied.filter(|implied| !offers.iter().any(|offer| offer.name == *implied)) {
            offers.insert(
                0,
                Offer {
                    name: implied.to_owned(),
                    rank: format!("0{implied}"),
                },
            );
        }
        offers
    } else {
        contextual(&world, probe_file, &path, name, &prefix, dotted.as_deref())
    };
    offers.retain(|offer| {
        prefix.is_empty()
            || offer.name.starts_with(&prefix)
            || dotted.as_deref().is_some_and(|dotted| offer.name.starts_with(dotted))
    });
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
    dotted: Option<&str>,
) -> Vec<Offer> {
    if enclosing_kind(path, "import_declaration").is_some() {
        let mut offers = packages(snapshot, dotted.unwrap_or(prefix));
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
    // Where a member starts, a name is a type or a modifier: nothing in
    // scope is read there.
    if starts_a_member(path, name) {
        let mut offers = types_matching_prefix(snapshot, prefix);
        offers.extend(keywords(file, path));
        return offers;
    }
    let mut offers = scope(snapshot, file, name.start);
    offers.extend(
        snapshot
            .function_names(prefix, file)
            .into_iter()
            .map(|name| Offer {
                rank: format!("2{name}"),
                name,
            }),
    );
    offers.extend(types_matching_prefix(snapshot, prefix));
    offers.extend(keywords(file, path));
    offers
}

/// In a `package` or `import` declaration, the dotted name written up to
/// the cursor: `com.dem` of `import com.dem|`.
fn dotted_prefix(path: &[&Syn], prefix: &str) -> Option<String> {
    let declaration = path.iter().position(|node| {
        matches!(node.kind.as_str(), "package_declaration" | "import_declaration")
    })?;
    let dotted = path[declaration + 1..]
        .iter()
        .find(|node| node.kind == "scoped_identifier")?;
    let before = dotted.text.split(PROBE).next()?;
    Some(format!("{before}{prefix}"))
}

/// Whether the name stands alone in a type's body, where a member is
/// being declared.
fn starts_a_member(path: &[&Syn], name: &Syn) -> bool {
    let is_body = |node: &Syn| {
        matches!(
            node.kind.as_str(),
            "class_body" | "interface_body" | "enum_body"
        )
    };
    let Some(at) = path
        .iter()
        .rposition(|node| node.start == name.start && node.kind == name.kind)
    else {
        return false;
    };
    match at.checked_sub(1).map(|parent| path[parent]) {
        Some(parent) if is_body(parent) => true,
        Some(parent) if parent.kind == "ERROR" && parent.children.len() == 1 => at
            .checked_sub(2)
            .is_some_and(|grandparent| is_body(path[grandparent])),
        _ => false,
    }
}

/// `package` and the rest, where a declaration or a member can start.
fn keywords(file: &FileView, path: &[&Syn]) -> Vec<Offer> {
    let language = features(file.language);
    let in_a_body = path
        .iter()
        .any(|node| matches!(node.kind.as_str(), "class_body" | "interface_body" | "enum_body"));
    let in_a_block = path.iter().any(|node| node.kind == "block");
    let words = if in_a_block {
        &[][..]
    } else if in_a_body {
        language.member_keywords()
    } else {
        language.file_keywords()
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
/// name in scope is looked up before the expression rules are tried; a
/// dotted name is a member of the type its qualifier names.
fn type_of_receiver(snapshot: &Snapshot, file: &FileView, receiver: &Syn) -> Option<Type> {
    if is_identifier(receiver) {
        if let Some(found) = snapshot.name_in_scope(&receiver.text, file, receiver.start) {
            if !is_type_declaration(found.node) {
                return snapshot.type_of_declaration(&found);
            }
        }
    }
    if is_dotted_name(receiver) {
        let (qualifier, last) = receiver.text.rsplit_once('.')?;
        let owner = snapshot.type_named(qualifier.trim(), file)?;
        let member = snapshot.member(&owner, last.trim(), None)?;
        return snapshot.type_of_declaration(&member);
    }
    snapshot.type_of_expression(receiver, file)
}

/// `a.b.c` the parser read as a name rather than as an expression.
fn is_dotted_name(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "scoped_type_identifier" | "scoped_identifier")
}

/// The members of a receiver's type that are visible here.
fn members(snapshot: &Snapshot, file: &FileView, receiver: &Syn, prefix: &str) -> Vec<Offer> {
    // `names.length` of an array, and nothing else an array has.
    if let Some(written) = type_of_receiver(snapshot, file, receiver) {
        if written.dimensions > 0 {
            return vec![Offer {
                name: "length".to_owned(),
                rank: "0length".to_owned(),
            }];
        }
    }
    let is_value = names_a_value(snapshot, file, receiver);
    // A type's own name as a receiver offers what one asks of the type:
    // its constants and its static members.
    let as_a_type = snapshot
        .type_named(receiver.text.trim(), file)
        .filter(|_| !is_value);
    if let Some(declaration) = as_a_type {
        return visible(snapshot, file, &declaration, Wanted::Static);
    }
    // A package as a receiver offers the types it holds.
    if !is_value && is_package(snapshot, receiver.text.trim()) {
        return types_in_package(snapshot, receiver.text.trim(), prefix);
    }
    let Some(written) = type_of_receiver(snapshot, file, receiver) else {
        return Vec::new();
    };
    let declaration = snapshot
        .of_type(&written, file)
        .or_else(|| bound_of(snapshot, file, receiver, &written));
    let mut offers = declaration
        .map(|declaration| visible(snapshot, file, &declaration, Wanted::Instance))
        .unwrap_or_default();
    let depth = offers.len();
    for (index, name) in snapshot
        .extension_names(&written, prefix, file)
        .into_iter()
        .enumerate()
    {
        if !offers.iter().any(|offer| offer.name == name) {
            let rank = features(file.language).member_rank(depth + 1, index, &name);
            offers.push(Offer { name, rank });
        }
    }
    offers
}

/// The bound a type parameter is declared with: `T extends Animal` is
/// an `Animal` as far as its members go.
fn bound_of<'a>(
    snapshot: &'a Snapshot,
    file: &'a FileView,
    receiver: &Syn,
    written: &Type,
) -> Option<Found<'a>> {
    let parameter = snapshot.name_in_scope(&written.name, file, receiver.start)?;
    if parameter.node.kind != "type_parameter" {
        return None;
    }
    let bound = parameter.node.first("type_bound")?;
    let first = bound.children.iter().find(|child| {
        matches!(
            child.kind.as_str(),
            "type_identifier" | "generic_type" | "scoped_type_identifier"
        )
    })?;
    snapshot.of_type(&type_of_node(first), file)
}

/// Whether some file of the workspace declares `name` as its package.
fn is_package(snapshot: &Snapshot, name: &str) -> bool {
    snapshot
        .files
        .iter()
        .any(|file| package_of(snapshot, file).as_deref() == Some(name))
}

/// The types a package declares, by name.
fn types_in_package(snapshot: &Snapshot, package: &str, prefix: &str) -> Vec<Offer> {
    let mut names: Vec<String> = Vec::new();
    for file in &snapshot.files {
        if package_of(snapshot, file).as_deref() != Some(package) {
            continue;
        }
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
            rank: format!("0{name}"),
            name,
        })
        .collect()
}

/// Whether a receiver names a variable rather than a type.
fn names_a_value(snapshot: &Snapshot, file: &FileView, receiver: &Syn) -> bool {
    if is_dotted_name(receiver) {
        return false;
    }
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
    /// What a value offers: what each instance has.
    Instance,
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
        let members = members_of(type_of.node);
        for (index, (member, named)) in members.iter().enumerate() {
            let is_static = member.text.contains("static") || member.kind == "enum_constant";
            let offered = match wanted {
                Wanted::Static => is_static,
                Wanted::Instance => member.kind != "enum_constant",
                Wanted::Everything => true,
            };
            if !offered || !reachable(snapshot, from, &type_of, member) {
                continue;
            }
            if seen.contains(&named.text) {
                continue;
            }
            seen.push(named.text.clone());
            let rank = features(from.language).member_rank(depth, index, &named.text);
            offers.push(Offer {
                name: named.text.clone(),
                rank,
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
    let modifiers: Vec<&str> = member
        .children
        .iter()
        .filter(|child| child.kind == "modifiers")
        .flat_map(|modifiers| modifiers.text.split_whitespace())
        .collect();
    let says = |word: &str| modifiers.contains(&word);
    let same_file = owner.file.uri == from.uri;
    let same_package = package_of(snapshot, owner.file) == package_of(snapshot, from);
    if says("private") {
        return same_file;
    }
    if says("protected") {
        return same_package || inherits_from(snapshot, from, owner);
    }
    // `internal` is the module's: a library's internals are not offered.
    if says("internal") {
        return !owner.file.library;
    }
    if says("public") || features(owner.file.language).public_by_default(owner.node) {
        return true;
    }
    same_package
}

/// Whether a type `from` declares extends `owner`: a protected member is
/// theirs to name.
fn inherits_from(snapshot: &Snapshot, from: &FileView, owner: &Found<'_>) -> bool {
    types_of(from).into_iter().any(|(name, _)| {
        snapshot.type_named(name, from).is_some_and(|declared| {
            snapshot
                .with_supertypes(&declared)
                .iter()
                .skip(1)
                .any(|above| above.file.uri == owner.file.uri && above.name.start == owner.name.start)
        })
    })
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
    // An inner class reads the members of the classes around it too.
    let outer = path_to(&file.tree, at)
        .into_iter()
        .rev()
        .filter(|node| is_type_declaration(node))
        .skip(1)
        .filter_map(|node| snapshot.type_named(&node.child("name")?.text, file))
        .collect::<Vec<_>>();
    for enclosing in outer {
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
        .cloned()
        .chain(std::iter::once(probed))
        .collect();
    Snapshot::new(files).reading(snapshot.libraries())
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
