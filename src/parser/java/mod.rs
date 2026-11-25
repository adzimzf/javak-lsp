//! Java, read with tree-sitter-java. The tree the semantic layer reads is
//! the grammar's own: the layer is written against Java's node kinds.

use tree_sitter::Node;

mod errors;
mod folds;

use super::treesitter::{at, empty_tree, first_of_kind, FoldKinds, Grammar};
use super::{Finding, Fold, Kind, Name, Outline, Position, Syn, Syntax};

pub struct Java;

pub static GRAMMAR: Grammar = Grammar {
    language: || tree_sitter_java::LANGUAGE.into(),
    names: &["identifier", "type_identifier", "field_identifier"],
    package_or_import: &["package_declaration", "import_declaration"],
    folds: FoldKinds {
        members: &["method_declaration", "constructor_declaration", "static_initializer"],
        body_wrappers: &[],
        blocks: &[
            "class_body",
            "interface_body",
            "enum_body",
            "annotation_type_body",
            "block",
            "constructor_body",
            "switch_block",
        ],
        switch_blocks: &["switch_block"],
        switch_arms: &["switch_block_statement_group", "switch_rule"],
        block_comments: &["block_comment"],
    },
};

impl Syntax for Java {
    fn syntax_errors(&self, text: &str) -> Vec<Finding> {
        errors::syntax_errors(text)
    }

    fn above_java_level(&self, tree: &Syn, level: u32) -> Vec<Finding> {
        errors::above_java_level(tree, level)
    }

    fn package(&self, text: &str) -> Option<String> {
        let tree = GRAMMAR.parse(text)?;
        let mut cursor = tree.root_node().walk();
        let declaration = tree
            .root_node()
            .children(&mut cursor)
            .find(|child| child.kind() == "package_declaration")?;
        let mut names = declaration.walk();
        let name = declaration
            .children(&mut names)
            .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))?;
        Some(name.utf8_text(text.as_bytes()).ok()?.to_owned())
    }

    fn outline(&self, text: &str) -> Vec<Outline> {
        let Some(tree) = GRAMMAR.parse(text) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        collect_outline(tree.root_node(), text, &mut entries);
        entries.sort_by_key(|entry| entry.selection);
        entries
    }

    fn tree(&self, text: &str) -> Syn {
        GRAMMAR.snapshot(text).unwrap_or_else(empty_tree)
    }

    fn name_at(&self, text: &str, cursor: Position) -> Option<Name> {
        GRAMMAR.name_at(text, cursor)
    }

    fn declared_names(&self, text: &str) -> Vec<String> {
        let Some(tree) = GRAMMAR.parse(text) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        names_declared(tree.root_node(), text, &mut names);
        names.sort();
        names.dedup();
        names
    }

    fn folds(&self, text: &str) -> Vec<Fold> {
        folds::folds(text)
    }
}

fn collect_outline(node: Node, text: &str, entries: &mut Vec<Outline>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        entries.extend(outline_of(child, text));
        collect_outline(child, text, entries);
    }
}

/// A declaration is one entry, except a field declaration: `int a, b;`
/// declares two.
fn outline_of(node: Node, text: &str) -> Vec<Outline> {
    if node.kind() == "field_declaration" {
        return fields_of(node, text);
    }
    let kind = match node.kind() {
        "class_declaration" | "record_declaration" => Kind::Class,
        "interface_declaration" | "annotation_type_declaration" => Kind::Interface,
        "enum_declaration" => Kind::Enum,
        "enum_constant" => Kind::EnumMember,
        "method_declaration" => Kind::Method,
        "constructor_declaration" | "compact_constructor_declaration" => Kind::Constructor,
        _ => return Vec::new(),
    };
    node.child_by_field_name("name")
        .into_iter()
        .filter_map(|name| named(node, name, kind, text))
        .collect()
}

/// Each name a field declaration declares. A `static final` field is a
/// constant; anything else is a field.
fn fields_of(node: Node, text: &str) -> Vec<Outline> {
    let modifiers = node
        .child_by_field_name("modifiers")
        .or_else(|| first_of_kind(node, "modifiers"))
        .and_then(|node| node.utf8_text(text.as_bytes()).ok())
        .unwrap_or_default();
    let is_constant = modifiers.contains("static") && modifiers.contains("final");
    let kind = if is_constant { Kind::Constant } else { Kind::Field };
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "variable_declarator")
        .filter_map(|declarator| {
            let name = declarator.child_by_field_name("name")?;
            named(node, name, kind, text)
        })
        .collect()
}

fn named(node: Node, name: Node, kind: Kind, text: &str) -> Option<Outline> {
    Some(Outline {
        name: name.utf8_text(text.as_bytes()).ok()?.to_owned(),
        kind,
        selection: at(name.start_byte(), text),
        start: at(node.start_byte(), text),
        end: at(node.end_byte(), text),
    })
}

/// The declarations whose `name` field names something the file declares.
const DECLARES_A_NAME: &[&str] = &[
    "class_declaration",
    "record_declaration",
    "interface_declaration",
    "annotation_type_declaration",
    "enum_declaration",
    "enum_constant",
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
    "variable_declarator",
    "formal_parameter",
    "spread_parameter",
    "catch_formal_parameter",
    "type_parameter",
    "record_pattern_component",
    "type_pattern",
    "inferred_parameter",
    "lambda_expression",
    "annotation_type_element_declaration",
];

fn names_declared(node: Node, text: &str, names: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if DECLARES_A_NAME.contains(&child.kind()) {
            let named = match child.kind() {
                // A lambda's parameters and a type parameter are named by
                // position, not by a `name` field.
                "lambda_expression" => child.child_by_field_name("parameters"),
                "type_parameter" => first_of_kind(child, "type_identifier"),
                _ => child.child_by_field_name("name"),
            };
            if let Some(name) = named {
                names.extend(identifiers_of(name, text));
            }
        }
        if child.kind() == "record_declaration" {
            if let Some(parameters) = child.child_by_field_name("parameters") {
                names.extend(identifiers_of(parameters, text));
            }
        }
        names_declared(child, text, names);
    }
}

/// Every identifier `node` is or holds: a name is one, a parameter list of
/// a lambda is each of its names.
fn identifiers_of(node: Node, text: &str) -> Vec<String> {
    if is_name(node) {
        return node
            .utf8_text(text.as_bytes())
            .ok()
            .map(|name| vec![name.to_owned()])
            .unwrap_or_default();
    }
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        names.extend(identifiers_of(child, text));
    }
    names
}

fn is_name(node: Node) -> bool {
    GRAMMAR.names.contains(&node.kind())
}
