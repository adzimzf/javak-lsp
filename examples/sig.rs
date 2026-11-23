use std::sync::Arc;

use javak_lsp::feature::semantic::{FileView, Snapshot};
use javak_lsp::language::Language;
use javak_lsp::parser::{syntax, Position};
use tower_lsp::lsp_types::Url;

fn main() {
    let text = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let line: u32 = std::env::args().nth(2).unwrap().parse().unwrap();
    let column: u32 = std::env::args().nth(3).unwrap().parse().unwrap();
    let path = std::env::args().nth(1).unwrap();
    let language = Language::of_path(&path).unwrap_or(Language::Java);
    let uri = Url::parse(if language == Language::Kotlin { "file:///x/E.kt" } else { "file:///x/E.java" })
        .unwrap();
    let snapshot = Snapshot::new(vec![FileView {
        uri: uri.clone(),
        language,
        tree: Arc::new(syntax(language).tree(&text)),
        text: Arc::new(text),
        library: false,
    }]);
    let at = Position { line, column };
    let file = snapshot.file(&uri).unwrap();
    match javak_lsp::feature::signature::help_at(&snapshot, file, at) {
        Some(help) => println!("{}({}) active={:?}", help.name, help.parameters.join(", "), help.active),
        None => println!("(nothing)"),
    }
}
