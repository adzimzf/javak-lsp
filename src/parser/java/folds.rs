//! Java's foldable regions, found as jdtls finds them.
//!
//! - **Comments**: each block comment and javadoc, even on one line, and
//!   each run of `//` lines longer than one line. A `//` after code on its
//!   line, or on the file's first line, is not in a run.
//! - **Regions** between `// region` and `// endregion` (`#region`,
//!   `<editor-fold>` and their ends too).
//! - **The imports**, first to last.
//! - **Types**, from their name's line to their closing brace, member types
//!   included; **methods and initializers** the same way, even on one line.
//!   A field is not a region, nor is anything in its initializer.
//! - **Inside a method**, read token by token: each block longer than one
//!   line, up to the line of its `}`, or the line before when another
//!   block opens on that line (`} else {`). A `switch`'s arms reach to the
//!   line before the next arm, the last one to the line before the
//!   switch's `}`. A block that opens on the line after its `case` folds
//!   from the `case`.

use std::collections::BTreeMap;

use tree_sitter::Node;

use super::GRAMMAR;
use crate::parser::{Fold, FoldKind};

pub fn folds(text: &str) -> Vec<Fold> {
    let Some(tree) = GRAMMAR.parse(text) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let lines = LineIndex::of(text);
    let mut tokens = Vec::new();
    collect_tokens(root, &mut tokens);
    let mut folds = Vec::new();
    file_ranges(&tokens, text, &lines, &mut folds);
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if is_type(child) {
            type_ranges(child, &tokens, &lines, &mut folds);
        }
    }
    // As jdtls, a type and a method on the same line are two regions.
    folds.sort_unstable();
    folds
}

/// Comments, regions and the imports: what the whole file's tokens show.
fn file_ranges(tokens: &[Node], text: &str, lines: &LineIndex, folds: &mut Vec<Fold>) {
    let mut imports: Option<(u32, u32)> = None;
    // A run of `//` lines, first to last.
    let mut run: Option<(u32, u32)> = None;
    // As jdtls reads the file, a token before the first stands on the
    // first line: a `//` there is a comment after code.
    let mut previous_line: Option<u32> = Some(0);
    let mut regions: Vec<u32> = Vec::new();
    for token in tokens {
        let line = lines.line(token.start_byte());
        match token.kind() {
            "block_comment" => folds.push(Fold {
                first_line: line,
                last_line: lines.line(token.end_byte().saturating_sub(1)),
                kind: Some(FoldKind::Comment),
            }),
            "line_comment" => {
                let source = &text[token.start_byte()..token.end_byte()];
                match region_marker(source) {
                    Some(Marker::Opens) => regions.push(line),
                    Some(Marker::Closes) => {
                        if let Some(first_line) = regions.pop() {
                            folds.push(Fold {
                                first_line,
                                last_line: line,
                                kind: Some(FoldKind::Region),
                            });
                        }
                    }
                    None if previous_line == Some(line) => comment_run(run, folds),
                    None => match run {
                        Some((first, last)) if line == last + 1 => run = Some((first, line)),
                        _ => {
                            comment_run(run, folds);
                            run = Some((line, line));
                        }
                    },
                }
            }
            "import" => {
                let first = imports.map_or(line, |(first, _)| first);
                imports = Some((first, line));
            }
            _ => {}
        }
        previous_line = Some(line);
    }
    if let Some((first_line, last_line)) = imports {
        folds.push(Fold {
            first_line,
            last_line,
            kind: Some(FoldKind::Imports),
        });
    }
    comment_run(run, folds);
}

fn comment_run(run: Option<(u32, u32)>, folds: &mut Vec<Fold>) {
    if let Some((first_line, last_line)) = run.filter(|(first, last)| last > first) {
        folds.push(Fold {
            first_line,
            last_line,
            kind: Some(FoldKind::Comment),
        });
    }
}

enum Marker {
    Opens,
    Closes,
}

/// `// region`, `//#region` or `// <editor-fold ...>`, and their ends.
fn region_marker(comment: &str) -> Option<Marker> {
    let rest = comment.strip_prefix("//")?;
    let trimmed = rest.trim_start();
    let spaced = rest.len() > trimmed.len();
    let word = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if word.starts_with("endregion") || (spaced && trimmed.starts_with("</editor-fold>")) {
        return Some(Marker::Closes);
    }
    let editor_fold = spaced && trimmed.starts_with("<editor-fold") && trimmed.contains('>');
    if word.starts_with("region") || editor_fold {
        return Some(Marker::Opens);
    }
    None
}

fn is_type(node: Node) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

/// A type from its name to its end, and its members.
fn type_ranges(node: Node, tokens: &[Node], lines: &LineIndex, folds: &mut Vec<Fold>) {
    let name = node.child_by_field_name("name").unwrap_or(node);
    folds.push(Fold {
        first_line: lines.line(name.start_byte()),
        last_line: lines.line(node.end_byte()),
        kind: None,
    });
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "enum_body_declarations" {
            let mut inner = child.walk();
            members.extend(child.children(&mut inner));
        } else {
            members.push(child);
        }
    }
    for member in members {
        match member.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "static_initializer"
            | "block" => member_ranges(member, tokens, lines, folds),
            _ if is_type(member) => type_ranges(member, tokens, lines, folds),
            _ => {}
        }
    }
}

/// A method or initializer from its name to its end, and the blocks and
/// switch arms in it.
fn member_ranges(node: Node, tokens: &[Node], lines: &LineIndex, folds: &mut Vec<Fold>) {
    let named = node.child_by_field_name("name").unwrap_or(node);
    folds.push(Fold {
        first_line: lines.line(named.start_byte()),
        last_line: lines.line(node.end_byte()),
        kind: None,
    });
    let start = tokens.partition_point(|token| token.start_byte() < node.start_byte());
    let end = tokens.partition_point(|token| token.start_byte() < node.end_byte());
    let mut body_seen = false;
    let mut braces: Vec<u32> = Vec::new();
    // The line of each open switch's last arm; `None` before its first.
    let mut arms: Vec<Option<u32>> = Vec::new();
    let mut found: BTreeMap<u32, u32> = BTreeMap::new();
    let mut previous_line = lines.line(node.start_byte());
    for token in &tokens[start..end] {
        let line = lines.line(token.start_byte());
        match token.kind() {
            "{" if !body_seen => body_seen = true,
            "{" => {
                // A block that opens where another closes ends the line
                // before.
                let closing_here: Vec<u32> = found
                    .iter()
                    .filter(|(_, last)| **last == line)
                    .map(|(&first, _)| first)
                    .collect();
                for first in closing_here {
                    found.remove(&first);
                    if first < line.saturating_sub(1) {
                        found.insert(first, line - 1);
                    }
                }
                let after_arm = previous_line != line && arms.last() == Some(&Some(previous_line));
                braces.push(if after_arm { previous_line } else { line });
            }
            "}" => {
                let Some(first) = braces.pop() else {
                    previous_line = line;
                    continue;
                };
                let last = line;
                if first < last {
                    found.insert(first, last);
                }
                // The switch's own brace closes its last arm.
                if let Some(&Some(arm)) = arms.last()
                    && first < arm
                    && last.saturating_sub(1) > arm
                {
                    found.insert(arm, last - 1);
                    arms.pop();
                }
            }
            "switch" => arms.push(None),
            "case" | "default" => {
                if let Some(arm) = arms.pop() {
                    if let Some(first) = arm.filter(|&first| line > first) {
                        found.insert(first, line - 1);
                    }
                    arms.push(Some(line));
                }
            }
            _ => {}
        }
        previous_line = line;
    }
    folds.extend(found.into_iter().map(|(first_line, last_line)| Fold {
        first_line,
        last_line,
        kind: None,
    }));
}

fn collect_tokens<'t>(node: Node<'t>, tokens: &mut Vec<Node<'t>>) {
    let atomic = matches!(
        node.kind(),
        "string_literal" | "character_literal" | "line_comment" | "block_comment"
    );
    if node.child_count() == 0 || atomic {
        if node.end_byte() > node.start_byte() {
            tokens.push(node);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, tokens);
    }
}

/// The zero-based line of a byte offset.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn of(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        LineIndex { starts }
    }

    fn line(&self, offset: usize) -> u32 {
        (self.starts.partition_point(|&start| start <= offset) - 1) as u32
    }
}
