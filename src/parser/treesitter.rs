//! What every tree-sitter grammar shares: parsing, syntax errors, names
//! at a position, folding and the `Syn` snapshot. A language describes
//! its own node kinds in a [`Grammar`]; its module (`java`, `kotlin`)
//! implements `Syntax` with these and with what only it knows.

use tree_sitter::{Node, Parser, Tree};

use super::{position_of, Finding, Fold, Name, Position, Syn};

/// The node kinds of one grammar that the shared readers ask about.
pub struct Grammar {
    /// The tree-sitter grammar.
    pub language: fn() -> tree_sitter::Language,
    /// What an identifier is.
    pub names: &'static [&'static str],
    /// What a `package` or `import` declaration is: the names in it are
    /// outside the file.
    pub package_or_import: &'static [&'static str],
    pub folds: FoldKinds,
}

/// What folds, by node kind.
pub struct FoldKinds {
    /// Declarations whose body folds from the declaration's own line.
    pub members: &'static [&'static str],
    /// A node between a member and its block (Kotlin's `function_body`).
    pub body_wrappers: &'static [&'static str],
    /// Brace-delimited bodies that fold from their own brace.
    pub blocks: &'static [&'static str],
    /// A `switch` block, whose arms fold one by one.
    pub switch_blocks: &'static [&'static str],
    /// An arm of a `switch`: the block it wraps folds as the arm.
    pub switch_arms: &'static [&'static str],
    pub block_comments: &'static [&'static str],
}

impl Grammar {
    pub fn parse(&self, text: &str) -> Option<Tree> {
        let mut parser = Parser::new();
        parser.set_language(&(self.language)()).ok()?;
        parser.parse(text, None)
    }

    /// The whole file as a `Syn`, as the grammar parses it.
    pub fn snapshot(&self, text: &str) -> Option<Syn> {
        let tree = self.parse(text)?;
        Some(snapshot(tree.root_node(), None, text))
    }

    pub fn syntax_errors(&self, text: &str) -> Vec<Finding> {
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut findings = Vec::new();
        collect_errors(tree.root_node(), text, &mut findings);
        findings.sort_by(|left, right| left.at.cmp(&right.at));
        findings.dedup();
        findings
    }

    pub fn name_at(&self, text: &str, cursor: Position) -> Option<Name> {
        let tree = self.parse(text)?;
        let node = self.identifier_at(tree.root_node(), text, cursor)?;
        Some(Name {
            text: node.utf8_text(text.as_bytes()).ok()?.to_owned(),
            start: at(node.start_byte(), text),
            end: at(node.end_byte(), text),
            in_package_or_import: self.inside_package_or_import(node),
        })
    }

    pub fn folds(&self, text: &str) -> Vec<Fold> {
        let Some(tree) = self.parse(text) else {
            return Vec::new();
        };
        let mut folds = Vec::new();
        self.collect_folds(tree.root_node(), text, &mut folds, &mut Vec::new());
        folds.extend(line_runs(text));
        folds.sort_unstable();
        folds.dedup();
        folds
    }

    /// The identifier at `at`, or the one that ends there.
    fn identifier_at<'tree>(&self, root: Node<'tree>, text: &str, at: Position) -> Option<Node<'tree>> {
        let offset = offset_of(text, at)?;
        let is_name = |node: &Node| self.names.contains(&node.kind());
        root.named_descendant_for_byte_range(offset, offset)
            .filter(is_name)
            .or_else(|| {
                let before = offset.saturating_sub(1);
                root.named_descendant_for_byte_range(before, before)
                    .filter(is_name)
            })
    }

    fn inside_package_or_import(&self, node: Node) -> bool {
        let mut current = Some(node);
        while let Some(ancestor) = current {
            if self.package_or_import.contains(&ancestor.kind()) {
                return true;
            }
            current = ancestor.parent();
        }
        false
    }

    /// Every body, every block comment, every switch arm. A member's body
    /// folds from the declaration (`static\n{` folds from the `static`); a
    /// statement's block folds from its brace.
    fn collect_folds(
        &self,
        node: Node,
        text: &str,
        folds: &mut Vec<Fold>,
        inside_member: &mut Vec<usize>,
    ) {
        let kinds = &self.folds;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(body) = self.member_body(child) {
                inside_member.push(body.start_byte());
                folds.extend(span(at(child.start_byte(), text).line, body, text));
            }
            let folded_by_its_arm = kinds.switch_arms.contains(&node.kind());
            let is_block = kinds.blocks.contains(&child.kind());
            if is_block && !folded_by_its_arm && !inside_member.contains(&child.start_byte()) {
                folds.extend(span(at(child.start_byte(), text).line, child, text));
            }
            if kinds.switch_blocks.contains(&child.kind()) {
                folds.extend(self.switch_arms(child, text));
            }
            if kinds.block_comments.contains(&child.kind()) {
                let first = at(child.start_byte(), text).line;
                let last = at(child.end_byte().saturating_sub(1), text).line;
                if last > first {
                    folds.push(Fold {
                        first_line: first,
                        last_line: last,
                    });
                }
            }
            self.collect_folds(child, text, folds, inside_member);
        }
    }

    /// The body a declaration folds from its own first line.
    fn member_body<'tree>(&self, node: Node<'tree>) -> Option<Node<'tree>> {
        if !self.folds.members.contains(&node.kind()) {
            return None;
        }
        let wrapped = self
            .folds
            .body_wrappers
            .iter()
            .find_map(|wrapper| first_of_kind(node, wrapper));
        if let Some(wrapper) = wrapped {
            return first_of_kind(wrapper, "block");
        }
        node.child_by_field_name("body")
            .or_else(|| first_of_kind(node, "block"))
            .or_else(|| first_of_kind(node, "constructor_body"))
    }

    /// One region per arm of a `switch` block, each reaching to the line
    /// before the next arm: an arm's own statements stop earlier, and the
    /// blank lines between two arms belong to the one above.
    fn switch_arms(&self, block: Node, text: &str) -> Vec<Fold> {
        let mut cursor = block.walk();
        let arms: Vec<Node> = block
            .children(&mut cursor)
            .filter(|child| self.folds.switch_arms.contains(&child.kind()))
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
}

/// A file that would not parse: an empty program.
pub fn empty_tree() -> Syn {
    Syn {
        kind: "program".to_owned(),
        field: None,
        text: String::new(),
        start: Position { line: 0, column: 0 },
        end: Position { line: 0, column: 0 },
        children: Vec::new(),
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

pub fn at(offset: usize, text: &str) -> Position {
    position_of(text, offset)
}

/// The position of a byte of `text` tree-sitter has placed at `point`:
/// only its own line is read, so a whole tree's positions take a time in
/// proportion to the file, not to its square.
fn at_point(offset: usize, point: tree_sitter::Point, text: &str) -> Position {
    let offset = offset.min(text.len());
    let line_start = offset.saturating_sub(point.column);
    let column = text
        .get(line_start..offset)
        .map_or(point.column, |line| line.encode_utf16().count());
    Position {
        line: point.row as u32,
        column: column as u32,
    }
}

pub fn first_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|child| child.kind() == kind)
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

/// `node` and everything below it, as a `Syn`.
pub fn snapshot(node: Node, field: Option<&str>, text: &str) -> Syn {
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
        start: at_point(node.start_byte(), node.start_position(), text),
        end: at_point(node.end_byte(), node.end_position(), text),
        children,
    }
}
