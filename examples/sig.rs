use javak_lsp::parser::treesitter::{Language, TreeSitter};
use javak_lsp::parser::{Position, Syntax};
use javak_lsp::semantic::{FileView, Snapshot};
use tower_lsp::lsp_types::Url;

fn main() {
    let text = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let line: u32 = std::env::args().nth(2).unwrap().parse().unwrap();
    let column: u32 = std::env::args().nth(3).unwrap().parse().unwrap();
    let syntax = TreeSitter::new(Language::Java);
    let uri = Url::parse("file:///x/E.java").unwrap();
    let snapshot = Snapshot {
        files: vec![FileView { uri: uri.clone(), text: text.clone(), tree: syntax.tree(&text) }],
    };
    let at = Position { line, column };
    let file = snapshot.file(&uri).unwrap();
    match javak_lsp::signature::help_at(&snapshot, file, at) {
        Some(help) => println!("{}({}) active={:?}", help.name, help.parameters.join(", "), help.active),
        None => println!("(nothing)"),
    }
}
