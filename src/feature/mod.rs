//! What the server answers: the semantic layer and each request built on
//! it. The requests read Java-shaped trees whatever the language (the
//! parser lowers Kotlin to that shape); what a language does its own way —
//! how a signature is written, which keywords are offered, what a
//! reference includes — is behind [`LanguageFeatures`], implemented in
//! `java` and `kotlin`.

pub mod completion;
pub mod highlight;
pub mod java;
pub mod kotlin;
pub mod query;
pub mod semantic;
pub mod signature;

use crate::language::Language;
use semantic::{Found, Snapshot};

/// One parameter of a declaration, as the source writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The type, with the receiver's type arguments filled in.
    pub written: String,
    pub name: String,
    /// Whether it takes the rest of the arguments (`String...`, `vararg`).
    pub spread: bool,
}

/// What a method's signature is made of.
#[derive(Clone, Debug)]
pub struct MethodShape {
    pub name: String,
    pub type_parameters: Vec<String>,
    /// The return type as written; `None` where none is written.
    pub returns: Option<String>,
    pub parameters: Vec<Parameter>,
    /// The `throws` clause as written.
    pub thrown: Option<String>,
}

/// What a language does its own way.
pub trait LanguageFeatures: Send + Sync {
    /// A method as hover shows it.
    fn method_signature(&self, method: &MethodShape) -> String;
    /// A parameter as hover and signature help show it.
    fn parameter(&self, parameter: &Parameter) -> String;
    /// How completion sorts a member of a type `depth` supertypes up, the
    /// `index`th its type declares.
    fn member_rank(&self, depth: usize, index: usize, name: &str) -> String;
    /// The keywords offered where a member can start.
    fn member_keywords(&self) -> &'static [&'static str];
    /// The keywords offered where a top-level declaration can start.
    fn file_keywords(&self) -> &'static [&'static str];
    /// Whether `textDocument/typeDefinition` is answered.
    fn answers_type_definition(&self) -> bool;
    /// Declarations whose uses are listed among the references of `found`
    /// besides its own.
    fn also_referenced<'a>(&self, snapshot: &'a Snapshot, found: &Found<'a>) -> Vec<Found<'a>>;
}

/// The features of a language.
pub fn features(language: Language) -> &'static dyn LanguageFeatures {
    match language {
        Language::Java => &java::Java,
        Language::Kotlin => &kotlin::Kotlin,
    }
}
