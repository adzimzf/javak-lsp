//! Java syntax errors, reported where and as the Eclipse compiler (jdtls)
//! reports them.
//!
//! tree-sitter says a token is missing, or that it could not place some
//! text; the Eclipse compiler says which token to insert to complete which
//! rule, or which token to delete. The two parsers recover differently, so
//! this reads the shape tree-sitter leaves and words it the way Eclipse
//! does for the same mistake:
//!
//! - a missing token is reported on the token before it:
//!   `Syntax error, insert ";" to complete BlockStatements`;
//! - a token nothing can use: `Syntax error on token "}", delete this token`;
//! - `int x = ;`: `Syntax error on token "=", VariableInitializer expected
//!   after this token`;
//! - `class Main` with no body: `Syntax error, insert "ClassBody" to
//!   complete ClassDeclaration`, on the name;
//! - a string literal its line ends inside: `String literal is not properly
//!   closed by a double-quote`, and the rest of the line is the literal.

use tree_sitter::Node;

use super::super::treesitter::at;
use super::super::{Finding, Position, Syn};
use super::GRAMMAR;

pub(super) fn syntax_errors(text: &str) -> Vec<Finding> {
    let (unclosed, mut text_read) = close_unterminated_strings(text);
    let mut findings: Vec<Finding> = Vec::new();
    // Each `;` a repair put in, where it stands in the text as read now.
    let mut inserted: Vec<Position> = Vec::new();
    let mut repairs = 0;
    let tree = loop {
        let Some(tree) = GRAMMAR.parse(&text_read) else {
            return Vec::new();
        };
        if repairs >= MAX_REPAIRS {
            break tree;
        }
        repairs += 1;
        let Some(Repair { inserted_at, text: repaired, findings: found }) = first_repair(tree.root_node(), &text_read) else {
            break tree;
        };
        let Some(offset) = inserted_at else {
            findings.extend(found);
            text_read = repaired;
            continue;
        };
        // What follows the `;` on its line moves one column on.
        let place = at(offset, &text_read);
        let shift = |position: &mut Position| {
            if position.line == place.line && position.column >= place.column {
                position.column += 1;
            }
        };
        for finding in &mut findings {
            shift(&mut finding.at);
            finding.end.iter_mut().for_each(shift);
        }
        inserted.iter_mut().for_each(shift);
        inserted.push(place);
        findings.extend(found);
        text_read = repaired;
    };
    collect(tree.root_node(), &text_read, &mut findings);
    // Back to the places of the text as written.
    let unshift = |position: &mut Position| {
        let before = inserted
            .iter()
            .filter(|place| place.line == position.line && place.column < position.column)
            .count();
        position.column -= before as u32;
    };
    for finding in &mut findings {
        unshift(&mut finding.at);
        finding.end.iter_mut().for_each(unshift);
    }
    // The unclosed literal is the one error at its place: Eclipse's scanner
    // reports it and its parser takes the literal as given.
    let literals: Vec<_> = unclosed.iter().map(|&offset| at(offset, text)).collect();
    findings.retain(|finding| !literals.contains(&finding.at));
    // Eclipse's range runs to the start of the next line.
    findings.extend(unclosed.into_iter().map(|offset| Finding {
        end: Some(Position { line: at(offset, text).line + 1, column: 0 }),
        at: at(offset, text),
        message: "String literal is not properly closed by a double-quote".to_owned(),
    }));
    findings.sort_by_key(|finding| finding.at);
    findings.dedup();
    findings
}

/// Where each string literal starts that its line ends inside, and the text
/// with each of those closed at the end of its line, as the Eclipse scanner
/// reads it: the literal is the rest of the line. Byte offsets stay where
/// they were.
fn close_unterminated_strings(text: &str) -> (Vec<usize>, String) {
    let bytes = text.as_bytes();
    let mut read = bytes.to_vec();
    let mut unclosed = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = line_end(bytes, index);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = find(bytes, index + 2, b"*/").map_or(bytes.len(), |end| end + 2);
            }
            b'"' if bytes[index..].starts_with(b"\"\"\"") => {
                index = find(bytes, index + 3, b"\"\"\"").map_or(bytes.len(), |end| end + 3);
            }
            quote @ (b'"' | b'\'') => {
                let start = index;
                index += 1;
                loop {
                    match bytes.get(index) {
                        None | Some(b'\n' | b'\r') => {
                            if quote == b'"' {
                                unclosed.push(start);
                                close(&mut read, start, index);
                            }
                            break;
                        }
                        Some(b'\\') => index += 2,
                        Some(&byte) if byte == quote => {
                            index += 1;
                            break;
                        }
                        Some(_) => index += 1,
                    }
                }
            }
            _ => index += 1,
        }
    }
    let read = String::from_utf8(read).unwrap_or_else(|_| text.to_owned());
    (unclosed, read)
}

/// Turns `read[start..end]`, a string literal without its closing quote,
/// into one of the same length that is closed: `"` and blanks and `"`, or a
/// `0` where there is room for only one character.
fn close(read: &mut [u8], start: usize, end: usize) {
    let end = end.min(read.len());
    for byte in &mut read[start..end] {
        *byte = b' ';
    }
    if end - start >= 2 {
        read[start] = b'"';
        read[end - 1] = b'"';
    } else {
        read[start] = b'0';
    }
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |offset| from + offset)
}

fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn collect(node: Node, text: &str, findings: &mut Vec<Finding>) {
    if node.is_missing() {
        missing(node, text, findings);
        return;
    }
    if node.is_error() {
        unplaced(node, text, findings);
        return;
    }
    if !node.has_error() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, text, findings);
    }
}

/// A token tree-sitter assumed: Eclipse asks for it on the token before,
/// the whole of that token.
fn missing(node: Node, text: &str, findings: &mut Vec<Finding>) {
    let token = node.kind();
    let before = token_before(node).unwrap_or(node);
    let place = |message: String| Finding {
        at: at(before.start_byte(), text),
        end: Some(at(before.end_byte(), text)),
        message,
    };
    if token == ";"
        && let Some(statement) = node.parent().filter(|parent| {
            parent.kind().ends_with("_statement")
                || matches!(parent.kind(), "local_variable_declaration" | "explicit_constructor_invocation")
        })
    {
        // `break`, `continue`, a `do` loop: the token before wants it.
        if matches!(
            statement.kind(),
            "break_statement" | "continue_statement" | "do_statement" | "explicit_constructor_invocation"
        ) {
            let spelled = before.utf8_text(text.as_bytes()).unwrap_or_default();
            findings.push(place(format!("Syntax error on token \"{spelled}\", ; expected after this token")));
            return;
        }
        // `x = name` reads on as a method reference, `name::new`.
        if ends_in_a_name(statement, before) {
            let rule = if ends_at_a_close(node) { "Expression" } else { "ReferenceExpression" };
            findings.push(place(format!("Syntax error, insert \":: IdentifierOrNew\" to complete {rule}")));
        }
    }
    let rule = completed_rule(node);
    findings.push(place(format!("Syntax error, insert \"{token}\" to complete {rule}")));
}

/// Java's statement keywords, which a name cannot be.
const KEYWORDS: &[&str] = &["case", "default", "if", "while", "for", "do", "try", "switch", "synchronized", "else", "catch", "finally"];

/// Whether an expression statement or a local's initializer ends in a
/// name (`m`, `a.b`), not in a field of `this` or of a call.
fn ends_in_a_name(statement: Node, last: Node) -> bool {
    if !matches!(statement.kind(), "expression_statement" | "local_variable_declaration") || last.kind() != "identifier" {
        return false;
    }
    let Some(parent) = last.parent() else {
        return false;
    };
    match parent.kind() {
        "variable_declarator" => parent.child_by_field_name("name") != Some(last),
        "field_access" => {
            let mut object = parent.child_by_field_name("object");
            while let Some(current) = object {
                match current.kind() {
                    "identifier" => return true,
                    "field_access" => object = current.child_by_field_name("object"),
                    _ => return false,
                }
            }
            false
        }
        _ => true,
    }
}

/// Whether what follows a missing `;` closes the code around it as Eclipse
/// sees it: the `}`s after it reach the end of a method's body before
/// any other token.
fn ends_at_a_close(node: Node) -> bool {
    let mut current = node;
    loop {
        let Some(close) = token_after(current) else {
            return true;
        };
        if close.kind() != "}" {
            return false;
        }
        let ends_a_body = close.parent().is_some_and(|block| {
            block.kind() == "constructor_body"
                || matches!(block.kind(), "class_body" | "interface_body" | "enum_body")
                || block.parent().is_some_and(|owner| {
                    matches!(owner.kind(), "method_declaration" | "constructor_declaration" | "static_initializer")
                })
        });
        if ends_a_body {
            return true;
        }
        current = close;
    }
}

/// The first token after `node`, comments left out.
fn token_after(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        let mut sibling = current.next_sibling();
        while let Some(candidate) = sibling {
            if !candidate.is_extra() && !candidate.is_missing() && candidate.end_byte() > candidate.start_byte() {
                return leaves(candidate).first().copied().or(Some(candidate));
            }
            sibling = candidate.next_sibling();
        }
        current = current.parent()?;
    }
}

/// The rule a missing `;` completes, by where its statement stands: the
/// first of a block or a class body completes the block's statements or
/// the body's declarations; a later one completes its own kind.
fn semicolon_rule(node: Node) -> Option<&'static str> {
    let statement = node.parent()?;
    let container = statement.parent()?;
    let mut previous = statement.prev_named_sibling();
    while let Some(candidate) = previous.filter(|candidate| candidate.is_extra() || candidate.kind() == "switch_label") {
        previous = candidate.prev_named_sibling();
    }
    let first = previous.is_none();
    let own = match statement.kind() {
        "expression_statement" => "Statement",
        "local_variable_declaration" => "LocalVariableDeclarationStatement",
        "return_statement" => "ReturnStatement",
        "throw_statement" => "ThrowStatement",
        "assert_statement" => "AssertStatement",
        "field_declaration" => "FieldDeclaration",
        "method_declaration" => return Some("MethodDeclaration"),
        _ => return None,
    };
    Some(match container.kind() {
        "class_body" | "interface_body" | "enum_body_declarations" | "annotation_type_body" => {
            if first || own != "FieldDeclaration" { "ClassBodyDeclarations" } else { own }
        }
        "block" | "constructor_body" | "switch_block_statement_group" => {
            if first { "BlockStatements" } else { own }
        }
        "program" => "CompilationUnit",
        // The one statement of an `if`, a loop.
        _ => "Statement",
    })
}

/// The rule Eclipse names as the one a missing token completes.
fn completed_rule(node: Node) -> &'static str {
    match node.kind() {
        ";" => {
            if let Some(rule) = semicolon_rule(node) {
                return rule;
            }
            let mut ancestor = node.parent();
            while let Some(current) = ancestor {
                match current.kind() {
                    "class_body"
                    | "enum_body"
                    | "interface_body"
                    | "annotation_type_body"
                    | "enum_body_declarations" => return "ClassBodyDeclarations",
                    "block"
                    | "constructor_body"
                    | "switch_block"
                    | "switch_block_statement_group"
                    | "lambda_expression" => return "BlockStatements",
                    "program" => return "CompilationUnit",
                    _ => ancestor = current.parent(),
                }
            }
            "BlockStatements"
        }
        "}" => match node.parent().map(|parent| parent.kind()) {
            Some("class_body") => "ClassBody",
            Some("interface_body") => "InterfaceBody",
            Some("enum_body") => "EnumBody",
            Some("annotation_type_body") => "AnnotationTypeBody",
            Some("array_initializer") => "ArrayInitializer",
            Some("switch_block") => "SwitchBlock",
            Some("block" | "constructor_body")
                if node
                    .parent()
                    .and_then(|block| block.parent())
                    .is_some_and(|owner| {
                        matches!(
                            owner.kind(),
                            "method_declaration" | "constructor_declaration"
                        )
                    }) =>
            {
                "MethodBody"
            }
            _ => "Block",
        },
        ")" | "]" => "Expression",
        _ => "Expression",
    }
}

/// How many `;` a file is repaired with before the rest is reported as
/// tree-sitter leaves it.
const MAX_REPAIRS: usize = 32;

/// The first text tree-sitter could not place that a `;` repairs: where
/// the `;` goes, the text with it, and what Eclipse says of it.
fn first_repair(root: Node, text: &str) -> Option<Repair> {
    const ERRORS_TRIED: usize = 4;
    let mut errors = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if errors.len() >= ERRORS_TRIED {
            break;
        }
        if node.is_error() || (node.is_missing() && node.kind() == ";") {
            errors.push(node);
            continue;
        }
        if !node.has_error() {
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    errors.into_iter().find_map(|error| {
        // A `;` tree-sitter assumed is put in, so that what follows reads
        // as it would with it (`x = y // note` and then `else`).
        if error.is_missing() {
            if let Some(repair) = label_then_token(error, text) {
                return Some(repair);
            }
            let offset = match keyword_named(error, text) {
                Some(keyword) => token_before(keyword)?.end_byte(),
                // `pos++` and then a statement: the `++` is `pos`'s.
                None if token_after(error).is_some_and(|after| matches!(after.kind(), "++" | "--")) => {
                    token_after(error)?.end_byte()
                }
                // Right after the token before it, not after a comment.
                None => token_before(error).map_or(error.start_byte(), |before| before.end_byte()),
            };
            return semicolon_at(offset, text);
        }
        repaired_by_semicolon(error, text)
    })
}

/// A text repaired with a `;`, and what Eclipse says of what it lacked.
struct Repair {
    /// Where the `;` was put in; `None` where it took a token's place.
    inserted_at: Option<usize>,
    text: String,
    findings: Vec<Finding>,
}

/// `continue` and `list.add(x)` on the next line: Eclipse reads `list` as
/// the label and wants a `;` for the `.` after it.
fn label_then_token(missing: Node, text: &str) -> Option<Repair> {
    let statement = missing.parent()?;
    let label = token_before(missing)?;
    let spelled_label = label.utf8_text(text.as_bytes()).unwrap_or_default();
    if !matches!(statement.kind(), "break_statement" | "continue_statement")
        || label.kind() != "identifier"
        || KEYWORDS.contains(&spelled_label)
    {
        return None;
    }
    let after = token_after(missing)?;
    if after.end_byte() != after.start_byte() + 1 || after.kind() == "}" {
        return None;
    }
    let spelled = after.utf8_text(text.as_bytes()).unwrap_or_default();
    let finding = Finding {
        at: at(after.start_byte(), text),
        end: Some(at(after.end_byte(), text)),
        message: format!("Syntax error on token \"{spelled}\", ; expected"),
    };
    let repaired = format!("{};{}", &text[..after.start_byte()], &text[after.end_byte()..]);
    Some(Repair { inserted_at: None, text: repaired, findings: vec![finding] })
}

/// A keyword tree-sitter took for a name in the statement a `;` is missing
/// from (`continue` and `if (...)` on the next line, a label `if`): the
/// `;` goes before it.
fn keyword_named<'tree>(missing: Node<'tree>, text: &str) -> Option<Node<'tree>> {
    if missing.kind() != ";" {
        return None;
    }
    leaves(missing.parent()?).into_iter().find(|leaf| {
        leaf.kind() == "identifier" && KEYWORDS.contains(&leaf.utf8_text(text.as_bytes()).unwrap_or_default())
    })
}

/// `text` with a `;` at `offset`, and what Eclipse says of its absence.
fn semicolon_at(offset: usize, text: &str) -> Option<Repair> {
    let repaired = format!("{};{}", &text[..offset], &text[offset..]);
    let tree = GRAMMAR.parse(&repaired)?;
    let semicolon = tree.root_node().descendant_for_byte_range(offset, offset + 1).filter(|found| found.kind() == ";")?;
    let mut findings = Vec::new();
    missing(semicolon, &repaired, &mut findings);
    Some(Repair { inserted_at: Some(offset), text: repaired, findings })
}

/// Text tree-sitter could not place that a `;` before it or after one of
/// its last tokens makes a statement of (`foo(1) }`): Eclipse asks for
/// the `;`.
fn repaired_by_semicolon(node: Node, text: &str) -> Option<Repair> {
    const TRIED: usize = 3;
    let tokens = leaves(node);
    tokens.first()?;
    // The end of the error first, where a statement would end; then
    // before it; then after its first tokens.
    let placed: Vec<&Node> = tokens.iter().filter(|token| !matches!(token.kind(), "{" | "}" | ";")).collect();
    let mut offsets: Vec<usize> = placed.iter().rev().take(TRIED).map(|token| token.end_byte()).collect();
    offsets.extend(token_before(node).map(|before| before.end_byte()));
    offsets.extend(placed.iter().take(TRIED).map(|token| token.end_byte()));
    let mut seen = std::collections::HashSet::new();
    offsets.retain(|offset| seen.insert(*offset));
    // The repair must leave the error's surroundings whole as well.
    let region_end = node.parent().map_or(node.end_byte(), |parent| parent.end_byte()) + 1;
    // `continue` and then `list.add(x)`: `list` is a label to Eclipse.
    // The `continue` may stand before the error.
    let mut with_before: Vec<Node> = token_before(node).into_iter().collect();
    with_before.extend(tokens.iter().copied());
    let tokens_seen = &with_before;
    for (index, token) in tokens_seen.iter().enumerate() {
        if !matches!(token.kind(), "continue" | "break") {
            continue;
        }
        let (Some(label), Some(after)) = (tokens_seen.get(index + 1), tokens_seen.get(index + 2)) else {
            continue;
        };
        let spelled_label = label.utf8_text(text.as_bytes()).unwrap_or_default();
        if label.kind() == "identifier"
            && !KEYWORDS.contains(&spelled_label)
            && after.end_byte() == after.start_byte() + 1
            && !matches!(after.kind(), "}" | ";")
        {
            let spelled = after.utf8_text(text.as_bytes()).unwrap_or_default();
            let finding = Finding {
                at: at(after.start_byte(), text),
                end: Some(at(after.end_byte(), text)),
                message: format!("Syntax error on token \"{spelled}\", ; expected"),
            };
            let repaired = format!("{};{}", &text[..after.start_byte()], &text[after.end_byte()..]);
            return Some(Repair { inserted_at: None, text: repaired, findings: vec![finding] });
        }
    }
    for offset in offsets {
        let repaired = format!("{};{}", &text[..offset], &text[offset..]);
        let tree = GRAMMAR.parse(&repaired)?;
        let root = tree.root_node();
        if has_error_in(root, node.start_byte().min(offset), region_end + 1) {
            continue;
        }
        let Some(semicolon) = root.descendant_for_byte_range(offset, offset + 1).filter(|found| found.kind() == ";") else {
            continue;
        };
        // `return` and then `if (b) foo();`: tree-sitter calls a method
        // `if`; the `;` goes before the keyword.
        if let Some(keyword) = keyword_named(semicolon, &repaired) {
            let before = token_before(keyword)?.end_byte();
            if before < offset {
                return semicolon_at(before, text);
            }
        }
        // What is reported lies before the `;`: the same places in `text`.
        let mut findings = Vec::new();
        missing(semicolon, &repaired, &mut findings);
        return Some(Repair { inserted_at: Some(offset), text: repaired, findings });
    }
    None
}

/// Whether tree-sitter could not place something between `start` and `end`.
fn has_error_in(node: Node, start: usize, end: usize) -> bool {
    if node.end_byte() < start || node.start_byte() > end {
        return false;
    }
    if node.is_error() || node.is_missing() {
        return true;
    }
    if !node.has_error() {
        return false;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    children.into_iter().any(|child| has_error_in(child, start, end))
}

/// Text tree-sitter could not place.
fn unplaced(node: Node, text: &str, findings: &mut Vec<Finding>) {
    let tokens = leaves(node);
    let Some(&first) = tokens.first() else {
        return;
    };
    let spelled = |token: Node| {
        token
            .utf8_text(text.as_bytes())
            .unwrap_or_default()
            .to_owned()
    };

    // `class Main` with nothing after it.
    if let Some(index) = tokens
        .iter()
        .position(|token| matches!(token.kind(), "class" | "interface" | "enum" | "record"))
    {
        let has_body = tokens.iter().any(|token| token.kind() == "{");
        if let Some(&name) = tokens.get(index + 1).filter(|_| !has_body) {
            let (body, declaration) = match tokens[index].kind() {
                "interface" => ("InterfaceBody", "InterfaceDeclaration"),
                "enum" => ("EnumBody", "EnumDeclaration"),
                "record" => ("RecordBody", "RecordDeclaration"),
                _ => ("ClassBody", "ClassDeclaration"),
            };
            findings.push(Finding {
                end: Some(at(name.end_byte(), text)),
                at: at(name.start_byte(), text),
                message: format!("Syntax error, insert \"{body}\" to complete {declaration}"),
            });
            return;
        }
    }

    // `int x = ;`
    let last = *tokens.last().expect("not empty");
    if last.kind() == "=" {
        findings.push(Finding {
            end: Some(at(last.end_byte(), text)),
            at: at(last.start_byte(), text),
            message: "Syntax error on token \"=\", VariableInitializer expected after this token"
                .to_owned(),
        });
        return;
    }

    findings.push(Finding {
        end: Some(at(first.end_byte(), text)),
        at: at(first.start_byte(), text),
        message: format!(
            "Syntax error on token \"{}\", delete this token",
            spelled(first)
        ),
    });
}

/// The tokens under `node`, comments left out.
fn leaves(node: Node) -> Vec<Node> {
    let mut tokens = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.child_count() == 0 || is_token(current) {
            if !current.is_extra() && !current.is_missing() {
                tokens.push(current);
            }
            continue;
        }
        let mut cursor = current.walk();
        let children: Vec<Node> = current.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    tokens
}

/// A node Eclipse reads as one token: a literal, whatever its parts.
fn is_token(node: Node) -> bool {
    matches!(node.kind(), "string_literal" | "character_literal")
}

/// The last token that ends at or before `node` starts.
fn token_before(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        let mut sibling = current.prev_sibling();
        while let Some(candidate) = sibling {
            if !candidate.is_extra()
                && !candidate.is_missing()
                && candidate.end_byte() > candidate.start_byte()
            {
                return leaves(candidate).last().copied().or(Some(candidate));
            }
            sibling = candidate.prev_sibling();
        }
        current = current.parent()?;
    }
}

/// A language feature and the first Java source level that has it, as
/// the Eclipse compiler names it.
struct Feature {
    name: &'static str,
    since: u32,
    /// Whether `node` is written with the feature.
    written_by: fn(&Syn) -> bool,
}

const FEATURES: &[Feature] = &[Feature {
    name: "Text Blocks",
    since: 15,
    written_by: |node| {
        node.kind == "string_literal"
            && node.children.first().is_some_and(|quote| quote.kind == "\"\"\"")
    },
}];

/// Every use of a feature the source level `level` does not have, at
/// where it is written: `The Java feature 'Text Blocks' is only available
/// with source level 15 and above`.
pub(super) fn above_java_level(tree: &Syn, level: u32) -> Vec<Finding> {
    tree.walk()
        .into_iter()
        .filter_map(|node| {
            let feature = FEATURES
                .iter()
                .find(|feature| level < feature.since && (feature.written_by)(node))?;
            Some(Finding {
                end: Some(node.end),
                at: node.start,
                message: format!(
                    "The Java feature '{}' is only available with source level {} and above",
                    feature.name, feature.since
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(text: &str) -> Vec<String> {
        syntax_errors(text)
            .into_iter()
            .map(|finding| {
                format!(
                    "{}:{} {}",
                    finding.at.line + 1,
                    finding.at.column + 1,
                    finding.message
                )
            })
            .collect()
    }

    #[test]
    fn a_later_statement_completes_its_own_kind() {
        assert_eq!(
            reported("class A {\n  void f() {\n    foo(1);\n    foo(2)\n    int x = 1\n  }\n}\n"),
            [
                "4:10 Syntax error, insert \";\" to complete Statement",
                "5:13 Syntax error, insert \";\" to complete LocalVariableDeclarationStatement",
            ]
        );
    }

    #[test]
    fn a_name_at_the_end_reads_on_as_a_method_reference() {
        assert_eq!(
            reported("class A {\n  void f(int m) {\n    m = m\n  }\n}\n"),
            [
                "3:9 Syntax error, insert \":: IdentifierOrNew\" to complete Expression",
                "3:9 Syntax error, insert \";\" to complete BlockStatements",
            ]
        );
        assert_eq!(
            reported("class A {\n  void f(int m) {\n    if (m > 0) {\n      foo();\n      m = m\n    }\n    foo();\n  }\n}\n"),
            [
                "5:11 Syntax error, insert \":: IdentifierOrNew\" to complete ReferenceExpression",
                "5:11 Syntax error, insert \";\" to complete Statement",
            ]
        );
    }

    #[test]
    fn a_statement_tree_sitter_cannot_place_is_repaired_with_its_semicolon() {
        // `foo(1) }` on one line, `return` before an `if`, `continue`
        // before a name Eclipse reads as its label.
        assert_eq!(
            reported("class A {\n  void f() { foo(1); foo(2) }\n}\n"),
            ["2:27 Syntax error, insert \";\" to complete Statement"]
        );
        assert_eq!(
            reported("class A {\n  void f(boolean b) {\n    if (b) return\n\n    if (b) foo();\n  }\n}\n"),
            ["3:12 Syntax error, insert \";\" to complete Statement"]
        );
        assert_eq!(
            reported("class A {\n  void f(java.util.List<String> l) {\n    for (String s : l) {\n      if (s.isEmpty()) continue\n      l.add(s);\n    }\n  }\n}\n"),
            ["5:8 Syntax error on token \".\", ; expected"]
        );
    }

    #[test]
    fn a_missing_semicolon_is_asked_for_on_the_token_before_it() {
        assert_eq!(
            reported("class A {\n  void f() {\n    int x = 10\n  }\n}\n"),
            ["3:13 Syntax error, insert \";\" to complete BlockStatements"]
        );
        assert_eq!(
            reported("class A {\n  int count = 1\n}\n"),
            ["2:15 Syntax error, insert \";\" to complete ClassBodyDeclarations"]
        );
    }

    #[test]
    fn an_unclosed_string_is_the_rest_of_its_line() {
        assert_eq!(
            reported("class A {\n  String f() { return \"x; }\n}\n"),
            [
                "2:23 String literal is not properly closed by a double-quote",
                "3:1 Syntax error, insert \"}\" to complete ClassBody",
            ]
        );
    }

    #[test]
    fn a_string_closed_on_its_line_and_a_text_block_are_not_errors() {
        assert!(reported("class A {\n  String s = \"a\\\"b\";\n  String t = \"\"\"\n    x\n    \"\"\";\n  char c = '\"';\n}\n").is_empty());
    }
}
