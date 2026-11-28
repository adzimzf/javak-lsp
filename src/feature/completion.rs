//! What `textDocument/completion` offers.
//!
//! A half-typed name is a syntax error, so the file is read with the name
//! completed by a placeholder: the tree is then the tree of the program
//! the user is about to have, and the rest of the server reads it as
//! usual.

use std::sync::Arc;

mod java;
mod shown;

pub use shown::{ItemKind, Shown};

use crate::language::Language;
use crate::parser::{syntax, Position, Syn};
use crate::feature::features;
use crate::feature::semantic::{
    type_of_node, is_identifier, is_type_declaration, members_of, package_of, path_to,
    types_of, Bindings, FileView, Found, Snapshot, Type,
};

/// The name the file is read with in place of what is being typed.
const PROBE: &str = "javakProbe";

#[derive(Clone, Debug, Default)]
pub struct Offer {
    pub name: String,
    /// What a client sorts by: the server's order, not the alphabet.
    pub rank: String,
    /// What the name is, for the client's icon and jdtls's order.
    pub kind: OfferKind,
    /// Each declaration the name offers, an overload each: jdtls offers
    /// them one by one. Empty for a keyword, a package or a local.
    pub declarations: Vec<Proposed>,
    /// A member of the receiver before the `.` (`text.|`), which jdtls
    /// ranks by relevance alone.
    pub member: bool,
    /// The qualified name of the type offered, described once the list
    /// is cut ([`shown::describe_type`]).
    pub qualified: Option<String>,
    /// What an offer without declarations shows (a local, a keyword).
    pub shown: Shown,
    /// The constructors of the type named, not the type: one name offers
    /// both after `new`.
    pub constructs: bool,
}

/// One declaration a name offers.
#[derive(Clone, Debug, Default)]
pub struct Proposed {
    /// `(String text, int count)` of a method, as written.
    pub parameters: Option<Vec<String>>,
    /// The type a method returns or a field holds.
    pub returns: Option<String>,
    /// Whether the receiver's own type declares it, not a supertype.
    pub own: bool,
    pub is_static: bool,
    pub deprecated: bool,
    /// Where it is in its type, for jdtls's order of equal proposals.
    pub index: usize,
    /// What JDT adds to its relevance for where it is declared and how it
    /// is reached: its receiver's own type, an instance member through an
    /// instance.
    pub relevance: i32,
    /// The names of a method's parameters, for the arguments it writes.
    pub names: Vec<String>,
    pub shown: Shown,
}

/// JDT's relevance points (`RelevanceConstants`) a completion weighs.
mod relevance {
    /// The name is spelled with the prefix's case.
    pub const CASE: i32 = 10;
    /// The name is the prefix.
    pub const EXACT_NAME: i32 = 4;
    /// An instance member through an instance.
    pub const NON_STATIC: i32 = 11;
    /// Of exactly the type the place wants (`int x = Units.|`).
    pub const EXACT_EXPECTED_TYPE: i32 = 30;
}

/// How many proposals jdtls answers at most
/// (`java.completion.maxResults`).
pub const MAX_PROPOSALS: usize = 50;

/// One completion item, as jdtls offers it: an overload each.
#[derive(Clone, Debug)]
pub struct Proposal {
    pub name: String,
    /// `append`, `length`, `class`.
    pub label: String,
    /// `(String str)` after a method's label.
    pub label_detail: Option<String>,
    pub kind: OfferKind,
    /// What a client sorts by: the server's order.
    pub sort: String,
    pub names: Vec<String>,
    pub qualified: Option<String>,
    pub shown: Shown,
}

/// The offers as jdtls's proposals: each overload of a name on its own,
/// a receiver's members in jdtls's order — by relevance, then kind, then
/// name (a longer name before its prefix, as JDT compares), then fewer
/// parameters, then the last declared first — the other offers in their
/// own order; the first [`MAX_PROPOSALS`] of them, and whether some were
/// left out.
pub fn proposals(offers: Vec<Offer>, prefix: &str) -> (Vec<Proposal>, bool) {
    struct Ranked {
        proposal: Proposal,
        member: bool,
        relevance: i32,
        parameters: usize,
        index: usize,
        order: usize,
        /// What JDT compares names by ([`completion_of`]).
        completion: String,
        /// A constructor, listed in the order it is offered.
        constructs: bool,
    }
    let mut ranked = Vec::new();
    for (order, offer) in offers.into_iter().enumerate() {
        let mut relevance = 0;
        if !prefix.is_empty() && offer.name.starts_with(prefix) {
            relevance += relevance::CASE;
        }
        if offer.name == prefix {
            relevance += relevance::EXACT_NAME;
        }
        let declarations = if offer.declarations.is_empty() {
            vec![Proposed::default()]
        } else {
            offer.declarations.clone()
        };
        for proposed in declarations {
            let mut shown = if proposed.shown.kind.is_some() {
                proposed.shown.clone()
            } else {
                offer.shown.clone()
            };
            if shown.description.is_none() {
                shown.description.clone_from(&proposed.returns);
            }
            shown.deprecated |= proposed.deprecated;
            let proposal = Proposal {
                name: offer.name.clone(),
                label: offer.name.clone(),
                label_detail: proposed
                    .parameters
                    .as_ref()
                    .map(|parameters| format!("({})", parameters.join(", "))),
                kind: offer.kind,
                sort: offer.rank.clone(),
                names: proposed.names.clone(),
                qualified: offer.qualified.clone(),
                shown,
            };
            ranked.push(Ranked {
                completion: completion_of(&proposal),
                proposal,
                member: offer.member,
                relevance: relevance + proposed.relevance,
                parameters: proposed.parameters.as_ref().map_or(0, Vec::len),
                index: proposed.index,
                order,
                constructs: offer.constructs,
            });
        }
    }
    let members = !ranked.is_empty() && ranked.iter().all(|ranked| ranked.member);
    if members {
        ranked.sort_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then(left.proposal.kind.cmp(&right.proposal.kind))
                .then_with(|| jdt_name_order(&left.completion, &right.completion))
                .then(left.parameters.cmp(&right.parameters))
                .then(right.index.cmp(&left.index))
        });
    } else {
        ranked.sort_by(|left, right| {
            let overloads = if left.constructs && right.constructs {
                left.index.cmp(&right.index)
            } else {
                left.parameters.cmp(&right.parameters).then(right.index.cmp(&left.index))
            };
            left.proposal
                .sort
                .cmp(&right.proposal.sort)
                .then(left.order.cmp(&right.order))
                .then(overloads)
        });
    }
    let incomplete = ranked.len() > MAX_PROPOSALS;
    let proposals = ranked
        .into_iter()
        .take(MAX_PROPOSALS)
        .enumerate()
        .map(|(position, ranked)| Proposal {
            // The client keeps the server's order.
            sort: format!("{position:04}"),
            ..ranked.proposal
        })
        .collect();
    (proposals, incomplete)
}

/// What JDT compares a proposal by: what it inserts, a method's
/// parentheses included (`item()` before `items()`).
fn completion_of(proposal: &Proposal) -> String {
    if proposal.kind == OfferKind::Method {
        format!("{}()", proposal.name)
    } else {
        proposal.name.clone()
    }
}

/// How JDT orders two names of equal relevance and kind: character by
/// character, a name that runs on past the other's end first
/// (`compareUnsigned` before `compare`).
fn jdt_name_order(left: &str, right: &str) -> std::cmp::Ordering {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    for (index, character) in left.iter().enumerate() {
        let Some(other) = right.get(index) else {
            return std::cmp::Ordering::Less;
        };
        match character.cmp(other) {
            std::cmp::Ordering::Equal => {}
            unequal => return unequal,
        }
    }
    right.len().cmp(&left.len())
}

/// What a name offered is, in the order jdtls ranks kinds that are
/// equally relevant (JDT's `CompletionProposal` kinds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum OfferKind {
    Field,
    Keyword,
    Local,
    #[default]
    Method,
    Package,
    Type,
}

/// What to offer at `at`, in the order to offer it.
pub fn offers(
    snapshot: &Snapshot,
    file: &FileView,
    at: Position,
    implied_package: Option<&str>,
) -> Vec<Offer> {
    gather(snapshot, file, at, implied_package).map(|(_, offers)| offers).unwrap_or_default()
}

/// A Java file's completion items at `at`, as jdtls answers them: its
/// proposals ([`proposals`]), each described as jdtls shows it, and
/// whether some were left out.
pub fn items(
    snapshot: &Snapshot,
    file: &FileView,
    at: Position,
    implied_package: Option<&str>,
) -> (Vec<Proposal>, bool) {
    let prefix = typed_prefix(&file.text, at);
    let Some((world, offers)) = gather(snapshot, file, at, implied_package) else {
        return (Vec::new(), false);
    };
    let (mut proposals, incomplete) = proposals(offers, &prefix);
    let probe_file = world.file(&file.uri).expect("the probed file is in the world");
    let site = Site::at(file, at);
    for proposal in &mut proposals {
        describe(&world, probe_file, at, proposal, &site);
    }
    (proposals, incomplete)
}

/// What the text around the cursor decides of what an item writes.
struct Site {
    /// The cursor stands before a declaration (`|List<String> names = …`),
    /// which jdtls reads as the type being completed: a method writes its
    /// name alone.
    bare: bool,
}

impl Site {
    fn at(file: &FileView, at: Position) -> Self {
        let line = file.text.lines().nth(at.line as usize).unwrap_or_default();
        let units: Vec<u16> = line.encode_utf16().collect();
        let split = (at.column as usize).min(units.len());
        let after = String::from_utf16_lossy(&units[split..]);
        Self { bare: starts_a_declaration(&after) }
    }
}

/// Whether `text` starts with `Type name =` or `Type name;`: a type,
/// with its type arguments and dimensions, then a name.
fn starts_a_declaration(text: &str) -> bool {
    let mut rest = text;
    let name_length = |text: &str| text.find(|character: char| !(is_name_character(character) || character == '.')).unwrap_or(text.len());
    let length = name_length(rest);
    if length == 0 || !rest.starts_with(|character: char| character.is_alphabetic() || character == '_') {
        return false;
    }
    rest = &rest[length..];
    if rest.starts_with('<') {
        let mut depth = 0;
        let mut end = None;
        for (index, character) in rest.char_indices() {
            match character {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { return false };
        rest = &rest[end..];
    }
    while let Some(after) = rest.strip_prefix("[]") {
        rest = after;
    }
    let trimmed = rest.trim_start();
    if trimmed.len() == rest.len() {
        return false;
    }
    let length = name_length(trimmed);
    length > 0 && trimmed[length..].trim_start().starts_with(['=', ';'])
}

/// Fills in what `proposal` shows and writes: a type from its declaration
/// ([`shown::describe_type`]), the kind of the rest from what it is, and
/// the text it inserts.
fn describe(world: &Snapshot, file: &FileView, at: Position, proposal: &mut Proposal, site: &Site) {
    if let Some(qualified) = &proposal.qualified
        && proposal.shown.kind.is_none()
        && let Some(described) = shown::describe_type(world, file, qualified, at)
    {
        // What the offer says itself (`List<java.lang.String>` of the type
        // a place wants) stands over what the declaration says.
        let offered = std::mem::take(&mut proposal.shown);
        // A name written qualified needs no import.
        let import = if offered.replaces.is_some() || offered.no_import { None } else { described.import.clone() };
        proposal.shown = Shown {
            label: offered.label.or(described.label),
            detail: offered.detail.or(described.detail),
            snippet: offered.snippet,
            replaces: offered.replaces,
            import,
            insert: offered.insert,
            ..described
        };
    }
    if proposal.shown.kind.is_none() {
        proposal.shown.kind = Some(match proposal.kind {
            OfferKind::Field => ItemKind::Field,
            OfferKind::Keyword => ItemKind::Keyword,
            OfferKind::Local => ItemKind::Variable,
            OfferKind::Method => ItemKind::Method,
            OfferKind::Package => ItemKind::Module,
            OfferKind::Type => ItemKind::Class,
        });
    }
    if proposal.shown.insert.is_some() {
        return;
    }
    let shown = &mut proposal.shown;
    match shown.kind {
        Some(ItemKind::Method) if proposal.label_detail.is_some() => {
            if site.bare {
                return;
            }
            let arguments: Vec<String> = proposal
                .names
                .iter()
                .enumerate()
                .map(|(index, name)| format!("${{{}:{name}}}", index + 1))
                .collect();
            // jdtls ends the call of a `void` method with `;`, wherever.
            let end = if shown.void { ";" } else { "" };
            shown.snippet = !arguments.is_empty();
            shown.insert = Some(format!("{}({}){end}", proposal.name, arguments.join(", ")));
        }
        Some(ItemKind::Keyword) if proposal.name == "new" => {
            shown.insert = Some("new ".to_owned());
        }
        _ => {}
    }
}

/// What jdtls writes in a `package` or `import` declaration: a package
/// as `(package)`, replacing the whole dotted name typed, and in an
/// import, `com.demo.*;` to go on from; a type `Name;`.
fn in_declarations(offers: &mut [Offer], path: &[&Syn], at: Position, dotted: Option<&str>) {
    let in_import = enclosing_kind(path, "import_declaration").is_some();
    if !in_import && enclosing_kind(path, "package_declaration").is_none() {
        return;
    }
    let dotted_start = dotted.map(|dotted| Position {
        line: at.line,
        column: at.column.saturating_sub(dotted.encode_utf16().count() as u32),
    });
    for offer in offers {
        match offer.kind {
            OfferKind::Package => {
                offer.shown = Shown {
                    insert: in_import.then(|| format!("{}.${{0:*}};", offer.name)),
                    snippet: in_import,
                    replaces: dotted_start,
                    ..shown::package(&offer.name)
                };
            }
            OfferKind::Type if in_import => {
                offer.shown.insert = Some(format!("{};", offer.name));
                offer.shown.no_import = true;
            }
            _ => {}
        }
    }
}

/// The offers at `at` and the snapshot they were read in: the file read
/// with the name being typed completed by a placeholder.
fn gather(
    snapshot: &Snapshot,
    file: &FileView,
    at: Position,
    implied_package: Option<&str>,
) -> Option<(Snapshot, Vec<Offer>)> {
    let prefix = typed_prefix(&file.text, at);
    if in_comment_or_string(&file.text, at) {
        return None;
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
    let name = probe_node(&probe_file.tree)?;
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
                    kind: OfferKind::Package,
                    ..Offer::default()
                },
            );
        }
        offers
    } else if file.language == Language::Java {
        java::contextual(&java::Context {
            snapshot: &world,
            file: probe_file,
            written: file,
            path: path.clone(),
            name,
            prefix: &prefix,
            dotted: dotted.as_deref(),
        })
    } else {
        contextual(&world, probe_file, &path, name, &prefix, dotted.as_deref())
    };
    if file.language == Language::Java {
        in_declarations(&mut offers, &path, at, dotted.as_deref());
        offers.retain(|offer| {
            prefix.is_empty()
                || java::matches(offer, &prefix)
                || dotted.as_deref().is_some_and(|dotted| java::starts_with(&offer.name, dotted))
        });
        // One offer per name, at the best place any of them has; types of
        // one name in different packages each.
        let mut kept: Vec<Offer> = Vec::new();
        for offer in offers {
            let same = |known: &&mut Offer| {
                known.name == offer.name
                    && known.constructs == offer.constructs
                    && (known.kind == OfferKind::Package) == (offer.kind == OfferKind::Package)
                    && (known.qualified.is_none() || offer.qualified.is_none() || known.qualified == offer.qualified)
            };
            match kept.iter_mut().find(same) {
                Some(known) if offer.rank < known.rank => known.rank = offer.rank,
                Some(_) => {}
                None => kept.push(offer),
            }
        }
        drop(path);
        return Some((world, kept));
    }
    offers.retain(|offer| {
        prefix.is_empty()
            || offer.name.starts_with(&prefix)
            || dotted.as_deref().is_some_and(|dotted| offer.name.starts_with(dotted))
    });
    offers.dedup_by(|left, right| left.name == right.name);
    drop(path);
    Some((world, offers))
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
        return members(snapshot, file, receiver, prefix, false, None);
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
                ..Offer::default()
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
            ..Offer::default()
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
fn members(
    snapshot: &Snapshot,
    file: &FileView,
    receiver: &Syn,
    prefix: &str,
    calling: bool,
    expected: Option<&str>,
) -> Vec<Offer> {
    // `names.length` of an array, and nothing else an array has.
    if let Some(written) = type_of_receiver(snapshot, file, receiver) {
        if written.dimensions > 0 {
            let array = shown::jdt_type(snapshot, file, &written);
            return vec![Offer {
                name: "length".to_owned(),
                rank: "0length".to_owned(),
                kind: OfferKind::Field,
                shown: Shown {
                    kind: Some(ItemKind::Field),
                    description: Some("int".to_owned()),
                    detail: Some(format!("{array}.length : int")),
                    ..Shown::default()
                },
                ..Offer::default()
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
        let mut offers = visible(snapshot, file, &declaration, Wanted::Static, None, calling, expected);
        // `Suit.class`: a class literal, after the members.
        if file.language == Language::Java {
            let literal = format!(
                "Class<{}>",
                snapshot.qualified_type_name(declaration.file, declaration.node)
            );
            offers.push(Offer {
                name: "class".to_owned(),
                rank: "9class".to_owned(),
                kind: OfferKind::Keyword,
                member: true,
                shown: Shown {
                    kind: Some(ItemKind::Field),
                    detail: Some(format!("class : {literal}")),
                    description: Some(literal),
                    ..Shown::default()
                },
                ..Offer::default()
            });
        }
        return offers;
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
        .map(|declaration| visible(snapshot, file, &declaration, Wanted::Instance, Some(receiver.start), calling, expected))
        .unwrap_or_default();
    let depth = offers.len();
    for (index, name) in snapshot
        .extension_names(&written, prefix, file)
        .into_iter()
        .enumerate()
    {
        if !offers.iter().any(|offer| offer.name == name) {
            let rank = features(file.language).member_rank(depth + 1, index, &name, "method_declaration");
            offers.push(Offer { name, rank, ..Offer::default() });
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
    let qualify = |name: &str| format!("{package}.{name}");
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
            qualified: Some(qualify(&name)),
            name,
            kind: OfferKind::Type,
            ..Offer::default()
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
///
/// `at` is where an instance is written for [`Wanted::Instance`]: in Java,
/// every value has `Object`'s methods, its protected ones only inside the
/// value's own type, as jdtls offers them. `calling` keeps methods
/// only.
fn visible(
    snapshot: &Snapshot,
    from: &FileView,
    of: &Found<'_>,
    wanted: Wanted,
    at: Option<Position>,
    calling: bool,
    expected: Option<&str>,
) -> Vec<Offer> {
    let mut offers: Vec<Offer> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // The parameter types of each offer's overloads, by offer.
    let mut signatures: Vec<Vec<Vec<String>>> = Vec::new();
    let java = from.language == Language::Java && at.is_some();
    let inside = |declaration: &Found<'_>| {
        at.is_some_and(|at| {
            declaration.file.uri == from.uri && declaration.node.start <= at && at <= declaration.node.end
        })
    };
    let mut owners = snapshot.with_supertypes(of);
    // Every value is an `Object`, written or not.
    if java
        && !owners.iter().any(|owner| owner.name.text == "Object")
        && let Some(object) = snapshot.type_named("Object", from)
    {
        owners.push(object);
    }
    for (depth, type_of) in owners.into_iter().enumerate() {
        let members = members_of(type_of.node);
        for (index, (member, named)) in members.iter().enumerate() {
            let is_static = member.text.contains("static") || member.kind == "enum_constant";
            // A constructor is named as its type is, not as a member.
            if matches!(member.kind.as_str(), "constructor_declaration" | "compact_constructor_declaration") {
                continue;
            }
            let says = |word: &str| {
                member
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|written| written == word))
            };
            // `Object`'s protected methods (`clone`, `finalize`) are a
            // value's only inside its own type.
            let object_of_interface = java && type_of.name.text == "Object" && type_of.file.library;
            if object_of_interface && says("protected") && !inside(of) {
                continue;
            }
            let offered = match wanted {
                Wanted::Static => is_static,
                Wanted::Instance => member.kind != "enum_constant",
                Wanted::Everything => true,
            } && (!calling || member.kind == "method_declaration");
            let object_method = object_of_interface && !says("private");
            if !offered || !(object_method || reachable(snapshot, from, &type_of, member)) {
                continue;
            }
            // Each overload is offered, an override once: as the nearest
            // type declares it.
            let is_method = member.kind == "method_declaration";
            let found = Found {
                file: type_of.file,
                node: member,
                name: named,
                bindings: type_of.bindings.clone(),
            };
            let signature = if is_method { snapshot.erased_parameter_types(&found) } else { Vec::new() };
            let parameters: Option<Vec<String>> = is_method.then(|| {
                crate::feature::semantic::parameters(member, &type_of.bindings)
                    .iter()
                    .map(|parameter| features(from.language).parameter(parameter))
                    .collect()
            });
            // A record's component is offered as its accessor, `x()`.
            let accessor = java && member.kind == "formal_parameter" && type_of.node.kind == "record_declaration";
            let parameters = if accessor { Some(Vec::new()) } else { parameters };
            let (shown, names) = match &parameters {
                Some(_) if accessor => (shown::accessor(snapshot, &type_of, &found), Vec::new()),
                Some(written) => shown::method(snapshot, &type_of, &found, written),
                None => (shown::field(snapshot, from, &type_of, &found), Vec::new()),
            };
            let proposed = Proposed {
                names,
                shown,
                parameters,
                returns: snapshot.return_written(&found).or_else(|| {
                    (!is_method).then(|| snapshot.type_of_declaration(&found).map(|written| written.written())).flatten()
                }),
                own: depth == 0,
                is_static: says("static")
                    || matches!(member.kind.as_str(), "enum_constant" | "constant_declaration"),
                deprecated: member
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.text.contains("@Deprecated")),
                index,
                relevance: if wanted == Wanted::Instance && !says("static") && member.kind != "constant_declaration" {
                    relevance::NON_STATIC
                } else {
                    0
                },
            };
            let mut proposed = proposed;
            if expected.is_some_and(|expected| proposed.returns.as_deref() == Some(expected)) {
                proposed.relevance += relevance::EXACT_EXPECTED_TYPE;
            }
            if let Some(at) = seen.iter().position(|known| *known == named.text) {
                let offer: &mut Offer = &mut offers[at];
                let overloaded = is_method
                    && offer.kind == OfferKind::Method
                    && !signatures[at].contains(&signature);
                if overloaded {
                    signatures[at].push(signature);
                    offer.declarations.push(proposed);
                }
                continue;
            }
            seen.push(named.text.clone());
            signatures.push(vec![signature]);
            let rank = features(from.language).member_rank(depth, index, &named.text, &member.kind);
            offers.push(Offer {
                name: named.text.clone(),
                rank,
                kind: if is_method || accessor { OfferKind::Method } else { OfferKind::Field },
                declarations: vec![proposed],
                member: wanted != Wanted::Everything,
                ..Offer::default()
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
        .map(|(index, (member, named))| {
            let constant = Found { file: declaration.file, node: member, name: named, bindings: Bindings::new() };
            Offer {
                name: named.text.clone(),
                rank: format!("0{index:04}"),
                kind: OfferKind::Field,
                shown: shown::field(snapshot, file, &declaration, &constant),
                ..Offer::default()
            }
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
    // jdtls shows an element as a field of its type and writes `name = `.
    members_of(declaration.node)
        .iter()
        .enumerate()
        .map(|(index, (member, named))| {
            let element = Found { file: declaration.file, node: member, name: named, bindings: Bindings::new() };
            let written = snapshot.return_written(&element).or_else(|| member.child("type").map(|written| written.text.clone()));
            Offer {
                name: named.text.clone(),
                rank: format!("0{index:04}"),
                kind: OfferKind::Field,
                shown: Shown {
                    kind: Some(ItemKind::Field),
                    detail: written.as_ref().map(|written| format!("{}.{} : {written}", declaration.name.text, named.text)),
                    description: written,
                    insert: Some(format!("{} = ", named.text)),
                    ..Shown::default()
                },
                ..Offer::default()
            }
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
            kind: OfferKind::Package,
            ..Offer::default()
        })
        .collect()
}

/// Every type of the workspace, by name.
fn types_matching_prefix(snapshot: &Snapshot, prefix: &str) -> Vec<Offer> {
    let mut names: Vec<(String, String)> = Vec::new();
    for file in &snapshot.files {
        for (name, declaration) in types_of(file) {
            if name.starts_with(prefix) && !names.iter().any(|(known, _)| known == name) {
                names.push((name.to_owned(), snapshot.qualified_type_name(file, declaration)));
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|(name, qualified)| Offer {
            rank: format!("5{name}"),
            name,
            kind: OfferKind::Type,
            qualified: Some(qualified),
            ..Offer::default()
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
            ..Offer::default()
        });
    }
    if let Some(enclosing) = snapshot.enclosing(file, at) {
        offers.extend(visible(snapshot, file, &enclosing, Wanted::Everything, None, false, None));
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
        offers.extend(visible(snapshot, file, &enclosing, Wanted::Everything, None, false, None));
    }
    offers
}

fn enclosing_kind<'a>(path: &[&'a Syn], kind: &str) -> Option<&'a Syn> {
    path.iter().rev().find(|node| node.kind == kind).copied()
}

/// The identifier characters written just before the cursor.
pub fn typed_prefix(text: &str, at: Position) -> String {
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
    // The cursor may sit inside a word: the rest of a name is the name
    // being replaced (`A|rrays.equals(…)`), a keyword stays where it is
    // (`|package com.demo;`).
    let rest = characters[column..]
        .iter()
        .take_while(|character| is_name_character(**character))
        .count();
    let word: String = characters[column..column + rest].iter().collect();
    let (skipped, separator) = if rest == 0 {
        (0, "")
    } else if JAVA_KEYWORDS.contains(&word.as_str()) {
        (0, " ")
    } else {
        (rest, "")
    };
    let after: String = characters[column + skipped..].iter().collect();
    *line = format!("{before}{PROBE}{separator}{after}");
    lines.join("\n") + "\n"
}

/// The words a name cannot be, so a cursor right before one is not in a
/// name.
const JAVA_KEYWORDS: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
    "const", "continue", "default", "do", "double", "else", "enum", "extends", "final",
    "finally", "float", "for", "goto", "if", "implements", "import", "instanceof", "int",
    "interface", "long", "native", "new", "package", "private", "protected", "public",
    "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "void", "volatile", "while", "true", "false",
    "null", "var", "record", "sealed", "non", "permits", "yield", "fun", "val", "object",
    "companion", "data", "when", "is", "in", "as", "typealias", "override", "open",
    "internal", "lateinit", "suspend", "inline",
];

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
