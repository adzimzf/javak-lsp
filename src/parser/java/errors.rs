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
use super::super::{Finding, Syn};
use super::GRAMMAR;

pub(super) fn syntax_errors(text: &str) -> Vec<Finding> {
    let (unclosed, text_read) = close_unterminated_strings(text);
    let Some(tree) = GRAMMAR.parse(&text_read) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    collect(tree.root_node(), &text_read, &mut findings);
    // The unclosed literal is the one error at its place: Eclipse's scanner
    // reports it and its parser takes the literal as given.
    let literals: Vec<_> = unclosed.iter().map(|&offset| at(offset, text)).collect();
    findings.retain(|finding| !literals.contains(&finding.at));
    findings.extend(unclosed.into_iter().map(|offset| Finding {
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

/// A token tree-sitter assumed: Eclipse asks for it on the token before.
fn missing(node: Node, text: &str, findings: &mut Vec<Finding>) {
    let token = node.kind();
    let rule = completed_rule(node);
    let before = token_before(node).unwrap_or(node);
    findings.push(Finding {
        at: at(before.start_byte(), text),
        message: format!("Syntax error, insert \"{token}\" to complete {rule}"),
    });
}

/// The rule Eclipse names as the one a missing token completes.
fn completed_rule(node: Node) -> &'static str {
    match node.kind() {
        ";" => {
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
            at: at(last.start_byte(), text),
            message: "Syntax error on token \"=\", VariableInitializer expected after this token"
                .to_owned(),
        });
        return;
    }

    findings.push(Finding {
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
