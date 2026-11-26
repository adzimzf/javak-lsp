//! Java formatting as jdtls formats: the Eclipse built-in profile.
//!
//! The formatter works on tokens, as Eclipse's does. It never changes a
//! token, only the whitespace between two, except for comments and text
//! blocks, which it lays out again. Each gap gets line breaks, a space or
//! nothing:
//!
//! - **A line of its own** for every statement and member, each switch
//!   label, a body's braces (`{` stays at the end of the line before),
//!   the statement of a brace-less `if`/`for`/`while`, and what follows a
//!   declaration's leading annotations.
//! - **Blank lines**: at most one is kept where the source had some.
//!   Members are apart by one around methods and member types, and when
//!   fields give way to something else. The package, the imports and each
//!   top-level type are one apart.
//! - **Wraps**: a line break the source has where Eclipse could wrap is
//!   kept. These are after a `,` or `(` of an argument list, around `+`,
//!   `*`, `&&` and the like, before a `.` of a call chain or the `?`/`:`
//!   of a conditional, and before `extends`, `implements` and `throws`.
//!   The kept line is indented two levels past the line the wrapped
//!   expression starts on. A line longer than 120 columns wraps at the
//!   last of these points that fits, outermost expression first; between
//!   two arguments as readily as before the `.` of their call, before the
//!   first argument only when that is not enough.
//! - **Spaces** around binary operators, after commas and keywords, before
//!   a `{`, inside an array initializer's braces. None inside parentheses
//!   or type arguments, around a `.`, or after a method's name.
//!
//! Comments get a space after `//` and single spaces between their words,
//! and wrap at 80 columns, a comment after code under itself. Block
//! comments and javadoc are laid out with a ` * ` on every line, with a
//! blank line before a javadoc's tags, the descriptions of its `@param`s
//! and `@throws` lined up, and an inline tag (`{@code ...}`, a `{@link}`'s
//! reference) or an HTML tag never cut. The file's header comment is left
//! as it is, and so is anything between `@formatter:off` and
//! `@formatter:on`, and a member that does not parse.
//!
//! Range formatting lays out the lines the range reaches: the lines around
//! keep their indentation, and the range's lines are indented from it.

use tree_sitter::Node;

use crate::parser::Position;
use crate::parser::java::GRAMMAR;

/// How far a wrapped line is indented past the line its expression starts on.
const CONTINUATION: usize = 2;
/// The column a line wraps at.
const LINE_LENGTH: usize = 120;
/// The column a comment wraps at, counted from where the comment starts.
const COMMENT_LENGTH: usize = 80;

/// What the client asks the indentation to be made of.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

/// One replacement, in the positions of the text as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: Position,
    pub end: Position,
    pub text: String,
}

/// The edits that format `text`, or only the lines `range` touches. A
/// member that does not parse is left as it is.
pub fn java(text: &str, options: Options, range: Option<(Position, Position)>) -> Vec<Edit> {
    let Some(tree) = GRAMMAR.parse(text) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut tokens = Vec::new();
    collect_tokens(root, &mut tokens);
    if tokens.is_empty() {
        return Vec::new();
    }
    let lines = Lines::of(text);
    let range = range.map(|(start, end)| (lines.unclamped(start), lines.unclamped(end)));
    let formatter = Formatter::new(text, options, tokens, range);
    formatter
        .format()
        .into_iter()
        .filter(|piece| text[piece.start..piece.end] != piece.text)
        .map(|piece| Edit {
            start: lines.position(piece.start),
            end: lines.position(piece.end),
            text: piece.text,
        })
        .collect()
}

/// Nodes the formatter takes whole: their insides are not code.
const ATOMIC: &[&str] = &[
    "string_literal",
    "character_literal",
    "line_comment",
    "block_comment",
];

/// Nodes whose `{ ... }` is a body: each member or statement in it is on a
/// line of its own, indented one level past the line the `{` is on.
const BODIES: &[&str] = &[
    "class_body",
    "interface_body",
    "enum_body",
    "annotation_type_body",
    "block",
    "constructor_body",
    "switch_block",
];

/// Declarations whose leading annotations are each on a line of their own.
const ANNOTATED: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
    "field_declaration",
    "constant_declaration",
    "local_variable_declaration",
    "annotation_type_element_declaration",
];

const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

fn collect_tokens<'t>(node: Node<'t>, tokens: &mut Vec<Node<'t>>) {
    if node.child_count() == 0 || ATOMIC.contains(&node.kind()) {
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

fn is_comment(node: Node) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// How the line a token starts is indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Indent {
    /// As a statement or member where it stands.
    Structural,
    /// Two levels past the line the token at this index is on.
    Continuation(usize),
    /// As the line the token at this index is on: an array initializer's
    /// closing brace on a line of its own.
    Closing(usize),
    /// As the token at this index where it starts a line: a comment before
    /// the statement of a brace-less `if`.
    StructuralOf(usize),
}

/// A place a line may wrap: kept where the source breaks the line there,
/// and taken when a line is too long if `long` says so.
#[derive(Clone, Copy, Debug)]
struct WrapPoint {
    indent: Indent,
    /// The expression or list that wraps: a line too long wraps the
    /// outermost first, at the last of its points that fits.
    owner: usize,
    depth: usize,
    long: bool,
}

/// The whitespace before a token, decided.
#[derive(Clone, Copy, Debug)]
struct Gap {
    breaks: usize,
    space: bool,
    indent: Indent,
    verbatim: bool,
}

/// Text that replaces `start..end` of the source: a gap or a token.
struct Piece {
    start: usize,
    end: usize,
    text: String,
}

struct Formatter<'a, 't> {
    text: &'a str,
    options: Options,
    tokens: Vec<Node<'t>>,
    /// Where a token starts a line of its own, and how it is indented.
    starts: Vec<Option<Indent>>,
    /// The blank lines a token needs before it at least.
    blank_lines: Vec<usize>,
    /// The gap before a token is a place the line may wrap.
    wraps: Vec<Option<WrapPoint>>,
    /// The wrap before a token is only for a line too long.
    unkept: Vec<bool>,
    /// The gap before a token and the token are left as they are.
    verbatim: Vec<bool>,
    /// The leading comments of the file, left as they are.
    header: usize,
    /// The byte offsets of the range to format, when not the whole file.
    /// As jdtls reads a range, a column past the end of its line reaches
    /// into the next.
    range: Option<(usize, usize)>,
}

impl<'a, 't> Formatter<'a, 't> {
    fn new(
        text: &'a str,
        options: Options,
        tokens: Vec<Node<'t>>,
        range: Option<(usize, usize)>,
    ) -> Self {
        let count = tokens.len();
        let mut formatter = Formatter {
            text,
            options,
            tokens,
            starts: vec![None; count],
            blank_lines: vec![0; count],
            wraps: vec![None; count],
            unkept: vec![false; count],
            verbatim: vec![false; count],
            header: 0,
            range,
        };
        if let Some(root) = formatter.tokens.first().map(|token| root_of(*token)) {
            formatter.mark(root, 0);
        }
        formatter.mark_verbatim();
        formatter.mark_errors(root_of(formatter.tokens[0]));
        formatter.header = formatter
            .tokens
            .iter()
            .take_while(|token| is_comment(**token))
            .count();
        formatter
    }

    fn source(&self, node: Node) -> &'a str {
        &self.text[node.start_byte()..node.end_byte()]
    }

    /// The index of the first token of `node` that is not a comment.
    fn first_token(&self, node: Node) -> usize {
        let start = node.start_byte();
        let mut index = self
            .tokens
            .partition_point(|token| token.start_byte() < start);
        while index + 1 < self.tokens.len() && is_comment(self.tokens[index]) {
            index += 1;
        }
        index
    }

    /// The index of the token right after `node`.
    /// The index of the first token after `node` that is not a comment.
    fn after(&self, node: Node) -> usize {
        let end = node.end_byte();
        let mut index = self
            .tokens
            .partition_point(|token| token.start_byte() < end);
        while index + 1 < self.tokens.len() && is_comment(self.tokens[index]) {
            index += 1;
        }
        index
    }

    fn index_of(&self, token: Node) -> usize {
        self.tokens
            .partition_point(|other| other.start_byte() < token.start_byte())
    }

    fn start(&mut self, index: usize, indent: Indent) {
        if index < self.tokens.len() && index > 0 {
            self.starts[index].get_or_insert(indent);
        }
    }

    fn need_blank_lines(&mut self, node: Node, count: usize) {
        if count == 0 {
            return;
        }
        let mut index = self.first_token(node);
        // The comments a declaration leads with come with it.
        while index > 0 && is_comment(self.tokens[index - 1]) {
            let comment = self.tokens[index - 1];
            let before = if index >= 2 {
                self.tokens[index - 2].end_byte()
            } else {
                0
            };
            if !self.text[before..comment.start_byte()].contains('\n') {
                break;
            }
            index -= 1;
        }
        self.blank_lines[index] = self.blank_lines[index].max(count);
    }

    /// A place a line too long may wrap, where a line break in the source
    /// is not kept.
    fn long_only(&mut self, index: usize, point: WrapPoint) {
        if index < self.tokens.len() && self.wraps[index].is_none() {
            self.wraps[index] = Some(point);
            self.unkept[index] = true;
        }
    }

    fn wrap(&mut self, index: usize, point: WrapPoint) {
        if index < self.tokens.len() && self.wraps[index].is_none() {
            self.wraps[index] = Some(point);
        }
    }

    /// Lines, blank lines and wrap points, from the tree.
    fn mark(&mut self, node: Node<'t>, depth: usize) {
        let kind = node.kind();
        let children = code_children(node);
        match kind {
            "program" => self.mark_program(&children),
            _ if BODIES.contains(&kind) => self.mark_body(node, &children),
            "enum_body_declarations" => {
                for child in children
                    .iter()
                    .skip_while(|child| child.kind() != ";")
                    .skip(1)
                {
                    self.start(self.first_token(*child), Indent::Structural);
                }
                self.mark_members(node, &children);
            }
            "switch_block_statement_group" => {
                let mut statements = 0;
                for child in &children {
                    match child.kind() {
                        "switch_label" => self.start(self.first_token(*child), Indent::Structural),
                        ":" => {}
                        "block" if statements == 0 => statements += 1,
                        _ => {
                            statements += 1;
                            self.start(self.first_token(*child), Indent::Structural);
                        }
                    }
                }
            }
            "if_statement" => {
                let consequence = node.child_by_field_name("consequence");
                let braced = consequence.is_some_and(|body| body.kind() == "block");
                if let Some(body) = consequence.filter(|_| !braced) {
                    self.start(self.first_token(body), Indent::Structural);
                }
                if let Some(alternative) = node.child_by_field_name("alternative") {
                    if !braced
                        && let Some(keyword) = children.iter().find(|child| child.kind() == "else")
                    {
                        self.start(self.index_of(*keyword), Indent::Structural);
                    }
                    if !matches!(alternative.kind(), "block" | "if_statement") {
                        self.start(self.first_token(alternative), Indent::Structural);
                    }
                }
            }
            "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
                if let Some(body) = node.child_by_field_name("body")
                    && body.kind() != "block"
                {
                    self.start(self.first_token(body), Indent::Structural);
                    if kind == "do_statement"
                        && let Some(keyword) = children.iter().find(|child| child.kind() == "while")
                    {
                        self.start(self.index_of(*keyword), Indent::Structural);
                    }
                }
            }
            "modifiers" => {
                let annotated = node
                    .parent()
                    .is_some_and(|parent| ANNOTATED.contains(&parent.kind()));
                if annotated {
                    for child in children.iter().take_while(|child| is_annotation(**child)) {
                        self.start(self.after(*child), Indent::Structural);
                    }
                }
            }
            "package_declaration" => {
                for child in children.iter().filter(|child| is_annotation(**child)) {
                    self.start(self.after(*child), Indent::Structural);
                }
            }
            "resource_specification" => {
                let anchor = node.parent().map_or(0, |owner| self.first_token(owner));
                for (index, child) in children.iter().enumerate() {
                    let next = children.get(index + 1);
                    if child.kind() == ";" && next.is_some_and(|next| next.kind() != ")") {
                        self.start(self.after(*child), Indent::Continuation(anchor));
                    }
                }
            }
            _ => {}
        }
        self.mark_wraps(node, &children, depth);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.mark(child, depth + 1);
        }
    }

    fn mark_program(&mut self, children: &[Node<'t>]) {
        let mut previous: Option<&str> = None;
        for child in children {
            self.start(self.first_token(*child), Indent::Structural);
            let kind = child.kind();
            let blank = match (previous, kind) {
                (None, _) => 0,
                (Some("package_declaration"), _) => 1,
                (Some("import_declaration"), "import_declaration") => 0,
                (Some("import_declaration"), _) => 1,
                (Some(_), ";") => 0,
                (Some(_), _) => 1,
            };
            self.need_blank_lines(*child, blank);
            if kind != ";" {
                previous = Some(kind);
            }
        }
    }

    fn mark_body(&mut self, node: Node<'t>, children: &[Node<'t>]) {
        let is_enum = node.kind() == "enum_body";
        let mut constants = false;
        for child in children.iter().skip(1) {
            match child.kind() {
                "enum_constant" if is_enum => {
                    if !constants {
                        self.start(self.first_token(*child), Indent::Structural);
                    }
                    constants = true;
                }
                "," if is_enum => {}
                "enum_body_declarations" if constants => {}
                ";" if !is_enum && node.kind() != "block" && node.kind() != "constructor_body" => {}
                _ => self.start(self.first_token(*child), Indent::Structural),
            }
        }
        if matches!(
            node.kind(),
            "class_body" | "interface_body" | "annotation_type_body"
        ) {
            self.mark_members(node, children);
        }
    }

    /// The blank lines between a type's members.
    fn mark_members(&mut self, node: Node<'t>, children: &[Node<'t>]) {
        let mut previous: Option<Member> = None;
        if node.kind() == "enum_body_declarations" {
            let has_constants = node.parent().is_some_and(|body| {
                code_children(body)
                    .iter()
                    .any(|child| child.kind() == "enum_constant")
            });
            if has_constants {
                previous = Some(Member::Constants);
            }
        }
        for child in children {
            let Some(member) = Member::of(child.kind()) else {
                continue;
            };
            if let Some(previous) = previous {
                let mut blank = match member {
                    Member::Field | Member::Initializer | Member::Constants => 0,
                    Member::Type | Member::Method => 1,
                };
                if !previous.same_chunk(member) {
                    blank = 1;
                }
                self.need_blank_lines(*child, blank);
            }
            previous = Some(member);
        }
    }

    fn mark_wraps(&mut self, node: Node<'t>, children: &[Node<'t>], depth: usize) {
        let kind = node.kind();
        let point = |owner: Node, long: bool, this: &Self| WrapPoint {
            indent: Indent::Continuation(this.first_token(owner)),
            owner: owner.id(),
            depth: depth_of(owner),
            long,
        };
        match kind {
            "argument_list"
            | "formal_parameters"
            | "annotation_argument_list"
            | "inferred_parameters" => {
                // Wrapped arguments are indented past the call, but a line
                // too long wraps the call's chain before its arguments. A
                // lambda's parameters are not wrapped for length.
                let owner = node.parent().unwrap_or(node);
                let long = owner.kind() != "lambda_expression";
                // A call's arguments are indented past its name.
                let anchor = match owner.child_by_field_name("name") {
                    Some(name) if owner.kind() == "method_invocation" => self.index_of(name),
                    _ => self.first_token(owner),
                };
                // Between two arguments a line wraps as readily as before a
                // `.` of the call's chain; before the first, only once that
                // is not enough.
                let chain = top_of_chain(owner, |parent, child| {
                    parent.kind() == "method_invocation"
                        && parent
                            .child_by_field_name("object")
                            .map(|object| object.id())
                            == Some(child.id())
                });
                for (index, child) in children.iter().enumerate() {
                    let empty = children
                        .get(index + 1)
                        .is_some_and(|next| next.kind() == ")");
                    let (group, kept) = match child.kind() {
                        "(" => (node, kind != "annotation_argument_list"),
                        "," => (chain, true),
                        _ => continue,
                    };
                    let at = self.after(*child);
                    let point = WrapPoint {
                        indent: Indent::Continuation(anchor),
                        owner: group.id(),
                        depth: depth_of(group),
                        long: long && !empty,
                    };
                    if kept {
                        self.wrap(at, point);
                    } else {
                        self.long_only(at, point);
                    }
                }
            }
            "array_initializer" | "element_value_array_initializer" => {
                let open = self.first_token(node);
                for (index, child) in children.iter().enumerate() {
                    match child.kind() {
                        "{" => self.wrap(self.after(*child), point(node, false, self)),
                        // A trailing comma leaves the break before `}` to it.
                        "," if children.get(index + 1).is_some_and(|next| next.kind() == "}") => {}
                        "," => self.wrap(self.after(*child), point(node, true, self)),
                        "}" => {
                            let at = self.index_of(*child);
                            self.wrap(
                                at,
                                WrapPoint {
                                    indent: Indent::Closing(open),
                                    owner: node.id(),
                                    depth,
                                    long: false,
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            "enum_body" => {
                for child in children.iter().filter(|child| child.kind() == ",") {
                    let next = self.after(*child);
                    let is_constant = self
                        .tokens
                        .get(next)
                        .is_some_and(|token| token.kind() != ";" && token.kind() != "}");
                    if is_constant {
                        self.wrap(
                            next,
                            WrapPoint {
                                indent: Indent::Structural,
                                owner: node.id(),
                                depth,
                                long: true,
                            },
                        );
                    }
                }
            }
            "field_declaration"
            | "local_variable_declaration"
            | "constant_declaration"
            | "type_list"
            | "throws" => {
                let owner = match kind {
                    "type_list" | "throws" => declaration_of(node),
                    _ => node,
                };
                for child in children.iter().filter(|child| child.kind() == ",") {
                    self.wrap(self.after(*child), point(owner, true, self));
                }
                if kind == "throws" {
                    self.wrap(self.first_token(node), point(owner, true, self));
                }
            }
            "superclass" | "super_interfaces" | "permits" => {
                let owner = declaration_of(node);
                self.wrap(self.first_token(node), point(owner, true, self));
            }
            "binary_expression" => {
                if let Some(operator) = node.child_by_field_name("operator")
                    && wraps_at(operator.kind())
                {
                    let owner = top_of_chain(node, |parent, child| {
                        parent.kind() == "binary_expression"
                            && parent
                                .child_by_field_name("operator")
                                .is_some_and(|op| same_operators(op.kind(), operator.kind()))
                            && parent.child_by_field_name("left").map(|left| left.id())
                                == Some(child.id())
                    });
                    let at = self.index_of(operator);
                    self.wrap(at, point(owner, true, self));
                    self.wrap(at + 1, point(owner, false, self));
                }
            }
            "ternary_expression" => {
                // A conditional nested in another's alternative wraps two
                // levels past the line it starts on, each deeper than the last.
                for child in children
                    .iter()
                    .filter(|child| matches!(child.kind(), "?" | ":"))
                {
                    self.wrap(self.index_of(*child), point(node, true, self));
                }
            }
            "method_invocation" => {
                let owner = top_of_chain(node, |parent, child| {
                    parent.kind() == "method_invocation"
                        && parent
                            .child_by_field_name("object")
                            .map(|object| object.id())
                            == Some(child.id())
                });
                // Eclipse wraps before a `.` that follows a call, never one
                // after a name (`errors.add(…)` stays together).
                let after_a_call = node
                    .child_by_field_name("object")
                    .is_some_and(|object| matches!(object.kind(), "method_invocation" | "object_creation_expression"));
                for child in children.iter().filter(|child| child.kind() == "." && after_a_call) {
                    let at = self.index_of(*child);
                    self.wrap(at, point(owner, true, self));
                }
            }
            _ => {}
        }
    }

    /// `@formatter:off` to `@formatter:on`, both comments included, and
    /// the gap before the first when it starts its line.
    fn mark_verbatim(&mut self) {
        let mut off = false;
        for index in 0..self.tokens.len() {
            let token = self.tokens[index];
            if is_comment(token) {
                let source = self.source(token);
                if !off && source.contains("@formatter:off") {
                    off = true;
                    // The line of the comment is indented, unless the
                    // comment starts it.
                    let line_start = self.text[..token.start_byte()]
                        .rfind('\n')
                        .map_or(0, |at| at + 1);
                    self.verbatim[index] = line_start == token.start_byte();
                    continue;
                } else if off && source.contains("@formatter:on") {
                    self.verbatim[index] = true;
                    off = false;
                    continue;
                }
            }
            self.verbatim[index] = off;
        }
    }

    /// Whether the gap before a token is in the range to format: one that
    /// ends where the range starts or starts where it ends is.
    fn in_range(&self, index: usize) -> bool {
        let Some((from, to)) = self.range else {
            return true;
        };
        let start = if index == 0 {
            0
        } else {
            self.tokens[index - 1].end_byte()
        };
        let end = self.tokens[index].start_byte();
        end >= from && start < to
    }

    /// Whether a token is in the range to format.
    fn token_in_range(&self, token: Node) -> bool {
        self.range
            .is_none_or(|(from, to)| token.start_byte() <= to && token.end_byte() >= from)
    }

    /// The inside of each member that does not parse.
    fn mark_errors(&mut self, root: Node<'t>) {
        if !root.has_error() {
            return;
        }
        let mut broken = Vec::new();
        collect_errors(root, &mut broken);
        for error in broken {
            let member = member_of(error);
            let (start, end) = (member.start_byte(), member.end_byte());
            for index in 1..self.tokens.len() {
                let token = self.tokens[index];
                if token.start_byte() > start && token.start_byte() < end {
                    self.verbatim[index] = true;
                }
            }
        }
    }

    fn newlines_before(&self, index: usize) -> usize {
        let start = self.tokens[index - 1].end_byte();
        let end = self.tokens[index].start_byte();
        self.text[start..end].matches('\n').count()
    }

    /// The gap before each token, the first one's included.
    fn gaps(&self, forced: &[bool]) -> Vec<Gap> {
        (0..self.tokens.len())
            .map(|index| self.gap(index, forced[index]))
            .collect()
    }

    fn gap(&self, index: usize, forced: bool) -> Gap {
        let none = Gap {
            breaks: 0,
            space: false,
            indent: Indent::Structural,
            verbatim: false,
        };
        if index == 0 {
            return none;
        }
        if self.verbatim[index] || index <= self.header || !self.in_range(index) {
            return Gap {
                verbatim: true,
                ..none
            };
        }
        let previous = self.tokens[index - 1];
        let token = self.tokens[index];
        let newlines = self.newlines_before(index);
        let line = |indent: Indent| Gap {
            breaks: newlines.clamp(1, 2).max(self.blank_lines[index] + 1),
            space: false,
            indent,
            verbatim: false,
        };
        if previous.kind() == "line_comment" {
            return line(self.after_comment(index));
        }
        if is_comment(token) && newlines == 0 {
            return Gap {
                space: true,
                ..none
            };
        }
        if previous.kind() == "block_comment" && newlines == 0 {
            let space = !matches!(token.kind(), "," | ";" | ")");
            return Gap { space, ..none };
        }
        if is_comment(token) {
            return line(self.indent_of_comment_line(index));
        }
        if is_comment(previous) {
            return line(self.after_comment(index));
        }
        if let Some(indent) = self.starts[index] {
            return line(indent);
        }
        if let Some(point) = self.wraps[index]
            && ((newlines > 0 && !self.unkept[index]) || forced)
        {
            return Gap {
                breaks: 1,
                space: false,
                indent: point.indent,
                verbatim: false,
            };
        }
        Gap {
            space: space_between(previous, token),
            ..none
        }
    }

    /// How a token a comment pushes onto a line of its own is indented: as
    /// where it would start a line anyway, as a part of its statement such
    /// as an `else`, or as a wrapped line inside an expression.
    fn after_comment(&self, index: usize) -> Indent {
        if let Some(indent) = self.starts[index].or(self.wraps[index].map(|point| point.indent)) {
            return indent;
        }
        let token = self.tokens[index];
        if is_comment(token) {
            return self.indent_of_comment_line(index);
        }
        let kind = parent_kind(token);
        let in_statement = kind.ends_with("_statement")
            || kind.ends_with("_declaration")
            || matches!(kind, "catch_clause" | "finally_clause" | "switch_label");
        if in_statement {
            return Indent::Structural;
        }
        match self.expression_anchor(index) {
            Some(anchor) => Indent::Continuation(anchor),
            None => Indent::Structural,
        }
    }

    /// A comment on a line of its own is indented as the statement or
    /// member it stands among, or inside an expression as a wrapped line.
    fn indent_of_comment_line(&self, index: usize) -> Indent {
        let token = self.tokens[index];
        let among_members = token.parent().is_some_and(|parent| {
            let kind = parent.kind();
            BODIES.contains(&kind)
                || matches!(
                    kind,
                    "program" | "enum_body_declarations" | "switch_block_statement_group"
                )
        });
        if among_members || index == 0 {
            return Indent::Structural;
        }
        // Inside an expression, as the line it comes before when that
        // line wraps there, else as the line before.
        let next = (index + 1..self.tokens.len()).find(|&at| !is_comment(self.tokens[at]));
        if let Some(next) = next {
            if self.starts[next] == Some(Indent::Structural) {
                return Indent::StructuralOf(next);
            }
            if let Some(point) = self.wraps[next].filter(|_| !self.unkept[next]) {
                return point.indent;
            }
        }
        Indent::Closing(index - 1)
    }

    /// The token a line inside an expression is indented past: the start
    /// of the innermost expression that wraps and starts before it.
    fn expression_anchor(&self, index: usize) -> Option<usize> {
        let mut current = self.tokens[index];
        while let Some(parent) = current.parent() {
            let kind = parent.kind();
            if BODIES.contains(&kind)
                || matches!(
                    kind,
                    "program" | "enum_body_declarations" | "switch_block_statement_group"
                )
            {
                return self
                    .statement_of(self.tokens[index])
                    .map(|statement| self.first_token(statement))
                    .filter(|&anchor| anchor < index);
            }
            let wraps = matches!(
                kind,
                "ternary_expression"
                    | "binary_expression"
                    | "argument_list"
                    | "formal_parameters"
                    | "array_initializer"
                    | "element_value_array_initializer"
                    | "method_invocation"
            );
            if wraps {
                let anchor = match (kind, parent.parent()) {
                    ("argument_list", Some(call)) => match call.child_by_field_name("name") {
                        Some(name) if call.kind() == "method_invocation" => self.index_of(name),
                        _ => self.first_token(call),
                    },
                    _ => self.first_token(parent),
                };
                if anchor < index {
                    return Some(anchor);
                }
            }
            current = parent;
        }
        None
    }

    /// The statement or member `node` is in.
    fn statement_of(&self, node: Node<'t>) -> Option<Node<'t>> {
        let mut current = node;
        while let Some(parent) = current.parent() {
            let kind = parent.kind();
            if BODIES.contains(&kind)
                || matches!(
                    kind,
                    "program" | "enum_body_declarations" | "switch_block_statement_group"
                )
            {
                return Some(current);
            }
            current = parent;
        }
        None
    }

    /// The structural indentation of a token starting a line, given the
    /// indentation of the lines before it.
    fn structural(&self, index: usize, indent_of_token: &dyn Fn(usize) -> usize) -> usize {
        let token = self.tokens[index];
        let mut extra = 0;
        let mut child = token;
        while let Some(parent) = child.parent() {
            let kind = parent.kind();
            if BODIES.contains(&kind) {
                let open = parent.child(0);
                let close = parent.child(parent.child_count().saturating_sub(1) as u32);
                let is = |node: Option<Node>| node.is_some_and(|node| node.id() == child.id());
                if is(open) {
                    child = parent;
                    continue;
                }
                // A body is indented past the line its statement or
                // expression starts on, wherever its `{` is.
                let owner = parent
                    .parent()
                    .filter(|owner| owner.kind() != "program")
                    .unwrap_or(parent);
                let anchor = if BODIES.contains(&owner.kind()) {
                    open.map_or(0, |open| self.index_of(open))
                } else {
                    self.first_token(owner)
                };
                let open_line = indent_of_token(anchor);
                if is(close) {
                    return open_line + extra;
                }
                return open_line + 1 + extra;
            }
            extra += match kind {
                "if_statement" => {
                    let is_field = |name| {
                        parent.child_by_field_name(name).map(|node| node.id()) == Some(child.id())
                    };
                    let consequence = is_field("consequence") && child.kind() != "block";
                    let alternative = is_field("alternative")
                        && !matches!(child.kind(), "block" | "if_statement");
                    usize::from(consequence || alternative)
                }
                "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
                    let is_body = parent.child_by_field_name("body").map(|node| node.id())
                        == Some(child.id());
                    usize::from(is_body && child.kind() != "block")
                }
                "switch_block_statement_group" => {
                    usize::from(!matches!(child.kind(), "switch_label" | ":"))
                }
                _ => 0,
            };
            child = parent;
        }
        extra
    }

    /// Every gap and token laid out, wrapping the lines that are too long.
    fn format(&self) -> Vec<Piece> {
        let mut forced = vec![false; self.tokens.len()];
        loop {
            let gaps = self.gaps(&forced);
            let indents = self.indents(&gaps);
            match self.too_long(&gaps, &indents, &forced) {
                Some(index) => forced[index] = true,
                None => return self.pieces(&gaps, &indents),
            }
        }
    }

    /// The indentation of the line each token is on.
    fn indents(&self, gaps: &[Gap]) -> Vec<usize> {
        let mut indents: Vec<usize> = vec![0; self.tokens.len()];
        let mut current = 0;
        for index in 0..self.tokens.len() {
            let gap = gaps[index];
            let starts_line =
                index == 0 || gap.breaks > 0 || (gap.verbatim && self.newlines_before(index) > 0);
            if starts_line {
                current = if gap.verbatim || index <= self.header {
                    self.source_indent(index)
                } else {
                    match gap.indent {
                        Indent::Structural => self.structural(index, &|at| indents[at]),
                        Indent::Continuation(anchor) => indents[anchor.min(index)] + CONTINUATION,
                        Indent::Closing(open) => indents[open.min(index)],
                        Indent::StructuralOf(next) => self.structural(next, &|at| indents[at]),
                    }
                };
            }
            indents[index] = current;
        }
        indents
    }

    /// The indentation the source gives the line of a token, in levels: a
    /// part of a level counts as a whole one.
    fn source_indent(&self, index: usize) -> usize {
        let start = self.tokens[index].start_byte();
        let line_start = self.text[..start].rfind('\n').map_or(0, |at| at + 1);
        let line = &self.text[line_start..start];
        let leading = &line[..line.len() - line.trim_start().len()];
        let width = columns(leading, self.options.tab_size);
        width.div_ceil(self.options.tab_size.max(1))
    }

    /// Whitespace that reaches `column`: tabs as far as they go, unless the
    /// client asks for spaces.
    fn alignment(&self, column: usize) -> String {
        let tab = self.options.tab_size.max(1);
        if self.options.insert_spaces {
            " ".repeat(column)
        } else {
            "\t".repeat(column / tab) + &" ".repeat(column % tab)
        }
    }

    fn indentation(&self, levels: usize) -> String {
        if self.options.insert_spaces {
            " ".repeat(levels * self.options.tab_size)
        } else {
            "\t".repeat(levels)
        }
    }

    /// The first token where a line that is too long wraps.
    fn too_long(&self, gaps: &[Gap], indents: &[usize], forced: &[bool]) -> Option<usize> {
        let mut index = 0;
        while index < self.tokens.len() {
            let mut end = index + 1;
            while end < self.tokens.len()
                && gaps[end].breaks == 0
                && !self.newline_in_verbatim(gaps, end)
            {
                end += 1;
            }
            if let Some(at) = self.wrap_in_line(index, end, gaps, indents, forced) {
                return Some(at);
            }
            index = end;
        }
        None
    }

    fn newline_in_verbatim(&self, gaps: &[Gap], index: usize) -> bool {
        gaps[index].verbatim && self.newlines_before(index) > 0
    }

    /// Where the line of tokens `start..end` wraps, if it is too long.
    fn wrap_in_line(
        &self,
        start: usize,
        end: usize,
        gaps: &[Gap],
        indents: &[usize],
        forced: &[bool],
    ) -> Option<usize> {
        let tab = self.options.tab_size;
        let mut width = indents[start] * tab;
        let mut ends = Vec::with_capacity(end - start);
        for (index, gap) in gaps.iter().enumerate().take(end).skip(start) {
            let token = self.tokens[index];
            if index > start && gap.space {
                width += 1;
            }
            if is_comment(token) && index > start {
                ends.push(width);
                continue;
            }
            let source = self.source(token);
            let first_line = source.split('\n').next().unwrap_or_default();
            width += first_line.chars().count();
            ends.push(width);
            if source.contains('\n') {
                break;
            }
        }
        let longest = ends.iter().copied().max().unwrap_or(0);
        if longest <= LINE_LENGTH {
            return None;
        }
        let candidates: Vec<(usize, WrapPoint)> = (start + 1..start + ends.len())
            .filter(|&index| !forced[index] && !gaps[index].verbatim)
            .filter_map(|index| Some((index, self.wraps[index].filter(|point| point.long)?)))
            .collect();
        let outermost = candidates.iter().map(|(_, point)| point.depth).min()?;
        let group = candidates
            .iter()
            .find(|(_, point)| point.depth == outermost)
            .map(|(_, point)| point.owner)?;
        let in_group: Vec<usize> = candidates
            .iter()
            .filter(|(_, point)| point.owner == group)
            .map(|(index, _)| *index)
            .collect();
        let fits = |index: usize| ends[index - 1 - start] <= LINE_LENGTH;
        let greedy = in_group
            .iter()
            .rev()
            .find(|&&index| fits(index))
            .or_else(|| in_group.first())
            .copied()?;
        // What is left after a wrap at `index`, from its continuation.
        let continuation = (indents[start] + CONTINUATION) * tab;
        let rest_fits = |index: usize| {
            let before = ends[index - 1 - start];
            continuation + longest.saturating_sub(before + 1) <= LINE_LENGTH
        };
        if fits(greedy) && rest_fits(greedy) {
            return Some(greedy);
        }
        // One wrap deeper that makes both lines fit costs Eclipse less than
        // the outer one and more after it.
        let single = candidates
            .iter()
            .filter(|(index, _)| fits(*index) && rest_fits(*index))
            .min_by_key(|(index, point)| (point.depth, std::cmp::Reverse(*index)))
            .map(|(index, _)| *index);
        Some(single.unwrap_or(greedy))
    }

    /// The text of each gap and of each token that is laid out again.
    fn pieces(&self, gaps: &[Gap], indents: &[usize]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let tab = self.options.tab_size;
        // The column the formatted text has reached, for a trailing
        // comment to wrap under itself.
        let mut column = 0;
        let advance = |column: usize, text: &str| match text.rfind('\n') {
            Some(at) => columns(&text[at + 1..], tab),
            None => column + columns(text, tab),
        };
        for (index, token) in self.tokens.iter().enumerate() {
            let gap = gaps[index];
            let start = if index == 0 {
                0
            } else {
                self.tokens[index - 1].end_byte()
            };
            let end = token.start_byte();
            let text = if index == 0 && !self.in_range(0) {
                self.text[start..end].to_owned()
            } else if index == 0 {
                // At most one blank line before the first token.
                let blank = self.text[start..end].contains('\n');
                if blank { "\n" } else { "" }.to_owned()
            } else if gap.verbatim {
                self.text[start..end].to_owned()
            } else if gap.breaks > 0 {
                "\n".repeat(gap.breaks) + &self.indentation(indents[index])
            } else if gap.space {
                " ".to_owned()
            } else {
                String::new()
            };
            column = advance(column, &text);
            pieces.extend(self.clipped(Piece { start, end, text }));
            let source = self.source(*token);
            let tag = is_comment(*token) && source.contains("@formatter:");
            if gap.verbatim || tag || index < self.header || !self.token_in_range(*token) {
                column = advance(column, source);
                continue;
            }
            let indent = self.indentation(indents[index]);
            let starts_line = index == 0 || gap.breaks > 0;
            let laid_out = match token.kind() {
                "line_comment" if starts_line => {
                    Some(line_comment(source, &indent, columns(&indent, self.options.tab_size)))
                }
                "line_comment" => Some(line_comment(source, &self.alignment(column), column)),
                "block_comment" => Some(block_comment(source, &indent, starts_line)),
                "string_literal" if source.starts_with("\"\"\"") => Some(text_block(
                    source,
                    &self.indentation(indents[index] + CONTINUATION),
                    self.options.tab_size,
                )),
                _ => None,
            };
            column = advance(column, laid_out.as_deref().unwrap_or(source));
            if let Some(text) = laid_out {
                pieces.push(Piece {
                    start: token.start_byte(),
                    end: token.end_byte(),
                    text,
                });
            }
        }
        if let Some(last) = self
            .tokens
            .last()
            .filter(|last| self.range.is_none_or(|(_, to)| last.end_byte() <= to))
        {
            let rest = &self.text[last.end_byte()..];
            let text = if rest.contains('\n') { "\n" } else { "" };
            pieces.push(Piece {
                start: last.end_byte(),
                end: self.text.len(),
                text: text.to_owned(),
            });
        }
        pieces
    }

    /// A gap as far as it is in the range to format. The part of a gap
    /// before the range stays: the rest of the gap's line breaks follow
    /// it, and when they cannot, the line is indented on from where that
    /// part leaves it.
    fn clipped(&self, piece: Piece) -> Option<Piece> {
        let Some((from, _)) = self.range else {
            return Some(piece);
        };
        if piece.start >= from
            || piece.end < from
            || self.text[piece.start..piece.end] == piece.text
        {
            return Some(piece);
        }
        let before = &self.text[piece.start..from.min(piece.end)];
        if let Some(rest) = piece.text.strip_prefix(before) {
            return Some(Piece {
                start: from,
                end: piece.end,
                text: rest.to_owned(),
            });
        }
        let (Some(source_at), Some(at)) = (before.rfind('\n'), piece.text.rfind('\n')) else {
            return None;
        };
        let missing = piece
            .text
            .matches('\n')
            .count()
            .saturating_sub(before.matches('\n').count());
        let tab = self.options.tab_size;
        let target = columns(&piece.text[at + 1..], tab);
        let text = if missing > 0 {
            "\n".repeat(missing) + &piece.text[at + 1..]
        } else {
            let present = columns(&before[source_at + 1..], tab).div_ceil(tab.max(1)) * tab;
            self.indentation(target.saturating_sub(present) / tab.max(1))
        };
        Some(Piece {
            start: from,
            end: piece.end,
            text,
        })
    }
}

/// The kinds of member that blank lines are put between, as Eclipse tells
/// them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Constants,
    Field,
    Initializer,
    Type,
    Method,
}

impl Member {
    fn of(kind: &str) -> Option<Self> {
        Some(match kind {
            "field_declaration" | "constant_declaration" => Member::Field,
            "static_initializer" | "block" => Member::Initializer,
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration" => Member::Method,
            _ if TYPE_DECLARATIONS.contains(&kind) => Member::Type,
            _ => return None,
        })
    }

    /// Fields and initializers go together, as do member types; anything
    /// else goes with its own kind only.
    fn same_chunk(self, other: Self) -> bool {
        let data = |member| matches!(member, Member::Field | Member::Initializer);
        self == other || (data(self) && data(other))
    }
}

/// The member of a type, or the top-level declaration, `node` is in.
fn member_of(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        let among_members = matches!(
            kind,
            "class_body"
                | "interface_body"
                | "enum_body"
                | "enum_body_declarations"
                | "annotation_type_body"
                | "program"
        );
        if among_members {
            return current;
        }
        current = parent;
    }
    current
}

/// The nodes tree-sitter could not read, outermost first.
fn collect_errors<'t>(node: Node<'t>, errors: &mut Vec<Node<'t>>) {
    if node.is_error() || node.is_missing() {
        errors.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() {
            collect_errors(child, errors);
        }
    }
}

fn root_of(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;
    }
    current
}

fn depth_of(node: Node) -> usize {
    let mut depth = 0;
    let mut current = node;
    while let Some(parent) = current.parent() {
        depth += 1;
        current = parent;
    }
    depth
}

/// The children of `node` that are code, not comments.
fn code_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| !is_comment(*child))
        .collect()
}

fn is_annotation(node: Node) -> bool {
    matches!(node.kind(), "annotation" | "marker_annotation")
}

/// The declaration a clause such as `throws` or `implements` belongs to.
fn declaration_of(node: Node) -> Node {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind().ends_with("_declaration") {
            return parent;
        }
        current = parent;
    }
    node
}

/// The outermost node of a chain `continues` links each node to its parent in.
fn top_of_chain<'t>(node: Node<'t>, continues: impl Fn(Node<'t>, Node<'t>) -> bool) -> Node<'t> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if !continues(parent, current) {
            break;
        }
        current = parent;
    }
    current
}

/// Binary operators a line may wrap at: not comparisons or shifts.
fn wraps_at(operator: &str) -> bool {
    matches!(
        operator,
        "+" | "-" | "*" | "/" | "%" | "&&" | "||" | "&" | "|" | "^"
    )
}

/// Operators Eclipse wraps as one list: `a + b - c`, `a && b || c`.
fn same_operators(left: &str, right: &str) -> bool {
    let group = |operator| match operator {
        "+" | "-" => 0,
        "*" | "/" | "%" => 1,
        "&&" | "||" => 2,
        "&" | "|" | "^" => 3,
        _ => 4,
    };
    group(left) == group(right) && group(left) != 4
}

fn parent_kind(node: Node) -> &'static str {
    node.parent().map_or("", |parent| parent.kind())
}

/// Whether two tokens on the same line have a space between them.
fn space_between(previous: Node, next: Node) -> bool {
    let (left, right) = (previous.kind(), next.kind());
    let (left_parent, right_parent) = (parent_kind(previous), parent_kind(next));
    let is_type_arguments = |kind: &str| matches!(kind, "type_arguments" | "type_parameters");
    let is_array = |kind: &str| {
        matches!(
            kind,
            "array_initializer" | "element_value_array_initializer"
        )
    };
    if matches!(right, ";" | "," | ")" | "]" | "." | "::" | "..." | "[") {
        return false;
    }
    if matches!(left, "(" | "[" | "." | "::" | "@") {
        return false;
    }
    if left == "{" && is_array(left_parent) {
        return right != "}";
    }
    if right == "}" && is_array(right_parent) {
        return left != "{";
    }
    if matches!(left_parent, "unary_expression" | "update_expression")
        && previous.prev_sibling().is_none()
        && !previous.is_named()
    {
        return false;
    }
    if right == "(" {
        return !matches!(left, "identifier" | "type_identifier" | "this" | "super")
            && !(left == ">" && is_type_arguments(left_parent));
    }
    if right_parent == "update_expression" && next.next_sibling().is_none() && !next.is_named() {
        return false;
    }
    if right == "<" && is_type_arguments(right_parent) {
        return !matches!(
            left,
            "identifier" | "type_identifier" | "scoped_type_identifier" | ">"
        );
    }
    if left == "<" && is_type_arguments(left_parent) {
        return false;
    }
    if right == ">" && is_type_arguments(right_parent) {
        return false;
    }
    if left == ">" && is_type_arguments(left_parent) {
        let in_call = previous
            .parent()
            .and_then(|arguments| arguments.parent())
            .is_some_and(|owner| {
                matches!(
                    owner.kind(),
                    "method_invocation" | "object_creation_expression"
                )
            });
        return !in_call || right == "{";
    }
    if right == ":"
        && matches!(
            right_parent,
            "labeled_statement" | "switch_block_statement_group" | "switch_label"
        )
    {
        return false;
    }
    true
}

/// A line comment with one space after `//` and between its words, cut
/// into lines that fit, each further line starting at `indent`.
fn line_comment(source: &str, indent: &str, column: usize) -> String {
    let content = &source[2..];
    if content.starts_with('/') || content.starts_with("$NON-NLS") || content.trim().is_empty() {
        return source.trim_end().to_owned();
    }
    let words: Vec<&str> = content.split_whitespace().collect();
    // 80 columns from where the comment starts, and never past the line's
    // 120.
    let width = (COMMENT_LENGTH - 3).min(LINE_LENGTH.saturating_sub(column + 3)).max(1);
    fill(&words, width)
        .into_iter()
        .map(|line| format!("// {line}"))
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

/// Words in lines no longer than `width`; a longer word is a line alone.
fn fill(words: &[&str], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in words {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// A block comment or javadoc laid out with ` * ` on each line. `indent`
/// is the indentation of the line it starts.
fn block_comment(source: &str, indent: &str, starts_line: bool) -> String {
    let javadoc = source.starts_with("/**") && source.len() > 4;
    if source.starts_with("/*-") || source == "/**/" || source == "/***/" {
        return source.to_owned();
    }
    let open = if javadoc { "/**" } else { "/*" };
    let inner = &source[open.len()..source.len() - 2];
    if !source.contains('\n') {
        let words: Vec<&str> = inner.split_whitespace().collect();
        let one_line = if words.is_empty() {
            format!("{open} */")
        } else {
            format!("{open} {} */", words.join(" "))
        };
        if one_line.chars().count() <= COMMENT_LENGTH || !starts_line {
            return one_line;
        }
    }
    let lines = comment_lines(inner, javadoc);
    if lines.is_empty() {
        return format!("{open}\n{indent} *\n{indent} */");
    }
    let mut out = String::from(open);
    for line in lines {
        out.push('\n');
        out.push_str(indent);
        match line {
            CommentLine::Blank => out.push_str(" *"),
            CommentLine::Inserted => out.push_str(" * "),
            CommentLine::Text(text) => {
                out.push_str(" * ");
                out.push_str(&text);
            }
            // The indentation pushed above is not the line's own, but for
            // an empty line.
            CommentLine::Raw(raw) if !raw.trim().is_empty() => {
                out.truncate(out.len() - indent.len());
                out.push_str(&raw);
            }
            CommentLine::Raw(_) => {}
        }
    }
    out.push('\n');
    out.push_str(indent);
    out.push_str(" */");
    out
}

enum CommentLine {
    /// A blank line the source has.
    Blank,
    /// A blank line the formatter puts in.
    Inserted,
    Text(String),
    /// A line kept as written, without a ` * `.
    Raw(String),
}

/// The lines of a block comment's inside: words single-spaced and wrapped,
/// `<pre>` kept as it is, and for javadoc a blank line before the tags
/// and around a `<pre>`, `<p>` on a line of its own, and the descriptions
/// of `@param` and `@throws` tags lined up under each other.
fn comment_lines(inner: &str, javadoc: bool) -> Vec<CommentLine> {
    let with_raw = stripped_with_raw(inner);
    let stripped: Vec<String> = with_raw.iter().map(|(line, _)| line.clone()).collect();
    let aligned = if javadoc {
        tag_alignment(&stripped)
    } else {
        vec![0; stripped.len()]
    };
    let width = COMMENT_LENGTH - 3;
    let mut lines: Vec<CommentLine> = Vec::new();
    let mut in_pre = false;
    let mut in_tags = false;
    let mut after_pre = false;
    let mut in_code_pre = false;
    // How far a tag's description continues past the ` * `: under the
    // description's first word.
    let mut hang = 0;
    let last_is_text =
        |lines: &Vec<CommentLine>| matches!(lines.last(), Some(CommentLine::Text(_) | CommentLine::Raw(_)));
    for (number, line) in stripped.iter().enumerate() {
        if in_pre {
            if line.contains("</pre>") {
                in_pre = false;
                after_pre = !in_code_pre;
            }
            match &with_raw[number].1 {
                // A line of a `<pre>` written without its `*`: as it is.
                Some(raw) if javadoc => lines.push(CommentLine::Raw(raw.clone())),
                _ if line.trim().is_empty() => lines.push(CommentLine::Blank),
                _ => lines.push(CommentLine::Text(line.trim_end().to_owned())),
            }
            continue;
        }
        let line = line.replace("<li> ", "<li>");
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            // A blank line written without its `*` Eclipse lays out itself.
            let unstarred = with_raw[number].1.is_some();
            lines.push(if javadoc && unstarred { CommentLine::Inserted } else { CommentLine::Blank });
            after_pre = false;
            continue;
        }
        let text = words.join(" ");
        if javadoc && after_pre {
            lines.push(CommentLine::Inserted);
        }
        after_pre = false;
        let pad = " ".repeat(hang);
        if javadoc && (text.starts_with("<pre>") || text.starts_with("<code><pre>")) {
            // A `<pre>` inside `<code>` is not set apart by blank lines.
            in_code_pre = text.starts_with("<code>");
            if last_is_text(&lines) && !in_code_pre {
                lines.push(CommentLine::Inserted);
            }
            in_pre = !text.contains("</pre>");
            after_pre = !in_pre && !in_code_pre;
            lines.push(CommentLine::Text(format!("{pad}{}", line.trim())));
            continue;
        }
        if javadoc && text.starts_with('@') {
            if !in_tags && last_is_text(&lines) {
                lines.push(CommentLine::Inserted);
            }

            in_tags = true;
            let named = is_named_tag(words[0]) && words.len() > 1;
            let lead = if named { 2 } else { 1 };
            let head = words[..lead].join(" ");
            hang = aligned[number].max(head.chars().count() + 1);
            let rest = words[lead..].join(" ");
            let mut filled =
                fill_units(&pieces_of(&rest, width - hang), width - hang, width - hang);
            if filled.first().is_some_and(String::is_empty) {
                filled.remove(0);
            }
            let first = match filled.first() {
                Some(description) => format!("{head:<hang$}{description}"),
                None => head,
            };
            lines.push(CommentLine::Text(first));
            let pad = " ".repeat(hang);
            lines.extend(
                filled
                    .iter()
                    .skip(1)
                    .map(|line| CommentLine::Text(format!("{pad}{line}"))),
            );
            continue;
        }
        let available = width - hang;
        // `</p>` on a line of its own, as Eclipse puts it.
        if javadoc && text.contains("</p>") && text != "</p>" {
            let mut rest = text.as_str();
            while let Some(at) = rest.find("</p>") {
                let before = rest[..at].trim();
                if !before.is_empty() {
                    let before = before.strip_prefix("<p>").map_or(before, str::trim_start);
                    if rest[..at].trim().starts_with("<p>") {
                        lines.push(CommentLine::Text(format!("{pad}<p>")));
                    }
                    if !before.is_empty() {
                        lines.extend(
                            fill_units(&pieces_of(before, available), available, available)
                                .into_iter()
                                .map(|line| CommentLine::Text(format!("{pad}{line}"))),
                        );
                    }
                }
                lines.push(CommentLine::Text(format!("{pad}</p>")));
                rest = rest[at + 4..].trim_start();
            }
            if !rest.is_empty() {
                lines.extend(
                    fill_units(&pieces_of(rest, available), available, available)
                        .into_iter()
                        .map(|line| CommentLine::Text(format!("{pad}{line}"))),
                );
            }
            continue;
        }
        if javadoc && text.starts_with("<p>") && text.len() > 3 {
            lines.push(CommentLine::Text(format!("{pad}<p>")));
            let rest = text[3..].trim_start();
            lines.extend(
                fill_units(&pieces_of(rest, available), available, available)
                    .into_iter()
                    .map(|line| CommentLine::Text(format!("{pad}{line}"))),
            );
            continue;
        }
        lines.extend(
            fill_units(&pieces_of(&text, available), available, available)
                .into_iter()
                .map(|line| CommentLine::Text(format!("{pad}{line}"))),
        );
    }
    lines
}

/// The lines between `/*` and `*/` without their leading ` * `, and
/// without blank lines at either end.
/// [`stripped_lines`], each with the line as written where it has no
/// leading `*`: Eclipse keeps such a line of a `<pre>` as it is.
fn stripped_with_raw(inner: &str) -> Vec<(String, Option<String>)> {
    let raw: Vec<&str> = inner.split('\n').collect();
    let count = raw.len();
    let mut stripped: Vec<(String, Option<String>)> = Vec::new();
    for (index, line) in raw.iter().enumerate() {
        let trimmed = line.trim_start();
        let starred = index == 0 || trimmed.starts_with('*');
        let rest = if index == 0 {
            trimmed
        } else {
            trimmed.strip_prefix('*').unwrap_or(trimmed)
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest).trim_end();
        if index + 1 == count && rest.trim().is_empty() {
            continue;
        }
        stripped.push((rest.to_owned(), (!starred).then(|| line.trim_end_matches('\r').to_owned())));
    }
    while stripped.first().is_some_and(|(line, _)| line.trim().is_empty()) {
        stripped.remove(0);
    }
    // A blank last line written without its `*` stays.
    while stripped.last().is_some_and(|(line, raw)| line.trim().is_empty() && raw.is_none()) {
        stripped.pop();
    }
    stripped
}

/// Tags whose first word is a name the description follows.
fn is_named_tag(tag: &str) -> bool {
    matches!(tag, "@param" | "@throws" | "@exception")
}

/// For each tag line, the column its description starts at: the same for
/// all the tags of a run of `@param`s, or of `@throws`, past the longest
/// name.
fn tag_alignment(lines: &[String]) -> Vec<usize> {
    let mut aligned = vec![0; lines.len()];
    fn group(tag: &str) -> &str {
        match tag {
            "@throws" | "@exception" => "@throws",
            other => other,
        }
    }
    let tags: Vec<(usize, Vec<&str>)> = lines
        .iter()
        .enumerate()
        .filter_map(|(number, line)| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .first()
                .is_some_and(|word| word.starts_with('@'))
                .then_some((number, words))
        })
        .collect();
    let mut start = 0;
    while start < tags.len() {
        let key = group(tags[start].1[0]);
        let mut end = start;
        while end < tags.len() && group(tags[end].1[0]) == key {
            end += 1;
        }
        if is_named_tag(tags[start].1[0]) {
            let column = tags[start..end]
                .iter()
                .filter(|(_, words)| words.len() > 1)
                .map(|(_, words)| words[0].chars().count() + words[1].chars().count() + 2)
                .max()
                .unwrap_or(0);
            for (number, _) in &tags[start..end] {
                aligned[*number] = column;
            }
        }
        start = end;
    }
    aligned
}

/// The pieces a comment line is filled with, each with whether a space
/// comes before it. An inline tag such as `{@link Foo bar}` or an HTML tag
/// such as `<a href="x">` is one piece with the text it touches; one too
/// long for a line is split inside its HTML tags only, at spaces, before
/// an attribute's quoted value and before an inline tag.
fn pieces_of(text: &str, width: usize) -> Vec<(String, bool)> {
    let mut units: Vec<String> = Vec::new();
    let mut open = false;
    for word in text.split_whitespace() {
        match units.last_mut() {
            Some(last) if open => {
                last.push(' ');
                last.push_str(word);
            }
            _ => units.push(word.to_owned()),
        }
        open = units.last().is_some_and(|unit| unclosed(unit));
    }
    let mut pieces = Vec::new();
    for unit in units {
        if unit.chars().count() <= width {
            pieces.push((unit, true));
            continue;
        }
        let mut first = true;
        for (part, space) in split_in_tags(&unit) {
            pieces.push((part, space || first));
            first = false;
        }
    }
    pieces
}

/// Whether a unit leaves something open that the next word belongs to:
/// an inline tag (of a `{@link}`, only up to the end of its reference, so
/// that its label may wrap), an HTML tag, or a `<code>` element.
fn unclosed(unit: &str) -> bool {
    #[derive(PartialEq)]
    enum Link {
        None,
        BeforeReference,
        Reference(i32),
        Label,
    }
    let characters: Vec<char> = unit.chars().collect();
    let starts = |at: usize, text: &str| {
        characters[at..]
            .iter()
            .copied()
            .take(text.len())
            .eq(text.chars())
    };
    let mut braces = 0;
    let mut tag = false;
    let mut code = false;
    let mut link = Link::None;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();
        let link_tag = ["{@linkplain", "{@link"].into_iter().find(|name| {
            starts(index, name) && matches!(characters.get(index + name.len()), None | Some(' '))
        });
        if let Some(name) = link_tag.filter(|_| braces == 0) {
            braces = 1;
            link = Link::BeforeReference;
            index += name.len();
            continue;
        }
        match character {
            '{' if next == Some('@') || braces > 0 => braces += 1,
            '}' if braces > 0 => {
                braces -= 1;
                if braces == 0 {
                    link = Link::None;
                }
            }
            '<' if braces == 0
                && next.is_some_and(|next| next.is_ascii_alphabetic() || next == '/') =>
            {
                if starts(index, "<code>") {
                    code = true;
                } else if starts(index, "</code>") {
                    code = false;
                }
                tag = true;
            }
            '>' if braces == 0 => tag = false,
            _ => {}
        }
        if braces == 1 {
            link = match (link, character) {
                (Link::BeforeReference, ' ') => Link::BeforeReference,
                (Link::BeforeReference, '(') => Link::Reference(1),
                (Link::BeforeReference, _) => Link::Reference(0),
                (Link::Reference(depth), '(') => Link::Reference(depth + 1),
                (Link::Reference(depth), ')') => Link::Reference(depth - 1),
                (Link::Reference(0), ' ') => Link::Label,
                (other, _) => other,
            };
        }
        index += 1;
    }
    let reference_done = matches!(link, Link::Reference(0) | Link::Label);
    tag || code || (braces > 0 && !reference_done)
}

/// A unit cut where an HTML tag in it may break: at a space, between `=`
/// and a quoted value, and before an inline tag. Each part says whether a
/// space came before it.
fn split_in_tags(unit: &str) -> Vec<(String, bool)> {
    let characters: Vec<char> = unit.chars().collect();
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut space = true;
    // A unit that closes a tag before it opens one starts inside a tag
    // the line before opened.
    let first_open = characters
        .windows(2)
        .position(|pair| pair[0] == '<' && (pair[1].is_ascii_alphabetic() || pair[1] == '/'));
    let first_close = characters.iter().position(|&character| character == '>');
    let mut tag = first_close.is_some_and(|close| first_open.is_none_or(|open| close < open));
    let mut braces = 0;
    for (index, &character) in characters.iter().enumerate() {
        let next = characters.get(index + 1).copied();
        let previous = index.checked_sub(1).map(|at| characters[at]);
        if tag && braces == 0 {
            let cut = match character {
                ' ' => Some(true),
                '"' if previous == Some('=') => Some(false),
                '{' if next == Some('@') => Some(false),
                _ => None,
            };
            if let Some(spaced) = cut {
                if !current.is_empty() {
                    parts.push((std::mem::take(&mut current), space));
                }
                space = spaced;
                if character == ' ' {
                    continue;
                }
            }
        }
        match character {
            '{' if next == Some('@') || braces > 0 => braces += 1,
            '}' if braces > 0 => braces -= 1,
            '<' if braces == 0
                && next.is_some_and(|next| next.is_ascii_alphabetic() || next == '/') =>
            {
                tag = true
            }
            '>' if braces == 0 => tag = false,
            _ => {}
        }
        current.push(character);
    }
    if !current.is_empty() {
        parts.push((current, space));
    }
    parts
}

/// Pieces in lines no longer than `first` (the first line) and `rest`;
/// a longer piece is a line alone.
fn fill_units(pieces: &[(String, bool)], first: usize, rest: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (piece, space) in pieces {
        let width = if lines.is_empty() { first } else { rest };
        let glue = usize::from(*space && !line.is_empty());
        if !line.is_empty() && line.chars().count() + glue + piece.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        } else if glue == 1 {
            line.push(' ');
        }
        line.push_str(piece);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// A text block with its lines indented by `indent` past their common
/// indentation.
fn text_block(source: &str, indent: &str, tab_size: usize) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    if lines.len() < 2 {
        return source.to_owned();
    }
    let common = lines[1..]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| columns(&line[..line.len() - line.trim_start().len()], tab_size))
        .min()
        .unwrap_or(0);
    let mut out = String::from(lines[0]);
    for line in &lines[1..] {
        out.push('\n');
        if line.trim().is_empty() {
            continue;
        }
        let leading = &line[..line.len() - line.trim_start().len()];
        let extra = columns(leading, tab_size) - common;
        out.push_str(indent);
        out.push_str(&" ".repeat(extra));
        out.push_str(line.trim_start());
    }
    out
}

/// The columns `text` takes, a tab reaching the next tab stop.
fn columns(text: &str, tab_size: usize) -> usize {
    let tab = tab_size.max(1);
    text.chars().fold(0, |width, character| {
        if character == '\t' {
            (width / tab + 1) * tab
        } else {
            width + 1
        }
    })
}

/// Byte offsets and LSP positions of one text.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn of(text: &'a str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Lines { text, starts }
    }

    fn position(&self, offset: usize) -> Position {
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let column = self.text[self.starts[line]..offset].encode_utf16().count();
        Position {
            line: line as u32,
            column: column as u32,
        }
    }

    /// The offset `at` names, its column counted on past the end of its line.
    fn unclamped(&self, at: Position) -> usize {
        let Some(&start) = self.starts.get(at.line as usize) else {
            return self.text.len();
        };
        let mut units = at.column as usize;
        for (offset, character) in self.text[start..].char_indices() {
            if units == 0 {
                return start + offset;
            }
            units = units.saturating_sub(character.len_utf16());
        }
        self.text.len()
    }

    #[cfg(test)]
    fn offset(&self, at: Position) -> usize {
        let Some(&start) = self.starts.get(at.line as usize) else {
            return self.text.len();
        };
        let mut units = at.column as usize;
        for (offset, character) in self.text[start..].char_indices() {
            if units == 0 || character == '\n' {
                return start + offset;
            }
            units = units.saturating_sub(character.len_utf16());
        }
        self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(text: &str) -> String {
        let options = Options {
            tab_size: 4,
            insert_spaces: false,
        };
        let lines = Lines::of(text);
        let mut edits = java(text, options, None);
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
        let mut out = text.to_owned();
        for edit in edits {
            let (start, end) = (lines.offset(edit.start), lines.offset(edit.end));
            out.replace_range(start..end, &edit.text);
        }
        out
    }

    #[test]
    fn lays_out_a_class() {
        assert_eq!(
            formatted("package p   ;\n      public class Baz {  String name;}\n"),
            "package p;\n\npublic class Baz {\n\tString name;\n}\n"
        );
    }

    #[test]
    fn leaves_a_member_that_does_not_parse() {
        assert_eq!(
            formatted("class A {\nint  a=1;\n  void bad(){ int x = ; }\n}\n"),
            "class A {\n\tint a = 1;\n\n\tvoid bad(){ int x = ; }\n}\n"
        );
    }

    #[test]
    fn formats_only_the_lines_a_range_touches() {
        let text = "class R {\n  int  a=1;\n      void m(){int x=1;}\n}\n";
        let options = Options {
            tab_size: 4,
            insert_spaces: false,
        };
        let range = (
            Position { line: 2, column: 0 },
            Position {
                line: 2,
                column: 24,
            },
        );
        let lines = Lines::of(text);
        let mut out = text.to_owned();
        let mut edits = java(text, options, Some(range));
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
        for edit in edits {
            out.replace_range(lines.offset(edit.start)..lines.offset(edit.end), &edit.text);
        }
        assert_eq!(
            out,
            "class R {\n  int  a=1;\n\n\tvoid m() {\n\t\tint x = 1;\n\t}\n}\n"
        );
    }

    #[test]
    fn wraps_a_line_comment_at_eighty_columns() {
        let words: Vec<&str> = std::iter::repeat_n("word", 30).collect();
        let comment = format!("//{}", words.join("  "));
        let laid_out = line_comment(&comment, "", 0);
        assert!(laid_out.lines().all(|line| line.len() <= COMMENT_LENGTH));
        assert!(laid_out.lines().all(|line| line.starts_with("// word")));
    }
}
