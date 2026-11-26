//! A Java file's `textDocument/documentSymbol` answer, as jdtls's
//! `DocumentSymbolHandler` builds it from the Java model: the package, then
//! each type as a tree of its members. A method is named with its parameter
//! types (`first(List<E>, String...) <E>`) and its return type is the
//! detail (` : E`); a type is named with its type parameters. A member's
//! range starts at the comments that lead it, as the Eclipse parser counts
//! them, and an anonymous or local class is a child of the member it is
//! written in.

use crate::parser::{Position, Syn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Package,
    Class,
    Interface,
    Enum,
    EnumMember,
    Field,
    Constant,
    Method,
    Constructor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: Kind,
    pub start: Position,
    pub end: Position,
    pub selection: (Position, Position),
    pub deprecated: bool,
    pub children: Vec<Symbol>,
}

/// The outline of a Java file's tree.
pub fn java(tree: &Syn) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    if let Some(package) = tree.first("package_declaration") {
        let name = package
            .children
            .iter()
            .find(|child| matches!(child.kind.as_str(), "scoped_identifier" | "identifier"));
        if let Some(name) = name {
            symbols.push(Symbol {
                name: name.text.clone(),
                detail: None,
                kind: Kind::Package,
                start: package.start,
                end: package.end,
                selection: (package.start, package.end),
                deprecated: false,
                children: Vec::new(),
            });
        }
    }
    symbols.extend(members(tree, None));
    symbols
}

/// The declarations among `container`'s children, each with the comments
/// that lead it. `header` is the record whose body this is, if one is.
fn members(container: &Syn, header: Option<&Syn>) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut previous_end: Option<Position> = None;
    let mut leading: Vec<&Syn> = Vec::new();
    // jdtls lists the fields Lombok generates (`@Slf4j`'s `log`), not its
    // methods or constructors.
    let listed = container
        .children
        .iter()
        .filter(|child| !child.is_generated() || child.kind == "field_declaration");
    for child in listed {
        if is_comment(child) {
            // A comment on the line the previous declaration or token ends
            // on is that one's, not the next declaration's — when it is the
            // first comment after it and not a javadoc.
            let trailing = leading.is_empty()
                && !child.text.starts_with("/**")
                && previous_end.is_some_and(|end| end.line == end_line(child));
            if !trailing {
                leading.push(child);
            }
            continue;
        }
        if child.kind == "enum_body_declarations" {
            symbols.extend(members(child, None));
        } else {
            symbols.extend(declared(child, &leading, header));
        }
        leading.clear();
        previous_end = Some(child.end);
    }
    symbols.sort_by_key(|symbol| symbol.start);
    symbols
}

/// The line a comment ends on, should its node take the line break.
fn end_line(comment: &Syn) -> u32 {
    if comment.end.column == 0 && comment.end.line > comment.start.line {
        comment.end.line - 1
    } else {
        comment.end.line
    }
}

fn is_comment(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "line_comment" | "block_comment")
}

/// The symbols one member declaration makes: a field declaration makes one
/// per name it declares, everything else one or none.
fn declared(node: &Syn, leading: &[&Syn], header: Option<&Syn>) -> Vec<Symbol> {
    let start = leading.first().map_or(node.start, |comment| comment.start);
    let javadoc = leading
        .iter()
        .rev()
        .find(|comment| comment.text.starts_with("/**"));
    let deprecated = deprecated_by(node, javadoc.copied());
    let symbol = |name: String, kind: Kind, named: &Syn, children: Vec<Symbol>| Symbol {
        name,
        detail: None,
        kind,
        start,
        end: node.end,
        selection: (named.start, named.end),
        deprecated,
        children,
    };
    match node.kind.as_str() {
        "class_declaration"
        | "record_declaration"
        | "interface_declaration"
        | "annotation_type_declaration"
        | "enum_declaration" => {
            let Some(named) = node.child("name") else {
                return Vec::new();
            };
            let kind = match node.kind.as_str() {
                "interface_declaration" | "annotation_type_declaration" => Kind::Interface,
                "enum_declaration" => Kind::Enum,
                _ => Kind::Class,
            };
            let mut name = named.text.clone();
            if let Some(parameters) = node.child("type_parameters") {
                name.push_str(&type_parameters(parameters));
            }
            let mut children = Vec::new();
            if node.kind == "record_declaration" {
                children.extend(components(node));
            }
            if let Some(body) = node.child("body") {
                let header = (node.kind == "record_declaration").then_some(node);
                children.extend(members(body, header));
            }
            children.sort_by_key(|symbol| symbol.start);
            vec![symbol(name, kind, named, children)]
        }
        "method_declaration" | "annotation_type_element_declaration" => {
            let Some(named) = node.child("name") else {
                return Vec::new();
            };
            let mut name = format!("{}({})", named.text, parameter_types(node).join(", "));
            if let Some(parameters) = node.child("type_parameters") {
                name.push(' ');
                name.push_str(&type_parameters(parameters));
            }
            let mut returned = node.child("type").map(simple_type).unwrap_or_default();
            for dimensions in node
                .children
                .iter()
                .filter(|child| child.kind == "dimensions")
            {
                returned.push_str(&"[]".repeat(dimensions.text.matches('[').count()));
            }
            let mut method = symbol(name, Kind::Method, named, local_types(node));
            method.detail = Some(format!(" : {returned}"));
            vec![method]
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            let Some(named) = node.child("name") else {
                return Vec::new();
            };
            let types = if node.kind == "compact_constructor_declaration" {
                header.map(parameter_types).unwrap_or_default()
            } else {
                parameter_types(node)
            };
            let name = format!("{}({})", named.text, types.join(", "));
            vec![symbol(name, Kind::Constructor, named, local_types(node))]
        }
        "field_declaration" | "constant_declaration" => {
            let kind = if has_modifier(node, "static") && has_modifier(node, "final") {
                Kind::Constant
            } else {
                Kind::Field
            };
            node.children
                .iter()
                .filter(|child| child.kind == "variable_declarator")
                .filter_map(|declarator| {
                    let named = declarator.child("name")?;
                    Some(symbol(
                        named.text.clone(),
                        kind,
                        named,
                        local_types(declarator),
                    ))
                })
                .collect()
        }
        "enum_constant" => {
            let Some(named) = node.child("name") else {
                return Vec::new();
            };
            let mut children = Vec::new();
            if let Some(body) = node.child("body") {
                let mut class = symbol("{...}".to_owned(), Kind::Enum, named, members(body, None));
                class.deprecated = false;
                children.push(class);
            }
            vec![symbol(
                named.text.clone(),
                Kind::EnumMember,
                named,
                children,
            )]
        }
        _ => Vec::new(),
    }
}

/// A record's components, the fields its header declares.
fn components(record: &Syn) -> Vec<Symbol> {
    let Some(parameters) = record.child("parameters") else {
        return Vec::new();
    };
    parameters
        .children
        .iter()
        .filter(|child| child.kind == "formal_parameter")
        .filter_map(|component| {
            let named = component.child("name")?;
            Some(Symbol {
                name: named.text.clone(),
                detail: None,
                kind: Kind::Field,
                start: component.start,
                end: component.end,
                selection: (named.start, named.end),
                deprecated: deprecated_by(component, None),
                children: Vec::new(),
            })
        })
        .collect()
}

/// The classes written inside a member — anonymous ones and local ones —
/// but not those inside them, which are theirs.
fn local_types(member: &Syn) -> Vec<Symbol> {
    let mut found = Vec::new();
    for child in &member.children {
        collect_local_types(child, &mut found);
    }
    found.sort_by_key(|symbol| symbol.start);
    found
}

fn collect_local_types(node: &Syn, found: &mut Vec<Symbol>) {
    match node.kind.as_str() {
        "class_declaration"
        | "record_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "annotation_type_declaration" => {
            found.extend(declared(node, &[], None));
            return;
        }
        "object_creation_expression" => {
            if let Some(body) = node.first("class_body")
                && let Some(created) = node.child("type")
            {
                // Whatever the arguments hold comes before the class.
                for child in node
                    .children
                    .iter()
                    .filter(|child| child.field.as_deref() == Some("arguments"))
                {
                    collect_local_types(child, found);
                }
                found.push(Symbol {
                    name: format!("new {}() {{...}}", simple_type(created)),
                    detail: None,
                    kind: Kind::Class,
                    start: node.start,
                    end: node.end,
                    selection: (created.start, created.end),
                    deprecated: false,
                    children: members(body, None),
                });
                return;
            }
        }
        _ => {}
    }
    for child in &node.children {
        collect_local_types(child, found);
    }
}

/// Whether a declaration is deprecated: annotated `@Deprecated`, or its
/// javadoc has a `@deprecated` tag.
fn deprecated_by(node: &Syn, javadoc: Option<&Syn>) -> bool {
    let annotated = modifiers_of(node).is_some_and(|modifiers| {
        modifiers
            .children
            .iter()
            .filter(|child| matches!(child.kind.as_str(), "marker_annotation" | "annotation"))
            .filter_map(|annotation| annotation.child("name"))
            .any(|name| matches!(name.text.as_str(), "Deprecated" | "java.lang.Deprecated"))
    });
    annotated
        || javadoc.is_some_and(|comment| {
            comment.text.split("@deprecated").skip(1).any(|after| {
                after
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric())
            })
        })
}

fn modifiers_of(node: &Syn) -> Option<&Syn> {
    node.first("modifiers")
}

fn has_modifier(node: &Syn, word: &str) -> bool {
    modifiers_of(node).is_some_and(|modifiers| {
        modifiers
            .children
            .iter()
            .any(|child| child.kind == word || child.text == word)
    })
}

/// `<T extends Comparable<T>, U>`: each type parameter as written, with
/// its spaces evened out.
fn type_parameters(parameters: &Syn) -> String {
    let each: Vec<String> = parameters
        .children
        .iter()
        .filter(|child| child.kind == "type_parameter")
        .map(|parameter| even_spaces(&parameter.text))
        .collect();
    format!("<{}>", each.join(", "))
}

fn even_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The type of each parameter of a method, a constructor or a record
/// header, as jdtls names it: unqualified, `...` for the rest.
fn parameter_types(declaration: &Syn) -> Vec<String> {
    let Some(parameters) = declaration.child("parameters") else {
        return Vec::new();
    };
    parameters
        .children
        .iter()
        .filter_map(|parameter| match parameter.kind.as_str() {
            "formal_parameter" => {
                let mut written = simple_type(parameter.child("type")?);
                if let Some(dimensions) = parameter.child("dimensions") {
                    written.push_str(&"[]".repeat(dimensions.text.matches('[').count()));
                }
                Some(written)
            }
            "spread_parameter" => {
                let written = parameter
                    .children
                    .iter()
                    .find(|child| is_type(child))
                    .map(simple_type)?;
                Some(format!("{written}..."))
            }
            _ => None,
        })
        .collect()
}

fn is_type(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "array_type"
            | "integral_type"
            | "floating_point_type"
            | "boolean_type"
            | "void_type"
            | "annotated_type"
    )
}

/// A type as jdtls's labels write it: each name unqualified, type
/// arguments after a comma and a space, annotations left out.
fn simple_type(node: &Syn) -> String {
    match node.kind.as_str() {
        "scoped_type_identifier" => node
            .children
            .iter()
            .rev()
            .find(|child| is_type(child))
            .map_or_else(|| node.text.clone(), simple_type),
        "generic_type" => {
            let base = node
                .children
                .iter()
                .find(|child| is_type(child))
                .map(simple_type)
                .unwrap_or_default();
            let arguments: Vec<String> = node
                .first("type_arguments")
                .map(|arguments| {
                    arguments
                        .children
                        .iter()
                        .filter(|child| is_type(child) || child.kind == "wildcard")
                        .map(simple_type)
                        .collect()
                })
                .unwrap_or_default();
            format!("{base}<{}>", arguments.join(", "))
        }
        "array_type" => {
            let element = node.child("element").map(simple_type).unwrap_or_default();
            let dimensions = node
                .child("dimensions")
                .map_or(1, |dimensions| dimensions.text.matches('[').count());
            format!("{element}{}", "[]".repeat(dimensions))
        }
        "wildcard" => {
            let bound = node.children.iter().find(|child| is_type(child));
            let keyword = node
                .children
                .iter()
                .find(|child| matches!(child.kind.as_str(), "extends" | "super"));
            match (keyword, bound) {
                (Some(keyword), Some(bound)) => {
                    format!("? {} {}", keyword.text, simple_type(bound))
                }
                _ => "?".to_owned(),
            }
        }
        "annotated_type" => node
            .children
            .iter()
            .find(|child| is_type(child) && child.kind != "annotated_type")
            .map_or_else(|| even_spaces(&node.text), simple_type),
        _ => even_spaces(&node.text),
    }
}
