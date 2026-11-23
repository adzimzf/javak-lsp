//! A Kotlin tree, lowered to the Java shape the semantic layer reads.
//!
//! The semantic layer names its nodes after the Java grammar, so a Kotlin
//! tree is lowered into the Java shape it means: a `fun` is a
//! `method_declaration` with its parameters, return type and body under
//! the same fields, a property is a field or a local, `a.b(c)` is a
//! `method_invocation`. Spans and texts stay the Kotlin source's, so every
//! position still points into the file as written. What has no Java
//! counterpart keeps its own kind, and the layer above passes over it.

use crate::parser::{Position, Syn};

/// A Kotlin tree as the Java-shaped tree the semantic layer reads.
pub fn lower(root: &Syn) -> Syn {
    lower_node(root, Context::File)
}

/// Where a node sits: a property is a field in a class body, a local in a
/// block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    File,
    Body,
    Block,
}

fn lower_node(node: &Syn, context: Context) -> Syn {
    match node.kind.as_str() {
        "source_file" => renamed(node, "program", context),
        "package_header" => package_declaration(node),
        "import" if !node.children.is_empty() => import(node),
        "class_declaration" | "object_declaration" => class(node),
        "companion_object" => {
            let mut lowered = renamed(node, "companion_object", Context::Body);
            lowered.field = node.field.clone();
            lowered
        }
        "class_body" | "enum_class_body" => body(node, &node.kind),
        "property_declaration" => property(node, context),
        "function_declaration" => function(node),
        "block" => block(node),
        "call_expression" => call(node),
        "navigation_expression" => navigation(node),
        "return_expression" => {
            let mut lowered = renamed(node, "return_statement", context);
            lowered.field = node.field.clone();
            lowered
        }
        "assignment" => assignment(node),
        "this_expression" => leaf(node, "this"),
        "number_literal" => leaf(node, "decimal_integer_literal"),
        "float_literal" => leaf(node, "decimal_floating_point_literal"),
        "character_literal" => leaf(node, "character_literal"),
        "boolean_literal" => leaf(node, node.text.trim()),
        "user_type" | "nullable_type" | "non_nullable_type" => written_type(node),
        "type_parameters" => type_parameters(node),
        _ => renamed(node, &node.kind, context),
    }
}

/// The node under another kind, its children lowered.
fn renamed(node: &Syn, kind: &str, context: Context) -> Syn {
    Syn {
        kind: kind.to_owned(),
        field: node.field.clone(),
        text: node.text.clone(),
        start: node.start,
        end: node.end,
        children: node
            .children
            .iter()
            .map(|child| lower_node(child, context))
            .collect(),
    }
}

/// The node under another kind, without its children.
fn leaf(node: &Syn, kind: &str) -> Syn {
    Syn {
        kind: kind.to_owned(),
        field: node.field.clone(),
        text: node.text.clone(),
        start: node.start,
        end: node.end,
        children: Vec::new(),
    }
}

fn with_field(mut node: Syn, field: &str) -> Syn {
    node.field = Some(field.to_owned());
    node
}

/// A node spanning `children`, made for the Java shape.
fn spanning(kind: &str, field: Option<&str>, source: &Syn, children: Vec<Syn>) -> Syn {
    let start = children.first().map_or(source.start, |child| child.start);
    let end = children.last().map_or(source.end, |child| child.end);
    Syn {
        kind: kind.to_owned(),
        field: field.map(str::to_owned),
        text: slice(source, start, end),
        start,
        end,
        children,
    }
}

/// The source between two positions inside `node`.
fn slice(node: &Syn, start: Position, end: Position) -> String {
    let mut text = String::new();
    let mut at = node.start;
    for character in node.text.chars() {
        if at >= end {
            break;
        }
        if at >= start {
            text.push(character);
        }
        if character == '\n' {
            at.line += 1;
            at.column = 0;
        } else {
            at.column += character.len_utf16() as u32;
        }
    }
    text
}

fn is_punctuation(node: &Syn) -> bool {
    node.children.is_empty() && !node.text.chars().any(char::is_alphanumeric)
}

/// `package com.demo` as Java's `package_declaration`.
fn package_declaration(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "package_declaration");
    lowered.children = node
        .children
        .iter()
        .map(|child| match child.kind.as_str() {
            "qualified_identifier" if child.text.contains('.') => {
                renamed(child, "scoped_identifier", Context::File)
            }
            "qualified_identifier" => leaf(child, "identifier"),
            _ => lower_node(child, Context::File),
        })
        .collect();
    lowered
}

fn import(node: &Syn) -> Syn {
    renamed(node, "import_declaration", Context::File)
}

/// A class, an interface, an enum or an object, by its keywords.
fn class(node: &Syn) -> Syn {
    let is_interface = node.first("interface").is_some();
    let is_enum = node
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.walk().iter().any(|word| word.kind == "enum"));
    let kind = if is_interface {
        "interface_declaration"
    } else if is_enum {
        "enum_declaration"
    } else {
        "class_declaration"
    };
    let mut lowered = leaf(node, kind);
    for child in &node.children {
        match child.kind.as_str() {
            "primary_constructor" => {
                if let Some(parameters) = child.first("class_parameters") {
                    lowered.children.push(class_parameters(parameters));
                }
            }
            "delegation_specifiers" => lowered.children.push(supertypes(child)),
            "class_body" | "enum_class_body" => {
                let body_kind = if is_interface {
                    "interface_body"
                } else if is_enum || child.kind == "enum_class_body" {
                    "enum_body"
                } else {
                    "class_body"
                };
                lowered
                    .children
                    .push(with_field(body(child, body_kind), "body"));
            }
            _ => lowered.children.push(lower_node(child, Context::File)),
        }
    }
    lowered
}

/// The parameters of a primary constructor: the class's components.
fn class_parameters(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "formal_parameters");
    lowered.field = Some("parameters".to_owned());
    lowered.children = node
        .children
        .iter()
        .map(|child| match child.kind.as_str() {
            "class_parameter" => parameter(child),
            _ => lower_node(child, Context::File),
        })
        .collect();
    lowered
}

/// `name: Type` as a `formal_parameter` with its `name` and `type`.
fn parameter(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "formal_parameter");
    for child in &node.children {
        let mut child = lower_node(child, Context::File);
        if child.kind == "identifier" && lowered.child("name").is_none() {
            child.field = Some("name".to_owned());
        } else if is_type(&child) && lowered.child("type").is_none() {
            child.field = Some("type".to_owned());
        }
        lowered.children.push(child);
    }
    lowered
}

fn is_type(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "type_identifier" | "generic_type" | "scoped_type_identifier" | "function_type"
    )
}

/// `: Shape(), Named` as the supertypes Java lists.
fn supertypes(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "super_interfaces");
    for specifier in node.children.iter().filter(|child| !is_punctuation(child)) {
        let written = specifier
            .walk()
            .into_iter()
            .find(|inner| inner.kind == "user_type")
            .map(written_type);
        lowered.children.extend(written);
    }
    lowered
}

/// A type as Java writes one: `Circle` a `type_identifier`, `List<T>` a
/// `generic_type`, `a.B` a `scoped_type_identifier`. `T?` is `T`.
fn written_type(node: &Syn) -> Syn {
    if matches!(node.kind.as_str(), "nullable_type" | "non_nullable_type") {
        let inner = node
            .children
            .iter()
            .find(|child| !is_punctuation(child))
            .map(written_type);
        return match inner {
            Some(mut inner) => {
                inner.field = node.field.clone();
                inner
            }
            None => leaf(node, "type_identifier"),
        };
    }
    let inner = node.children.iter().find(|child| !is_punctuation(child));
    if let Some(inner) = inner.filter(|_| node.kind == "parenthesized_type") {
        return written_type(inner);
    }
    if node.kind != "user_type" {
        return lower_node(node, Context::File);
    }
    let names: Vec<&Syn> = node.all("identifier").collect();
    let arguments = node.first("type_arguments");
    let name = match names.as_slice() {
        [] => return leaf(node, "type_identifier"),
        [single] => leaf(single, "type_identifier"),
        [first, .., last] => {
            let mut scoped = spanning(
                "scoped_type_identifier",
                None,
                node,
                vec![leaf(first, "type_identifier"), leaf(last, "type_identifier")],
            );
            scoped.children = node
                .children
                .iter()
                .filter(|child| child.kind != "type_arguments")
                .map(|child| {
                    if child.kind == "identifier" {
                        leaf(child, "type_identifier")
                    } else {
                        leaf(child, &child.kind)
                    }
                })
                .collect();
            scoped
        }
    };
    let Some(arguments) = arguments else {
        let mut name = name;
        name.field = node.field.clone();
        return name;
    };
    let mut lowered_arguments = leaf(arguments, "type_arguments");
    lowered_arguments.children = arguments
        .children
        .iter()
        .map(|child| match child.kind.as_str() {
            "type_projection" => child
                .children
                .iter()
                .find(|inner| !is_punctuation(inner) && inner.kind != "type_projection_modifiers")
                .map_or_else(|| leaf(child, "wildcard"), written_type),
            _ => leaf(child, &child.kind),
        })
        .collect();
    let mut generic = leaf(node, "generic_type");
    generic.children = vec![name, lowered_arguments];
    generic
}

fn type_parameters(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "type_parameters");
    lowered.field = Some("type_parameters".to_owned());
    lowered.children = node
        .children
        .iter()
        .map(|child| {
            if child.kind != "type_parameter" {
                return leaf(child, &child.kind);
            }
            let mut parameter = leaf(child, "type_parameter");
            for inner in &child.children {
                match inner.kind.as_str() {
                    "identifier" => parameter.children.push(leaf(inner, "type_identifier")),
                    "user_type" | "nullable_type" => {
                        let bound = written_type(inner);
                        parameter
                            .children
                            .push(spanning("type_bound", None, inner, vec![bound]));
                    }
                    _ => parameter.children.push(lower_node(inner, Context::File)),
                }
            }
            parameter
        })
        .collect();
    lowered
}

/// A class body; an enum's entries are its constants.
fn body(node: &Syn, kind: &str) -> Syn {
    let mut lowered = leaf(node, kind);
    lowered.field = node.field.clone();
    lowered.children = node
        .children
        .iter()
        .map(|child| match child.kind.as_str() {
            "enum_entry" => {
                let mut constant = renamed(child, "enum_constant", Context::Body);
                if let Some(name) = constant
                    .children
                    .iter_mut()
                    .find(|inner| inner.kind == "identifier")
                {
                    name.field = Some("name".to_owned());
                }
                constant
            }
            _ => lower_node(child, Context::Body),
        })
        .collect();
    lowered
}

/// `val name: Type = value`: a field in a class body, a local in a block,
/// each with one declarator. A property written without its type is
/// inferred from its value, as Java's `var` is.
fn property(node: &Syn, context: Context) -> Syn {
    let kind = if context == Context::Block {
        "local_variable_declaration"
    } else {
        "field_declaration"
    };
    let mut lowered = leaf(node, kind);
    let declaration = node.first("variable_declaration");
    let name = declaration.and_then(|declaration| declaration.first("identifier"));
    let written = declaration.and_then(|declaration| {
        declaration
            .children
            .iter()
            .find(|child| !matches!(child.kind.as_str(), "identifier") && !is_punctuation(child))
    });
    let assigned = node.children.iter().position(|child| child.kind == "=");
    let value = assigned.and_then(|at| node.children.get(at + 1));
    for child in &node.children {
        match child.kind.as_str() {
            "modifiers" => lowered.children.push(lower_node(child, context)),
            "val" | "var" => {
                lowered.children.push(leaf(child, &child.kind));
                if written.is_none() {
                    let mut inferred = leaf(child, "inferred_type");
                    inferred.text = "var".to_owned();
                    lowered.children.push(with_field(inferred, "type"));
                }
            }
            "variable_declaration" => {
                if let Some(written) = written {
                    lowered
                        .children
                        .push(with_field(written_type(written), "type"));
                }
                let Some(name) = name else { continue };
                let mut parts = vec![with_field(leaf(name, "identifier"), "name")];
                if let Some(value) = value {
                    parts.push(with_field(lower_node(value, Context::Block), "value"));
                }
                let mut declarator = spanning("variable_declarator", Some("declarator"), node, parts);
                // The declarator ends where its value does, or at its name.
                if value.is_none() {
                    declarator.end = name.end;
                    declarator.text = name.text.clone();
                }
                lowered.children.push(declarator);
            }
            _ => {}
        }
    }
    let value_lowered = value.is_some();
    // The value sits inside the declarator; anything after it (a getter,
    // a delegate) is kept as it is.
    for child in &node.children {
        let is_part = matches!(
            child.kind.as_str(),
            "modifiers" | "val" | "var" | "variable_declaration" | "="
        ) || (value_lowered && value.is_some_and(|value| value.start == child.start));
        if !is_part {
            lowered.children.push(lower_node(child, context));
        }
    }
    lowered
}

/// `fun name(parameters): Returned { … }` as a `method_declaration`.
fn function(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "method_declaration");
    let mut after_parameters = false;
    for child in &node.children {
        match child.kind.as_str() {
            "function_value_parameters" => {
                lowered.children.push(function_parameters(child));
                after_parameters = true;
            }
            "function_body" => lowered.children.push(function_body(child)),
            "type_parameters" => lowered.children.push(type_parameters(child)),
            "identifier" if lowered.child("name").is_none() => {
                lowered.children.push(with_field(leaf(child, "identifier"), "name"));
            }
            "user_type" | "nullable_type" | "non_nullable_type" | "function_type"
                if after_parameters =>
            {
                lowered
                    .children
                    .push(with_field(written_type(child), "type"));
            }
            _ => lowered.children.push(lower_node(child, Context::Body)),
        }
    }
    lowered
}

/// The parameters of a `fun`; a `vararg` one is spread.
fn function_parameters(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "formal_parameters");
    lowered.field = Some("parameters".to_owned());
    let mut spread = false;
    for child in &node.children {
        match child.kind.as_str() {
            "parameter_modifiers" => {
                spread = child.text.contains("vararg");
            }
            "parameter" => {
                let mut parameter = parameter(child);
                if spread {
                    parameter.kind = "spread_parameter".to_owned();
                    spread = false;
                }
                lowered.children.push(parameter);
            }
            _ => lowered.children.push(leaf(child, &child.kind)),
        }
    }
    lowered
}

/// A body is a block; `= value` returns the value.
fn function_body(node: &Syn) -> Syn {
    if let Some(block) = node.first("block") {
        return with_field(self::block(block), "body");
    }
    let Some(value) = node.children.iter().find(|child| !is_punctuation(child)) else {
        return with_field(leaf(node, "block"), "body");
    };
    let returned = spanning(
        "return_statement",
        None,
        node,
        vec![lower_node(value, Context::Block)],
    );
    let mut block = leaf(node, "block");
    block.field = Some("body".to_owned());
    block.children = vec![returned];
    block
}

/// A block's statements as Java's: an expression on its own is an
/// expression statement.
fn block(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "block");
    lowered.field = node.field.clone();
    lowered.children = node
        .children
        .iter()
        .map(|child| {
            let statement = lower_node(child, Context::Block);
            let is_statement = is_punctuation(child)
                || matches!(
                    statement.kind.as_str(),
                    "local_variable_declaration"
                        | "return_statement"
                        | "expression_statement"
                        | "class_declaration"
                        | "line_comment"
                        | "block_comment"
                );
            if is_statement {
                statement
            } else {
                spanning("expression_statement", None, child, vec![statement])
            }
        })
        .collect();
    lowered
}

/// `a = b` as an expression statement Java writes the same way.
fn assignment(node: &Syn) -> Syn {
    let expression = renamed(node, "assignment_expression", Context::Block);
    spanning("expression_statement", node.field.as_deref(), node, vec![expression])
}

/// `receiver.name(arguments)` as a `method_invocation`; a call of
/// anything else keeps its own shape.
fn call(node: &Syn) -> Syn {
    let Some(callee) = node.children.first() else {
        return renamed(node, "call_expression", Context::Block);
    };
    let arguments = node
        .first("value_arguments")
        .map(|arguments| with_field(argument_list(arguments), "arguments"));
    let mut lowered = leaf(node, "method_invocation");
    match callee.kind.as_str() {
        "identifier" => {
            lowered
                .children
                .push(with_field(leaf(callee, "identifier"), "name"));
        }
        "navigation_expression" => {
            let Some((object, dot, name)) = navigated(callee) else {
                return renamed(node, "call_expression", Context::Block);
            };
            lowered.children.push(with_field(object, "object"));
            lowered.children.push(dot);
            lowered.children.push(with_field(name, "name"));
        }
        _ => return renamed(node, "call_expression", Context::Block),
    }
    lowered.children.extend(arguments);
    for child in node.children.iter().skip(1) {
        if child.kind != "value_arguments" {
            lowered.children.push(lower_node(child, Context::Block));
        }
    }
    lowered
}

/// The receiver, the dot and the name of `receiver.name`.
fn navigated(node: &Syn) -> Option<(Syn, Syn, Syn)> {
    let object = node.children.first()?;
    let name = node.children.last().filter(|name| name.kind == "identifier")?;
    let dot = node
        .children
        .iter()
        .find(|child| matches!(child.kind.as_str(), "." | "?." | "::"))?;
    Some((
        lower_node(object, Context::Block),
        leaf(dot, "."),
        leaf(name, "identifier"),
    ))
}

/// `receiver.name` read as a value: a field access.
fn navigation(node: &Syn) -> Syn {
    let Some((object, dot, name)) = navigated(node) else {
        return renamed(node, "navigation_expression", Context::Block);
    };
    let mut lowered = leaf(node, "field_access");
    lowered.children = vec![
        with_field(object, "object"),
        dot,
        with_field(name, "field"),
    ];
    lowered
}

/// `(a, b)` of a call; an argument is the value it passes.
fn argument_list(node: &Syn) -> Syn {
    let mut lowered = leaf(node, "argument_list");
    lowered.children = node
        .children
        .iter()
        .map(|child| {
            if child.kind != "value_argument" {
                return leaf(child, &child.kind);
            }
            let value = child
                .children
                .iter()
                .rev()
                .find(|inner| !is_punctuation(inner))
                .unwrap_or(child);
            lower_node(value, Context::Block)
        })
        .collect();
    lowered
}
