//! How Java writes and offers things.

use super::semantic::{Found, Snapshot};
use crate::parser::Syn;
use super::{LanguageFeatures, Literal, MethodShape, Parameter};

pub struct Java;

impl LanguageFeatures for Java {
    /// `<T> T unwrap(T value) throws Failure`, as jdtls words it.
    fn method_signature(&self, method: &MethodShape) -> String {
        let introduced = if method.type_parameters.is_empty() {
            String::new()
        } else {
            format!("<{}> ", method.type_parameters.join(", "))
        };
        let returns = method
            .returns
            .as_ref()
            .map(|returns| format!("{returns} "))
            .unwrap_or_default();
        let parameters: Vec<String> = method
            .parameters
            .iter()
            .map(|parameter| self.parameter(parameter))
            .collect();
        let thrown = method
            .thrown
            .as_ref()
            .map(|thrown| format!(" {thrown}"))
            .unwrap_or_default();
        format!(
            "{introduced}{returns}{}({}){thrown}",
            method.name,
            parameters.join(", ")
        )
    }

    /// `String s`, `String... parts`.
    fn parameter(&self, parameter: &Parameter) -> String {
        let spread = if parameter.spread { "..." } else { "" };
        format!("{}{spread} {}", parameter.written, parameter.name)
    }

    /// As jdtls ranks them: fields and constants before methods, each by
    /// name, wherever in the hierarchy the member is declared.
    fn member_rank(&self, _depth: usize, _index: usize, name: &str, kind: &str) -> String {
        let group = match kind {
            "field_declaration" | "constant_declaration" | "enum_constant" => 0,
            _ => 1,
        };
        format!("{group}{name}")
    }

    fn member_keywords(&self) -> &'static [&'static str] {
        &["private", "protected", "public", "static", "final", "class"]
    }

    fn file_keywords(&self) -> &'static [&'static str] {
        &["package", "import", "public", "class", "interface", "enum"]
    }

    fn answers_type_definition(&self) -> bool {
        true
    }

    fn also_referenced<'a>(&self, _snapshot: &'a Snapshot, _found: &Found<'a>) -> Vec<Found<'a>> {
        Vec::new()
    }

    fn implicit_packages(&self) -> &'static [&'static str] {
        &["java.lang"]
    }

    fn has_top_level_functions(&self) -> bool {
        false
    }

    /// An interface's members are public.
    fn public_by_default(&self, owner: &Syn) -> bool {
        matches!(
            owner.kind.as_str(),
            "interface_declaration" | "annotation_type_declaration"
        )
    }

    fn literal_type(&self, kind: &str, text: &str) -> &'static str {
        match Literal::of(kind, text) {
            Literal::Int => "int",
            Literal::Long => "long",
            Literal::Float => "float",
            Literal::Double => "double",
            Literal::Char => "char",
            Literal::Boolean => "boolean",
        }
    }
}
