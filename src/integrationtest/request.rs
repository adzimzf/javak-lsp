//! The request kinds a case can ask, and how each answer is written down.
//!
//! An answer is rendered as the facts a reader would check by eye — a
//! position and the source line there, a name — one per line, in an order
//! that does not depend on the server. JSON shapes, URIs of the temporary
//! workspace and details one server words differently from another are
//! left out, so the same expectation holds whichever server is asked.

use serde_json::{json, Value};

use super::fixture::Fixture;
use super::server::{OnError, Session};

/// What an answer with nothing in it is rendered as.
pub(super) const NOTHING: &str = "(nothing)";

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Definition,
    TypeDefinition,
    Implementation,
    References,
    DocumentHighlight,
    Completion,
    Hover,
    SignatureHelp,
    PrepareRename,
    /// The new name is `renamed`, or `Renamed` where the old one starts
    /// with a capital.
    Rename,
    DocumentSymbol,
    FoldingRange,
    /// The query is the identifier under the cursor.
    WorkspaceSymbol,
    /// Every diagnostic of the cursor's file, pulled; the cursor itself is
    /// not used.
    Diagnostics,
    Declaration,
    /// Asked for the selection `$0`…`$1`, or the empty range at `$0`, with
    /// no diagnostics in its context.
    CodeAction,
    /// The whole file, or the selection `$0`…`$1` when the case has one.
    Formatting,
    /// The whole file.
    InlayHint,
    CodeLens,
    /// Prepared at the cursor, then the calls into and out of the first
    /// item.
    CallHierarchy,
    /// Prepared at the cursor, then the supertypes and subtypes of the
    /// first item.
    TypeHierarchy,
    SemanticTokens,
    SelectionRange,
}

impl Kind {
    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::DocumentHighlight => "textDocument/documentHighlight",
            Self::Completion => "textDocument/completion",
            Self::Hover => "textDocument/hover",
            Self::SignatureHelp => "textDocument/signatureHelp",
            Self::PrepareRename => "textDocument/prepareRename",
            Self::Rename => "textDocument/rename",
            Self::DocumentSymbol => "textDocument/documentSymbol",
            Self::FoldingRange => "textDocument/foldingRange",
            Self::WorkspaceSymbol => "workspace/symbol",
            Self::Diagnostics => "textDocument/diagnostic",
            Self::Declaration => "textDocument/declaration",
            Self::CodeAction => "textDocument/codeAction",
            Self::Formatting => "textDocument/formatting",
            Self::InlayHint => "textDocument/inlayHint",
            Self::CodeLens => "textDocument/codeLens",
            Self::CallHierarchy => "textDocument/prepareCallHierarchy",
            Self::TypeHierarchy => "textDocument/prepareTypeHierarchy",
            Self::SemanticTokens => "textDocument/semanticTokens/full",
            Self::SelectionRange => "textDocument/selectionRange",
        }
    }

    fn params(self, session: &Session, fixture: &Fixture) -> Value {
        let cursor = &fixture.cursor;
        let document = json!({"uri": session.uri(&cursor.path)});
        let position = json!({"line": cursor.line, "character": cursor.character});
        let end = fixture.selection_end.as_ref().unwrap_or(cursor);
        let selection = json!({
            "start": position,
            "end": {"line": end.line, "character": end.character},
        });
        let options = json!({"tabSize": 4, "insertSpaces": false});
        match self {
            Self::DocumentSymbol
            | Self::FoldingRange
            | Self::Diagnostics
            | Self::CodeLens
            | Self::SemanticTokens => {
                json!({"textDocument": document})
            }
            Self::CodeAction => json!({
                "textDocument": document,
                "range": selection,
                "context": {"diagnostics": []},
            }),
            Self::Formatting if fixture.selection_end.is_some() => json!({
                "textDocument": document,
                "range": selection,
                "options": options,
            }),
            Self::Formatting => json!({"textDocument": document, "options": options}),
            Self::InlayHint => json!({
                "textDocument": document,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": fixture.line_count(&cursor.path), "character": 0},
                },
            }),
            Self::SelectionRange => json!({"textDocument": document, "positions": [position]}),
            Self::WorkspaceSymbol => json!({"query": fixture.word_at_cursor()}),
            Self::References => json!({
                "textDocument": document,
                "position": position,
                "context": {"includeDeclaration": true},
            }),
            Self::Rename => json!({
                "textDocument": document,
                "position": position,
                "newName": new_name(&fixture.word_at_cursor()),
            }),
            _ => json!({"textDocument": document, "position": position}),
        }
    }

    /// A server says "this cannot be renamed" with an error as often as
    /// with `null`; for the two rename requests both read as nothing.
    fn on_error(self) -> OnError {
        match self {
            Self::Definition
            | Self::TypeDefinition
            | Self::Implementation
            | Self::References
            | Self::DocumentHighlight
            | Self::Completion
            | Self::Hover
            | Self::SignatureHelp
            | Self::DocumentSymbol
            | Self::FoldingRange
            | Self::WorkspaceSymbol
            | Self::Diagnostics => OnError::Panic,
            // Renaming says "cannot" with an error; the rest are requests a
            // server may not serve at all, which reads as nothing.
            _ => OnError::AnswerNothing,
        }
    }

    /// The method that range formatting is asked with instead.
    fn method_for(self, fixture: &Fixture) -> &'static str {
        match self {
            Self::Formatting if fixture.selection_end.is_some() => "textDocument/rangeFormatting",
            _ => self.method(),
        }
    }

    /// Asks the request at the fixture's cursor and renders the answer,
    /// which it returns with the server's own answer. `expected` is the
    /// answer the case writes down: a name it lists is one it asks about,
    /// whoever else spells it.
    pub(super) async fn ask_raw(
        self,
        session: &mut Session,
        fixture: &Fixture,
        expected: &str,
        settles: Settles,
    ) -> (String, Value) {
        let params = self.params(session, fixture);
        let settled = |answer: &Value| match settles {
            Settles::Immediately => true,
            Settles::WhenAnswered => has_content(answer),
        };
        let answer = if matches!(self, Self::Diagnostics) && session.is_child() {
            // Another server may only push them: what it last published.
            session
                .pushed_diagnostics(&fixture.cursor.path, |answer| {
                    matches!(settles, Settles::Immediately) || has_content(answer)
                })
                .await
        } else {
            session
                .request_until(self.method_for(fixture), params, self.on_error(), settled)
                .await
        };
        let answer = match self {
            Self::CallHierarchy => {
                hierarchy(
                    session,
                    &answer,
                    "callHierarchy",
                    ["incomingCalls", "outgoingCalls"],
                )
                .await
            }
            Self::TypeHierarchy => {
                hierarchy(
                    session,
                    &answer,
                    "typeHierarchy",
                    ["supertypes", "subtypes"],
                )
                .await
            }
            Self::Completion => resolved_completion(session, fixture, expected, answer).await,
            _ => answer,
        };
        let view = View { session, fixture };
        let mut lines = match self {
            Self::Definition | Self::TypeDefinition | Self::Implementation | Self::References => {
                view.locations(&answer)
            }
            Self::DocumentHighlight if is_java(&fixture.cursor.path) => {
                view.highlights(&fixture.cursor.path, &answer)
            }
            Self::DocumentHighlight => view.ranges(&fixture.cursor.path, &answer),
            Self::Completion => view.completion_names(&answer),
            Self::Hover => hover_signature(&answer, &fixture.word_at_cursor()),
            Self::SignatureHelp => signature(&answer),
            Self::PrepareRename => view.rename_target(&answer),
            Self::Rename => view.edited_places(&answer),
            Self::DocumentSymbol if is_java(&fixture.cursor.path) => java_outline(&answer),
            Self::DocumentSymbol => document_symbols(&answer),
            Self::FoldingRange => folded_lines(&answer, is_java(&fixture.cursor.path)),
            Self::WorkspaceSymbol => view.workspace_types(&answer),
            Self::Diagnostics => view.reported(&fixture.cursor.path, &answer),
            Self::Declaration => view.locations(&answer),
            Self::CodeAction => action_titles(&answer),
            Self::Formatting => view.formatted(&answer),
            Self::InlayHint => inlay_hints(&answer),
            Self::CodeLens => view.lenses(&answer),
            Self::CallHierarchy | Self::TypeHierarchy => view.hierarchy(&answer),
            Self::SemanticTokens => view.semantic_tokens(&answer),
            Self::SelectionRange => selection_ranges(&answer),
        };
        if lines.is_empty() {
            lines.push(NOTHING.to_owned());
        }
        (lines.join("\n") + "\n", answer)
    }
}

/// Whether an empty answer is the case's answer or the server still
/// reading what was just opened. A case that expects nothing says so, and
/// is asked once instead of for the whole patience.
#[derive(Clone, Copy)]
pub(super) enum Settles {
    Immediately,
    WhenAnswered,
}

/// `Renamed` for `Circle`, `renamed` for `area`: a rename keeps the case
/// convention of what it renames.
fn new_name(old_name: &str) -> &'static str {
    let is_type_name = old_name.chars().next().is_some_and(char::is_uppercase);
    if is_type_name {
        "Renamed"
    } else {
        "renamed"
    }
}

/// `null`, `[]` and an empty completion list are what a server says while
/// it is still reading the workspace.
fn has_content(answer: &Value) -> bool {
    match answer {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => fields
            .get("items")
            .and_then(Value::as_array)
            .is_none_or(|items| !items.is_empty()),
        _ => true,
    }
}

/// One answer, or a list of them, as a list.
fn as_list(answer: &Value) -> Vec<&Value> {
    match answer {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().collect(),
        single => vec![single],
    }
}

/// What a rendering needs to turn the server's URIs and positions back into
/// the fixture's paths and source lines.
struct View<'a> {
    session: &'a Session,
    fixture: &'a Fixture,
}

impl View<'_> {
    /// `/path:line:column  source line`, one-based, in source order.
    fn locations(&self, answer: &Value) -> Vec<String> {
        let places = as_list(answer).into_iter().filter_map(|location| {
            let uri = location
                .get("uri")
                .or_else(|| location.get("targetUri"))?
                .as_str()?;
            let range = location
                .get("targetSelectionRange")
                .or_else(|| location.get("range"))?;
            Some(Place::new(self.session.display(uri), range))
        });
        self.in_source_order(places.collect())
    }

    /// `/path:line:column-end severity [code] message` of each diagnostic of
    /// the file at `path`, in source order, then its tags. The code is what
    /// a quick fix is chosen by, the range what an editor underlines and the
    /// tags whether it is faded (`unnecessary`) or struck out
    /// (`deprecated`): each is part of the answer, not its wording.
    fn reported(&self, path: &str, answer: &Value) -> Vec<String> {
        let items = answer.get("items").unwrap_or(answer);
        let mut reported: Vec<(Place, String)> = as_list(items)
            .into_iter()
            .filter_map(|diagnostic| {
                let place = Place::new(format!("/{path}"), diagnostic.get("range")?);
                let severity = match diagnostic["severity"].as_u64() {
                    Some(1) => "error",
                    Some(2) => "warning",
                    Some(3) => "info",
                    Some(4) => "hint",
                    _ => "(no severity)",
                };
                let code = match &diagnostic["code"] {
                    Value::String(code) => format!(" [{code}]"),
                    Value::Number(code) => format!(" [{code}]"),
                    _ => String::new(),
                };
                let tags: String = as_list(&diagnostic["tags"])
                    .into_iter()
                    .filter_map(|tag| match tag.as_u64() {
                        Some(1) => Some("  (unnecessary)"),
                        Some(2) => Some("  (deprecated)"),
                        _ => None,
                    })
                    .collect();
                let message = diagnostic["message"].as_str()?;
                Some((place, format!("{severity}{code} {message}{tags}")))
            })
            .collect();
        reported.sort();
        reported.dedup();
        reported
            .iter()
            .map(|(place, said)| format!("{}:{}  {said}", place.path, place.position()))
            .collect()
    }

    /// The ranges of `answer`, all in the file at `path`.
    fn ranges(&self, path: &str, answer: &Value) -> Vec<String> {
        let places = as_list(answer)
            .into_iter()
            .filter_map(|item| Some(Place::new(format!("/{path}"), item.get("range")?)));
        self.in_source_order(places.collect())
    }

    /// A Java file's highlights, each with where it ends and whether it is
    /// read or written: `path:line:column-end kind  source line`, where
    /// `end` is a column, or `line:column` for a range over lines.
    fn highlights(&self, path: &str, answer: &Value) -> Vec<String> {
        let mut highlights: Vec<(Place, String)> = as_list(answer)
            .into_iter()
            .filter_map(|item| {
                let range = item.get("range")?;
                let place = Place::new(format!("/{path}"), range);
                let end = &range["end"];
                let (line, column) = (end["line"].as_u64()?, end["character"].as_u64()?);
                let end = if line as usize == place.line {
                    format!("{}", column + 1)
                } else {
                    format!("{}:{}", line + 1, column + 1)
                };
                let kind = match item["kind"].as_u64() {
                    Some(2) => " read",
                    Some(3) => " write",
                    _ => "",
                };
                Some((place, format!("-{end}{kind}")))
            })
            .collect();
        highlights.sort();
        highlights.dedup();
        highlights
            .iter()
            .map(|(place, end)| {
                let source = self
                    .fixture
                    .line(&place.path, place.line)
                    .unwrap_or_default();
                format!(
                    "{}:{}:{}{end}  {}",
                    place.path,
                    place.line + 1,
                    place.column + 1,
                    source.trim()
                )
            })
            .collect()
    }

    fn in_source_order(&self, mut places: Vec<Place>) -> Vec<String> {
        places.sort();
        places.dedup();
        places.iter().map(|place| self.show(place)).collect()
    }

    fn show(&self, place: &Place) -> String {
        if place.path.starts_with(super::server::LIBRARY) {
            return place.path.clone();
        }
        let source = self
            .fixture
            .line(&place.path, place.line)
            .unwrap_or_default();
        format!("{}:{}  {}", place.path, place.position(), source.trim())
    }

    /// The items offered, best first, as a client would sort them — only
    /// those named as the fixture spells a name or the case asks for: what
    /// a JDK adds (`hashCode`, `getClass`, …) depends on the JDK the server
    /// found, while a keyword (`package`), what every array has (`length`)
    /// or a package name (`com.demo`) is an answer a case can ask for
    /// without writing it in its sources ([`kept_by_name`]).
    ///
    /// An item is what an editor shows and does with it, not its name
    /// alone:
    ///
    /// ```text
    /// test(String x) · void  method  deprecated
    ///   inserts test(${1:x}); over "tes"
    ///   detail Test.test(String x) : void
    ///   doc Tests a thing.
    ///   adds import java.util.List;
    /// ```
    ///
    /// The first line is the label as shown (the label and its
    /// `labelDetails`, or a label that writes them itself — `test(String x)
    /// : void`, `List - java.util`), the kind and the deprecated tag. Then
    /// the text the item puts in the buffer and the source text it
    /// replaces: the item's edit, the list's `editRange`, or, without
    /// either, the word before the cursor as an editor would take it. Then
    /// the detail, the first line of the documentation and the other edits
    /// it makes (an import), as `completionItem/resolve` fills them in.
    fn completion_names(&self, answer: &Value) -> Vec<String> {
        let defaults = &answer["itemDefaults"];
        let items = answer.get("items").unwrap_or(answer);
        let mut offered: Vec<(String, String, String)> = as_list(items)
            .into_iter()
            .filter_map(|item| {
                let label = item["label"].as_str()?;
                let sort_key = item["sortText"].as_str().unwrap_or(label);
                let shown = self.completion_item(item, defaults);
                Some((sort_key.to_owned(), label.to_owned(), shown))
            })
            .collect();
        offered.sort();
        let mut shown: Vec<String> = offered.into_iter().map(|(_, _, shown)| shown).collect();
        shown.dedup();
        shown
    }

    fn completion_item(&self, item: &Value, defaults: &Value) -> String {
        let (label, description) = presented_label(item);
        let kind = completion_kind(item["kind"].as_u64());
        let deprecated = item["deprecated"].as_bool() == Some(true)
            || as_list(&item["tags"]).iter().any(|tag| tag.as_u64() == Some(1));
        let mut lines = vec![format!(
            "{label}{}  {kind}{}",
            if description.is_empty() {
                String::new()
            } else {
                format!(" · {description}")
            },
            if deprecated { "  deprecated" } else { "" }
        )];
        let (text, range) = inserted(item, defaults);
        let replaced = range.map_or_else(
            || self.word_before_cursor(),
            |range| self.cursor_file_text(range),
        );
        lines.push(format!("  inserts {} over {replaced:?}", one_line(&text)));
        if let Some(detail) = item["detail"].as_str().filter(|detail| !detail.trim().is_empty()) {
            lines.push(format!("  detail {}", one_line(detail.trim())));
        }
        let documentation = match &item["documentation"] {
            Value::String(text) => text.as_str(),
            other => other["value"].as_str().unwrap_or_default(),
        };
        if let Some(first) = documentation
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("```"))
        {
            lines.push(format!("  doc {}", without_link_targets(first)));
        }
        let mut added: Vec<String> = as_list(&item["additionalTextEdits"])
            .into_iter()
            .filter_map(|edit| edit["newText"].as_str())
            .map(|text| one_line(text.trim()))
            .filter(|text| !text.is_empty())
            .map(|text| format!("  adds {text}"))
            .collect();
        added.sort();
        lines.extend(added);
        lines.join("\n")
    }

    /// The identifier in front of the cursor: what an editor replaces with
    /// an item that names no range of its own.
    fn word_before_cursor(&self) -> String {
        let cursor = &self.fixture.cursor;
        let line = self
            .fixture
            .line(&format!("/{}", cursor.path), cursor.line as usize)
            .unwrap_or_default();
        let units: Vec<u16> = line.encode_utf16().collect();
        let before =
            String::from_utf16_lossy(&units[..(cursor.character as usize).min(units.len())]);
        let word: String = before
            .chars()
            .rev()
            .take_while(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
            .collect();
        word.chars().rev().collect()
    }

    /// The text of the cursor's file a range covers.
    fn cursor_file_text(&self, range: &Value) -> String {
        let text = self.fixture.text(&self.fixture.cursor.path).unwrap_or_default();
        let offset = |position: &Value| byte_offset(text, position);
        let (start, end) = (offset(&range["start"]), offset(&range["end"]));
        text.get(start..end.max(start)).unwrap_or_default().to_owned()
    }

    /// Where the identifier a rename would change starts.
    fn rename_target(&self, answer: &Value) -> Vec<String> {
        let range = answer.get("range").unwrap_or(answer);
        if range.get("start").is_none() {
            return Vec::new();
        }
        let place = Place::new(format!("/{}", self.fixture.cursor.path), range);
        let mut shown = vec![self.show(&place)];
        if let Some(placeholder) = answer["placeholder"].as_str() {
            shown.push(format!("placeholder {placeholder}"));
        }
        shown
    }

    /// What a rename leaves: each line it changes, as it reads afterwards,
    /// `/path:line  text`, and each file it moves, `/old -> /new`. Where an
    /// edit starts, how much it replaces and what it writes all show in the
    /// lines it leaves, whether a server answers one edit per name or one
    /// per file.
    fn edited_places<'a>(&self, answer: &'a Value) -> Vec<String> {
        let mut edits: Vec<(String, Vec<&'a Value>)> = Vec::new();
        let mut add = |uri: &str, list: &'a Value| {
            let path = self.session.display(uri);
            match edits.iter_mut().find(|(known, _)| *known == path) {
                Some((_, known)) => known.extend(as_list(list)),
                None => edits.push((path, as_list(list))),
            }
        };
        if let Some(changes) = answer["changes"].as_object() {
            for (uri, list) in changes {
                add(uri, list);
            }
        }
        let mut moves = Vec::new();
        for change in as_list(&answer["documentChanges"]) {
            if let Some(uri) = change["textDocument"]["uri"].as_str() {
                add(uri, &change["edits"]);
            } else if change["kind"].as_str() == Some("rename") {
                let (Some(old), Some(new)) =
                    (change["oldUri"].as_str(), change["newUri"].as_str())
                else {
                    continue;
                };
                moves.push(format!(
                    "moves {} -> {}",
                    self.session.display(old),
                    self.session.display(new)
                ));
            } else if let Some(kind) = change["kind"].as_str() {
                let uri = change["uri"].as_str().unwrap_or_default();
                moves.push(format!("{kind}s {}", self.session.display(uri)));
            }
        }
        let mut lines: Vec<(String, usize, String)> = Vec::new();
        for (path, list) in &edits {
            let Some(text) = path.strip_prefix('/').and_then(|path| self.fixture.text(path)) else {
                lines.push((path.clone(), 0, format!("{} edits outside the fixture", list.len())));
                continue;
            };
            let after = apply_edits(text, list);
            let before: Vec<&str> = text.lines().collect();
            for (index, line) in after.lines().enumerate() {
                if before.get(index) != Some(&line) {
                    lines.push((path.clone(), index, line.trim().to_owned()));
                }
            }
            if after.lines().count() < before.len() {
                lines.push((path.clone(), before.len(), "(lines removed)".to_owned()));
            }
        }
        lines.sort();
        lines.dedup();
        let mut shown: Vec<String> = lines
            .into_iter()
            .map(|(path, index, line)| format!("{path}:{}  {line}", index + 1))
            .collect();
        moves.sort();
        shown.extend(moves);
        shown
    }

    /// `name  kind in container  /path:line` of the types found in the
    /// fixture's files, sorted: the container (the package, or the type a
    /// nested one is in) is what tells two types of one name apart in a
    /// picker.
    /// Whether fields and methods are searchable too differs by server.
    fn workspace_types(&self, answer: &Value) -> Vec<String> {
        let mut lines: Vec<String> = as_list(answer)
            .into_iter()
            .filter(|symbol| matches!(symbol["kind"].as_u64(), Some(5 | 10 | 11)))
            .filter_map(|symbol| {
                let path = self.session.display(symbol["location"]["uri"].as_str()?);
                let line = symbol["location"]["range"]["start"]["line"].as_u64()?;
                self.fixture.line(&path, line as usize)?;
                let name = identifier_prefix(symbol["name"].as_str()?);
                let kind = symbol_kind(symbol["kind"].as_u64());
                let container = symbol["containerName"].as_str().unwrap_or_default();
                let container = if container.is_empty() {
                    String::new()
                } else {
                    format!(" in {container}")
                };
                Some(format!("{name}  {kind}{container}  {path}:{}", line + 1))
            })
            .collect();
        lines.sort();
        lines.dedup();
        lines
    }
}

/// A zero-based position in a file, ordered as the source reads, and where
/// the range it starts ends, when it is a range: a definition that points
/// at the start of a name and one that covers it are different answers.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    path: String,
    line: usize,
    column: usize,
    end: Option<(usize, usize)>,
}

impl Place {
    fn new(path: String, range: &Value) -> Self {
        let start = &range["start"];
        let end = &range["end"];
        let end = match (end["line"].as_u64(), end["character"].as_u64()) {
            (Some(line), Some(column)) => Some((line as usize, column as usize)),
            _ => None,
        };
        Self {
            path,
            line: start["line"].as_u64().unwrap_or(0) as usize,
            column: start["character"].as_u64().unwrap_or(0) as usize,
            end,
        }
    }

    /// `line:column`, one-based, then `-column` or `-line:column` where it
    /// ends.
    fn position(&self) -> String {
        let start = format!("{}:{}", self.line + 1, self.column + 1);
        match self.end {
            None => start,
            Some((line, column)) if line == self.line => format!("{start}-{}", column + 1),
            Some((line, column)) => format!("{start}-{}:{}", line + 1, column + 1),
        }
    }
}

/// `scaled` of `scaled(double, int) : Circle`.
/// The dotted name a completion label starts with: `com.demo` of a package,
/// `put` of `put(K key, V value)`.
fn qualified_name_prefix(label: &str) -> &str {
    let end = label
        .find(|character: char| !character.is_alphanumeric() && !matches!(character, '_' | '.'))
        .unwrap_or(label.len());
    let name = label[..end].trim_end_matches('.');
    // A nested type is `IFoo` to one server and `Foo.IFoo` to another; a
    // package (`java.util`) is lower case and kept whole.
    let nested = name.contains('.')
        && name
            .split('.')
            .all(|segment| segment.starts_with(|character: char| character.is_uppercase()));
    if nested {
        return name.rsplit('.').next().unwrap_or(name);
    }
    name
}

/// The label an editor shows and the description beside it: the label
/// and its `labelDetails`, or, for a server that writes them into the
/// label itself, the label cut where they start — `test(String x) : void`
/// (a member and its type) or `List - java.util` (a type and its package).
fn presented_label(item: &Value) -> (String, String) {
    let label = item["label"].as_str().unwrap_or_default();
    let details = &item["labelDetails"];
    if details.is_object() {
        let detail = details["detail"].as_str().unwrap_or_default();
        let description = details["description"].as_str().unwrap_or_default();
        return (format!("{label}{detail}"), description.to_owned());
    }
    for separator in [" : ", " - "] {
        if let Some((shown, description)) = label.rsplit_once(separator) {
            return (shown.to_owned(), description.to_owned());
        }
    }
    (label.to_owned(), String::new())
}

/// What choosing `item` writes, and the range it writes over when the
/// answer names one: its own `textEdit`, or the list's `editRange` with the
/// item's `textEditText` (or `insertText`, or label) as the text.
fn inserted<'a>(item: &'a Value, defaults: &'a Value) -> (String, Option<&'a Value>) {
    let label = item["label"].as_str().unwrap_or_default();
    let edit = &item["textEdit"];
    if let Some(text) = edit["newText"].as_str() {
        let range = edit.get("range").or_else(|| edit.get("insert"));
        return (text.to_owned(), range);
    }
    let default_range = &defaults["editRange"];
    let default_range = if default_range.get("insert").is_some() {
        Some(&default_range["insert"])
    } else if default_range.get("start").is_some() {
        Some(default_range)
    } else {
        None
    };
    let text = default_range
        .and_then(|_| item["textEditText"].as_str())
        .or_else(|| item["insertText"].as_str())
        .unwrap_or(label);
    (text.to_owned(), default_range)
}

/// `text` on one line: a snippet's newlines and tabs written `\n`, `\t`.
fn one_line(text: &str) -> String {
    text.replace('\n', "\\n").replace('\t', "\\t")
}

fn completion_kind(kind: Option<u64>) -> &'static str {
    match kind {
        Some(1) => "text",
        Some(2) => "method",
        Some(3) => "function",
        Some(4) => "constructor",
        Some(5) => "field",
        Some(6) => "variable",
        Some(7) => "class",
        Some(8) => "interface",
        Some(9) => "module",
        Some(10) => "property",
        Some(11) => "unit",
        Some(12) => "value",
        Some(13) => "enum",
        Some(14) => "keyword",
        Some(15) => "snippet",
        Some(16) => "color",
        Some(17) => "file",
        Some(18) => "reference",
        Some(19) => "folder",
        Some(20) => "enum-member",
        Some(21) => "constant",
        Some(22) => "struct",
        Some(23) => "event",
        Some(24) => "operator",
        Some(25) => "type-parameter",
        _ => "(no kind)",
    }
}

/// Whether a completion item labelled `label` is one a case looks at: a
/// name its sources spell, or one its expected answer lists (an item's
/// first line starts with its name).
fn kept_by_name(fixture: &Fixture, expected: &str, label: &str) -> bool {
    let name = qualified_name_prefix(label);
    fixture.spells(name)
        || expected
            .lines()
            .filter(|line| !line.starts_with(' '))
            .any(|line| qualified_name_prefix(line.trim()) == name)
}

/// The completion answer with only the items a case looks at, each as
/// `completionItem/resolve` fills it in where the server resolves items: an
/// editor resolves the item it shows, and the documentation, the detail
/// and the import an item adds may only come then. An item without `data`
/// is sent with the list's default `data`, as the protocol says a client
/// does.
async fn resolved_completion(
    session: &mut Session,
    fixture: &Fixture,
    expected: &str,
    answer: Value,
) -> Value {
    let defaults = answer.get("itemDefaults").cloned().unwrap_or(Value::Null);
    let items = answer.get("items").cloned().unwrap_or(answer);
    let mut kept = Vec::new();
    for item in as_list(&items) {
        let Some(label) = item["label"].as_str() else {
            continue;
        };
        if !kept_by_name(fixture, expected, label) {
            continue;
        }
        let mut item = item.clone();
        if session.resolves_completion() {
            if item.get("data").is_none() && !defaults["data"].is_null() {
                item["data"] = defaults["data"].clone();
            }
            let resolved = session
                .request_until(
                    "completionItem/resolve",
                    item.clone(),
                    OnError::AnswerNothing,
                    |_| true,
                )
                .await;
            if resolved.is_object() {
                item = resolved;
            }
        }
        kept.push(item);
    }
    json!({"items": kept, "itemDefaults": defaults})
}

/// The byte offset in `text` of an LSP position, which counts UTF-16 units.
fn byte_offset(text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let Some(start) = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .nth(line)
    else {
        return text.len();
    };
    let mut units = position["character"].as_u64().unwrap_or(0) as usize;
    let mut end = start;
    for character in text[start..].chars() {
        if units == 0 || character == '\n' {
            break;
        }
        units = units.saturating_sub(character.len_utf16());
        end += character.len_utf8();
    }
    end
}

fn identifier_prefix(label: &str) -> &str {
    let end = label
        .find(|character: char| !character.is_alphanumeric() && character != '_')
        .unwrap_or(label.len());
    &label[..end]
}

/// Every line a hover shows, its first line of code (the signature) with
/// `name` unqualified: one server writes `void print(String)`, another
/// `void Printer.print(String)`. A constructor is named after its class —
/// `Printer.Printer(String)` — and is shown as `Printer(String)`.
///
/// The documentation below the signature is the answer too: a hover that
/// drops the Javadoc, or its `@param` lines, is a different hover. Code
/// fences, blank lines and the targets of links (URIs of the temporary
/// workspace) are left out, so what is compared is what a reader sees.
fn hover_signature(answer: &Value, name: &str) -> Vec<String> {
    let text = as_list(&answer["contents"])
        .into_iter()
        .filter_map(|part| part.as_str().or_else(|| part["value"].as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut lines = Vec::new();
    let mut signature_shown = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("```") || line == "---" || line == "***" {
            continue;
        }
        let line = without_link_targets(line);
        if signature_shown {
            lines.push(line);
        } else {
            lines.push(unqualified(&line, name));
            signature_shown = true;
        }
    }
    lines
}

/// `[text](target)` as `[text]`.
fn without_link_targets(line: &str) -> String {
    let mut shown = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        shown.push_str(&rest[..=at]);
        let after = &rest[at + 2..];
        rest = after.find(')').map_or("", |close| &after[close + 1..]);
    }
    shown.push_str(rest);
    shown
}

/// `line` without the dotted qualifier in front of its first `name`.
fn unqualified(line: &str, name: &str) -> String {
    let Some(mut at) = line.find(name) else {
        return line.to_owned();
    };
    let constructor_of_name = format!("{name}.{name}");
    if !name.is_empty() && line[at..].starts_with(&constructor_of_name) {
        at += name.len() + 1;
    }
    let qualifier_start = line[..at]
        .rfind(|character: char| !character.is_alphanumeric() && !matches!(character, '_' | '.'))
        .map_or(0, |separator| separator + 1);
    format!("{}{}", &line[..qualifier_start], &line[at..])
}

/// Every signature offered, the active one marked `>`, each as its whole
/// label with the active parameter in brackets — `> foo(int a, [String b])
/// : String` — and the first line of its documentation below it. Which
/// overloads are offered, which is chosen, the return type and the
/// parameter names are all part of the answer.
fn signature(answer: &Value) -> Vec<String> {
    let active_signature = answer["activeSignature"].as_u64().unwrap_or(0) as usize;
    let mut lines = Vec::new();
    for (index, signature) in as_list(&answer["signatures"]).into_iter().enumerate() {
        let label = signature["label"].as_str().unwrap_or_default();
        let active = index == active_signature;
        let active_parameter = signature["activeParameter"]
            .as_u64()
            .or_else(|| answer["activeParameter"].as_u64())
            .filter(|_| active);
        let spans = parameter_spans(label, &as_list(&signature["parameters"]));
        let marked = active_parameter
            .and_then(|at| spans.get(at as usize).copied().flatten())
            .map_or_else(
                || label.to_owned(),
                |(start, end)| {
                    format!("{}[{}]{}", &label[..start], &label[start..end], &label[end..])
                },
            );
        let parameter_names: Vec<String> = spans
            .iter()
            .map(|span| span.map_or_else(|| "?".to_owned(), |(start, end)| label[start..end].to_owned()))
            .collect();
        let missing = parameter_names.iter().any(|name| name == "?");
        lines.push(format!(
            "{}{marked}{}",
            if active { "> " } else { "  " },
            if missing {
                format!("  (parameters: {})", parameter_names.join(", "))
            } else {
                String::new()
            }
        ));
        let documentation = match &signature["documentation"] {
            Value::String(text) => text.as_str(),
            other => other["value"].as_str().unwrap_or_default(),
        };
        if let Some(first) = documentation.lines().map(str::trim).find(|line| !line.is_empty()) {
            lines.push(format!("    {}", without_link_targets(first)));
        }
    }
    lines
}

/// Where each parameter's label sits in the signature's label, in bytes: a
/// `[start, end]` in UTF-16 units, or a string found in the label after the
/// parameter before it (`(int a, int b)` has two `int`s).
fn parameter_spans(label: &str, parameters: &[&Value]) -> Vec<Option<(usize, usize)>> {
    let byte = |units: u64| -> Option<usize> {
        let mut counted = 0;
        for (at, character) in label.char_indices() {
            if counted == units as usize {
                return Some(at);
            }
            counted += character.len_utf16();
        }
        (counted == units as usize).then_some(label.len())
    };
    let mut searched_from = label.find('(').map_or(0, |at| at + 1);
    parameters
        .iter()
        .map(|parameter| {
            let parameter = &parameter["label"];
            if let Some(offsets) = parameter.as_array() {
                let span = (byte(offsets.first()?.as_u64()?)?, byte(offsets.get(1)?.as_u64()?)?);
                searched_from = span.1;
                return Some(span);
            }
            let text = parameter.as_str()?;
            let start = searched_from + label.get(searched_from..)?.find(text)?;
            searched_from = start + text.len();
            Some((start, searched_from))
        })
        .collect()
}

const PACKAGE: u64 = 4;

fn is_java(path: &str) -> bool {
    path.ends_with(".java")
}

/// A Java file's outline as jdtls gives it: a tree, each symbol as
/// `kind name detail  range at selection`, nested two spaces under its
/// parent, and `deprecated` where it is tagged so.
fn java_outline(answer: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    outline_lines(answer, 0, &mut lines);
    lines
}

fn outline_lines(symbols: &Value, depth: usize, lines: &mut Vec<String>) {
    let mut symbols = as_list(symbols);
    symbols.sort_by_key(|symbol| start_of(&symbol["range"]));
    for symbol in symbols {
        let deprecated = symbol["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_u64() == Some(1)))
            || symbol["deprecated"].as_bool() == Some(true);
        lines.push(format!(
            "{}{} {}{}  {} at {}{}",
            "  ".repeat(depth),
            symbol_kind(symbol["kind"].as_u64()),
            symbol["name"].as_str().unwrap_or("?"),
            symbol["detail"].as_str().unwrap_or_default(),
            span(&symbol["range"]),
            span(&symbol["selectionRange"]),
            if deprecated { "  deprecated" } else { "" },
        ));
        outline_lines(&symbol["children"], depth + 1, lines);
    }
}

fn start_of(range: &Value) -> (u64, u64) {
    let start = &range["start"];
    (
        start["line"].as_u64().unwrap_or(0),
        start["character"].as_u64().unwrap_or(0),
    )
}

/// `line:column-end`, one-based, `end` written as `line:column` when the
/// range goes over lines.
fn span(range: &Value) -> String {
    let (line, column) = start_of(range);
    let end = &range["end"];
    let (end_line, end_column) = (
        end["line"].as_u64().unwrap_or(0),
        end["character"].as_u64().unwrap_or(0),
    );
    if end_line == line {
        format!("{}:{}-{}", line + 1, column + 1, end_column + 1)
    } else {
        format!(
            "{}:{}-{}:{}",
            line + 1,
            column + 1,
            end_line + 1,
            end_column + 1
        )
    }
}

/// The outline in source order: `kind name`. Nesting is not shown, nor the
/// package — one server answers a tree under the package and another a flat
/// list of the same declarations.
fn document_symbols(answer: &Value) -> Vec<String> {
    let mut symbols = Vec::new();
    flatten_symbols(answer, &mut symbols);
    symbols.sort_by_key(|symbol| {
        let start = symbol
            .get("selectionRange")
            .unwrap_or(&symbol["location"]["range"])["start"]
            .clone();
        (start["line"].as_u64(), start["character"].as_u64())
    });
    symbols
        .into_iter()
        .map(|symbol| {
            let name = identifier_prefix(symbol["name"].as_str().unwrap_or("?"));
            format!("{} {name}", symbol_kind(symbol["kind"].as_u64()))
        })
        .collect()
}

fn flatten_symbols<'a>(symbols: &'a Value, flat: &mut Vec<&'a Value>) {
    for symbol in as_list(symbols) {
        if symbol["kind"].as_u64() != Some(PACKAGE) {
            flat.push(symbol);
        }
        flatten_symbols(&symbol["children"], flat);
    }
}

fn symbol_kind(kind: Option<u64>) -> &'static str {
    match kind {
        Some(1) => "file",
        Some(2) => "module",
        Some(3) => "namespace",
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(7) => "property",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(22) => "enum-member",
        Some(23) => "struct",
        Some(24) => "event",
        Some(25) => "operator",
        Some(26) => "type-parameter",
        _ => "symbol",
    }
}

/// `first..last` line of each folded region, one-based, sorted. For Java
/// a region's kind follows, when it has one: `3..5 comment`.
fn folded_lines(answer: &Value, with_kind: bool) -> Vec<String> {
    let mut regions: Vec<(u64, u64, &str)> = as_list(answer)
        .into_iter()
        .filter_map(|range| {
            let kind = range["kind"]
                .as_str()
                .filter(|_| with_kind)
                .unwrap_or_default();
            Some((
                range["startLine"].as_u64()?,
                range["endLine"].as_u64()?,
                kind,
            ))
        })
        .collect();
    regions.sort_unstable();
    regions
        .into_iter()
        .map(|(start, end, kind)| {
            format!("{}..{} {kind}", start + 1, end + 1)
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The prepared items of a call or type hierarchy, and for the first one
/// the answer of each of `directions`, as
/// `{"items": [...], "<direction>": [...]}`.
async fn hierarchy(
    session: &mut Session,
    prepared: &Value,
    prefix: &str,
    directions: [&str; 2],
) -> Value {
    let mut answer = json!({"items": prepared.clone()});
    if let Some(item) = as_list(prepared).first().copied().cloned() {
        for direction in directions {
            let found = session
                .request_until(
                    &format!("{prefix}/{direction}"),
                    json!({"item": item}),
                    OnError::AnswerNothing,
                    |_| true,
                )
                .await;
            answer[direction] = found;
        }
    }
    answer
}

/// `title  (kind)` of each action, sorted: which comes first is up to the
/// client. The kind is what an editor files an action under (a quick fix,
/// a refactoring, a source action) and what `editor.codeActionsOnSave`
/// picks one by; `preferred` and why an action is `disabled` follow.
fn action_titles(answer: &Value) -> Vec<String> {
    let mut titles: Vec<String> = as_list(answer)
        .into_iter()
        .filter_map(|action| {
            let mut shown = action["title"].as_str()?.to_owned();
            match action["kind"].as_str() {
                Some(kind) if !kind.is_empty() => shown.push_str(&format!("  ({kind})")),
                _ if action.get("command").is_some_and(Value::is_string) => {
                    shown.push_str("  (command)");
                }
                _ => shown.push_str("  (no kind)"),
            }
            if action["isPreferred"].as_bool() == Some(true) {
                shown.push_str("  preferred");
            }
            if let Some(reason) = action["disabled"]["reason"].as_str() {
                shown.push_str(&format!("  disabled: {reason}"));
            }
            Some(shown)
        })
        .collect();
    titles.sort();
    titles.dedup();
    titles
}

/// `line:column  label  (kind)` of each hint, one-based, in source order,
/// with a `␣` where the hint asks for padding: `name: ` and `name:` read
/// differently in the editor. The kind is `type` or `parameter`.
fn inlay_hints(answer: &Value) -> Vec<String> {
    let mut hints: Vec<(u64, u64, String)> = as_list(answer)
        .into_iter()
        .filter_map(|hint| {
            let label = match &hint["label"] {
                Value::String(text) => text.clone(),
                parts => as_list(parts)
                    .into_iter()
                    .filter_map(|part| part["value"].as_str())
                    .collect(),
            };
            let kind = match hint["kind"].as_u64() {
                Some(1) => "  (type)",
                Some(2) => "  (parameter)",
                _ => "",
            };
            let left = if hint["paddingLeft"].as_bool() == Some(true) { "␣" } else { "" };
            let right = if hint["paddingRight"].as_bool() == Some(true) { "␣" } else { "" };
            let position = &hint["position"];
            Some((
                position["line"].as_u64()?,
                position["character"].as_u64()?,
                format!("{left}{}{right}{kind}", label.trim()),
            ))
        })
        .collect();
    hints.sort();
    hints
        .into_iter()
        .map(|(line, column, label)| format!("{}:{}  {label}", line + 1, column + 1))
        .collect()
}

/// The ranges around the cursor, innermost first, as `first:column..last:column`.
fn selection_ranges(answer: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let mut range = as_list(answer).first().copied();
    while let Some(selection) = range {
        let (start, end) = (&selection["range"]["start"], &selection["range"]["end"]);
        lines.push(format!(
            "{}:{}..{}:{}",
            start["line"].as_u64().unwrap_or(0) + 1,
            start["character"].as_u64().unwrap_or(0) + 1,
            end["line"].as_u64().unwrap_or(0) + 1,
            end["character"].as_u64().unwrap_or(0) + 1,
        ));
        range = selection.get("parent").filter(|parent| !parent.is_null());
    }
    lines
}

impl View<'_> {
    /// The cursor's file once the edits are applied, line by line.
    fn formatted(&self, answer: &Value) -> Vec<String> {
        let edits = as_list(answer);
        if edits.is_empty() {
            return Vec::new();
        }
        let path = &self.fixture.cursor.path;
        let text = self.fixture.text(path).unwrap_or_default();
        apply_edits(text, &edits)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `/path:line:column  source line` of each lens, with the title of its
    /// command when the server already resolved it.
    fn lenses(&self, answer: &Value) -> Vec<String> {
        let path = format!("/{}", self.fixture.cursor.path);
        let mut lenses: Vec<(Place, String)> = as_list(answer)
            .into_iter()
            .filter_map(|lens| {
                let place = Place::new(path.clone(), lens.get("range")?);
                let title = lens["command"]["title"].as_str().unwrap_or_default();
                Some((place, title.to_owned()))
            })
            .collect();
        lenses.sort();
        lenses
            .iter()
            .map(|(place, title)| {
                let shown = self.show(place);
                if title.is_empty() {
                    shown
                } else {
                    format!("{shown}  [{title}]")
                }
            })
            .collect()
    }

    /// `item`, then `incomingCalls` / `outgoingCalls` (or `supertypes` /
    /// `subtypes`): `kind name detail  /path:line:column-end  source line`,
    /// each group in source order, and under a call the places it is made
    /// (`fromRanges`): which calls a caller makes, not only that it calls.
    fn hierarchy(&self, answer: &Value) -> Vec<String> {
        let mut lines = Vec::new();
        let show = |item: &Value| -> Option<String> {
            let uri = item["uri"].as_str()?;
            let range = item.get("selectionRange").or_else(|| item.get("range"))?;
            let place = Place::new(self.session.display(uri), range);
            let detail = item["detail"].as_str().unwrap_or_default();
            let detail = if detail.is_empty() {
                String::new()
            } else {
                format!(" {detail}")
            };
            Some(format!(
                "{} {}{detail}  {}",
                symbol_kind(item["kind"].as_u64()),
                item["name"].as_str()?,
                self.show(&place)
            ))
        };
        for (group, key, inner) in [
            ("item", "items", None),
            ("incoming", "incomingCalls", Some("from")),
            ("outgoing", "outgoingCalls", Some("to")),
            ("supertype", "supertypes", None),
            ("subtype", "subtypes", None),
        ] {
            let mut shown: Vec<String> = as_list(&answer[key])
                .into_iter()
                .filter_map(|entry| {
                    let mut line = format!("{group} {}", show(inner.map_or(entry, |field| &entry[field]))?);
                    let mut calls: Vec<Place> = as_list(&entry["fromRanges"])
                        .into_iter()
                        .map(|range| Place::new(String::new(), range))
                        .collect();
                    calls.sort();
                    for call in calls {
                        line.push_str(&format!("\n    called at {}", call.position()));
                    }
                    Some(line)
                })
                .collect();
            shown.sort();
            lines.extend(shown);
        }
        lines
    }

    /// `line:column  text  type [modifiers]` of each token, decoded with the
    /// legend the server gave in `initialize`.
    fn semantic_tokens(&self, answer: &Value) -> Vec<String> {
        let legend = self.session.semantic_legend();
        let types = as_list(&legend["tokenTypes"]);
        let modifiers = as_list(&legend["tokenModifiers"]);
        let data: Vec<u64> = as_list(&answer["data"])
            .into_iter()
            .filter_map(Value::as_u64)
            .collect();
        let path = format!("/{}", self.fixture.cursor.path);
        let (mut line, mut column) = (0, 0);
        let mut lines = Vec::new();
        for token in data.chunks_exact(5) {
            if token[0] > 0 {
                line += token[0];
                column = token[1];
            } else {
                column += token[1];
            }
            let source: Vec<u16> = self
                .fixture
                .line(&path, line as usize)
                .unwrap_or_default()
                .encode_utf16()
                .collect();
            let start = (column as usize).min(source.len());
            let end = (start + token[2] as usize).min(source.len());
            let text = String::from_utf16_lossy(&source[start..end]);
            let kind = types
                .get(token[3] as usize)
                .and_then(|kind| kind.as_str())
                .unwrap_or("?");
            let set: Vec<&str> = modifiers
                .iter()
                .enumerate()
                .filter(|(bit, _)| token[4] & (1 << bit) != 0)
                .filter_map(|(_, modifier)| modifier.as_str())
                .collect();
            let mut shown = format!("{}:{}  {text}  {kind}", line + 1, column + 1);
            if !set.is_empty() {
                shown.push_str(&format!(" [{}]", set.join(", ")));
            }
            lines.push(shown);
        }
        lines
    }
}

/// `text` with the LSP `edits` applied; positions count UTF-16 units.
fn apply_edits(text: &str, edits: &[&Value]) -> String {
    let offset = |position: &Value| byte_offset(text, position);
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            (
                offset(&edit["range"]["start"]),
                offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    spans.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut result = text.to_owned();
    for (start, end, new_text) in spans {
        result.replace_range(start..end.max(start), new_text);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that writes the signature into the label and one that
    /// gives it as `labelDetails` show the editor the same item.
    #[test]
    fn a_label_reads_the_same_with_or_without_label_details() {
        let legacy = json!({"label": "test(String x) : void"});
        let detailed = json!({
            "label": "test",
            "labelDetails": {"detail": "(String x)", "description": "void"},
        });
        assert_eq!(presented_label(&legacy), presented_label(&detailed));
        let legacy_type = json!({"label": "List - java.util"});
        let detailed_type = json!({"label": "List", "labelDetails": {"description": "java.util"}});
        assert_eq!(presented_label(&legacy_type), presented_label(&detailed_type));
    }

    /// The list's `editRange` and the item's `textEditText` are what an
    /// item without an edit of its own writes; an item's own edit wins.
    #[test]
    fn an_item_inserts_its_edit_or_the_lists_default() {
        let range = json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 3}});
        let defaults = json!({"editRange": range});
        let item = json!({"label": "test", "insertText": "test", "textEditText": "test(${1:x})"});
        let (text, at) = inserted(&item, &defaults);
        assert_eq!(text, "test(${1:x})");
        assert_eq!(at, Some(&range));

        let own = json!({"label": "test", "textEdit": {"newText": "tested", "range": range}});
        assert_eq!(inserted(&own, &Value::Null).0, "tested");

        let plain = json!({"label": "test", "insertText": "test()"});
        assert_eq!(inserted(&plain, &Value::Null), ("test()".to_owned(), None));
    }

    /// Parameters given as offsets and as strings land on the same text,
    /// and two parameters of one type are told apart.
    #[test]
    fn parameters_are_found_by_offset_or_in_order() {
        let label = "add(int a, int b) : int";
        let by_offset = [json!({"label": [4, 9]}), json!({"label": [11, 16]})];
        let by_text = [json!({"label": "int"}), json!({"label": "int"})];
        let spans = |parameters: &[Value]| parameter_spans(label, &parameters.iter().collect::<Vec<_>>());
        assert_eq!(spans(&by_offset), vec![Some((4, 9)), Some((11, 16))]);
        assert_eq!(spans(&by_text), vec![Some((4, 7)), Some((11, 14))]);
    }

    #[test]
    fn a_link_keeps_its_text_not_its_target() {
        assert_eq!(
            without_link_targets("Source: *[demo](file:///tmp/x/A.java#4)* and [B](jdt://b)"),
            "Source: *[demo]* and [B]"
        );
    }

    /// The active signature is marked and its active parameter bracketed;
    /// the other overloads are listed too.
    #[test]
    fn every_overload_is_shown_with_the_active_one_marked() {
        let answer = json!({
            "activeSignature": 1,
            "activeParameter": 1,
            "signatures": [
                {"label": "add(int a) : int", "parameters": [{"label": "int a"}]},
                {"label": "add(int a, int b) : int",
                 "parameters": [{"label": "int a"}, {"label": "int b"}],
                 "documentation": {"kind": "markdown", "value": "Adds.\n\nMore."}},
            ],
        });
        assert_eq!(
            signature(&answer),
            vec!["  add(int a) : int", "> add(int a, [int b]) : int", "    Adds."]
        );
    }

    #[test]
    fn a_position_counts_utf16_units() {
        let text = "a\n€x = 1\n";
        assert_eq!(byte_offset(text, &json!({"line": 1, "character": 1})), 5);
        assert_eq!(byte_offset(text, &json!({"line": 5, "character": 0})), text.len());
    }
}
