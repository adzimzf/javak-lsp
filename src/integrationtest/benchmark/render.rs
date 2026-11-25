//! An answer written down as lines a reader compares by eye, the same way
//! for both servers: places as `/path:line:column` below the project root
//! (or in a library, whichever jar or module each server found it in),
//! lists sorted, and the wording a server chooses for itself (a hover's
//! documentation, a completion's detail) left out.

use serde_json::Value;

use super::probe::Method;
use crate::integrationtest::server::{Session, LIBRARY};

pub(super) const NOTHING: &str = "(nothing)";

/// What an answer is read against.
pub(super) struct View<'a> {
    pub(super) session: &'a Session,
    /// The file's text as the server has it, edits included.
    pub(super) text: &'a str,
    /// The completion prefix, the symbol query.
    pub(super) word: &'a str,
}

impl View<'_> {
    pub(super) fn render(&self, method: Method, answer: &Value) -> Vec<String> {
        if is_empty(answer) && method != Method::Formatting {
            return vec![NOTHING.to_owned()];
        }
        let lines = match method {
            Method::Definition
            | Method::Declaration
            | Method::TypeDefinition
            | Method::Implementation
            | Method::References => sorted(self.locations(answer)),
            Method::DocumentHighlight => sorted(
                items(answer)
                    .map(|highlight| {
                        format!("{} {}", range(&highlight["range"]), kind_of_highlight(highlight))
                    })
                    .collect(),
            ),
            Method::Hover => hover(answer),
            Method::SignatureHelp => signature_help(answer),
            Method::PrepareRename => vec![if answer.get("range").is_some() {
                range(&answer["range"])
            } else if answer.get("start").is_some() {
                range(answer)
            } else {
                "(the default range)".to_owned()
            }],
            Method::Rename => sorted(self.workspace_edit(answer)),
            Method::CallHierarchy | Method::TypeHierarchy => {
                answer.as_array().map_or_else(Vec::new, |lines| {
                    lines.iter().filter_map(|line| line.as_str().map(str::to_owned)).collect()
                })
            }
            Method::CodeAction => sorted(
                items(answer)
                    .map(|action| action["title"].as_str().unwrap_or_default().to_owned())
                    .collect(),
            ),
            Method::SelectionRange => selection_ranges(answer),
            Method::WorkspaceSymbol => sorted(
                items(answer)
                    .filter(|symbol| {
                        symbol["name"].as_str().is_some_and(|name| name.starts_with(self.word))
                    })
                    .map(|symbol| {
                        format!(
                            "{} {} in {} at {}",
                            symbol_kind(&symbol["kind"]),
                            symbol["name"].as_str().unwrap_or_default(),
                            symbol["containerName"].as_str().unwrap_or_default(),
                            self.place(&symbol["location"]["uri"]),
                        )
                    })
                    .collect(),
            ),
            Method::Completion => self.completion(answer),
            Method::Diagnostics => sorted(
                items(answer)
                    .map(|diagnostic| {
                        format!(
                            "{} {} {}",
                            range(&diagnostic["range"]),
                            severity(&diagnostic["severity"]),
                            diagnostic["message"].as_str().unwrap_or_default()
                        )
                    })
                    .collect(),
            ),
            Method::DocumentSymbol => {
                let mut lines = Vec::new();
                for symbol in items(answer) {
                    outline(symbol, 0, &mut lines);
                }
                lines
            }
            Method::FoldingRange => {
                let mut folds: Vec<(u64, u64, String)> = items(answer)
                    .map(|fold| {
                        (
                            fold["startLine"].as_u64().unwrap_or_default() + 1,
                            fold["endLine"].as_u64().unwrap_or_default() + 1,
                            fold["kind"].as_str().unwrap_or("region").to_owned(),
                        )
                    })
                    .collect();
                folds.sort();
                folds.dedup();
                folds
                    .into_iter()
                    .map(|(start, end, kind)| format!("{start}-{end} {kind}"))
                    .collect()
            }
            Method::SemanticTokens => self.semantic_tokens(answer),
            Method::Formatting => formatted(self.text, answer),
            Method::CodeLens => sorted(
                items(answer)
                    .map(|lens| {
                        let title = lens["command"]["title"].as_str().unwrap_or("(unresolved)");
                        format!("{} {title}", range(&lens["range"]))
                    })
                    .collect(),
            ),
            Method::InlayHint => sorted(
                items(answer)
                    .map(|hint| {
                        let position = &hint["position"];
                        let label = match &hint["label"] {
                            Value::String(label) => label.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .map(|part| part["value"].as_str().unwrap_or_default())
                                .collect(),
                            _ => String::new(),
                        };
                        format!(
                            "{}:{} {label}",
                            position["line"].as_u64().unwrap_or_default() + 1,
                            position["character"].as_u64().unwrap_or_default() + 1
                        )
                    })
                    .collect(),
            ),
        };
        if lines.is_empty() {
            vec![NOTHING.to_owned()]
        } else {
            lines
        }
    }

    /// A file named the same way whichever server names it.
    pub(super) fn place(&self, uri: &Value) -> String {
        let shown = self.session.display(uri.as_str().unwrap_or_default());
        // A library file is named by its package path alone: jdtls says
        // which jar or module it is in, javak-lsp which directory it
        // extracted the jar to. A package directory has no `.` in its name.
        if let Some(inside) = shown.strip_prefix(&format!("{LIBRARY}/")) {
            if let Some((first, rest)) = inside.split_once('/') {
                if first.contains('.') || first.starts_with("src-") {
                    return format!("{LIBRARY}/{rest}");
                }
            }
        }
        shown
    }

    pub(super) fn location(&self, location: &Value) -> String {
        let uri = location.get("targetUri").unwrap_or(&location["uri"]);
        let range = location
            .get("targetSelectionRange")
            .unwrap_or(&location["range"]);
        let place = self.place(uri);
        // Each server reads its own copy of a library file (a class file,
        // a source jar), so only the file is compared, not the line.
        if place.starts_with(LIBRARY) {
            return place;
        }
        format!("{place}:{}", start(range))
    }

    fn locations(&self, answer: &Value) -> Vec<String> {
        if answer.is_object() {
            return vec![self.location(answer)];
        }
        items(answer).map(|location| self.location(location)).collect()
    }

    /// What a rename changes: each line that reads differently once the
    /// edits are made, as `/path:line text`. Two servers can make the same
    /// change in different edits (jdtls sends a few wide ones), so the
    /// result is what is compared, not the edits.
    fn workspace_edit(&self, answer: &Value) -> Vec<String> {
        let uris: Vec<Value> = answer["changes"]
            .as_object()
            .map(|changes| changes.keys().map(|uri| Value::String(uri.clone())).collect())
            .unwrap_or_default();
        let mut by_file: Vec<(&Value, Vec<&Value>)> = answer["changes"]
            .as_object()
            .map(|changes| {
                uris.iter()
                    .zip(changes.values())
                    .map(|(uri, file_edits)| (uri, items(file_edits).collect()))
                    .collect()
            })
            .unwrap_or_default();
        let mut lines = Vec::new();
        for change in items(&answer["documentChanges"]) {
            if change.get("textDocument").is_some() {
                by_file.push((&change["textDocument"]["uri"], items(&change["edits"]).collect()));
            } else if let Some(kind) = change["kind"].as_str() {
                let target = change.get("newUri").unwrap_or(&change["uri"]);
                lines.push(format!("{kind} {}", self.place(target)));
            }
        }
        for (uri, edits) in by_file {
            let place = self.place(uri);
            let text = uri
                .as_str()
                .and_then(|uri| tower_lsp::lsp_types::Url::parse(uri).ok())
                .and_then(|uri| uri.to_file_path().ok())
                .and_then(|path| std::fs::read_to_string(path).ok());
            let Some(text) = text else {
                // A file the harness cannot read: the edits themselves.
                lines.extend(edits.iter().map(|edit| {
                    format!(
                        "{place}:{} -> {}",
                        range(&edit["range"]),
                        edit["newText"].as_str().unwrap_or_default()
                    )
                }));
                continue;
            };
            let before: Vec<&str> = text.lines().collect();
            let after = applied(&text, edits.iter().copied());
            if before.len() == after.len() {
                for (number, (old, new)) in before.iter().zip(&after).enumerate() {
                    if *old != new.as_str() {
                        lines.push(format!("{place}:{} {}", number + 1, new.trim()));
                    }
                }
            } else {
                let after: Vec<String> = after;
                let before: Vec<String> = before.iter().map(|line| (*line).to_owned()).collect();
                lines.extend(
                    super::report::diff(&before, &after)
                        .into_iter()
                        .map(|line| format!("{place} {line}")),
                );
            }
        }
        lines
    }

    /// The names offered, as a client shows them after filtering by what
    /// was typed: each name once, sorted.
    fn completion(&self, answer: &Value) -> Vec<String> {
        let list = answer.get("items").unwrap_or(answer);
        let prefix = self.word.to_lowercase();
        let mut names: Vec<String> = items(list)
            .map(|item| {
                let text = item["filterText"]
                    .as_str()
                    .or_else(|| item["label"].as_str())
                    .unwrap_or_default();
                text.chars()
                    .take_while(|character| character.is_alphanumeric() || *character == '_')
                    .collect::<String>()
            })
            .filter(|name| !name.is_empty() && name.to_lowercase().starts_with(&prefix))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn semantic_tokens(&self, answer: &Value) -> Vec<String> {
        let legend = self.session.semantic_legend();
        let types = legend["tokenTypes"].as_array().cloned().unwrap_or_default();
        let modifiers = legend["tokenModifiers"].as_array().cloned().unwrap_or_default();
        let data: Vec<u64> = items(&answer["data"]).filter_map(Value::as_u64).collect();
        let (mut line, mut character) = (0, 0);
        let mut lines = Vec::new();
        for token in data.chunks_exact(5) {
            if token[0] > 0 {
                line += token[0];
                character = token[1];
            } else {
                character += token[1];
            }
            let kind = types
                .get(token[3] as usize)
                .and_then(Value::as_str)
                .unwrap_or("?");
            let mut set: Vec<&str> = modifiers
                .iter()
                .enumerate()
                .filter(|(bit, _)| token[4] & (1 << bit) != 0)
                .filter_map(|(_, modifier)| modifier.as_str())
                .collect();
            set.sort();
            lines.push(format!(
                "{}:{} len {} {kind} {}",
                line + 1,
                character + 1,
                token[2],
                set.join(",")
            ));
        }
        lines
    }
}

fn is_empty(answer: &Value) -> bool {
    match answer {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => {
            map.is_empty() || map.get("items").and_then(Value::as_array).is_some_and(Vec::is_empty)
        }
        _ => false,
    }
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn sorted(mut lines: Vec<String>) -> Vec<String> {
    lines.sort();
    lines.dedup();
    lines
}

fn start(range: &Value) -> String {
    format!(
        "{}:{}",
        range["start"]["line"].as_u64().unwrap_or_default() + 1,
        range["start"]["character"].as_u64().unwrap_or_default() + 1
    )
}

/// `line:column-line:column`, one-based.
pub(super) fn range(range: &Value) -> String {
    format!(
        "{}-{}:{}",
        start(range),
        range["end"]["line"].as_u64().unwrap_or_default() + 1,
        range["end"]["character"].as_u64().unwrap_or_default() + 1
    )
}

fn kind_of_highlight(highlight: &Value) -> &'static str {
    match highlight["kind"].as_u64() {
        Some(2) => "read",
        Some(3) => "write",
        _ => "text",
    }
}

fn severity(severity: &Value) -> &'static str {
    match severity.as_u64() {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "?",
    }
}

pub(super) fn symbol_kind(kind: &Value) -> &'static str {
    const KINDS: [&str; 26] = [
        "?", "file", "module", "namespace", "package", "class", "method", "property", "field",
        "constructor", "enum", "interface", "function", "variable", "constant", "string",
        "number", "boolean", "array", "object", "key", "null", "enumMember", "struct", "event",
        "operator",
    ];
    let index = kind.as_u64().unwrap_or_default() as usize;
    match index {
        26 => "typeParameter",
        _ => KINDS.get(index).copied().unwrap_or("?"),
    }
}

/// A hover as the code it shows: the signature, not the documentation
/// under it, which each server formats in its own way. Whether there is
/// documentation at all is kept.
fn hover(answer: &Value) -> Vec<String> {
    let mut code = Vec::new();
    let mut prose = false;
    let mut read = |text: &str, language: Option<&str>| {
        if language.is_some() {
            code.push(collapse(text));
            return;
        }
        let mut in_fence = false;
        let mut block = String::new();
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                if in_fence {
                    code.push(collapse(&block));
                    block.clear();
                }
                in_fence = !in_fence;
            } else if in_fence {
                block.push_str(line);
                block.push('\n');
            } else if !line.trim().is_empty() {
                prose = true;
            }
        }
    };
    let contents = &answer["contents"];
    let parts: Vec<&Value> = match contents {
        Value::Array(parts) => parts.iter().collect(),
        other => vec![other],
    };
    for part in parts {
        match part {
            Value::String(text) => read(text, None),
            Value::Object(object) => {
                let value = object.get("value").and_then(Value::as_str).unwrap_or_default();
                read(value, object.get("language").and_then(Value::as_str));
            }
            _ => {}
        }
    }
    if prose {
        code.push("(with documentation)".to_owned());
    }
    code
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn signature_help(answer: &Value) -> Vec<String> {
    let mut lines: Vec<String> = items(&answer["signatures"])
        .map(|signature| collapse(signature["label"].as_str().unwrap_or_default()))
        .collect();
    let active = answer["activeSignature"].as_u64().unwrap_or_default();
    let parameter = answer["activeParameter"].as_u64().unwrap_or_default();
    lines.push(format!("active signature {active}, parameter {parameter}"));
    lines
}

fn selection_ranges(answer: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for selection in items(answer) {
        let mut current = Some(selection);
        while let Some(selection) = current {
            lines.push(range(&selection["range"]));
            current = selection.get("parent").filter(|parent| !parent.is_null());
        }
    }
    lines
}

fn outline(symbol: &Value, depth: usize, lines: &mut Vec<String>) {
    let place = symbol
        .get("selectionRange")
        .or_else(|| symbol["location"].get("range"))
        .map(start)
        .unwrap_or_default();
    lines.push(format!(
        "{}{} {} {place}",
        "  ".repeat(depth),
        symbol_kind(&symbol["kind"]),
        symbol["name"].as_str().unwrap_or_default()
    ));
    for child in items(&symbol["children"]) {
        outline(child, depth + 1, lines);
    }
}

/// The file as it reads after the edits, so two servers that write the
/// same result in different edits agree.
fn formatted(text: &str, answer: &Value) -> Vec<String> {
    applied(text, items(answer))
}

/// `text` with the LSP text edits made, as lines.
fn applied<'a>(text: &str, edits: impl Iterator<Item = &'a Value>) -> Vec<String> {
    let offsets = line_offsets(text);
    let offset = |position: &Value| {
        let line = position["line"].as_u64().unwrap_or_default() as usize;
        let character = position["character"].as_u64().unwrap_or_default() as usize;
        let Some(&line_start) = offsets.get(line) else {
            return text.len();
        };
        let rest = &text[line_start..];
        let mut units = 0;
        for (index, character_here) in rest.char_indices() {
            if units >= character || character_here == '\n' {
                return line_start + index;
            }
            units += character_here.len_utf16();
        }
        text.len()
    };
    let mut edits: Vec<(usize, usize, &str)> = edits
        .map(|edit| {
            (
                offset(&edit["range"]["start"]),
                offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    edits.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut result = text.to_owned();
    for (start, end, new_text) in edits {
        if start <= end && end <= result.len() {
            result.replace_range(start..end, new_text);
        }
    }
    result.lines().map(str::to_owned).collect()
}

fn line_offsets(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_hover_is_its_code() {
        let jdtls = json!({"contents": [
            {"language": "java", "value": "String  greet(int times)"},
            "Says hello.\n\n * **Parameters:**",
        ]});
        let javak = json!({"contents": {"kind": "markdown", "value":
            "```java\nString greet(int times)\n```\n---\nSays hello."}});
        assert_eq!(hover(&jdtls), hover(&javak));
        assert_eq!(hover(&javak), ["String greet(int times)", "(with documentation)"]);
    }

    #[test]
    fn formatting_is_the_text_it_leaves() {
        let text = "class A {\nint  a;\n}\n";
        let edits = json!([
            {"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}},
             "newText": "    "},
            {"range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 5}},
             "newText": " "},
        ]);
        assert_eq!(formatted(text, &edits), ["class A {", "    int a;", "}"]);
        assert_eq!(formatted(text, &json!([])), ["class A {", "int  a;", "}"]);
    }
}
