use javak_lsp::language::Language;
use javak_lsp::parser::syntax;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let language = Language::of_path(&path).unwrap_or(Language::Java);
    show(&syntax(language).tree(&text), 0);
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
