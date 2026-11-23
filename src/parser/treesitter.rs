//! The tree-sitter implementation of `Syntax`, one grammar per language.

use tree_sitter::{Node, Parser, Tree};

use super::{position_of, Finding, Fold, Kind, Name, Outline, Position, Syn, Syntax};

/// A language the server reads. The grammar is behind `Syntax`, so the
/// server never names tree-sitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Java,
    Kotlin,
}

pub struct TreeSitter {
    language: Language,
}

impl TreeSitter {
    pub fn new(language: Language) -> Self {
        Self { language }
    }

    fn parse(&self, text: &str) -> Option<Tree> {
        let mut parser = Parser::new();
        let grammar = match self.language {
            // ponytail: Kotlin reads as nothing until its grammar is wired
            // up; Java is the iteration's language.
            Language::Java | Language::Kotlin => tree_sitter_java::LANGUAGE,
        };
        parser.set_language(&grammar.into()).ok()?;
        parser.parse(text, None)
    }
}

impl Syntax for TreeSitter {
    fn syntax_errors(&self, text: &str) -> Vec<Finding> {
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut findings = Vec::new();
        collect_errors(tree.root_node(), text, &mut findings);
        findings.sort_by(|left, right| left.at.cmp(&right.at));
        findings.dedup();
        findings
    }

    fn package(&self, text: &str) -> Option<String> {
        let tree = self.parse(text)?;
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
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        collect_outline(tree.root_node(), text, &mut entries);
        entries.sort_by_key(|entry| entry.selection);
        entries
    }

    fn tree(&self, text: &str) -> Syn {
        match self.parse(text) {
            Some(tree) => snapshot(tree.root_node(), None, text),
            None => Syn {
                kind: "program".to_owned(),
                field: None,
                text: String::new(),
                start: Position { line: 0, column: 0 },
                end: Position { line: 0, column: 0 },
                children: Vec::new(),
            },
        }
    }

    fn name_at(&self, text: &str, cursor: Position) -> Option<Name> {
        let tree = self.parse(text)?;
        let node = identifier_at(tree.root_node(), text, cursor)?;
        Some(Name {
            text: node.utf8_text(text.as_bytes()).ok()?.to_owned(),
            start: at(node.start_byte(), text),
            end: at(node.end_byte(), text),
            in_package_or_import: inside_package_or_import(node),
        })
    }

    fn declared_names(&self, text: &str) -> Vec<String> {
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        names_declared(tree.root_node(), text, &mut names);
        names.sort();
        names.dedup();
        names
    }

    fn folds(&self, text: &str) -> Vec<Fold> {
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut folds = Vec::new();
        collect_folds(tree.root_node(), text, &mut folds, &mut Vec::new());
        folds.extend(line_runs(text));
        folds.sort_unstable();
        folds.dedup();
        folds
    }
}

/// A missing token is named; anything else the parser could not place is
/// shown as the source it could not place, first line only.
fn collect_errors(node: Node, text: &str, findings: &mut Vec<Finding>) {
    if node.is_missing() {
        findings.push(Finding {
            at: at(node.start_byte(), text),
            message: format!("missing `{}`", node.kind()),
        });
        return;
    }
    if node.is_error() {
        let source = node.utf8_text(text.as_bytes()).unwrap_or_default();
        let first_line = source.lines().next().unwrap_or_default().trim();
        findings.push(Finding {
            at: at(node.start_byte(), text),
            message: format!("unexpected `{first_line}`"),
        });
        return;
    }
    if !node.has_error() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, text, findings);
    }
}

fn at(offset: usize, text: &str) -> Position {
    position_of(text, offset)
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

fn first_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|child| child.kind() == kind)
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

/// Every body, every block comment, every switch arm, and the runs of
/// imports and line comments. A member's body folds from the declaration
/// (`static\n{` folds from the `static`); a statement's block folds from
/// its brace.
fn collect_folds(node: Node, text: &str, folds: &mut Vec<Fold>, inside_member: &mut Vec<usize>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(body) = member_body(child) {
            inside_member.push(body.start_byte());
            folds.extend(span(at(child.start_byte(), text).line, body, text));
        }
        let folded_by_its_arm = is_switch_arm(node);
        if is_block(child) && !folded_by_its_arm && !inside_member.contains(&child.start_byte()) {
            folds.extend(span(at(child.start_byte(), text).line, child, text));
        }
        if child.kind() == "switch_block" {
            folds.extend(switch_arms(child, text));
        }
        if child.kind() == "block_comment" {
            let first = at(child.start_byte(), text).line;
            let last = at(child.end_byte().saturating_sub(1), text).line;
            if last > first {
                folds.push(Fold {
                    first_line: first,
                    last_line: last,
                });
            }
        }
        collect_folds(child, text, folds, inside_member);
    }
}

/// The body a declaration folds from its own first line.
fn member_body<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let folds_from_its_declaration = matches!(
        node.kind(),
        "method_declaration" | "constructor_declaration" | "static_initializer"
    );
    if !folds_from_its_declaration {
        return None;
    }
    node.child_by_field_name("body")
        .or_else(|| first_of_kind(node, "block"))
        .or_else(|| first_of_kind(node, "constructor_body"))
}

/// `first_line` to where `node` ends: the line of its last brace, or the
/// line before it where code follows the brace (`} catch (…) {` belongs to
/// the clause that opens there, not the one that closed).
fn span(first_line: u32, node: Node, text: &str) -> Option<Fold> {
    let last = at(node.end_byte().saturating_sub(1), text).line;
    let rest = text
        .lines()
        .nth(last as usize)
        .map_or("", |line| line[closing_column(line)..].trim());
    let continues = !rest.is_empty() && !rest.starts_with("//") && !rest.starts_with("/*");
    let last_line = if continues { last.checked_sub(1)? } else { last };
    (last_line >= first_line).then_some(Fold {
        first_line,
        last_line,
    })
}

/// Just past the last `}` of `line`.
fn closing_column(line: &str) -> usize {
    line.rfind('}').map_or(line.len(), |brace| brace + 1)
}

/// One region per arm of a `switch` block, each reaching to the line
/// before the next arm: an arm's own statements stop earlier, and the
/// blank lines between two arms belong to the one above.
fn switch_arms(block: Node, text: &str) -> Vec<Fold> {
    let mut cursor = block.walk();
    let arms: Vec<Node> = block
        .children(&mut cursor)
        .filter(|child| is_switch_arm(*child))
        .collect();
    let block_end = at(block.end_byte().saturating_sub(1), text).line;
    arms.iter()
        .enumerate()
        .filter_map(|(index, arm)| {
            let first_line = at(arm.start_byte(), text).line;
            let next = arms.get(index + 1).map(|next| at(next.start_byte(), text).line);
            let last_line = next.unwrap_or(block_end).checked_sub(1)?;
            (last_line > first_line).then_some(Fold {
                first_line,
                last_line,
            })
        })
        .collect()
}

/// What folds by line alone: a run of imports, a run of `//` comments,
/// and the regions `// region`/`// endregion`, `// #region`/`//#endregion`
/// and `// <editor-fold>`/`// </editor-fold>` mark out.
fn line_runs(text: &str) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut imports: Option<(u32, u32)> = None;
    let mut comments: Option<(u32, u32)> = None;
    let mut regions: Vec<u32> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let index = index as u32;
        let line = line.trim();
        let marker = region_marker(line);
        let is_comment = line.starts_with("//") && marker.is_none();
        let is_import = line.starts_with("import ");
        extend(&mut imports, is_import, index, &mut folds);
        extend(&mut comments, is_comment, index, &mut folds);
        match marker {
            Some(Marker::Opens) => regions.push(index),
            Some(Marker::Closes) => {
                if let Some(first_line) = regions.pop() {
                    folds.push(Fold {
                        first_line,
                        last_line: index,
                    });
                }
            }
            None => {}
        }
    }
    close(imports, &mut folds);
    close(comments, &mut folds);
    folds
}

enum Marker {
    Opens,
    Closes,
}

fn region_marker(line: &str) -> Option<Marker> {
    let comment = line.strip_prefix("//")?.trim();
    let word = comment.trim_start_matches(['#', '<', '/', ' ']);
    if comment.starts_with("</") || word.starts_with("endregion") {
        return Some(Marker::Closes);
    }
    if word.starts_with("region") || word.starts_with("editor-fold") {
        return Some(Marker::Opens);
    }
    None
}

/// Carries a run of like lines along, closing it where it ends.
fn extend(run: &mut Option<(u32, u32)>, continues: bool, index: u32, folds: &mut Vec<Fold>) {
    if continues {
        *run = Some(run.map_or((index, index), |(first, _)| (first, index)));
        return;
    }
    close(run.take(), folds);
}

fn close(run: Option<(u32, u32)>, folds: &mut Vec<Fold>) {
    if let Some((first_line, last_line)) = run {
        if last_line > first_line {
            folds.push(Fold {
                first_line,
                last_line,
            });
        }
    }
}

/// A brace-delimited body: it folds from its own opening brace.
fn is_block(node: Node) -> bool {
    matches!(
        node.kind(),
        "class_body"
            | "interface_body"
            | "enum_body"
            | "annotation_type_body"
            | "block"
            | "constructor_body"
            | "switch_block"
    )
}

/// An arm of a `switch`: the block it wraps folds as the arm, not twice.
fn is_switch_arm(node: Node) -> bool {
    matches!(node.kind(), "switch_block_statement_group" | "switch_rule")
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

/// The identifier at `at`, or the one that ends there.
fn identifier_at<'tree>(root: Node<'tree>, text: &str, at: Position) -> Option<Node<'tree>> {
    let offset = offset_of(text, at)?;
    let found = root
        .named_descendant_for_byte_range(offset, offset)
        .filter(|node| is_name(*node))
        .or_else(|| {
            root.named_descendant_for_byte_range(offset.saturating_sub(1), offset.saturating_sub(1))
                .filter(|node| is_name(*node))
        })?;
    Some(found)
}

fn is_name(node: Node) -> bool {
    matches!(node.kind(), "identifier" | "type_identifier" | "field_identifier")
}

/// The byte offset of a UTF-16 position.
fn offset_of(text: &str, at: Position) -> Option<usize> {
    let mut line = 0;
    let mut column = 0;
    for (offset, character) in text.char_indices() {
        if line == at.line && column == at.column {
            return Some(offset);
        }
        if character == '\n' {
            if line == at.line {
                return Some(offset);
            }
            line += 1;
            column = 0;
        } else {
            column += character.len_utf16() as u32;
        }
    }
    (line == at.line && column == at.column).then_some(text.len())
}

/// Whether `node` sits inside a `package` or `import` declaration.
fn inside_package_or_import(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(ancestor) = current {
        if matches!(
            ancestor.kind(),
            "package_declaration" | "import_declaration"
        ) {
            return true;
        }
        current = ancestor.parent();
    }
    false
}

/// `node` and everything below it, as a `Syn`.
fn snapshot(node: Node, field: Option<&str>, text: &str) -> Syn {
    let mut cursor = node.walk();
    let children: Vec<Syn> = node
        .children(&mut cursor)
        .enumerate()
        .map(|(index, child)| {
            let field = node.field_name_for_child(index as u32);
            snapshot(child, field, text)
        })
        .collect();
    Syn {
        kind: node.kind().to_owned(),
        field: field.map(str::to_owned),
        text: node.utf8_text(text.as_bytes()).unwrap_or_default().to_owned(),
        start: at(node.start_byte(), text),
        end: at(node.end_byte(), text),
        children,
    }
}
