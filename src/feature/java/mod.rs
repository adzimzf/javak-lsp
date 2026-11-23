//! How Java writes and offers things.

use super::semantic::{Found, Snapshot};
use super::{LanguageFeatures, MethodShape, Parameter};

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

    /// A type's own members in source order, then what it inherits by name.
    fn member_rank(&self, depth: usize, index: usize, name: &str) -> String {
        if depth == 0 {
            format!("0{index:04}")
        } else {
            format!("1{name}")
        }
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
}
