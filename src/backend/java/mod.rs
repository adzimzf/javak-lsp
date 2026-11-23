//! How the protocol names Java.

use super::LanguageProtocol;

pub struct Java;

impl LanguageProtocol for Java {
    fn language_id(&self) -> &'static str {
        "java"
    }
}
