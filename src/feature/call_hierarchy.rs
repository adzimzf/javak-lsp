//! Call hierarchy: who calls a method, constructor or field, and what a
//! method calls, answered as jdtls answers it.
//!
//! - An item is a method, a constructor, a field (its readers and writers
//!   are its callers), a type (its instances' creators, which call its
//!   default constructor) or an initializer. On no name, the cursor's
//!   enclosing method, constructor or initializer; between members, nothing.
//! - A caller is listed once, at its first call; its other calls are
//!   further ranges of the same entry. The same goes for what a member
//!   calls.

use tower_lsp::lsp_types::Url;

use super::semantic::{
    is_type_declaration, parameters, path_to, type_of_node, type_parameters_of, Bindings, FileView,
    Found, Snapshot,
};
use crate::parser::{Kind, Position, Syn};

/// One entry of either direction: the member at the other end, and where
/// the calls between the two are written.
pub struct Calls<'a> {
    pub member: Found<'a>,
    /// Start and end of each call, in source order, in [`Self::file`].
    pub at: Vec<(Position, Position)>,
    /// The file the calls are written in.
    pub file: &'a Url,
}

/// The item the cursor asks about.
pub fn item_at<'a>(snapshot: &'a Snapshot, uri: &Url, at: Position) -> Option<Found<'a>> {
    // An item in a library file this snapshot has not read yet: read now.
    snapshot.read_library_at(uri);
    let named = snapshot.declaration_at(uri, at);
    if let Some(found) = named.filter(|found| kind_of(found).is_some()) {
        return Some(found);
    }
    let file = snapshot.file(uri)?;
    enclosing_member(file, at, false)
}

/// The member a call written at `at` belongs to: the method, constructor,
/// initializer or field around it. A lambda belongs to the member it is in,
/// a method of an anonymous class is a member of its own.
fn enclosing_member(file: &FileView, at: Position, fields_too: bool) -> Option<Found<'_>> {
    let path = path_to(&file.tree, at);
    for &node in path.iter().rev() {
        let named = |name| Found {
            file,
            node,
            name,
            bindings: Bindings::new(),
        };
        match node.kind.as_str() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => {
                return Some(named(node.child("name")?));
            }
            "static_initializer" => return Some(named(node)),
            "block" if is_initializer(node, &path) => return Some(named(node)),
            "field_declaration" if fields_too => {
                let declarator = node
                    .all("variable_declarator")
                    .find(|declarator| declarator.covers(at))?;
                return Some(Found {
                    file,
                    node: declarator,
                    name: declarator.child("name")?,
                    bindings: Bindings::new(),
                });
            }
            "class_body" | "interface_body" | "enum_body" | "record_body" => return None,
            _ => {}
        }
    }
    None
}

/// The lambda a call written at `at` is in, before the member it is in.
fn lambda_around(file: &FileView, at: Position) -> Option<Found<'_>> {
    let path = path_to(&file.tree, at);
    let lambda = path.iter().rev().find(|node| {
        matches!(
            node.kind.as_str(),
            "lambda_expression"
                | "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "static_initializer"
                | "class_body"
                | "field_declaration"
        )
    })?;
    (lambda.kind == "lambda_expression").then_some(Found {
        file,
        node: lambda,
        name: lambda,
        bindings: Bindings::new(),
    })
}

/// Whether the name at `at` is called as `super.name(…)`.
fn is_super_call(file: &FileView, at: Position) -> bool {
    path_to(&file.tree, at)
        .iter()
        .rev()
        .find(|node| node.kind == "method_invocation")
        .is_some_and(|call| {
            call.child("name").is_some_and(|name| name.start == at)
                && call.child("object").is_some_and(|object| object.kind == "super")
        })
}

/// `T extends org.demo.Node` as jdtls labels it: `T extends Node`.
fn simple_names(written: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str(word.rsplit('.').next().unwrap_or(word));
            word.clear();
        }
    };
    for character in written.chars() {
        if character.is_alphanumeric() || matches!(character, '_' | '$' | '.') {
            word.push(character);
        } else {
            flush(&mut word, &mut out);
            out.push(character);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// A block right inside a class body: an instance initializer.
fn is_initializer(block: &Syn, path: &[&Syn]) -> bool {
    let at = path.iter().position(|node| std::ptr::eq(*node, block));
    at.and_then(|at| at.checked_sub(1))
        .and_then(|parent| path.get(parent))
        .is_some_and(|parent| parent.kind.ends_with("_body") && parent.kind != "constructor_body")
}

/// What a member is, for the client; `None` for a name that is not one.
pub fn kind_of(found: &Found<'_>) -> Option<Kind> {
    let node = found.node;
    Some(match node.kind.as_str() {
        "method_declaration" | "annotation_type_element_declaration" => Kind::Method,
        "constructor_declaration" | "compact_constructor_declaration" => Kind::Constructor,
        // jdtls makes an initializer a constructor, and a lambda the
        // method it implements.
        "static_initializer" | "block" => Kind::Constructor,
        "lambda_expression" => Kind::Method,
        "field_declaration" | "constant_declaration" => Kind::Field,
        "variable_declarator" if is_field(found) => Kind::Field,
        "enum_constant" => Kind::EnumMember,
        "interface_declaration" | "annotation_type_declaration" => Kind::Interface,
        "enum_declaration" => Kind::Enum,
        _ if is_type_declaration(node) => Kind::Class,
        _ => return None,
    })
}

/// A variable declared in a class body, not in a method.
fn is_field(found: &Found<'_>) -> bool {
    let path = path_to(&found.file.tree, found.name.start);
    path.iter()
        .rev()
        .find(|node| {
            node.kind.ends_with("_declaration") || node.kind == "local_variable_declaration"
        })
        .is_some_and(|node| {
            matches!(
                node.kind.as_str(),
                "field_declaration" | "constant_declaration"
            )
        })
}

/// How jdtls names a member: `of(T) <T> : Opt<T>`, `Base()`,
/// `protectedField`, `Opt<T>`, `{...}`.
pub fn label(snapshot: &Snapshot, found: &Found<'_>) -> String {
    let node = found.node;
    let name = &found.name.text;
    let parameter_types = || -> String {
        parameters(node, &Bindings::new())
            .iter()
            .map(|parameter| {
                let spread = if parameter.spread { "..." } else { "" };
                format!("{}{spread}", parameter.written)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    match node.kind.as_str() {
        "method_declaration" | "annotation_type_element_declaration" => {
            let introduced = type_parameters_of(node);
            let introduced = if introduced.is_empty() {
                String::new()
            } else {
                format!(" <{}>", simple_names(&introduced.join(", ")))
            };
            let returns = node
                .child("type")
                .map(|written| type_of_node(written).written())
                .unwrap_or_else(|| "void".to_owned());
            format!("{name}({}){introduced} : {returns}", parameter_types())
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            format!("{name}({})", parameter_types())
        }
        "static_initializer" | "block" => "{...}".to_owned(),
        // Named after the method it implements, its type variables as the
        // lambda's target type gives them (`accept(Node)` for a
        // `Consumer<? super Node>`).
        "lambda_expression" => snapshot
            .lambda_method(node, found.file)
            .map_or_else(|| "lambda$".to_owned(), |method| {
                let bound: Bindings = method
                    .bindings
                    .iter()
                    .map(|(variable, bound)| {
                        let mut bound = bound.clone();
                        bound.wildcard = None;
                        (variable.clone(), bound)
                    })
                    .filter(|(_, bound)| bound.name != "?")
                    .collect();
                let parameter_types = parameters(method.node, &bound)
                    .iter()
                    .map(|parameter| format!("{}{}", parameter.written, if parameter.spread { "..." } else { "" }))
                    .collect::<Vec<_>>()
                    .join(", ");
                let returns = method
                    .node
                    .child("type")
                    .map(|written| super::semantic::substituted(type_of_node(written), &bound).written())
                    .unwrap_or_else(|| "void".to_owned());
                format!("{}({parameter_types}) : {returns}", method.name.text)
            }),
        _ if is_type_declaration(node) => {
            snapshot.signature_of(found).unwrap_or_else(|| name.clone())
        }
        _ => name.clone(),
    }
}

/// Who calls `of`: each member with a call of it, at its first call. A
/// call of a method `of` overrides may run `of` and counts too (not
/// through `super`); one of a method overriding it does not, as jdtls
/// counts them. A call in a lambda is the
/// lambda's, as jdtls lists them.
pub fn incoming<'a>(snapshot: &'a Snapshot, of: &Found<'_>) -> Vec<Calls<'a>> {
    let related = if of.file.language == crate::language::Language::Java && of.node.kind == "method_declaration" {
        snapshot.overridden_by_call(of)
    } else {
        Vec::new()
    };
    let mut targets: Vec<&Found<'_>> = vec![of];
    targets.extend(related.iter());
    // The item's type and those above it: a call of a method the item
    // overrides may run the item only on a receiver of one of them.
    let ancestors: Vec<(Url, Position)> = if related.is_empty() {
        Vec::new()
    } else {
        snapshot
            .owner_of(of)
            .map(|owner| {
                snapshot
                    .with_supertypes(&owner)
                    .iter()
                    .map(|ancestor| (ancestor.file.uri.clone(), ancestor.name.start))
                    .collect()
            })
            .unwrap_or_default()
    };
    let receiver_fits = |file: &FileView, start: Position| -> bool {
        let Some(call) = path_to(&file.tree, start)
            .into_iter()
            .rev()
            .find(|node| node.kind == "method_invocation" && node.child("name").is_some_and(|name| name.start == start))
        else {
            return true;
        };
        let receiver = match call.child("object") {
            Some(object) => snapshot.type_of_expression(object, file).and_then(|written| snapshot.of_type(&written, file)),
            None => snapshot.enclosing(file, start),
        };
        receiver.is_none_or(|receiver| ancestors.contains(&(receiver.file.uri.clone(), receiver.name.start)))
    };
    let mut sites: Vec<(&FileView, Position, Position)> = call_sites_of_any(snapshot, &targets)
        .into_iter()
        // A call of a related method through `super` runs that one.
        .filter(|(file, start, _, index)| *index == 0 || !is_super_call(file, *start))
        .filter(|(file, start, _, index)| {
            // Only for a method above the item: one below it is called on
            // its own type.
            let above = index
                .checked_sub(1)
                .and_then(|at| related.get(at))
                .and_then(|method| snapshot.owner_of(method))
                .is_some_and(|owner| ancestors.contains(&(owner.file.uri.clone(), owner.name.start)));
            !above || receiver_fits(file, *start)
        })
        .map(|(file, start, end, _)| (file, start, end))
        .collect();
    // A caller is shown at the call JDT meets first walking the tree: the
    // outer of two chained calls (`a.first().second()` at `second`).
    let call_span = |file: &FileView, start: Position| {
        path_to(&file.tree, start)
            .into_iter()
            .rev()
            .find(|node| matches!(node.kind.as_str(), "method_invocation" | "object_creation_expression"))
            .map_or((start, start), |call| (call.start, call.end))
    };
    sites.sort_by(|left, right| {
        let (left_start, left_end) = call_span(left.0, left.1);
        let (right_start, right_end) = call_span(right.0, right.1);
        (left.0.uri.as_str(), left_start, std::cmp::Reverse(left_end))
            .cmp(&(right.0.uri.as_str(), right_start, std::cmp::Reverse(right_end)))
    });
    sites.dedup_by(|left, right| left.0.uri == right.0.uri && left.1 == right.1);
    let mut callers: Vec<Calls<'a>> = Vec::new();
    for (file, start, end) in sites {
        let Some(caller) = lambda_around(file, start).or_else(|| enclosing_member(file, start, true)) else {
            continue;
        };
        let key = identity(&caller);
        match callers
            .iter_mut()
            .find(|calls| identity(&calls.member) == key)
        {
            Some(calls) => calls.at.push((start, end)),
            None => callers.push(Calls {
                member: caller,
                at: vec![(start, end)],
                file: &file.uri,
            }),
        }
    }
    callers
}

/// Every call of `of` in the workspace, in source order, with the start
/// and end [`incoming`] reports it at: a method's name, the `new` of a
/// creation, the `this` or `super` of a chained constructor. A field is
/// called where it is read or written.
pub fn call_sites<'a>(
    snapshot: &'a Snapshot,
    of: &Found<'_>,
) -> Vec<(&'a FileView, Position, Position)> {
    call_sites_of_any(snapshot, &[of])
        .into_iter()
        .map(|(file, start, end, _)| (file, start, end))
        .collect()
}

/// [`call_sites`] of several members of one name at once — a method and
/// the ones it overrides or is overridden by — each call resolved once,
/// with which of `targets` it calls.
pub fn call_sites_of_any<'a>(
    snapshot: &'a Snapshot,
    targets: &[&Found<'_>],
) -> Vec<(&'a FileView, Position, Position, usize)> {
    let mut sites: Vec<(&'a FileView, Position, Position, usize)> = Vec::new();
    let Some(of) = targets.first() else {
        return sites;
    };
    let wanted: Vec<(&Url, Position)> = targets.iter().map(|target| identity(target)).collect();
    match kind_of(of) {
        Some(Kind::Field | Kind::EnumMember) => {
            for (index, target) in targets.iter().enumerate() {
                for (uri, at) in snapshot.uses_of(target) {
                    if (uri, at) == (wanted[index].0, wanted[index].1) {
                        continue;
                    }
                    let Some(file) = snapshot.file(uri) else {
                        continue;
                    };
                    let end = Position {
                        line: at.line,
                        column: at.column + target.name.text.encode_utf16().count() as u32,
                    };
                    sites.push((file, at, end, index));
                }
            }
        }
        Some(Kind::Method | Kind::Constructor | Kind::Class | Kind::Interface | Kind::Enum) => {
            // A constructor and its type are called by the type's name.
            let name = match of.node.kind.as_str() {
                "constructor_declaration" | "compact_constructor_declaration" => {
                    enclosing_type_name(of).unwrap_or_else(|| of.name.text.clone())
                }
                _ => of.name.text.clone(),
            };
            // In Java the libraries' sources too, as jdtls searches them. A
            // library calls only its own declarations and its
            // dependencies', never the workspace's.
            let library = if of.file.language == crate::language::Language::Java
                && targets.iter().any(|target| target.file.library)
            {
                // A method is called by its name before `(`; a type or a
                // constructor by its name after `new`, also before `(`.
                snapshot.library_files_calling(&name)
            } else {
                Vec::new()
            };
            let files: Vec<&'a FileView> = snapshot
                .files
                .iter()
                .filter(|file| !file.library)
                .chain(library)
                .filter(|file| file.text.contains(name.as_str()))
                .collect();
            let in_file = |file: &'a FileView| {
                let mut found = Vec::new();
                for node in file.tree.walk() {
                    if !may_call(node, &name) {
                        continue;
                    }
                    let Some((called, start, end)) = call_of(snapshot, node, file) else {
                        continue;
                    };

                    if let Some(index) = wanted.iter().position(|wanted| identity(&called) == *wanted) {
                        found.push((file, start, end, index));
                    }
                }
                found
            };
            // On every core: a common name is called in thousands of the
            // JDK's files.
            let threads = std::thread::available_parallelism()
                .map_or(4, std::num::NonZero::get)
                .min(files.len().max(1));
            let chunk = files.len().div_ceil(threads).max(1);
            sites = std::thread::scope(|scope| {
                let workers: Vec<_> = files
                    .chunks(chunk)
                    .map(|chunk| scope.spawn(|| chunk.iter().flat_map(|file| in_file(file)).collect::<Vec<_>>()))
                    .collect();
                workers
                    .into_iter()
                    .flat_map(|worker| worker.join().unwrap_or_default())
                    .collect()
            });
        }
        _ => {}
    }
    sites
        .sort_by(|left, right| (left.0.uri.as_str(), left.1).cmp(&(right.0.uri.as_str(), right.1)));
    sites
}

/// What `of` calls, each callee once, in the order first called.
pub fn outgoing<'a>(snapshot: &'a Snapshot, of: &Found<'a>) -> Vec<Calls<'a>> {
    let body: Vec<&Syn> = match of.node.kind.as_str() {
        "method_declaration"
        | "constructor_declaration"
        | "compact_constructor_declaration"
        | "static_initializer"
        | "block" => vec![of.node],
        "variable_declarator" => of.node.child("value").into_iter().collect(),
        _ => Vec::new(),
    };
    let mut callees: Vec<Calls<'a>> = Vec::new();
    for root in body {
        for node in calls_within(root) {
            let Some((called, start, end)) = call_of(snapshot, node, of.file) else {
                continue;
            };
            let key = identity(&called);
            match callees
                .iter_mut()
                .find(|calls| identity(&calls.member) == key)
            {
                Some(calls) => calls.at.push((start, end)),
                None => callees.push(Calls {
                    member: called,
                    at: vec![(start, end)],
                    file: &of.file.uri,
                }),
            }
        }
    }
    callees
}

/// The calls written in `root`, leaving out the bodies of classes declared
/// in it: their methods are members of their own.
fn calls_within(root: &Syn) -> Vec<&Syn> {
    let mut calls = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_call(node) {
            calls.push(node);
        }
        for child in node.children.iter().rev() {
            if child.kind != "class_body" && !is_type_declaration(child) {
                pending.push(child);
            }
        }
    }
    calls
}

fn is_call(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "method_invocation"
            | "object_creation_expression"
            | "explicit_constructor_invocation"
            | "method_reference"
    )
}

/// Whether `node` is a call that may be of something called `name`: only
/// those are resolved.
fn may_call(node: &Syn, name: &str) -> bool {
    match node.kind.as_str() {
        "method_invocation" => node.child("name").is_some_and(|called| called.text == name),
        "object_creation_expression" => node
            .child("type")
            .is_some_and(|created| simple_name(&type_of_node(created).name) == name),
        "explicit_constructor_invocation" => true,
        "method_reference" => node.children.last().is_some_and(|last| last.text == name),
        _ => false,
    }
}

/// What a call calls, and where the call is written: a method's name, the
/// `new` of a creation, the `super` or `this` of a chained constructor.
fn call_of<'a>(
    snapshot: &'a Snapshot,
    call: &'a Syn,
    file: &'a FileView,
) -> Option<(Found<'a>, Position, Position)> {
    match call.kind.as_str() {
        "method_invocation" => {
            let name = call.child("name")?;
            Some((snapshot.invoked(call, file)?, name.start, name.end))
        }
        "object_creation_expression" => {
            let end = call
                .child("arguments")
                .map_or(call.end, |arguments| arguments.end);
            Some((snapshot.created(call, file)?, call.start, end))
        }
        "explicit_constructor_invocation" => {
            Some((snapshot.chained(call, file)?, call.start, call.end))
        }
        "method_reference" => {
            let name = call.children.last()?;
            let called = snapshot.declaration_at(&file.uri, name.start)?;
            kind_of(&called)?;
            Some((called, name.start, name.end))
        }
        _ => None,
    }
}

/// The name of the type a constructor belongs to.
fn enclosing_type_name(constructor: &Found<'_>) -> Option<String> {
    let path = path_to(&constructor.file.tree, constructor.node.start);
    let owner = path
        .iter()
        .rev()
        .find(|node| is_type_declaration(node) && !std::ptr::eq(**node, constructor.node))?;
    Some(owner.child("name")?.text.clone())
}

fn simple_name(written: &str) -> &str {
    written.rsplit('.').next().unwrap_or(written)
}

/// What tells two declarations apart: their file and where their name is.
fn identity<'a>(found: &Found<'a>) -> (&'a Url, Position) {
    (&found.file.uri, found.selection())
}
