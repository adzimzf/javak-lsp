use javak_lsp::parser::treesitter::{Language, TreeSitter};
use javak_lsp::parser::Syntax;

fn main() {
    let text = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let syntax = TreeSitter::new(Language::Java);
    show(&syntax.tree(&text), 0);
}

fn show(node: &javak_lsp::parser::Syn, depth: usize) {
    let text = node.text.lines().next().unwrap_or("");
    println!(
        "{:indent$}{} [{}] {:?}",
        "",
        node.kind,
        node.field.clone().unwrap_or_default(),
        &text[..text.len().min(40)],
        indent = depth * 2
    );
    for child in &node.children {
        show(child, depth + 1);
    }
}
