//! Kotlin, read with tree-sitter-kotlin-ng. The semantic layer reads the
//! Kotlin tree lowered to the Java shape ([`lower`]); the outline and the
//! declared names are read off the Kotlin tree itself ([`outline`]).

mod lower;
mod outline;

use super::treesitter::{empty_tree, FoldKinds, Grammar};
use super::{Finding, Fold, Name, Outline, Position, Syn, Syntax};

pub struct Kotlin;

pub static GRAMMAR: Grammar = Grammar {
    language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    names: &["identifier"],
    package_or_import: &["package_header", "import"],
    folds: FoldKinds {
        members: &[
            "function_declaration",
            "secondary_constructor",
            "anonymous_initializer",
        ],
        body_wrappers: &["function_body"],
        blocks: &["class_body", "enum_class_body", "block", "lambda_literal"],
        switch_blocks: &[],
        switch_arms: &[],
        block_comments: &["block_comment", "multiline_comment"],
    },
};

impl Syntax for Kotlin {
    fn syntax_errors(&self, text: &str) -> Vec<Finding> {
        GRAMMAR.syntax_errors(text)
    }

    fn package(&self, text: &str) -> Option<String> {
        outline::package(&GRAMMAR.snapshot(text)?)
    }

    fn outline(&self, text: &str) -> Vec<Outline> {
        let Some(tree) = GRAMMAR.snapshot(text) else {
            return Vec::new();
        };
        let mut entries = outline::outline(&tree);
        entries.sort_by_key(|entry| entry.selection);
        entries
    }

    fn tree(&self, text: &str) -> Syn {
        GRAMMAR
            .snapshot(text)
            .map_or_else(empty_tree, |tree| lower::lower(&tree))
    }

    fn name_at(&self, text: &str, cursor: Position) -> Option<Name> {
        GRAMMAR.name_at(text, cursor)
    }

    fn declared_names(&self, text: &str) -> Vec<String> {
        GRAMMAR
            .snapshot(text)
            .map(|tree| outline::declared_names(&tree))
            .unwrap_or_default()
    }

    fn folds(&self, text: &str) -> Vec<Fold> {
        GRAMMAR.folds(text)
    }
}
