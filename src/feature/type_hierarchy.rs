//! Type hierarchy for Java, as jdtls's `TypeHierarchyHandler` answers it.
//!
//! - The item is the type or the method the cursor names; on no name, the
//!   file's first type; on a field, a variable or a type parameter, none.
//!   A method's item is shown with its type's name and kind.
//! - Its supertypes are the direct ones: `extends` and `implements`, and
//!   `Object` for a class that names no superclass (`Enum` for an enum,
//!   `Record` for a record). Its subtypes are the types that name it among
//!   theirs, anonymous classes too (`new Iterator() {...}`).
//! - From a method, each related type is shown at its method of the same
//!   signature; one without such a method is shown as a type of kind
//!   `null`, and `Object` then not at all.
//! - A type is named as JDT names it: below its package, nested names
//!   joined with `$` (`Token$Doctype`), its package as the detail.

use crate::feature::semantic::{is_anonymous_class, is_type_declaration, members_of, path_to, Bindings, FileView, Found, Snapshot};
use crate::parser::Position;

/// What an item stands for: a type, and the method it was asked from.
pub struct Item<'a> {
    pub of: Found<'a>,
    pub method: Option<Found<'a>>,
    /// A related type without the method asked about.
    pub without_method: bool,
}

impl<'a> Item<'a> {
    /// The declaration the item shows: the method where there is one.
    pub fn shown(&self) -> &Found<'a> {
        self.method.as_ref().unwrap_or(&self.of)
    }
}

/// The item at `at`.
pub fn item_at<'a>(snapshot: &'a Snapshot, file: &'a FileView, at: Position) -> Option<Item<'a>> {
    let Some(found) = snapshot.declaration_at(&file.uri, at) else {
        // On no name, the file's first type.
        let first = file.tree.children.iter().find(|node| is_type_declaration(node))?;
        let of = Found {
            file,
            node: first,
            name: first.child("name")?,
            bindings: Bindings::new(),
        };
        return Some(Item { of, method: None, without_method: false });
    };
    if is_type_declaration(found.node) || is_anonymous_class(found.node) {
        return Some(Item { of: found, method: None, without_method: false });
    }
    if matches!(
        found.node.kind.as_str(),
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration"
    ) {
        let of = owner(&found)?;
        return Some(Item { of, method: Some(found), without_method: false });
    }
    None
}

/// The type or anonymous class a member is declared in.
pub fn owner<'a>(member: &Found<'a>) -> Option<Found<'a>> {
    let path = path_to(&member.file.tree, member.node.start);
    let declaration = path
        .iter()
        .rev()
        .filter(|node| !std::ptr::eq(**node, member.node))
        .find(|node| is_type_declaration(node) || is_anonymous_class(node))?;
    let name = if is_anonymous_class(declaration) {
        declaration.child("type")?
    } else {
        declaration.child("name")?
    };
    Some(Found {
        file: member.file,
        node: declaration,
        name,
        bindings: Bindings::new(),
    })
}

/// The direct supertypes of an item.
pub fn supertypes<'a>(snapshot: &'a Snapshot, item: &Item<'a>) -> Vec<Item<'a>> {
    let mut above = snapshot.supertypes(&item.of);
    let kind = item.of.node.kind.as_str();
    let names_a_superclass = item.of.node.children.iter().any(|child| child.kind == "superclass");
    // What a class extends without saying so.
    let implied = match kind {
        "enum_declaration" => Some("Enum"),
        "record_declaration" => Some("Record"),
        "class_declaration" if !names_a_superclass => Some("Object"),
        _ if is_anonymous_class(item.of.node)
            && above.first().is_some_and(|created| created.node.kind == "interface_declaration") =>
        {
            Some("Object")
        }
        _ => None,
    };
    if let Some(implied) = implied {
        let is_object_itself = item.of.name.text == "Object" && item.of.file.library;
        if !is_object_itself && let Some(found) = snapshot.type_named(&format!("java.lang.{implied}"), item.of.file) {
            above.insert(0, found);
        }
    }
    related(snapshot, item, above)
}

/// The direct subtypes of an item.
pub fn subtypes<'a>(snapshot: &'a Snapshot, item: &Item<'a>) -> Vec<Item<'a>> {
    let wanted = (&item.of.file.uri, item.of.name.start);
    let mut below: Vec<Found<'a>> = snapshot
        .subtypes_of(&item.of)
        .into_iter()
        .filter(|candidate| {
            snapshot
                .supertypes(candidate)
                .iter()
                .any(|supertype| (&supertype.file.uri, supertype.name.start) == wanted)
        })
        .collect();
    // The libraries' too, as jdtls searches them.
    below.extend(snapshot.library_subtypes_of(&item.of, true));
    related(snapshot, item, below)
}

/// Each related type as an item: at the method matching the one asked
/// about, if one was.
fn related<'a>(snapshot: &'a Snapshot, item: &Item<'a>, types: Vec<Found<'a>>) -> Vec<Item<'a>> {
    let Some(method) = &item.method else {
        return types
            .into_iter()
            .map(|of| Item { of, method: None, without_method: false })
            .collect();
    };
    let wanted = snapshot.erased_parameter_types(method);
    types
        .into_iter()
        .filter_map(|of| {
            let matching = members_of(of.node).into_iter().find(|(member, named)| {
                member.kind == method.node.kind
                    && named.text == method.name.text
                    && snapshot.erased_parameter_types(&Found {
                        file: of.file,
                        node: member,
                        name: named,
                        bindings: Bindings::new(),
                    }) == wanted
            });
            match matching {
                Some((member, named)) => {
                    let found = Found {
                        file: of.file,
                        node: member,
                        name: named,
                        bindings: Bindings::new(),
                    };
                    Some(Item { of, method: Some(found), without_method: false })
                }
                // `Object` only where the method is declared there.
                None if of.name.text == "Object" && of.file.library => None,
                None => Some(Item { of, method: None, without_method: true }),
            }
        })
        .collect()
}

/// The name and the detail an item is shown with: `Token$Doctype` in
/// `org.jsoup.parser`, `new Iterator() {...}` for an anonymous class.
pub fn name_and_detail(snapshot: &Snapshot, of: &Found<'_>) -> (String, String) {
    let package = snapshot.package(of.file).unwrap_or_default();
    if is_anonymous_class(of.node) {
        let created = of.name.text.split('<').next().unwrap_or_default().trim();
        let created = created.rsplit('.').next().unwrap_or(created);
        return (format!("new {created}() {{...}}"), package);
    }
    let qualified = snapshot.qualified_type_name(of.file, of.node);
    let below_package = match qualified.strip_prefix(&format!("{package}.")) {
        Some(rest) if !package.is_empty() => rest.to_owned(),
        _ => qualified,
    };
    (below_package.replace('.', "$"), package)
}
