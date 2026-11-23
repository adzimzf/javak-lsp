//! How the protocol names Kotlin.

use super::LanguageProtocol;

pub struct Kotlin;

impl LanguageProtocol for Kotlin {
    fn language_id(&self) -> &'static str {
        "kotlin"
    }
}
