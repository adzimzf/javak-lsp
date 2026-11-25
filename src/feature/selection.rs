//! What `textDocument/selectionRange` answers for Java, as jdtls answers
//! it: every node of the Eclipse compiler's syntax tree (the JDT DOM) that
//! holds the cursor, innermost first, and before them the comment the
//! cursor is in.
//!
//! The tree read here is tree-sitter's, which is shaped differently, so it
//! is walked as the JDT DOM would be:
//!
//! - nodes JDT has no counterpart for are passed over: a class body, an
//!   argument list, a modifier list, the parentheses of an `if` condition,
//!   the groups of a `switch`;
//! - a member declaration starts at its javadoc, which JDT holds inside it;
//! - a type name is two nodes in JDT, the type and the name in it
//!   (`SimpleType` around `SimpleName`), and a lambda parameter a
//!   declaration around its name;
//! - `a + b + c` is one expression in JDT, not two;
//! - the file is the whole text.

use crate::parser::{Position, Syn};

/// A range from `.0` to `.1`.
pub type Span = (Position, Position);

/// The ranges around `at`, innermost first.
pub fn ranges(tree: &Syn, text: &str, at: Position) -> Vec<Span> {
    let path = jdt_path(tree, at);
    let Some(innermost) = path.last() else {
        return Vec::new();
    };
    // A member holds its javadoc: the cursor in it is in the member.
    if is_documented_kind(&innermost.kind) && at < innermost.start {
        let parent = path.len().checked_sub(2).map(|index| path[index]);
        if let Some(javadoc) = parent
            .and_then(|parent| javadoc_of(parent, innermost))
            .filter(|javadoc| holds((javadoc.start, javadoc.end), at))
        {
            let mut spans = javadoc_spans(javadoc, at);
            spans.push((javadoc.start, javadoc.end));
            spans.extend(spans_of(&path, text));
            return spans;
        }
    }
    if is_comment(innermost) {
        // A comment is no node: it comes before the nodes around it.
        let mut spans = vec![(innermost.start, innermost.end)];
        spans.extend(spans_of(&path[..path.len() - 1], text));
        return spans;
    }
    spans_of(&path, text)
}

/// The nodes holding `at`, outermost first, found as JDT's `NodeFinder`
/// finds them: where two touch at `at` (`name‸.call()`), the later one,
/// and only nodes JDT has — a token of punctuation holds nothing.
fn jdt_path(tree: &Syn, at: Position) -> Vec<&Syn> {
    let mut path = vec![tree];
    let mut node = tree;
    while let Some(next) = node
        .children
        .iter()
        .rev()
        .find(|child| reaches(child, node, at))
    {
        path.push(next);
        node = next;
    }
    path
}

/// Whether `node` holds `at` as JDT sees it: it is a JDT node holding it,
/// or a container JDT has no node for (`(int a)`) with one inside that
/// does — a container only touching `at` does not take it from the name
/// before it (`run‸(int a)`).
fn reaches(node: &Syn, parent: &Syn, at: Position) -> bool {
    if node.start == node.end {
        return false;
    }
    if is_comment(node) {
        // A javadoc is held by the member it documents.
        let documents = node.text.starts_with("/**") && documented(parent, node).is_some();
        return !documents && holds((node.start, node.end), at);
    }
    if !is_named(node) {
        return is_modifier_token(node, parent) && holds((node.start, node.end), at);
    }
    // The span JDT gives it: a member from its javadoc, `case 0` with its
    // colon.
    match node_spans(node, Some(parent), &parent.children, "").last() {
        Some(span) => holds(*span, at),
        None => {
            holds((node.start, node.end), at)
                && node.children.iter().any(|child| reaches(child, node, at))
        }
    }
}

/// A modifier keyword, which is a node in JDT.
fn is_modifier_token(token: &Syn, parent: &Syn) -> bool {
    parent.kind == "modifiers"
        || (matches!(parent.kind.as_str(), "static_initializer" | "import_declaration")
            && token.kind == "static")
}

/// The nodes JDT makes inside a javadoc that hold `at`, innermost first:
/// the tag (`@param …`, or the description, a tag without a name) and in
/// it the name it documents, a `{@link}` and the runs of text.
fn javadoc_spans(comment: &Syn, at: Position) -> Vec<Span> {
    let mut nodes: Vec<Span> = Vec::new();
    let lines = javadoc_lines(comment);
    // Lines up to the first block tag are the description; each block tag
    // runs until the next.
    let mut tags: Vec<(usize, usize)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let starts_tag = line.text.starts_with('@')
            && line.text[1..].starts_with(|character: char| character.is_ascii_alphabetic());
        if starts_tag || tags.is_empty() {
            tags.push((index, index));
        }
        if let Some(last) = tags.last_mut() {
            last.1 = index;
        }
    }
    for (first, last) in tags {
        let content: Vec<&JavadocLine> = lines[first..=last]
            .iter()
            .filter(|line| !line.text.is_empty())
            .collect();
        let (Some(opening), Some(closing)) = (content.first(), content.last()) else {
            continue;
        };
        let tag = (opening.start, closing.end());
        if !holds(tag, at) {
            continue;
        }
        let mut inside: Vec<Span> = Vec::new();
        // A `{@code …}` or `{@link …}` may run over several lines.
        let mut open: Option<(Position, usize)> = None;
        for line in &content {
            let mut from = 0;
            if std::ptr::eq(*line, *opening) && line.text.starts_with('@') {
                let name_end = line.text.find(char::is_whitespace).unwrap_or(line.text.len());
                let name = &line.text[1..name_end];
                from = name_end;
                // The documented name: a parameter, an exception, a link.
                if matches!(name, "param" | "throws" | "exception" | "see" | "serialField") {
                    let rest = &line.text[name_end..];
                    let word_start = name_end + (rest.len() - rest.trim_start().len());
                    let word_end = reference_end(&line.text, word_start);
                    if word_start < word_end {
                        reference(line, word_start, word_end, &mut inside);
                        from = word_end;
                    }
                }
            }
            text_elements(line, from, &mut open, &mut inside);
        }
        if let Some((start, _)) = open {
            inside.push((start, closing.end()));
        }
        let mut held: Vec<Span> = inside.into_iter().filter(|span| holds(*span, at)).collect();
        // A span inside another comes first; of two that only touch at
        // `at`, the later, as `NodeFinder` takes it.
        held.sort_by(|left, right| {
            let left_inside = left.0 >= right.0 && left.1 <= right.1;
            let right_inside = right.0 >= left.0 && right.1 <= left.1;
            match (left_inside, right_inside) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => right.0.cmp(&left.0),
            }
        });
        if let Some(innermost) = held.first().copied() {
            // Two siblings that touch at `at`: only the later one.
            nodes.extend(
                held.into_iter()
                    .filter(|span| span == &innermost || (span.0 <= innermost.0 && innermost.1 <= span.1)),
            );
        }
        nodes.push(tag);
    }
    nodes
}

/// The text runs of a javadoc line from byte `from` on, and the inline
/// tags (`{@link Type#member label}`) with the reference and the text in
/// them. `open` is an inline tag a previous line left open: where it
/// starts and how deep its braces are.
fn text_elements(
    line: &JavadocLine,
    from: usize,
    open: &mut Option<(Position, usize)>,
    inside: &mut Vec<Span>,
) {
    let text = &line.text;
    let mut run_start = from;
    let mut index = from;
    while index < text.len() {
        if open.is_none() && text[index..].starts_with("{@") {
            if run_start < index {
                inside.push((line.at(run_start), line.at(index)));
            }
            *open = Some((line.at(index), 1));
            let name_start = index + 2;
            let name_end = text[name_start..]
                .find(|character: char| character.is_whitespace() || character == '}')
                .map_or(text.len(), |end| name_start + end);
            index = name_end;
            if matches!(&text[name_start..name_end], "link" | "linkplain" | "see" | "value") {
                let rest = &text[name_end..];
                let start = name_end + (rest.len() - rest.trim_start().len());
                let end = reference_end(text, start);
                if start < end {
                    reference(line, start, end, inside);
                    index = end;
                }
            }
            // The label, or the code, with the space before it.
            run_start = index;
            continue;
        }
        let character = text[index..].chars().next().unwrap_or(' ');
        if let Some((start, depth)) = open.as_mut() {
            match character {
                '{' => *depth += 1,
                '}' => {
                    *depth -= 1;
                    if *depth == 0 {
                        if run_start < index {
                            inside.push((line.at(run_start), line.at(index)));
                        }
                        inside.push((*start, line.at(index + 1)));
                        *open = None;
                        index += 1;
                        run_start = index;
                        continue;
                    }
                }
                _ => {}
            }
        }
        index += character.len_utf8();
    }
    if run_start < text.len() {
        inside.push((line.at(run_start), line.at(text.len())));
    }
}

/// Where a reference written from byte `start` ends: at a space or the
/// closing brace, past the parameters of `#member(String a, int b)`.
fn reference_end(text: &str, start: usize) -> usize {
    let mut depth = 0usize;
    for (offset, character) in text[start..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '}' if depth == 0 => return start + offset,
            _ if character.is_whitespace() && depth == 0 => return start + offset,
            _ => {}
        }
    }
    text.len()
}

/// A reference in a javadoc: a name, or `Type#member(…)` with the type and
/// the member's name in it.
fn reference(line: &JavadocLine, start: usize, end: usize, inside: &mut Vec<Span>) {
    inside.push((line.at(start), line.at(end)));
    let written = &line.text[start..end];
    if let Some(hash) = written.find('#') {
        if hash > 0 {
            inside.push((line.at(start), line.at(start + hash)));
        }
        let member_start = start + hash + 1;
        let member_end = written[hash + 1..]
            .find('(')
            .map_or(end, |open| member_start + open);
        if member_start < member_end {
            inside.push((line.at(member_start), line.at(member_end)));
        }
    }
}

/// One line of a javadoc: its text without the comment's margin and
/// marks, and where that text starts.
struct JavadocLine {
    text: String,
    start: Position,
}

impl JavadocLine {
    /// Where the text's byte `index` is.
    fn at(&self, index: usize) -> Position {
        Position {
            line: self.start.line,
            column: self.start.column + self.text[..index].encode_utf16().count() as u32,
        }
    }

    fn end(&self) -> Position {
        self.at(self.text.len())
    }
}

fn javadoc_lines(comment: &Syn) -> Vec<JavadocLine> {
    let mut lines = Vec::new();
    for (index, raw) in comment.text.split('\n').enumerate() {
        let mut column = if index == 0 { comment.start.column } else { 0 };
        let mut rest = raw.trim_end_matches('\r');
        let skip = |text: &str, count: usize, column: &mut u32| -> String {
            *column += text[..count].encode_utf16().count() as u32;
            text[count..].to_owned()
        };
        let mut owned;
        if index == 0 {
            owned = skip(rest, 3, &mut column);
            rest = &owned;
        }
        let blank = rest.len() - rest.trim_start().len();
        owned = skip(rest, blank, &mut column);
        rest = &owned;
        if rest.starts_with('*') && !rest.starts_with("*/") {
            owned = skip(rest, 1, &mut column);
            rest = &owned;
            let blank = rest.len() - rest.trim_start().len();
            owned = skip(rest, blank, &mut column);
            rest = &owned;
        }
        let text = rest.strip_suffix("*/").unwrap_or(rest).trim_end().to_owned();
        lines.push(JavadocLine {
            text,
            start: Position {
                line: comment.start.line + index as u32,
                column,
            },
        });
    }
    lines
}

fn holds(span: Span, at: Position) -> bool {
    span.0 <= at && at <= span.1
}

/// The JDT nodes of a path of tree-sitter nodes, outermost first, as
/// ranges innermost first.
fn spans_of(path: &[&Syn], text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    for (index, node) in path.iter().enumerate().rev() {
        let parent = index.checked_sub(1).map(|at| path[at]);
        let siblings = parent.map_or(&[][..], |parent| parent.children.as_slice());
        for span in node_spans(node, parent, siblings, text) {
            spans.push(span);
        }
        if let Some(parent) = parent {
            spans.extend(enclosing_only_in_jdt(node, parent));
        }
    }
    spans
}

/// The JDT nodes a tree-sitter node stands for, innermost first: none, one,
/// or two of the same range.
fn node_spans(
    node: &Syn,
    parent: Option<&Syn>,
    siblings: &[Syn],
    text: &str,
) -> Vec<Span> {
    let span = (node.start, node.end);
    let parent_kind = parent.map_or("", |parent| parent.kind.as_str());
    match node.kind.as_str() {
        "program" => vec![(Position { line: 0, column: 0 }, end_of(text))],
        // Containers JDT has no node for.
        "class_body" | "interface_body" | "enum_body" | "annotation_type_body" => {
            // A body written after `new Type(…)` or an enum constant is an
            // anonymous class in JDT.
            if matches!(parent_kind, "object_creation_expression" | "enum_constant") {
                vec![span]
            } else {
                Vec::new()
            }
        }
        "enum_body_declarations" | "formal_parameters" | "argument_list" | "modifiers"
        | "type_arguments" | "type_parameters" | "type_bound" | "superclass"
        | "super_interfaces" | "extends_interfaces" | "type_list" | "throws"
        | "annotation_argument_list" | "switch_block" | "switch_block_statement_group"
        | "resource_specification" | "finally_clause" | "inferred_parameters"
        | "dimensions_expr" | "string_fragment" | "escape_sequence" | "multiline_string_fragment"
        | "asterisk" | "ERROR" | "super" => Vec::new(),
        // A condition's parentheses belong to the statement.
        "parenthesized_expression"
            if matches!(
                parent_kind,
                "if_statement"
                    | "while_statement"
                    | "do_statement"
                    | "switch_expression"
                    | "switch_statement"
                    | "synchronized_statement"
            ) =>
        {
            Vec::new()
        }
        // `a + b + c`: JDT makes one expression of a chain of the same
        // operator, where tree-sitter nests the left part.
        "binary_expression"
            if parent.is_some_and(|parent| {
                parent.kind == "binary_expression"
                    && parent.child("left").is_some_and(|left| left.start == node.start && left.end == node.end)
                    && operator(parent) == operator(node)
            }) =>
        {
            Vec::new()
        }
        // A single caught type is the type itself; several are a union.
        "catch_type" => {
            let types = node.children.iter().filter(|child| is_named(child)).count();
            if types > 1 { vec![span] } else { Vec::new() }
        }
        // `case 0:` takes its colon in JDT.
        "switch_label" => {
            let colon = siblings
                .iter()
                .find(|sibling| sibling.kind == ":" && sibling.start >= node.end);
            vec![(node.start, colon.map_or(node.end, |colon| colon.end))]
        }
        // A member starts at its javadoc.
        kind if is_documented_kind(kind) => {
            let start = parent
                .and_then(|parent| javadoc_of(parent, node))
                .map_or(node.start, |javadoc| javadoc.start);
            vec![(start, node.end)]
        }
        "type_identifier" => {
            let declares = matches!(parent_kind, "type_parameter")
                || (node.field.as_deref() == Some("name") && is_documented_kind(parent_kind));
            if declares || parent_kind == "scoped_type_identifier" {
                vec![span]
            } else {
                // `SimpleType` around `SimpleName`.
                vec![span, span]
            }
        }
        "scoped_type_identifier" => {
            if parent_kind == "scoped_type_identifier" {
                vec![span]
            } else {
                vec![span, span]
            }
        }
        "identifier" => {
            // A lambda's parameter is declared: a fragment around its name.
            let declared = parent_kind == "inferred_parameters"
                || (parent_kind == "lambda_expression" && node.field.as_deref() == Some("parameters"));
            if declared { vec![span, span] } else { vec![span] }
        }
        // `for (int i = 0; …)`: the declaration ends before its `;`.
        "local_variable_declaration" if parent_kind == "for_statement" => {
            let end = node
                .children
                .iter()
                .rev()
                .find(|child| child.kind != ";")
                .map_or(node.end, |last| last.end);
            vec![(node.start, end)]
        }
        _ if !is_named(node) => {
            // A modifier keyword is a node in JDT; other tokens are not.
            if parent.is_some_and(|parent| is_modifier_token(node, parent)) {
                vec![span]
            } else {
                Vec::new()
            }
        }
        _ => vec![span],
    }
}

/// A JDT node between `node` and its parent that tree-sitter does not
/// make: the variable of `for (Type name : …)`, the array type of
/// `new Type[]{…}`.
fn enclosing_only_in_jdt(node: &Syn, parent: &Syn) -> Option<Span> {
    match parent.kind.as_str() {
        "enhanced_for_statement" if matches!(node.field.as_deref(), Some("type" | "name")) => {
            let first = parent
                .children
                .iter()
                .find(|child| child.kind == "modifiers" || child.field.as_deref() == Some("type"))?;
            let name = parent.child("name")?;
            Some((first.start, name.end))
        }
        "array_creation_expression" if node.field.as_deref() == Some("type") => {
            let last = parent
                .children
                .iter()
                .rev()
                .find(|child| matches!(child.kind.as_str(), "dimensions" | "dimensions_expr"))?;
            Some((node.start, last.end))
        }
        _ => None,
    }
}

/// The declarations JDT gives their javadoc.
fn is_documented_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "field_declaration"
            | "constant_declaration"
            | "enum_constant"
            | "annotation_type_element_declaration"
    )
}

/// The javadoc right before `node` among `parent`'s children.
fn javadoc_of<'a>(parent: &'a Syn, node: &Syn) -> Option<&'a Syn> {
    let index = parent
        .children
        .iter()
        .position(|child| std::ptr::eq(child, node))?;
    // Other comments may stand between (`/** … */ // note`).
    parent.children[..index]
        .iter()
        .rev()
        .take_while(|previous| is_comment(previous))
        .find(|previous| previous.text.starts_with("/**"))
}

/// The declaration a javadoc among `parent`'s children documents.
fn documented<'a>(parent: &'a Syn, comment: &Syn) -> Option<&'a Syn> {
    let index = parent
        .children
        .iter()
        .position(|child| std::ptr::eq(child, comment))?;
    let next = parent.children[index + 1..]
        .iter()
        .find(|next| !is_comment(next))
        .filter(|next| is_documented_kind(&next.kind))?;
    javadoc_of(parent, next)
        .is_some_and(|javadoc| std::ptr::eq(javadoc, comment))
        .then_some(next)
}

fn operator(binary: &Syn) -> Option<&str> {
    binary.child("operator").map(|operator| operator.kind.as_str())
}

fn is_comment(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "line_comment" | "block_comment")
}

/// Whether tree-sitter names the node: a token of the grammar (`(`, `if`,
/// `public`) is its own kind. `this`, `true`, `false` are named nodes that
/// read as their kind.
fn is_named(node: &Syn) -> bool {
    matches!(node.kind.as_str(), "this" | "true" | "false")
        || (node.kind != node.text
            && node.kind.chars().all(|character| character.is_ascii_lowercase() || character == '_'))
}

/// Where the text ends.
fn end_of(text: &str) -> Position {
    let line = text.matches('\n').count() as u32;
    let last = text.rsplit('\n').next().unwrap_or_default();
    Position {
        line,
        column: last.encode_utf16().count() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::java::Java;
    use crate::parser::Syntax;

    fn shown(text: &str, line: u32, column: u32) -> Vec<String> {
        let tree = Java.tree(text);
        ranges(&tree, text, Position { line, column })
            .into_iter()
            .map(|(start, end)| {
                format!("{}:{}-{}:{}", start.line + 1, start.column + 1, end.line + 1, end.column + 1)
            })
            .collect()
    }

    #[test]
    fn a_method_name_is_in_its_declaration_its_type_and_the_file() {
        let text = "package p;\n\nclass A {\n    /** Doc. */\n    void run() {\n    }\n}\n";
        assert_eq!(
            shown(text, 4, 10),
            vec!["5:10-5:13", "4:5-6:6", "3:1-7:2", "1:1-8:1"]
        );
    }

    #[test]
    fn a_type_name_is_a_type_around_a_name() {
        let text = "class A {\n    String s;\n}\n";
        assert_eq!(
            shown(text, 1, 5),
            vec!["2:5-2:11", "2:5-2:11", "2:5-2:14", "1:1-3:2", "1:1-4:1"]
        );
    }

    #[test]
    fn a_chain_of_one_operator_is_one_expression() {
        let text = "class A {\n    int s = 1 + 2 + 3;\n}\n";
        assert_eq!(
            shown(text, 1, 12),
            vec!["2:13-2:14", "2:13-2:22", "2:9-2:22", "2:5-2:23", "1:1-3:2", "1:1-4:1"]
        );
    }

    #[test]
    fn a_line_comment_comes_before_the_nodes_around_it() {
        let text = "class A {\n    void run() {\n        // note\n    }\n}\n";
        assert_eq!(
            shown(text, 2, 12),
            vec!["3:9-3:16", "2:16-4:6", "2:5-4:6", "1:1-5:2", "1:1-6:1"]
        );
    }
}
