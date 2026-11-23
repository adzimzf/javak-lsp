//! How Kotlin writes and offers things. Where the Kotlin cases record an
//! answer of the reference server they were taken from, this follows it.

use super::semantic::{members_of, Bindings, Found, Snapshot};
use crate::parser::Syn;
use super::{LanguageFeatures, Literal, MethodShape, Parameter};

pub struct Kotlin;

impl LanguageFeatures for Kotlin {
    /// `fun area(): Double`; a function returning nothing writes no type.
    fn method_signature(&self, method: &MethodShape) -> String {
        let introduced = if method.type_parameters.is_empty() {
            String::new()
        } else {
            format!("<{}> ", method.type_parameters.join(", "))
        };
        let returns = method
            .returns
            .as_ref()
            .map(|returns| format!(": {returns}"))
            .unwrap_or_default();
        let parameters: Vec<String> = method
            .parameters
            .iter()
            .map(|parameter| self.parameter(parameter))
            .collect();
        format!(
            "fun {introduced}{}({}){returns}",
            method.name,
            parameters.join(", ")
        )
    }

    /// `text: String`, `vararg parts: String`.
    fn parameter(&self, parameter: &Parameter) -> String {
        let spread = if parameter.spread { "vararg " } else { "" };
        format!("{spread}{}: {}", parameter.name, parameter.written)
    }

    /// Members by name, nearest type first.
    fn member_rank(&self, depth: usize, _index: usize, name: &str) -> String {
        format!("{depth}{name}")
    }

    fn member_keywords(&self) -> &'static [&'static str] {
        &[
            "private", "protected", "public", "internal", "override", "fun", "val", "var",
            "class", "object",
        ]
    }

    fn file_keywords(&self) -> &'static [&'static str] {
        &[
            "package", "import", "class", "interface", "object", "enum", "fun", "val",
        ]
    }

    /// The reference server the Kotlin cases were recorded from answers no
    /// type definition.
    fn answers_type_definition(&self) -> bool {
        false
    }

    /// A local that shares its name with a property of its class: the
    /// reference server lists the property and its uses too.
    fn also_referenced<'a>(&self, snapshot: &'a Snapshot, found: &Found<'a>) -> Vec<Found<'a>> {
        if found.node.kind != "local_variable_declaration" {
            return Vec::new();
        }
        let Some(class) = snapshot.enclosing(found.file, found.name.start) else {
            return Vec::new();
        };
        members_of(class.node)
            .into_iter()
            .filter(|(member, named)| {
                member.kind == "field_declaration" && named.text == found.name.text
            })
            .map(|(member, named)| Found {
                file: class.file,
                node: member,
                name: named,
                bindings: Bindings::new(),
            })
            .collect()
    }

    /// Kotlin's default imports on the JVM; `kotlin.String` before
    /// `java.lang.String`.
    fn implicit_packages(&self) -> &'static [&'static str] {
        &[
            "kotlin",
            "kotlin.annotation",
            "kotlin.collections",
            "kotlin.comparisons",
            "kotlin.io",
            "kotlin.ranges",
            "kotlin.sequences",
            "kotlin.text",
            "kotlin.jvm",
            "java.lang",
        ]
    }

    fn has_top_level_functions(&self) -> bool {
        true
    }

    /// Kotlin's default visibility is `public`.
    fn public_by_default(&self, _owner: &Syn) -> bool {
        true
    }

    fn literal_type(&self, kind: &str, text: &str) -> &'static str {
        match Literal::of(kind, text) {
            Literal::Int => "Int",
            Literal::Long => "Long",
            Literal::Float => "Float",
            Literal::Double => "Double",
            Literal::Char => "Char",
            Literal::Boolean => "Boolean",
        }
    }
}
