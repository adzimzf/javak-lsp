//! What a completion item shows and writes, as jdtls fills it in: its
//! kind, the description beside its label (a member's type, a type's
//! package), its detail (`Printer.print(String text) : void`), what it
//! inserts and the import it adds.
//!
//! A member is described where it is offered, from its declaration. A type
//! is offered by its qualified name and described only once the list is
//! cut to the proposals sent ([`describe_type`]): an empty prefix offers
//! hundreds of them, and reading each one's declaration would cost more
//! than the whole answer.

use crate::feature::semantic::{
    FileView, Found, Snapshot, Type, imports, is_type_declaration, parameters, path_to,
};
use crate::parser::{Position, Syn};

/// The kind a client shows an item with (LSP's `CompletionItemKind`), as
/// jdtls picks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Text,
    Method,
    Constructor,
    Field,
    Constant,
    EnumMember,
    Variable,
    Class,
    Interface,
    Enum,
    Struct,
    Module,
    Keyword,
    Snippet,
}

/// What an item shows beside its name, and what it writes.
#[derive(Clone, Debug, Default)]
pub struct Shown {
    pub kind: Option<ItemKind>,
    /// Beside the label: a member's type, a type's package.
    pub description: Option<String>,
    /// `Printer.print(String text) : void`, `java.util.List`.
    pub detail: Option<String>,
    pub documentation: Option<String>,
    /// The import choosing the item adds.
    pub import: Option<String>,
    /// What it writes, where that is not the name (or the call).
    pub insert: Option<String>,
    /// Whether [`Shown::insert`] is a snippet.
    pub snippet: bool,
    /// A method returning `void`: its call ends the statement.
    pub void: bool,
    /// The label, where it is not the name (`Shapes<T>`).
    pub label: Option<String>,
    pub deprecated: bool,
    /// Where the text the item replaces starts, where that is before the
    /// name typed (a qualified name being written).
    pub replaces: Option<Position>,
    /// Choosing it adds no import (it is the import being written).
    pub no_import: bool,
}

/// A type as jdtls's labels write it: the name of its declaration, nested
/// in the types around it but without its package, and its type arguments
/// fully qualified — `List<java.lang.String>`, `Foo.Stream[]`.
pub fn jdt_type(snapshot: &Snapshot, from: &FileView, written: &Type) -> String {
    let arguments = arguments_qualified(snapshot, from, written);
    let name = match snapshot.of_type(written, from) {
        Some(found) if is_type_declaration(found.node) => {
            let qualified = snapshot.qualified_type_name(found.file, found.node);
            match snapshot.package(found.file) {
                Some(package) if !package.is_empty() => {
                    qualified.strip_prefix(&format!("{package}.")).unwrap_or(&qualified).to_owned()
                }
                _ => qualified,
            }
        }
        _ => written.name.clone(),
    };
    format!(
        "{}{name}{arguments}{}",
        written.wildcard.unwrap_or_default(),
        "[]".repeat(written.dimensions)
    )
}

/// A type with its package and every type argument qualified:
/// `java.util.List<java.lang.String>`.
pub fn qualified(snapshot: &Snapshot, from: &FileView, written: &Type) -> String {
    let name = match snapshot.of_type(written, from) {
        Some(found) if is_type_declaration(found.node) => {
            snapshot.qualified_type_name(found.file, found.node)
        }
        _ => written.name.clone(),
    };
    format!(
        "{}{name}{}{}",
        written.wildcard.unwrap_or_default(),
        arguments_qualified(snapshot, from, written),
        "[]".repeat(written.dimensions)
    )
}

fn arguments_qualified(snapshot: &Snapshot, from: &FileView, written: &Type) -> String {
    if written.arguments.is_empty() {
        return String::new();
    }
    let inner: Vec<String> =
        written.arguments.iter().map(|argument| qualified(snapshot, from, argument)).collect();
    format!("<{}>", inner.join(","))
}

/// Whether `member` says `word` among its modifiers.
fn says(member: &Syn, word: &str) -> bool {
    member
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|written| written == word))
}

/// The name of the type that declares a member, as a detail starts:
/// `Shapes`, or `Shapes<T>` with its type parameters for a field.
fn owner_name(owner: &Found<'_>, with_parameters: bool) -> String {
    let parameters = owner
        .node
        .child("type_parameters")
        .filter(|_| with_parameters)
        .map(|written| type_parameter_names(&written.text))
        .unwrap_or_default();
    format!("{}{parameters}", owner.name.text)
}

/// `<K, V>` of `<K extends Comparable<K>, V>`, written as JDT does.
fn type_parameter_names(declared: &str) -> String {
    let inner = declared.trim().trim_start_matches('<').trim_end_matches('>');
    let mut names = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for character in inner.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                names.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    names.push(current);
    let names: Vec<&str> = names
        .iter()
        .filter_map(|name| name.split_whitespace().next())
        .collect();
    format!("<{}>", names.join(","))
}

/// A field, constant or enum constant `member` of `owner`, offered in
/// `from`.
pub fn field(snapshot: &Snapshot, from: &FileView, owner: &Found<'_>, member: &Found<'_>) -> Shown {
    let node = member.node;
    let kind = if node.kind == "enum_constant" {
        ItemKind::EnumMember
    } else if node.kind == "constant_declaration" || (says(node, "static") && says(node, "final")) {
        ItemKind::Constant
    } else {
        ItemKind::Field
    };
    let written = snapshot.type_of_declaration(member);
    let description = written.as_ref().map(|written| jdt_type(snapshot, from, written));
    // jdtls shows the value of a text constant, not of a number.
    let documentation = (kind == ItemKind::Constant)
        .then(|| constant_value(member))
        .flatten()
        .filter(|value| value.starts_with('"'))
        .map(|value| format!("Value: {value}"))
        .or_else(|| snapshot.javadoc_summary_of(member));
    Shown {
        kind: Some(kind),
        detail: description
            .as_ref()
            .map(|shown| format!("{}.{} : {shown}", owner_name(owner, true), member.name.text)),
        description,
        documentation,
        ..Shown::default()
    }
}

/// The literal a constant is initialized with, as written.
fn constant_value(member: &Found<'_>) -> Option<String> {
    let declarator = member
        .node
        .children
        .iter()
        .find(|child| child.kind == "variable_declarator" && child.child("name").is_some_and(|named| named.text == member.name.text))?;
    let value = declarator.child("value")?;
    value.kind.ends_with("literal").then(|| value.text.clone())
        .or_else(|| matches!(value.kind.as_str(), "true" | "false").then(|| value.text.clone()))
}

/// A method `member` of `owner`: its parameters as the label shows them
/// and their names, with the rest of what the item shows.
pub fn method(snapshot: &Snapshot, owner: &Found<'_>, member: &Found<'_>, written: &[String]) -> (Shown, Vec<String>) {
    let names: Vec<String> = parameters(member.node, &member.bindings)
        .into_iter()
        .map(|parameter| parameter.name)
        .collect();
    let returns = snapshot.return_written(member);
    let detail = format!(
        "{}.{}({}){}",
        owner_name(owner, false),
        member.name.text,
        written.join(", "),
        returns.as_ref().map(|returns| format!(" : {returns}")).unwrap_or_default()
    );
    let shown = Shown {
        kind: Some(ItemKind::Method),
        description: returns.clone(),
        detail: Some(detail),
        documentation: snapshot.javadoc_summary_of(member),
        void: returns.as_deref() == Some("void"),
        ..Shown::default()
    };
    (shown, names)
}

/// The accessor a record component `member` of `owner` has: what jdtls
/// offers by the component's name outside a body of its own.
pub fn accessor(snapshot: &Snapshot, owner: &Found<'_>, member: &Found<'_>) -> Shown {
    let returns = snapshot.type_of_declaration(member).map(|written| written.written());
    Shown {
        kind: Some(ItemKind::Method),
        detail: Some(format!(
            "{}.{}(){}",
            owner_name(owner, false),
            member.name.text,
            returns.as_ref().map(|returns| format!(" : {returns}")).unwrap_or_default()
        )),
        description: returns,
        ..Shown::default()
    }
}

/// A package, as jdtls shows one: `com.demo · (package)`.
pub fn package(name: &str) -> Shown {
    Shown {
        kind: Some(ItemKind::Module),
        description: Some("(package)".to_owned()),
        detail: Some(format!("(package) {name}")),
        ..Shown::default()
    }
}

/// A local variable or parameter.
pub fn variable(snapshot: &Snapshot, from: &FileView, found: &Found<'_>) -> Shown {
    Shown {
        kind: Some(ItemKind::Variable),
        description: snapshot
            .type_of_declaration(found)
            .map(|written| jdt_type(snapshot, from, &written)),
        ..Shown::default()
    }
}

/// The kind of item a type declaration is.
pub fn type_kind(declaration: &Syn) -> ItemKind {
    match declaration.kind.as_str() {
        "interface_declaration" | "annotation_type_declaration" => ItemKind::Interface,
        "enum_declaration" => ItemKind::Enum,
        "record_declaration" => ItemKind::Struct,
        _ => ItemKind::Class,
    }
}

/// A type named `qualified`, offered in `from`: its label (with its type
/// parameters, and the generic types around an inner class), its package
/// or the type it is nested in, its qualified name, and the import naming
/// it adds where the file cannot name it yet. `None` where nothing of that
/// name is found; the item then shows the name alone.
///
/// A generic type is written with its type parameters (`Shapes<T>`) only
/// where the cursor `at` is inside it, where jdtls reads the name as the
/// type itself rather than as its raw type.
pub fn describe_type(snapshot: &Snapshot, from: &FileView, qualified: &str, at: Position) -> Option<Shown> {
    let found = snapshot.type_named(qualified, from)?;
    if found.node.kind == "type_parameter" {
        return Some(Shown {
            kind: Some(ItemKind::Class),
            detail: Some(found.name.text.clone()),
            ..Shown::default()
        });
    }
    let package = snapshot.package(found.file).filter(|package| !package.is_empty());
    let enclosing: Vec<&Syn> = path_to(&found.file.tree, found.node.start)
        .into_iter()
        .filter(|node| node.start < found.node.start && is_type_declaration(node))
        .collect();
    let inside = |node: &Syn| found.file.uri == from.uri && node.start <= at && at <= node.end;
    let own_parameters = found
        .node
        .child("type_parameters")
        .filter(|_| inside(found.node))
        .map(|written| type_parameter_names(&written.text))
        .unwrap_or_default();
    let is_inner = !enclosing.is_empty()
        && found.node.kind == "class_declaration"
        && !says(found.node, "static")
        && enclosing.last().is_some_and(|outer| outer.kind == "class_declaration");
    let generic_outer = is_inner
        && enclosing.iter().any(|outer| outer.child("type_parameters").is_some() && inside(outer));
    // The chain of names a qualified name has below its package.
    let chain = |with_parameters: bool| -> Vec<String> {
        enclosing
            .iter()
            .filter_map(|outer| {
                let name = outer.child("name")?.text.clone();
                let parameters = outer
                    .child("type_parameters")
                    .filter(|_| with_parameters)
                    .map(|written| type_parameter_names(&written.text))
                    .unwrap_or_default();
                Some(format!("{name}{parameters}"))
            })
            .collect()
    };
    let simple = format!("{}{own_parameters}", found.name.text);
    let (label, description) = if generic_outer {
        let mut names = chain(true);
        names.push(simple.clone());
        (names.join("."), package.clone())
    } else if enclosing.is_empty() {
        (simple.clone(), package.clone())
    } else {
        let mut names: Vec<String> = package.iter().cloned().collect();
        names.extend(chain(false));
        (simple.clone(), Some(names.join(".")))
    };
    let detail = {
        let mut names: Vec<String> = package.iter().cloned().collect();
        names.extend(chain(generic_outer));
        names.push(simple);
        names.join(".")
    };
    let import = needs_import(snapshot, from, &found, package.as_deref(), enclosing.is_empty())
        .then(|| format!("import {};\n", snapshot.qualified_type_name(found.file, found.node)));
    Some(Shown {
        kind: Some(type_kind(found.node)),
        label: Some(label),
        description,
        detail: Some(detail),
        import,
        documentation: snapshot.javadoc_summary_of(&found),
        deprecated: deprecated(found.node),
        ..Shown::default()
    })
}

/// Whether naming a top-level type `found` in `from` needs an import: it
/// is in another package that is not `java.lang`, and nothing imports it
/// by name or its package on demand.
fn needs_import(snapshot: &Snapshot, from: &FileView, found: &Found<'_>, package: Option<&str>, top_level: bool) -> bool {
    let Some(package) = package else {
        return false;
    };
    if !top_level || package == "java.lang" || snapshot.package(from).as_deref() == Some(package) {
        return false;
    }
    let qualified = format!("{package}.{}", found.name.text);
    !imports(from).into_iter().any(|import| {
        (!import.is_static && import.wildcard && import.name == package)
            || (!import.wildcard && import.name == qualified)
    })
}

/// Whether a declaration is marked deprecated, by annotation or Javadoc.
pub fn deprecated(node: &Syn) -> bool {
    node.first("modifiers")
        .is_some_and(|modifiers| modifiers.text.contains("@Deprecated"))
}
