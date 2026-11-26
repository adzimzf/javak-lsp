//! What the Eclipse compiler (jdtls) reports of a Java file that takes
//! knowing types, said as it says it:
//!
//! - an enum `switch` statement with no `default` that leaves constants
//!   out: `The enum constant X needs a corresponding case label in this
//!   enum switch on E`, a warning on the switched expression, once per
//!   constant;
//! - a call of a method no type in the receiver's hierarchy declares by
//!   that name: `The method m(String) is undefined for the type T`, on the
//!   method's name;
//! - a cast from a type to a type variable: `Type safety: Unchecked cast
//!   from Node to T`, a warning on the cast;
//! - a member of a deprecated type that is not deprecated itself: `The
//!   enclosing type T is deprecated, perhaps this member should be marked
//!   as deprecated, too?`, information on its name and parameters;
//! - `@SuppressWarnings("unused")` that suppresses nothing: Eclipse
//!   cannot tell, as jdtls leaves some of the `unused` checks off.
//!
//! As in [`super::checks`], a check speaks only where it is sure: where a
//! type it would have to look into is unknown, or generic, it says
//! nothing.

use super::compiler::{Level, Report};
use super::{imports, is_anonymous_class, is_type_declaration, path_to, FileView, Found, Snapshot};
use crate::language::Language;
use crate::parser::{Finding, Position, Syn};

/// Methods every class has from `Object`.
const OBJECT_METHODS: &[&str] = &[
    "clone",
    "equals",
    "finalize",
    "getClass",
    "hashCode",
    "notify",
    "notifyAll",
    "toString",
    "wait",
];

impl Snapshot {
    /// What the compiler says of a Java file that takes types to know.
    pub fn type_reports(&self, file: &FileView) -> Vec<Report> {
        if file.language != Language::Java {
            return Vec::new();
        }
        let mut reports = Vec::new();
        for node in file.tree.walk() {
            match node.kind.as_str() {
                "switch_expression" => reports.extend(self.missing_enum_cases(file, node)),
                "method_invocation" => reports.extend(self.undefined_method(file, node)),
                "cast_expression" => reports.extend(self.unchecked_cast(file, node)),
                _ => {}
            }
        }
        reports.extend(deprecated_enclosing_type(file));
        reports.extend(unused_not_analysed(file));
        reports
    }

    /// The constants an enum `switch` statement without `default` has no
    /// `case` for.
    fn missing_enum_cases(&self, file: &FileView, switch: &Syn) -> Vec<Report> {
        let path = path_to(&file.tree, switch.start);
        // A switch expression must cover every constant: an error of its
        // own, not this warning.
        let parent = path
            .iter()
            .position(|node| std::ptr::eq(*node, switch))
            .and_then(|index| index.checked_sub(1))
            .map(|index| path[index]);
        let is_statement = parent.is_some_and(|parent| {
            matches!(
                parent.kind.as_str(),
                "block"
                    | "constructor_body"
                    | "switch_block_statement_group"
                    | "labeled_statement"
                    | "if_statement"
                    | "while_statement"
                    | "for_statement"
                    | "enhanced_for_statement"
                    | "do_statement"
            )
        });
        if !is_statement {
            return Vec::new();
        }
        let Some(condition) = switch.child("condition") else {
            return Vec::new();
        };
        let Some(selector) = condition.children.iter().find(|child| !matches!(child.kind.as_str(), "(" | ")")) else {
            return Vec::new();
        };
        let Some(body) = switch.child("body") else {
            return Vec::new();
        };
        let mut covered: Vec<&str> = Vec::new();
        for label in body.walk() {
            if label.kind != "switch_label" || !is_label_of(body, label) {
                continue;
            }
            if label.children.iter().any(|child| child.kind == "default") {
                return Vec::new();
            }
            for value in label.children.iter().filter(|child| child.kind != "case" && !matches!(child.kind.as_str(), "," | ":" | "->")) {
                match value.kind.as_str() {
                    "identifier" => covered.push(&value.text),
                    "field_access" | "scoped_identifier" => {
                        covered.push(value.text.rsplit('.').next().unwrap_or_default().trim());
                    }
                    // A pattern, `null`: not a plain enum switch.
                    _ => return Vec::new(),
                }
            }
        }
        let Some(declaration) = self.switch_of(file, switch.start) else {
            return Vec::new();
        };
        if declaration.node.kind != "enum_declaration" {
            return Vec::new();
        }
        let certain = self
            .where_type_is_written(selector, file)
            .and_then(|(written, written_in, spread)| (!spread).then(|| self.written_type(written, written_in)).flatten())
            .is_some_and(|found| found.file.uri == declaration.file.uri && found.node.start == declaration.node.start);
        if !certain {
            return Vec::new();
        }
        let Some(enum_name) = readable_name(&declaration) else {
            return Vec::new();
        };
        let constants = declaration
            .node
            .child("body")
            .map(|body| {
                body.children
                    .iter()
                    .filter(|child| child.kind == "enum_constant")
                    .filter_map(|constant| constant.child("name"))
                    .map(|name| name.text.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        constants
            .into_iter()
            .filter(|constant| !covered.contains(constant))
            .map(|constant| Report {
                finding: Finding {
                    at: selector.start,
                    end: Some(selector.end),
                    message: format!(
                        "The enum constant {constant} needs a corresponding case label in this enum switch on {enum_name}"
                    ),
                },
                level: Level::Warning,
                unnecessary: false,
            })
            .collect()
    }

    /// A call of a method no type of the receiver's hierarchy declares by
    /// its name.
    fn undefined_method<'a>(&'a self, file: &'a FileView, call: &'a Syn) -> Option<Report> {
        let name = call.child("name")?;
        if OBJECT_METHODS.contains(&name.text.as_str()) {
            return None;
        }
        let innermost = |at: Position| {
            path_to(&file.tree, at)
                .into_iter()
                .rev()
                .find(|node| is_type_declaration(node) || is_anonymous_class(node))
        };
        let this_type = |declaration: &'a Syn| -> Option<(Found<'a>, String)> {
            let found = self.declared(file, declaration)?;
            let shown = self.shown_declaration(&found)?;
            Some((found, shown))
        };
        // Each type whose methods the call may be of, and the type the
        // message names.
        let (receivers, shown): (Vec<Found<'_>>, String) = match call.child("object") {
            // Unqualified: the enclosing types', and a static import's.
            None => {
                let static_import = imports(file)
                    .iter()
                    .any(|import| import.is_static && (import.wildcard || import.last_segment() == name.text));
                if static_import {
                    return None;
                }
                let path = path_to(&file.tree, call.start);
                let mut enclosing = Vec::new();
                for node in path.iter().rev() {
                    if is_anonymous_class(node) {
                        return None;
                    }
                    if is_type_declaration(node) {
                        enclosing.push(self.declared(file, node)?);
                    }
                }
                let shown = self.shown_declaration(enclosing.first()?)?;
                (enclosing, shown)
            }
            Some(object) if object.kind == "this" => {
                let (found, shown) = this_type(innermost(call.start).filter(|node| is_type_declaration(node))?)?;
                (vec![found], shown)
            }
            Some(object) if object.kind == "super" => {
                let class = self.declared(file, innermost(call.start).filter(|node| node.kind == "class_declaration")?)?;
                match class.node.child("superclass") {
                    Some(superclass) => {
                        let written = superclass.children.iter().find(|child| child.kind != "extends")?;
                        let found = self.written_type(written, file)?;
                        (vec![found], self.rendered(written, file, true)?)
                    }
                    None => (vec![self.type_named("java.lang.Object", file)?], "Object".to_owned()),
                }
            }
            Some(object) => {
                let (found, shown) = self.receiver_type(file, object)?;
                (vec![found], shown)
            }
        };
        let mut defined = false;
        for receiver in &receivers {
            if !matches!(receiver.node.kind.as_str(), "class_declaration" | "interface_declaration") {
                return None;
            }
            if self.method_names_of(receiver)?.contains(&name.text) {
                defined = true;
            }
        }
        if defined {
            return None;
        }
        let type_name = shown;
        let arguments = call.child("arguments")?;
        let given = arguments
            .children
            .iter()
            .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment"))
            .map(|argument| self.argument_type_in_message(argument, file))
            .collect::<Option<Vec<String>>>()?;
        Some(Report {
            finding: Finding {
                at: name.start,
                end: Some(name.end),
                message: format!("The method {}({}) is undefined for the type {type_name}", name.text, given.join(", ")),
            },
            level: Level::Error,
            unnecessary: false,
        })
    }

    /// The names of the methods of a type and the types above it, once
    /// per type and snapshot; `None` where some of them are unknown.
    fn method_names_of(&self, of: &Found<'_>) -> Option<std::sync::Arc<std::collections::HashSet<String>>> {
        let key = (of.file.uri.clone(), of.node.start.line, of.node.start.column);
        if let Some(known) = self.method_names.get(&key) {
            return known.clone();
        }
        let names = self
            .methods_of_caller(of.node, of.file)
            .map(|methods| std::sync::Arc::new(methods.into_keys().collect::<std::collections::HashSet<String>>()));
        self.method_names.insert(key, names.clone());
        names
    }

    /// A type declaration as a message names it from inside: `G<T>` for a
    /// generic one. `None` for a class inside a generic one.
    fn shown_declaration(&self, found: &Found<'_>) -> Option<String> {
        let path = path_to(&found.file.tree, found.name.start);
        let outer_generic = path
            .iter()
            .filter(|node| is_type_declaration(node) && !std::ptr::eq(**node, found.node))
            .any(|node| node.child("type_parameters").is_some());
        if outer_generic {
            return None;
        }
        let plain = plain_name(found);
        let variables = type_variables(found.node);
        Some(if variables.is_empty() { plain } else { format!("{plain}<{}>", variables.join(",")) })
    }

    /// The declaration of the type an expression a method is called on
    /// has, or the type it names (`Validate.isTrue`), where it is certain,
    /// and the type as the message names it.
    fn receiver_type<'a>(&'a self, file: &'a FileView, object: &'a Syn) -> Option<(Found<'a>, String)> {
        // A type's name: a static call.
        let names_a_type = |found: &Found<'_>| is_type_declaration(found.node);
        if object.kind == "identifier"
            && self.name_in_scope(&object.text, file, object.start).is_none_or(|found| names_a_type(&found))
        {
            let found = self.type_named(&object.text, file)?;
            let certain = is_type_declaration(found.node) && self.is_what_file_names(&found, &object.text, file);
            let shown = plain_name(&found);
            return certain.then_some((found, shown));
        }
        let (written, written_in, spread) = self.where_type_is_written(object, file)?;
        if spread {
            return None;
        }
        // A type variable is no type the sources declare.
        let type_variable = path_to(&written_in.tree, written.start)
            .into_iter()
            .any(|node| type_variables(node).contains(&written.text));
        if type_variable {
            return None;
        }
        let found = self.written_type(written, written_in)?;
        // Type variables of another file's declaration stand for what the
        // call binds them to there: not said here.
        let shown = self.rendered(written, written_in, written_in.uri == file.uri)?;
        Some((found, shown))
    }

    /// The declaration a type as written means in the file it is written
    /// in, where Java's rules alone say so.
    fn written_type<'a>(&'a self, written: &Syn, written_in: &'a FileView) -> Option<Found<'a>> {
        let base = match written.kind.as_str() {
            "generic_type" => written.children.first()?,
            "type_identifier" | "scoped_type_identifier" => written,
            _ => return None,
        };
        let name: String = base.text.split_whitespace().collect();
        let found = self.type_named(&name, written_in)?;
        self.is_written_as(&found, &name, written_in).then_some(found)
    }

    /// A type as written, as a message names it: `Map<String,List<T>>`,
    /// `Document.OutputSettings`, `int[]`. Type variables only where
    /// `variables` allows them; nothing for a wildcard, which the compiler
    /// names as a capture.
    fn rendered(&self, written: &Syn, written_in: &FileView, variables: bool) -> Option<String> {
        match written.kind.as_str() {
            "integral_type" | "floating_point_type" | "boolean_type" => Some(written.text.clone()),
            "type_identifier" | "scoped_type_identifier" => {
                let in_scope = path_to(&written_in.tree, written.start)
                    .into_iter()
                    .any(|node| type_variables(node).contains(&written.text));
                if in_scope {
                    return variables.then(|| written.text.clone());
                }
                let found = self.written_type(written, written_in)?;
                if found.node.child("type_parameters").is_some() {
                    // A raw type.
                    return Some(plain_name(&found));
                }
                self.shown_declaration(&found)
            }
            "generic_type" => {
                let base = self.written_type(written, written_in)?;
                let arguments = written.child("type_arguments").or_else(|| written.children.get(1))?;
                let rendered = arguments
                    .children
                    .iter()
                    .filter(|child| !matches!(child.kind.as_str(), "<" | ">" | ","))
                    .map(|argument| self.rendered(argument, written_in, variables))
                    .collect::<Option<Vec<String>>>()?;
                Some(format!("{}<{}>", plain_name(&base), rendered.join(",")))
            }
            "array_type" => {
                let element = written.child("element")?;
                let dimensions = written.child("dimensions")?.text.matches('[').count();
                Some(format!("{}{}", self.rendered(element, written_in, variables)?, "[]".repeat(dimensions)))
            }
            _ => None,
        }
    }

    /// Whether `found` is what `written_in` means by `name`, a simple name
    /// or one of a nested type (`Document.OutputSettings`).
    fn is_written_as(&self, found: &Found<'_>, name: &str, written_in: &FileView) -> bool {
        let Some((outer, inner)) = name.split_once('.') else {
            return found.name.text == name && self.is_what_file_names(found, name, written_in);
        };
        let Some(outer_type) = self.type_named(outer, written_in) else {
            return false;
        };
        self.is_what_file_names(&outer_type, outer, written_in)
            && self.qualified_type_name(found.file, found.node)
                == format!("{}.{inner}", self.qualified_type_name(outer_type.file, outer_type.node))
    }

    /// The type node an expression's type is written as, the file it is
    /// written in, and whether it is a varargs parameter (an array of it):
    /// a variable's declared type, a method's return type, a created type.
    fn where_type_is_written<'a>(&'a self, expression: &'a Syn, file: &'a FileView) -> Option<(&'a Syn, &'a FileView, bool)> {
        let (declaration, in_file): (&Syn, &FileView) = match expression.kind.as_str() {
            "identifier" => {
                let found = self.name_in_scope(&expression.text, file, expression.start)?;
                // A lambda's parameter has the type it is inferred to have.
                if found.node.kind == "lambda_expression" {
                    return None;
                }
                let path = path_to(&found.file.tree, found.name.start);
                let declaration = path.iter().rev().find(|node| {
                    matches!(
                        node.kind.as_str(),
                        "local_variable_declaration"
                            | "formal_parameter"
                            | "spread_parameter"
                            | "field_declaration"
                            | "catch_formal_parameter"
                            | "resource"
                            | "enhanced_for_statement"
                    )
                })?;
                // `int a[]`: the dimensions are the name's.
                if found.node.child("dimensions").is_some() {
                    return None;
                }
                (*declaration, found.file)
            }
            "method_invocation" => {
                let method = self.invoked(expression, file)?;
                if method.node.child("dimensions").is_some() {
                    return None;
                }
                (method.node, method.file)
            }
            "object_creation_expression" | "cast_expression" => (expression, file),
            "parenthesized_expression" => {
                let inner = expression.children.iter().find(|child| !matches!(child.kind.as_str(), "(" | ")"))?;
                return self.where_type_is_written(inner, file);
            }
            _ => return None,
        };
        if declaration.kind == "spread_parameter" {
            let written = declaration.children.iter().find(|child| {
                matches!(child.kind.as_str(), "type_identifier" | "scoped_type_identifier" | "generic_type" | "integral_type" | "floating_point_type" | "boolean_type")
            })?;
            return Some((written, in_file, true));
        }
        let written = declaration.child("type")?;
        (written.text != "var").then_some((written, in_file, false))
    }

    /// Whether `found` is the type `name` means in `file` by Java's rules
    /// alone: a type the file declares, a single-type import, a top-level
    /// type of its package, of an on-demand import, of `java.lang`. A name
    /// only an inherited member type or a guess explains is not certain.
    pub(super) fn is_what_file_names(&self, found: &Found<'_>, name: &str, file: &FileView) -> bool {
        let same = |other: &Found<'_>| other.file.uri == found.file.uri && other.node.start == found.node.start;
        if let Some(declared) = self.declared_type(file, name) {
            return found.file.uri == file.uri && found.node.start == declared.start;
        }
        let all_imports = imports(file);
        if let Some(import) = all_imports.iter().find(|import| !import.is_static && !import.wildcard && import.last_segment() == name) {
            return self.qualified_type_name(found.file, found.node) == import.name;
        }
        let qualified = self.qualified_type_name(found.file, found.node);
        let own = self.package(file).unwrap_or_default();
        let mut packages = vec![own];
        packages.extend(all_imports.iter().filter(|import| import.wildcard && !import.is_static).map(|import| import.name.clone()));
        packages.push("java.lang".to_owned());
        // The first package with a top-level type of the name decides.
        for package in packages {
            let full = if package.is_empty() { name.to_owned() } else { format!("{package}.{name}") };
            if let Some(candidate) = self.type_named(&full, file)
                && self.qualified_type_name(candidate.file, candidate.node) == full
            {
                return same(&candidate) && qualified == full;
            }
        }
        false
    }

    /// The type of an argument as the compiler writes it in a message.
    fn argument_type_in_message(&self, argument: &Syn, file: &FileView) -> Option<String> {
        let boolean = match argument.kind.as_str() {
            "binary_expression" => argument.children.iter().any(|child| {
                matches!(child.kind.as_str(), "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||")
            }),
            "instanceof_expression" => true,
            "unary_expression" => argument.children.first().is_some_and(|operator| operator.kind == "!"),
            _ => false,
        };
        if boolean {
            return Some("boolean".to_owned());
        }
        if argument.kind == "parenthesized_expression" {
            let inner = argument.children.iter().find(|child| !matches!(child.kind.as_str(), "(" | ")"))?;
            return self.argument_type_in_message(inner, file);
        }
        if argument.kind == "this" {
            let declaration = path_to(&file.tree, argument.start)
                .into_iter()
                .rev()
                .find(|node| is_type_declaration(node) || is_anonymous_class(node))
                .filter(|node| is_type_declaration(node))?;
            return self.shown_declaration(&self.declared(file, declaration)?);
        }
        let literal = argument.kind.ends_with("_literal") || matches!(argument.kind.as_str(), "true" | "false");
        if literal {
            return self.argument_type_for_message(argument, file);
        }
        // `a + 1`, `"x" + a`: string concatenation, numeric promotion.
        if argument.kind == "binary_expression" && argument.children.len() == 3 {
            let operator = argument.children[1].kind.as_str();
            let left = self.argument_type_in_message(&argument.children[0], file)?;
            let right = self.argument_type_in_message(&argument.children[2], file)?;
            if operator == "+" && (left == "String" || right == "String") {
                return Some("String".to_owned());
            }
            if matches!(operator, "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^") {
                return promoted(&left, &right);
            }
            if matches!(operator, "<<" | ">>" | ">>>") {
                return promoted(&left, "int");
            }
            return None;
        }
        if argument.kind == "unary_expression" {
            let operator = argument.children.first()?.kind.as_str();
            let operand = self.argument_type_in_message(argument.children.get(1)?, file)?;
            return matches!(operator, "-" | "+" | "~").then(|| promoted(&operand, "int")).flatten();
        }
        // An enum constant: its enum.
        if argument.kind == "identifier"
            && let Some(found) = self.name_in_scope(&argument.text, file, argument.start)
            && found.node.kind == "enum_constant"
        {
            let declaration = path_to(&found.file.tree, found.node.start)
                .into_iter()
                .rev()
                .find(|node| node.kind == "enum_declaration")?;
            return self.shown_declaration(&self.declared(found.file, declaration)?);
        }
        // A call, a variable: its type where the type is certain.
        if matches!(argument.kind.as_str(), "method_invocation" | "identifier" | "object_creation_expression" | "cast_expression") {
            let (written, written_in, spread) = self.where_type_is_written(argument, file)?;
            let shown = self.rendered(written, written_in, written_in.uri == file.uri)?;
            return Some(if spread { format!("{shown}[]") } else { shown });
        }
        None
    }

    /// A cast to a type variable of something that is not of it.
    fn unchecked_cast(&self, file: &FileView, cast: &Syn) -> Option<Report> {
        let target = cast.child("type")?;
        if target.kind != "type_identifier" {
            return None;
        }
        let path = path_to(&file.tree, cast.start);
        let is_type_variable = path.iter().any(|node| type_variables(node).contains(&target.text));
        if !is_type_variable {
            return None;
        }
        let value = cast.child("value")?;
        let from = self.type_of_expression(value, file)?;
        if from.name == target.text || !from.arguments.is_empty() || from.dimensions > 0 || from.name.is_empty() {
            return None;
        }
        // From a type variable of its own, or a primitive: not this.
        if path.iter().any(|node| type_variables(node).contains(&from.name))
            || from.name.starts_with(|character: char| character.is_lowercase())
        {
            return None;
        }
        // Only from the variable's bound, a type of no parameters of its
        // own (`T extends Node`, from `Node`), is it said for certain.
        let bound = path.iter().find_map(|node| {
            node.child("type_parameters")?.all("type_parameter").find_map(|parameter| {
                let named = parameter.children.iter().find(|child| matches!(child.kind.as_str(), "identifier" | "type_identifier"))?;
                (named.text == target.text).then(|| parameter.first("type_bound").map(|bound| bound.text.clone()))
            })
        })?;
        let bound = bound?;
        if bound.trim_start_matches("extends").trim() != from.name {
            return None;
        }
        Some(Report {
            finding: Finding {
                at: cast.start,
                end: Some(cast.end),
                message: format!("Type safety: Unchecked cast from {} to {}", from.written(), target.text),
            },
            level: Level::Warning,
            unnecessary: false,
        })
    }
}

/// The names of the type variables a declaration declares.
fn type_variables(declaration: &Syn) -> Vec<String> {
    declaration
        .child("type_parameters")
        .map(|parameters| {
            parameters
                .all("type_parameter")
                .filter_map(|parameter| {
                    parameter
                        .children
                        .iter()
                        .find(|child| matches!(child.kind.as_str(), "identifier" | "type_identifier"))
                        .map(|name| name.text.clone())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `label` labels a case of `body` itself, not of a switch inside.
fn is_label_of(body: &Syn, label: &Syn) -> bool {
    path_to(body, label.start)
        .iter()
        .filter(|node| node.kind == "switch_block" && !std::ptr::eq(**node, body))
        .count()
        == 0
}

/// A type as the compiler names it in a message: `Outer.Inner` for a
/// member type; `None` for a generic one, named with its parameters.
fn readable_name(of: &Found<'_>) -> Option<String> {
    let path = path_to(&of.file.tree, of.name.start);
    path.iter()
        .filter(|node| is_type_declaration(node))
        .map(|node| {
            let generic = node.child("type_parameters").is_some();
            (!generic).then(|| node.child("name").map(|name| name.text.clone()))?
        })
        .collect::<Option<Vec<String>>>()
        .map(|names| names.join("."))
}

/// The type of a numeric operation on two operand types: `int` at least.
fn promoted(left: &str, right: &str) -> Option<String> {
    const ORDER: &[&str] = &["int", "long", "float", "double"];
    let rank = |name: &str| match name {
        "byte" | "short" | "char" | "int" => Some(0),
        _ => ORDER.iter().position(|known| *known == name),
    };
    Some(ORDER[rank(left)?.max(rank(right)?)].to_owned())
}

/// A type's name with the types it is nested in: `Token.Doctype`.
fn plain_name(of: &Found<'_>) -> String {
    path_to(&of.file.tree, of.name.start)
        .iter()
        .filter(|node| is_type_declaration(node))
        .filter_map(|node| node.child("name").map(|name| name.text.clone()))
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether a declaration says `@Deprecated`.
fn says_deprecated(declaration: &Syn) -> bool {
    declaration.first("modifiers").is_some_and(|modifiers| {
        modifiers.children.iter().any(|modifier| {
            matches!(modifier.kind.as_str(), "marker_annotation" | "annotation")
                && modifier
                    .child("name")
                    .is_some_and(|name| matches!(name.text.as_str(), "Deprecated" | "java.lang.Deprecated"))
        })
    })
}

/// The members of a deprecated type that do not say so themselves, but
/// for the private ones.
fn deprecated_enclosing_type(file: &FileView) -> Vec<Report> {
    let mut reports = Vec::new();
    for declaration in file.tree.walk() {
        if !is_type_declaration(declaration) || !says_deprecated(declaration) {
            continue;
        }
        let Some(type_name) = declaration.child("name").and_then(|name| {
            readable_name(&Found { file, node: declaration, name, bindings: super::Bindings::new() })
        }) else {
            continue;
        };
        let Some(body) = declaration.child("body") else {
            continue;
        };
        let members = body.children.iter().chain(
            body.children
                .iter()
                .filter(|child| child.kind == "enum_body_declarations")
                .flat_map(|declarations| declarations.children.iter()),
        );
        for member in members {
            let place = match member.kind.as_str() {
                "method_declaration" | "constructor_declaration" => {
                    let (Some(name), Some(parameters)) = (member.child("name"), member.child("parameters")) else {
                        continue;
                    };
                    let end = member.child("throws").or(member.first("throws")).map_or(parameters.end, |throws| throws.end);
                    vec![(name.start, end)]
                }
                "field_declaration" | "constant_declaration" => member
                    .children
                    .iter()
                    .filter(|child| child.kind == "variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .map(|name| (name.start, name.end))
                    .collect(),
                "enum_constant" => member.child("name").map(|name| (name.start, name.end)).into_iter().collect(),
                _ => continue,
            };
            if says_deprecated(member) {
                continue;
            }
            let modifiers = member.first("modifiers").filter(|modifiers| modifiers.start == member.start || member.children.first().is_some_and(|first| std::ptr::eq(first, *modifiers)));
            let private = modifiers.is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "private"));
            if private {
                continue;
            }
            for (start, end) in place {
                reports.push(Report {
                    finding: Finding {
                        at: start,
                        end: Some(end),
                        message: format!(
                            "The enclosing type {type_name} is deprecated, perhaps this member should be marked as deprecated, too?"
                        ),
                    },
                    level: Level::Information,
                    unnecessary: false,
                });
            }
        }
    }
    reports
}

/// `@SuppressWarnings("unused")` that suppresses none of the `unused`
/// checks Eclipse makes: jdtls leaves some of them off, so Eclipse cannot
/// say whether it suppresses anything. Where a private member or a local
/// under it is named nowhere else, it may suppress that, and nothing is
/// said.
fn unused_not_analysed(file: &FileView) -> Vec<Report> {
    let mut mentions: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for node in file.tree.walk() {
        if node.kind == "identifier" {
            *mentions.entry(node.text.as_str()).or_default() += 1;
        }
    }
    let unused_inside = |declaration: &Syn| {
        declaration.walk().into_iter().any(|node| {
            let private = node
                .first("modifiers")
                .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "private"));
            let names: Vec<&Syn> = match node.kind.as_str() {
                "field_declaration" if private => node
                    .children
                    .iter()
                    .filter(|child| child.kind == "variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .collect(),
                "local_variable_declaration" => node
                    .children
                    .iter()
                    .filter(|child| child.kind == "variable_declarator")
                    .filter_map(|declarator| declarator.child("name"))
                    .collect(),
                "method_declaration" | "constructor_declaration" | "class_declaration" if private => {
                    node.child("name").into_iter().collect()
                }
                _ => Vec::new(),
            };
            names.iter().any(|name| mentions.get(name.text.as_str()).copied().unwrap_or_default() <= 1)
        })
    };
    let mut reports = Vec::new();
    for annotation in file.tree.walk() {
        let suppresses = annotation.kind == "annotation"
            && annotation.child("name").is_some_and(|name| name.text == "SuppressWarnings");
        if !suppresses {
            continue;
        }
        let Some(literal) = annotation
            .walk()
            .into_iter()
            .find(|node| node.kind == "string_literal" && node.text == "\"unused\"")
        else {
            continue;
        };
        // The declaration the annotation is on.
        let declaration = path_to(&file.tree, annotation.start)
            .into_iter()
            .rev()
            .find(|node| node.start <= annotation.start && node.kind != "modifiers" && node.children.iter().any(|child| child.kind == "modifiers" && child.start <= annotation.start && annotation.end <= child.end));
        if declaration.is_none_or(&unused_inside) {
            continue;
        }
        reports.push(Report {
            finding: Finding {
                at: literal.start,
                end: Some(literal.end),
                message: "At least one of the problems in category 'unused' is not analysed due to a compiler option being ignored".to_owned(),
            },
            level: Level::Information,
            unnecessary: false,
        });
    }
    reports
}
