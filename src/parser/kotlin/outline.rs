//! What a Kotlin file declares, in Kotlin's own terms: read off the Kotlin
//! tree as the grammar parses it, not off the lowered one.

use crate::parser::{Kind, Outline, Syn};

/// The package a Kotlin file declares.
pub fn package(tree: &Syn) -> Option<String> {
    let header = tree.first("package_header")?;
    Some(header.first("qualified_identifier")?.text.clone())
}

/// The outline of a Kotlin file, as Kotlin names its declarations: a
/// `fun` in a class is a method and outside one a function, a property is
/// a property and a `const` one a constant. An enum class also has what
/// Kotlin gives every enum (`entries`, `values()`, `valueOf()`, `name`,
/// `ordinal`), a companion object is an object called `Companion`.
pub fn outline(tree: &Syn) -> Vec<Outline> {
    let mut entries = Vec::new();
    outline_below(tree, false, &mut entries);
    entries
}

fn outline_below(node: &Syn, in_a_class: bool, entries: &mut Vec<Outline>) {
    for child in &node.children {
        let mut nested_in_a_class = in_a_class;
        match child.kind.as_str() {
            "class_declaration" | "object_declaration" => {
                let is_interface = child.first("interface").is_some();
                let is_enum = child
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.walk().iter().any(|word| word.kind == "enum"));
                let kind = if is_interface {
                    Kind::Interface
                } else if is_enum {
                    Kind::Enum
                } else if child.kind == "object_declaration" {
                    Kind::Object
                } else {
                    Kind::Class
                };
                if let Some(name) = child.child("name") {
                    entries.push(entry(child, name, &name.text, kind));
                    if is_enum {
                        for (synthetic, kind) in [
                            ("entries", Kind::Property),
                            ("values", Kind::Function),
                            ("valueOf", Kind::Function),
                            ("name", Kind::Property),
                            ("ordinal", Kind::Property),
                        ] {
                            entries.push(entry(child, name, synthetic, kind));
                        }
                    }
                }
                if let Some(parameters) = child
                    .first("primary_constructor")
                    .and_then(|constructor| constructor.first("class_parameters"))
                {
                    for parameter in parameters.all("class_parameter") {
                        let is_property =
                            parameter.first("val").is_some() || parameter.first("var").is_some();
                        if let Some(name) = parameter.first("identifier").filter(|_| is_property) {
                            entries.push(entry(parameter, name, &name.text, Kind::Property));
                        }
                    }
                }
                nested_in_a_class = true;
            }
            "companion_object" => {
                let name = child.first("identifier");
                let at = name.or_else(|| child.first("companion")).unwrap_or(child);
                let named = name.map_or("Companion", |name| name.text.as_str());
                entries.push(entry(child, at, named, Kind::Object));
                nested_in_a_class = true;
            }
            "enum_entry" => {
                if let Some(name) = child.first("identifier") {
                    entries.push(entry(child, name, &name.text, Kind::EnumMember));
                }
            }
            "function_declaration" => {
                if let Some(name) = child.child("name").or_else(|| child.first("identifier")) {
                    let kind = if in_a_class { Kind::Method } else { Kind::Function };
                    entries.push(entry(child, name, &name.text, kind));
                }
                continue;
            }
            "property_declaration" => {
                let is_const = child
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.walk().iter().any(|word| word.kind == "const"));
                let kind = if is_const { Kind::Constant } else { Kind::Property };
                let name = child
                    .first("variable_declaration")
                    .and_then(|declaration| declaration.first("identifier"));
                if let Some(name) = name {
                    entries.push(entry(child, name, &name.text, kind));
                }
                continue;
            }
            "secondary_constructor" => {
                let at = child.first("constructor").unwrap_or(child);
                entries.push(entry(child, at, "constructor", Kind::Constructor));
                continue;
            }
            _ => {}
        }
        outline_below(child, nested_in_a_class, entries);
    }
}

fn entry(node: &Syn, name: &Syn, named: &str, kind: Kind) -> Outline {
    Outline {
        name: named.to_owned(),
        kind,
        selection: name.start,
        start: node.start,
        end: node.end,
    }
}

/// Every name a Kotlin file declares.
pub fn declared_names(tree: &Syn) -> Vec<String> {
    let mut names = Vec::new();
    for node in tree.walk() {
        let named = match node.kind.as_str() {
            "class_declaration" | "object_declaration" | "function_declaration" => {
                node.child("name").or_else(|| node.first("identifier"))
            }
            "class_parameter" | "parameter" | "variable_declaration" | "enum_entry"
            | "type_parameter" | "companion_object" => node.first("identifier"),
            _ => None,
        };
        names.extend(named.map(|name| name.text.clone()));
        if node.kind == "lambda_parameters" {
            names.extend(
                node.walk()
                    .into_iter()
                    .filter(|inner| inner.kind == "identifier")
                    .map(|inner| inner.text.clone()),
            );
        }
    }
    names.sort();
    names.dedup();
    names
}
