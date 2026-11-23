//! Prints what completion offers for a `.case` file, so a failing case can
//! be looked at without the server in the way.

use std::sync::Arc;

use javak_lsp::feature::semantic::{FileView, Snapshot};
use javak_lsp::language::Language;
use javak_lsp::parser::{syntax, Position};
use tower_lsp::lsp_types::Url;

fn main() {
    let case = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let mut files: Vec<(String, String)> = Vec::new();
    for section in case.split("//- ").skip(1) {
        let (path, body) = section.split_once('\n').unwrap();
        let body = body.split("//= expected").next().unwrap();
        files.push((path.trim().to_owned(), body.to_owned()));
    }
    let mut cursor = None;
    let views: Vec<FileView> = files
        .iter()
        .map(|(path, body)| {
            let uri = Url::parse(&format!("file:///work/src/main/java{path}")).unwrap();
            let text = if let Some(at) = body.find("$0") {
                let before = &body[..at];
                let line = before.lines().count() as u32 - 1;
                let column = before.lines().last().unwrap_or("").chars().count() as u32;
                cursor = Some((uri.clone(), Position { line, column }));
                body.replace("$0", "")
            } else {
                body.clone()
            };
            let language = Language::of_path(path).unwrap_or(Language::Java);
            FileView {
                uri,
                language,
                tree: Arc::new(syntax(language).tree(&text)),
                text: Arc::new(text),
                library: false,
            }
        })
        .collect();
    let snapshot = Snapshot { files: views };
    let (uri, at) = cursor.expect("a case has a cursor");
    let file = snapshot.file(&uri).unwrap();
    for view in &snapshot.files {
        eprintln!("file {} package {:?}", view.uri, javak_lsp::feature::semantic::package_of(&snapshot, view));
    }
    if let Ok(name) = std::env::var("TYPE") {
        let found = snapshot.type_named(&name, file).unwrap();
        eprintln!("named {} -> {} in {}", name, found.name.text, found.file.uri);
        eprintln!("supertype {:?}", snapshot.supertype_of(&found).map(|found| found.name.text.clone()));
        for part in snapshot.with_supertypes(&found) {
            println!("family {} ({})", part.name.text, part.file.uri);
            for (member, named) in javak_lsp::feature::semantic::members_of(part.node) {
                println!("  {} {:?}", named.text, member.kind);
            }
        }
    }
    for offer in javak_lsp::feature::completion::offers(&snapshot, file, at, None) {
        println!("{} [{}]", offer.name, offer.rank);
    }
}
