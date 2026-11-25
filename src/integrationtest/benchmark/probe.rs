//! The questions asked of a project, chosen from its own code.
//!
//! A few of its source files are picked, spread over the sorted list, and
//! in each one the places a developer would ask about: method calls, type
//! names, field accesses and method declarations, read with tree-sitter.
//! Every pick is deterministic, so a project at the same commit is asked
//! the same questions by both servers and in every run.
//!
//! By default the picks are evenly spaced. With a seed they are random
//! instead — other files, other places, the cursor anywhere in a name —
//! but the same seed picks the same again, so a run that found something
//! is replayed by its seed. Each file draws from its own stream (the seed
//! and the file's path), so picking other files does not move the places
//! picked in one.
//!
//! Some questions change the file first, the way typing does: a method
//! name cut back to its first letter for completion, a call to a method
//! that does not exist and a missing `;` for diagnostics. The change is
//! sent as a `didChange`, asked about, and taken back.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The requests asked. `Edited*` variants ask after an edit.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Method {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
    DocumentHighlight,
    Hover,
    SignatureHelp,
    PrepareRename,
    Rename,
    CallHierarchy,
    TypeHierarchy,
    CodeAction,
    SelectionRange,
    WorkspaceSymbol,
    Completion,
    Diagnostics,
    DocumentSymbol,
    FoldingRange,
    SemanticTokens,
    Formatting,
    CodeLens,
    InlayHint,
}

impl Method {
    pub(super) fn lsp(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::Declaration => "textDocument/declaration",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::DocumentHighlight => "textDocument/documentHighlight",
            Self::Hover => "textDocument/hover",
            Self::SignatureHelp => "textDocument/signatureHelp",
            Self::PrepareRename => "textDocument/prepareRename",
            Self::Rename => "textDocument/rename",
            Self::CallHierarchy => "textDocument/prepareCallHierarchy",
            Self::TypeHierarchy => "textDocument/prepareTypeHierarchy",
            Self::CodeAction => "textDocument/codeAction",
            Self::SelectionRange => "textDocument/selectionRange",
            Self::WorkspaceSymbol => "workspace/symbol",
            Self::Completion => "textDocument/completion",
            // Pulled, or pushed by a server that does not answer it.
            Self::Diagnostics => "textDocument/diagnostic",
            Self::DocumentSymbol => "textDocument/documentSymbol",
            Self::FoldingRange => "textDocument/foldingRange",
            Self::SemanticTokens => "textDocument/semanticTokens/full",
            Self::Formatting => "textDocument/formatting",
            Self::CodeLens => "textDocument/codeLens",
            Self::InlayHint => "textDocument/inlayHint",
        }
    }

    /// The short name the report groups by: `callHierarchy`, `hover`.
    pub(super) fn name(self) -> String {
        let name = format!("{self:?}");
        let mut characters = name.chars();
        characters
            .next()
            .map(|first| first.to_lowercase().chain(characters).collect())
            .unwrap_or_default()
    }
}

/// A change made before a question and taken back after it: the bytes
/// `start..end` of the file replaced with `text`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Edit {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) text: String,
    /// What the change does, for the report.
    pub(super) note: String,
}

impl Edit {
    pub(super) fn apply(&self, text: &str) -> String {
        let mut edited = text.to_owned();
        edited.replace_range(self.start..self.end, &self.text);
        edited
    }
}

/// One question: a request at a place in a file, maybe after an edit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Probe {
    /// Unique in its project, and the same in every run.
    pub(super) id: String,
    /// Below the project root, `/`-separated.
    pub(super) file: String,
    pub(super) method: Method,
    /// Zero-based, in UTF-16 units, in the text as edited.
    pub(super) line: u32,
    pub(super) character: u32,
    /// The identifier asked about: a workspace symbol's query, a rename's
    /// old name, a completion's prefix.
    pub(super) word: String,
    pub(super) edit: Option<Edit>,
}

/// How many files and places of each kind are asked about.
#[derive(Clone, Copy)]
pub(super) struct Budget {
    pub(super) files: usize,
    pub(super) places: usize,
    pub(super) edits: usize,
    /// Random picks from this seed instead of evenly spaced ones.
    pub(super) seed: Option<u64>,
}

/// Every question asked of the project at `root`.
pub(super) fn probes(root: &Path, budget: Budget) -> Vec<Probe> {
    let mut probes = Vec::new();
    for file in pick_files(root, budget.files, &mut Picker::new(budget.seed, "files")) {
        let text = std::fs::read_to_string(root.join(&file)).unwrap_or_default();
        probes.extend(file_probes(&file, &text, budget));
    }
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for probe in &mut probes {
        let count = seen.entry(probe.id.clone()).or_default();
        *count += 1;
        if *count > 1 {
            probe.id = format!("{} #{count}", probe.id);
        }
    }
    probes
}

/// `count` main source files, spread over the sorted list: production code
/// under `src/main/java` where the project has it, of a size worth asking.
fn pick_files(root: &Path, count: usize, picker: &mut Picker) -> Vec<String> {
    let mut all = Vec::new();
    crate::indexer::walk_sources(root, |path| {
        if path.extension().is_some_and(|extension| extension == "java") {
            if let Ok(relative) = path.strip_prefix(root) {
                all.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    all.sort();
    let main: Vec<String> = all
        .iter()
        .filter(|path| path.contains("src/main/java/"))
        .cloned()
        .collect();
    let pool = if main.is_empty() { all } else { main };
    let sized: Vec<String> = pool
        .iter()
        .filter(|path| {
            let lines = std::fs::read_to_string(root.join(path))
                .map_or(0, |text| text.lines().count());
            (40..=1500).contains(&lines)
        })
        .cloned()
        .collect();
    picker.pick(if sized.is_empty() { pool } else { sized }, count)
}

/// How items are picked: evenly spaced, or at random from a seed.
enum Picker {
    Spread,
    Seeded(u64),
}

impl Picker {
    /// The picker of one stream: `name` is a file's path, or `files`.
    fn new(seed: Option<u64>, name: &str) -> Self {
        match seed {
            None => Self::Spread,
            // FNV-1a: the same on every machine and Rust version, which
            // the standard library's hasher does not promise.
            Some(seed) => Self::Seeded(name.bytes().fold(
                seed ^ 0xcbf2_9ce4_8422_2325,
                |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3),
            )),
        }
    }

    /// splitmix64.
    fn next(&mut self) -> u64 {
        let Self::Seeded(state) = self else { return 0 };
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = *state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }

    /// `count` of `items`, in their order.
    fn pick<T: Clone>(&mut self, items: Vec<T>, count: usize) -> Vec<T> {
        if matches!(self, Self::Spread) || items.len() <= count {
            return spread(items, count);
        }
        let mut indices: Vec<usize> = (0..items.len()).collect();
        for chosen in 0..count {
            let other = chosen + self.below(indices.len() - chosen);
            indices.swap(chosen, other);
        }
        let mut chosen = indices[..count].to_vec();
        chosen.sort_unstable();
        chosen.into_iter().map(|index| items[index].clone()).collect()
    }

    /// Where in a name the cursor goes: one character in, where servers
    /// agree on the token it touches, or anywhere in it at random.
    fn inside(&mut self, start: usize, end: usize, text: &str) -> usize {
        match self {
            Self::Spread => start + usize::from(end - start > 1),
            Self::Seeded(_) => {
                let boundaries: Vec<usize> = text[start..end]
                    .char_indices()
                    .map(|(offset, _)| start + offset)
                    .collect();
                boundaries[self.below(boundaries.len())]
            }
        }
    }
}

/// `count` items of `items`, evenly spaced, in their order.
fn spread<T: Clone>(items: Vec<T>, count: usize) -> Vec<T> {
    if items.len() <= count {
        return items;
    }
    (0..count)
        .map(|index| items[index * items.len() / count].clone())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Call,
    Type,
    Field,
    MethodDeclaration,
}

/// A place in the source worth asking about.
#[derive(Clone)]
struct Spot {
    kind: Kind,
    start: usize,
    end: usize,
    word: String,
    /// A call's argument list, just after its `(`.
    arguments: Option<usize>,
    /// Whether a call has a receiver (`a.b()`), so completion after the
    /// `.` asks for its members.
    qualified: bool,
}

/// The `;` that ends an expression statement, at its byte offset.
#[derive(Clone)]
struct Semicolon(usize);

fn file_probes(file: &str, text: &str, budget: Budget) -> Vec<Probe> {
    let mut probes = Vec::new();
    let whole_file = [
        Method::Diagnostics,
        Method::DocumentSymbol,
        Method::FoldingRange,
        Method::SemanticTokens,
        Method::Formatting,
        Method::CodeLens,
        Method::InlayHint,
    ];
    for method in whole_file {
        probes.push(probe(file, text, method, 0, "", None));
    }
    let (spots, semicolons) = spots(text);
    let mut picker = Picker::new(budget.seed, file);
    let of_kind = |kind: Kind, picker: &mut Picker| -> Vec<Spot> {
        let mut words = std::collections::HashSet::new();
        let unique = spots
            .iter()
            .filter(|spot| spot.kind == kind && words.insert(spot.word.clone()))
            .cloned()
            .collect();
        picker.pick(unique, budget.places)
    };
    let asked: &[(Kind, &[Method])] = &[
        (
            Kind::Call,
            &[
                Method::Definition,
                Method::Declaration,
                Method::TypeDefinition,
                Method::Implementation,
                Method::References,
                Method::DocumentHighlight,
                Method::Hover,
                Method::PrepareRename,
                Method::Rename,
                Method::CallHierarchy,
                Method::CodeAction,
            ],
        ),
        (
            Kind::Type,
            &[
                Method::Definition,
                Method::TypeDefinition,
                Method::Implementation,
                Method::References,
                Method::DocumentHighlight,
                Method::Hover,
                Method::TypeHierarchy,
                Method::WorkspaceSymbol,
            ],
        ),
        (
            Kind::Field,
            &[
                Method::Definition,
                Method::TypeDefinition,
                Method::References,
                Method::DocumentHighlight,
                Method::Hover,
            ],
        ),
        (
            Kind::MethodDeclaration,
            &[
                Method::References,
                Method::Implementation,
                Method::DocumentHighlight,
                Method::Hover,
                Method::CallHierarchy,
                Method::PrepareRename,
                Method::Rename,
                Method::SelectionRange,
            ],
        ),
    ];
    for (kind, methods) in asked {
        for spot in of_kind(*kind, &mut picker) {
            let inside = picker.inside(spot.start, spot.end, text);
            for method in *methods {
                probes.push(probe(file, text, *method, inside, &spot.word, None));
            }
            if let Some(arguments) = spot.arguments {
                probes.push(probe(file, text, Method::SignatureHelp, arguments, &spot.word, None));
            }
        }
    }

    let qualified: Vec<Spot> = spots
        .iter()
        .filter(|spot| spot.kind == Kind::Call && spot.qualified && spot.word.len() > 1)
        .cloned()
        .collect();
    for (index, spot) in picker.pick(qualified, budget.edits).into_iter().enumerate() {
        let prefix: String = spot.word.chars().take(1).collect();
        let cut = Edit {
            start: spot.start,
            end: spot.end,
            text: prefix.clone(),
            note: format!("`{}` cut to `{prefix}`", spot.word),
        };
        let at = spot.start + prefix.len();
        probes.push(probe(file, text, Method::Completion, at, &prefix, Some(cut)));
        if index == 0 {
            let emptied = Edit {
                start: spot.start,
                end: spot.end,
                text: String::new(),
                note: format!("`{}` removed after the `.`", spot.word),
            };
            probes.push(probe(file, text, Method::Completion, spot.start, "", Some(emptied)));
        }
        let missing = format!("{}Missing", spot.word);
        let unknown = Edit {
            start: spot.start,
            end: spot.end,
            text: missing.clone(),
            note: format!("`{}` called as `{missing}`", spot.word),
        };
        probes.push(probe(file, text, Method::Diagnostics, spot.start, &missing, Some(unknown)));
    }
    if let Some(Semicolon(offset)) = picker.pick(semicolons, 1).into_iter().next() {
        let dropped = Edit {
            start: offset,
            end: offset + 1,
            text: String::new(),
            note: "a `;` removed".to_owned(),
        };
        probes.push(probe(file, text, Method::Diagnostics, offset, "", Some(dropped)));
    }
    probes
}

/// A question about `file`, asked at byte `offset` of its text — of the
/// edited text, when there is an edit.
fn probe(
    file: &str,
    text: &str,
    method: Method,
    offset: usize,
    word: &str,
    edit: Option<Edit>,
) -> Probe {
    let edited = edit.as_ref().map(|edit| edit.apply(text));
    let (line, character) = position(edited.as_deref().unwrap_or(text), offset);
    let whole_file = offset == 0 && word.is_empty() && edit.is_none();
    let mut id = if whole_file {
        format!("{file} {}", method.name())
    } else {
        format!("{file}:{}:{} {}", line + 1, character + 1, method.name())
    };
    if let Some(edit) = &edit {
        id.push_str(&format!(" after {}", edit.note));
    }
    Probe {
        id,
        file: file.to_owned(),
        method,
        line,
        character,
        word: word.to_owned(),
        edit,
    }
}

/// The zero-based line and UTF-16 column of byte `offset`.
pub(super) fn position(text: &str, offset: usize) -> (u32, u32) {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    (
        before.matches('\n').count() as u32,
        before[line_start..].encode_utf16().count() as u32,
    )
}

/// Every place worth asking about in `text`, in source order, and every
/// `;` that ends an expression statement.
fn spots(text: &str) -> (Vec<Spot>, Vec<Semicolon>) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("the Java grammar loads");
    let Some(tree) = parser.parse(text, None) else {
        return (Vec::new(), Vec::new());
    };
    let mut spots = Vec::new();
    let mut semicolons = Vec::new();
    let mut stack = vec![tree.root_node()];
    let word = |node: tree_sitter::Node| text[node.byte_range()].to_owned();
    while let Some(node) = stack.pop() {
        let spot = |kind, name: tree_sitter::Node| Spot {
            kind,
            start: name.start_byte(),
            end: name.end_byte(),
            word: word(name),
            arguments: None,
            qualified: false,
        };
        match node.kind() {
            "method_invocation" => {
                if let Some(name) = node.child_by_field_name("name") {
                    spots.push(Spot {
                        arguments: node
                            .child_by_field_name("arguments")
                            .map(|arguments| arguments.start_byte() + 1),
                        qualified: node.child_by_field_name("object").is_some(),
                        ..spot(Kind::Call, name)
                    });
                }
            }
            "type_identifier" => spots.push(spot(Kind::Type, node)),
            "field_access" => {
                if let Some(field) = node.child_by_field_name("field") {
                    spots.push(spot(Kind::Field, field));
                }
            }
            "method_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    spots.push(spot(Kind::MethodDeclaration, name));
                }
            }
            "expression_statement" if text[node.byte_range()].ends_with(';') => {
                semicolons.push(Semicolon(node.end_byte() - 1));
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    spots.sort_by_key(|spot| spot.start);
    semicolons.sort_by_key(|semicolon| semicolon.0);
    (spots, semicolons)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "package a;\n\
        import java.util.List;\n\
        class Main {\n\
            int size;\n\
            void run(List<String> names) {\n\
                names.add(\"x\");\n\
                this.size = names.size();\n\
            }\n\
        }\n";

    #[test]
    fn asks_about_calls_types_fields_and_declarations() {
        let budget = Budget { files: 1, places: 2, edits: 1, seed: None };
        let probes = file_probes("a/Main.java", SOURCE, budget);
        let ids: Vec<&str> = probes.iter().map(|probe| probe.id.as_str()).collect();
        assert!(ids.contains(&"a/Main.java documentSymbol"));
        assert!(ids.contains(&"a/Main.java:6:8 definition"), "{ids:#?}");
        assert!(ids.contains(&"a/Main.java:6:11 signatureHelp"), "{ids:#?}");
        assert!(ids.contains(&"a/Main.java:5:11 typeHierarchy"), "{ids:#?}");
        assert!(ids.contains(&"a/Main.java:7:7 hover"), "{ids:#?}");
        assert!(ids.contains(&"a/Main.java:5:7 callHierarchy"), "{ids:#?}");
    }

    fn ids(seed: Option<u64>) -> Vec<String> {
        let budget = Budget { files: 1, places: 1, edits: 1, seed };
        file_probes("a/Main.java", SOURCE, budget)
            .into_iter()
            .map(|probe| probe.id)
            .collect()
    }

    #[test]
    fn a_seed_picks_the_same_every_time() {
        assert_eq!(ids(Some(7)), ids(Some(7)));
        assert_eq!(ids(None), ids(None));
        let seeds: std::collections::HashSet<Vec<String>> =
            (0..20).map(|seed| ids(Some(seed))).collect();
        assert!(seeds.len() > 1, "other seeds pick other places");
    }

    #[test]
    fn completion_is_asked_with_the_name_cut_back() {
        let budget = Budget { files: 1, places: 0, edits: 1, seed: None };
        let probes = file_probes("a/Main.java", SOURCE, budget);
        let completion = probes
            .iter()
            .find(|probe| probe.method == Method::Completion && !probe.word.is_empty())
            .expect("a completion probe");
        let edited = completion.edit.as_ref().unwrap().apply(SOURCE);
        assert!(edited.contains("names.a(\"x\");"));
        assert_eq!((completion.line, completion.character), (5, 7));
        assert!(probes.iter().any(|probe| probe.id.ends_with("after a `;` removed")));
    }
}
