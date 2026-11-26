//! Semantic tokens for Java, as jdtls's `SemanticTokensVisitor` gives
//! them: each name with the kind of what it means and that declaration's
//! modifiers, the modifier keywords, the keywords that open a type and its
//! supertypes (`class`, `interface`, `extends`, `implements`), and in
//! Javadoc the tags and the names they refer to.
//!
//! - Kinds and modifiers are jdtls's legend ([`TYPES`], [`MODIFIERS`]):
//!   `readonly` is `final`, `generic` a type with type parameters or a
//!   method of one or with its own, `typeArgument` a type written as a type
//!   argument, `constructor` a type's name where it stands for one of its
//!   constructors.
//! - Implicit modifiers count as the compiler sets them: an interface's
//!   methods are public (and abstract without a body), its fields public,
//!   static and final, its member types public and static; a member enum,
//!   interface or record is static; an enum constant is public, static and
//!   final. An interface itself is not shown as abstract.
//! - A name nothing resolves is left without a token.

use std::collections::HashMap;

use super::{is_type_declaration, javadoc_references, path_to, FileView, Found, Snapshot};
use crate::parser::{Position, Syn};

/// jdtls's token types, in its order.
pub const TYPES: &[&str] = &[
    "namespace",
    "class",
    "interface",
    "enum",
    "enumMember",
    "type",
    "typeParameter",
    "method",
    "property",
    "variable",
    "parameter",
    "modifier",
    "keyword",
    "decorator",
    "annotationMember",
    "record",
    "recordComponent",
];

/// jdtls's token modifiers, in its order: the bit of each is its index.
pub const MODIFIERS: &[&str] = &[
    "abstract",
    "static",
    "readonly",
    "deprecated",
    "declaration",
    "documentation",
    "public",
    "private",
    "protected",
    "native",
    "generic",
    "typeArgument",
    "importDeclaration",
    "constructor",
];

const NAMESPACE: u32 = 0;
const CLASS: u32 = 1;
const INTERFACE: u32 = 2;
const ENUM: u32 = 3;
const ENUM_MEMBER: u32 = 4;
const TYPE_PARAMETER: u32 = 6;
const METHOD: u32 = 7;
const PROPERTY: u32 = 8;
const VARIABLE: u32 = 9;
const PARAMETER: u32 = 10;
const MODIFIER: u32 = 11;
const KEYWORD: u32 = 12;
const DECORATOR: u32 = 13;
const ANNOTATION_MEMBER: u32 = 14;
const RECORD: u32 = 15;

const ABSTRACT: u32 = 1 << 0;
const STATIC: u32 = 1 << 1;
const READONLY: u32 = 1 << 2;
const DEPRECATED: u32 = 1 << 3;
const DECLARATION: u32 = 1 << 4;
const DOCUMENTATION: u32 = 1 << 5;
const PUBLIC: u32 = 1 << 6;
const PRIVATE: u32 = 1 << 7;
const PROTECTED: u32 = 1 << 8;
const NATIVE: u32 = 1 << 9;
const GENERIC: u32 = 1 << 10;
const TYPE_ARGUMENT: u32 = 1 << 11;
const IMPORT_DECLARATION: u32 = 1 << 12;
const CONSTRUCTOR: u32 = 1 << 13;

/// The keywords a `modifiers` list holds that jdtls shows as modifiers.
const MODIFIER_WORDS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "native",
    "synchronized",
    "transient",
    "volatile",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
];

/// One token: where it starts, how long it is (in UTF-16 units), its kind
/// and its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub at: Position,
    pub length: u32,
    pub kind: u32,
    pub modifiers: u32,
}

impl Snapshot {
    /// The semantic tokens of a Java file, in order.
    pub fn semantic_tokens(&self, file: &FileView) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut kinds = Kinds::default();
        for node in file.tree.walk() {
            match node.kind.as_str() {
                "modifiers" => {
                    for word in &node.children {
                        if MODIFIER_WORDS.contains(&word.kind.as_str()) {
                            tokens.push(keyword(word, MODIFIER));
                        }
                    }
                }
                "class_declaration" | "interface_declaration" => {
                    for word in &node.children {
                        if matches!(word.kind.as_str(), "class" | "interface") {
                            tokens.push(keyword(word, MODIFIER));
                        }
                    }
                    // With supertypes, jdtls shows the header's first
                    // `extends`, a type parameter's bound's as well, and
                    // `implements`.
                    let supertypes: Vec<&Syn> = node
                        .children
                        .iter()
                        .filter(|child| matches!(child.kind.as_str(), "superclass" | "super_interfaces" | "extends_interfaces"))
                        .collect();
                    if !supertypes.is_empty() {
                        let header: Vec<&Syn> = node
                            .children
                            .iter()
                            .filter(|child| matches!(child.kind.as_str(), "type_parameters" | "superclass" | "super_interfaces" | "extends_interfaces"))
                            .collect();
                        let first_extends = header
                            .iter()
                            .flat_map(|part| part.walk())
                            .find(|word| word.kind == "extends");
                        tokens.extend(first_extends.map(|word| keyword(word, MODIFIER)));
                        for list in &supertypes {
                            for word in &list.children {
                                if matches!(word.kind.as_str(), "implements" | "permits") {
                                    tokens.push(keyword(word, MODIFIER));
                                }
                            }
                        }
                    }
                }
                "static_initializer" => {
                    for word in &node.children {
                        if word.kind == "static" {
                            tokens.push(keyword(word, MODIFIER));
                        }
                    }
                }
                "class_literal" => {
                    for word in &node.children {
                        if word.kind == "class" {
                            tokens.push(keyword(word, KEYWORD));
                        }
                    }
                }
                "identifier" | "type_identifier" => {
                    if let Some(token) = self.name_token(file, node, &mut kinds) {
                        tokens.push(token);
                    }
                }
                "block_comment" if node.text.starts_with("/**") => {
                    tokens.extend(self.javadoc_tokens(file, node, &mut kinds));
                }
                _ => {}
            }
        }
        tokens.sort_by_key(|token| token.at);
        tokens.dedup_by_key(|token| token.at);
        tokens
    }

    /// The token of a name in code.
    fn name_token(&self, file: &FileView, name: &Syn, kinds: &mut Kinds) -> Option<Token> {
        let path = path_to(&file.tree, name.start);
        // jdtls leaves an interface's own supertypes without tokens.
        if path.iter().any(|node| node.kind == "extends_interfaces") {
            return None;
        }
        // `xs.length`: an array's length.
        if name.text == "length"
            && let Some(access) = path.iter().rev().nth(1).filter(|parent| parent.kind == "field_access")
            && access.child("field").is_some_and(|field| field.start == name.start)
            && let Some(object) = access.child("object")
            && self.type_of_expression(object, file).is_some_and(|written| written.dimensions > 0)
        {
            return Some(token(name, PROPERTY, PUBLIC | READONLY));
        }
        let mut context = 0;
        if path.iter().any(|node| node.kind == "import_declaration") {
            context |= IMPORT_DECLARATION;
        }
        // The packages of a package declaration or an import.
        let in_header = path
            .iter()
            .find(|node| matches!(node.kind.as_str(), "package_declaration" | "import_declaration"));
        if let Some(header) = in_header {
            return self.header_token(file, header, name, context);
        }
        if let Some(namespace) = self.package_part(file, &path, name, context) {
            return Some(namespace);
        }
        let found = self.declaration_at(&file.uri, name.start)?;
        // A name standing for a package: `java.util.List` in code.
        if found.node.kind == "package_declaration" {
            return Some(token(name, NAMESPACE, context));
        }
        let parent = path.iter().rev().nth(1).copied();
        let is_qualifier = parent.is_some_and(|parent| {
            parent.kind == "scoped_type_identifier" && parent.children.last().is_some_and(|last| last.start != name.start)
        });
        if is_qualifier {
            // `Range` of `Range.AttributeRange`: not itself the argument.
        } else if parent.is_some_and(|parent| parent.kind == "type_arguments")
            || path
                .iter()
                .rev()
                .skip(1)
                .take_while(|node| matches!(node.kind.as_str(), "scoped_type_identifier" | "generic_type" | "annotated_type" | "array_type"))
                .count()
                > 0
                && path
                    .iter()
                    .rev()
                    .skip(1)
                    .find(|node| !matches!(node.kind.as_str(), "scoped_type_identifier" | "generic_type" | "annotated_type" | "array_type"))
                    .is_some_and(|node| node.kind == "type_arguments")
        {
            context |= TYPE_ARGUMENT;
        }
        let declares = found.file.uri == file.uri && found.name.start == name.start;
        if declares {
            context |= DECLARATION;
        }
        // `new Thing()` and a constructor's own name stand for a constructor.
        // The creation whose type ends with this name (`new Token.Doctype()`,
        // `new ArrayList<>()`).
        let creation = path.iter().rev().skip(1).find(|node| {
            !matches!(node.kind.as_str(), "scoped_type_identifier" | "generic_type" | "type_arguments" | "annotated_type")
        });
        let creates = creation.is_some_and(|creation| {
            creation.kind == "object_creation_expression"
                && creation.child("type").is_some_and(|written| {
                    let base = if written.kind == "generic_type" { written.children.first() } else { Some(written) };
                    base.is_some_and(|base| base.end == name.end && base.start <= name.start)
                })
        });
        // `new Iterator<T>() {…}`: an anonymous class's constructor.
        if creates && creation.is_some_and(|creation| super::is_anonymous_class(creation)) {
            return Some(token(name, CLASS, CONSTRUCTOR | (context & TYPE_ARGUMENT)));
        }
        if creates || found.node.kind == "constructor_declaration" {
            let constructor = if found.node.kind == "constructor_declaration" {
                Some(found.clone())
            } else {
                creation
                    .filter(|creation| creation.kind == "object_creation_expression")
                    .and_then(|creation| self.created(creation, file))
                    .filter(|created| created.node.kind == "constructor_declaration")
            };
            let visibility = match &constructor {
                Some(constructor) => kinds.of(self, constructor).1 & (PUBLIC | PRIVATE | PROTECTED),
                None => {
                    let class = if is_type_declaration(found.node) { Some(found.clone()) } else { None };
                    class.map_or(0, |class| kinds.of(self, &class).1 & (PUBLIC | PRIVATE | PROTECTED))
                }
            };
            let declaration = if found.node.kind == "constructor_declaration" && declares { DECLARATION } else { 0 };
            let kind = if found.node.kind == "constructor_declaration" {
                let owner = path_to(&found.file.tree, found.node.start)
                    .into_iter()
                    .rev()
                    .find(|node| is_type_declaration(node) && !std::ptr::eq(*node, found.node))?;
                kinds.of(self, &Found { file: found.file, node: owner, name: owner.child("name")?, bindings: super::Bindings::new() }).0
            } else {
                kinds.of(self, &found).0
            };
            let generic_type = is_type_declaration(found.node) && found.node.child("type_parameters").is_some() && with_type_arguments(&path, name);
            let generic_constructor = found.node.kind == "constructor_declaration" && kinds.of(self, &found).1 & GENERIC != 0;
            let generic = if generic_type || generic_constructor { GENERIC } else { 0 };
            return Some(token(name, kind, CONSTRUCTOR | visibility | declaration | generic | (context & TYPE_ARGUMENT)));
        }
        let (kind, mut modifiers) = kinds.of(self, &found);
        if is_type_declaration(found.node) && !declares && !with_type_arguments(&path, name) {
            modifiers &= !GENERIC;
        }
        Some(token(name, kind, modifiers | context))
    }

    /// A package's name in a type named in full in code
    /// (`org.jsoup.nodes.Document`): a namespace.
    fn package_part(&self, file: &FileView, path: &[&Syn], name: &Syn, context: u32) -> Option<Token> {
        // The whole dotted name this one is part of, and that it is not the last of.
        let dotted = path
            .iter()
            .rev()
            .skip(1)
            .take_while(|node| matches!(node.kind.as_str(), "scoped_type_identifier" | "scoped_identifier" | "field_access"))
            .last()?;
        let written: String = dotted.text.split_whitespace().collect();
        let before: String = dotted
            .walk()
            .into_iter()
            .filter(|node| matches!(node.kind.as_str(), "identifier" | "type_identifier") && node.start <= name.start)
            .map(|node| node.text.as_str())
            .collect::<Vec<_>>()
            .join(".");
        if before == written || !written.starts_with(&before) {
            return None;
        }
        // Not a variable, not a type: a package, where the rest names a type.
        let first = written.split('.').next()?;
        if self.name_in_scope(first, file, name.start).is_some_and(|found| !is_type_declaration(found.node) || found.name.text == first) {
            return None;
        }
        if self.type_named(&before, file).is_some_and(|found| self.qualified_type_name(found.file, found.node) == before) {
            return None;
        }
        let mut prefix = before.clone();
        for segment in written[before.len()..].split('.').filter(|segment| !segment.is_empty()) {
            prefix = format!("{prefix}.{segment}");
            if let Some(found) = self.type_named(&prefix, file)
                && is_type_declaration(found.node)
                && self.qualified_type_name(found.file, found.node) == prefix
            {
                return Some(token(name, NAMESPACE, context & IMPORT_DECLARATION));
            }
        }
        None
    }

    /// A name of a package or import declaration: packages are namespaces,
    /// an import's last names are the type (and member) it imports.
    fn header_token(&self, file: &FileView, header: &Syn, name: &Syn, context: u32) -> Option<Token> {
        if header.kind == "package_declaration" {
            return Some(token(name, NAMESPACE, 0));
        }
        let written: String = header
            .text
            .trim_start_matches("import")
            .trim()
            .trim_start_matches("static")
            .trim()
            .trim_end_matches(';')
            .trim()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        // The dotted name up to and with this one.
        let upto: String = header
            .walk()
            .into_iter()
            .filter(|node| node.kind == "identifier" && node.start <= name.start)
            .map(|node| node.text.as_str())
            .collect::<Vec<_>>()
            .join(".");
        if !written.starts_with(&upto) {
            return None;
        }
        match self.type_named(&upto, file) {
            Some(found) if is_type_declaration(found.node) && self.qualified_type_name(found.file, found.node) == upto => {
                let (kind, modifiers) = Kinds::default().of(self, &found);
                Some(token(name, kind, modifiers | context))
            }
            _ => {
                // A static import's member, or a package.
                let Some((owner, member)) = upto.rsplit_once('.') else {
                    return Some(token(name, NAMESPACE, context));
                };
                if let Some(owner_type) = self.type_named(owner, file)
                    && self.qualified_type_name(owner_type.file, owner_type.node) == owner
                {
                    let found = self.member(&owner_type, member, None)?;
                    let (kind, modifiers) = Kinds::default().of(self, &found);
                    return Some(token(name, kind, modifiers | context));
                }
                Some(token(name, NAMESPACE, context))
            }
        }
    }

    /// The tokens of a Javadoc comment: its tags, `@param`'s names, and
    /// the names its references (`{@link}`, `@see`, `@throws`) are made of.
    fn javadoc_tokens(&self, file: &FileView, comment: &Syn, kinds: &mut Kinds) -> Vec<Token> {
        let mut tokens = Vec::new();
        let text = &comment.text;
        let position_of = |offset: usize| {
            let before = &text[..offset];
            let line = before.matches('\n').count() as u32;
            let line_start = before.rfind('\n').map_or(0, |at| at + 1);
            let column = before[line_start..].encode_utf16().count() as u32;
            if line == 0 {
                Position { line: comment.start.line, column: comment.start.column + column }
            } else {
                Position { line: comment.start.line + line, column }
            }
        };
        let documented = self.documented_declaration(file, comment);
        // Tags: `@param` at a line's start, `{@code` inline.
        let bytes = text.as_bytes();
        for (offset, _) in text.match_indices('@') {
            let inline = offset > 0 && bytes[offset - 1] == b'{';
            let at_line_start = text[..offset]
                .rsplit('\n')
                .next()
                .is_some_and(|before| before.trim_start().trim_start_matches("/**").trim_start_matches('*').trim().is_empty());
            if !inline && !at_line_start {
                continue;
            }
            let tag: String = text[offset + 1..].chars().take_while(|character| character.is_ascii_alphabetic()).collect();
            if tag.is_empty() {
                continue;
            }
            tokens.push(Token {
                at: position_of(offset),
                length: tag.len() as u32 + 1,
                kind: KEYWORD,
                modifiers: DOCUMENTATION,
            });
            // `@param name`, `@param <T>`.
            if tag == "param" && !inline {
                let rest = &text[offset + 6..];
                let gap = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                let rest = &rest[gap..];
                let (bracketed, named) = match rest.strip_prefix('<') {
                    Some(inner) => (1, inner),
                    None => (0, rest),
                };
                let parameter: String = named.chars().take_while(|character| character.is_alphanumeric() || *character == '_').collect();
                if parameter.is_empty() {
                    continue;
                }
                let at = position_of(offset + 6 + gap + bracketed);
                if let Some(declaration) = &documented {
                    let (kind, found) = if bracketed == 1 {
                        (TYPE_PARAMETER, declared_type_parameter(declaration, &parameter))
                    } else {
                        (PARAMETER, declared_parameter(declaration, &parameter))
                    };
                    if found {
                        tokens.push(Token { at, length: parameter.encode_utf16().count() as u32, kind, modifiers: DOCUMENTATION });
                    }
                }
            }
        }
        // References.
        for (written, start) in javadoc_references(comment) {
            let type_part = written.split(['#', '(']).next().unwrap_or_default();
            let mut segment_names: Vec<&str> = Vec::new();
            if !type_part.is_empty() {
                segment_names.extend(type_part.split('.'));
            }
            let mut offset = 0;
            // The type's own names: a package's are namespaces where the
            // whole names a type.
            let whole = (!type_part.is_empty()).then(|| self.type_named(type_part, file)).flatten();
            for segment in type_part.split('.').filter(|_| !type_part.is_empty()) {
                let prefix = &type_part[..offset + segment.len()];
                let at = Position { line: start.line, column: start.column + type_part[..offset].encode_utf16().count() as u32 };
                let named = self
                    .type_named(prefix, file)
                    .filter(|found| is_type_declaration(found.node) && (prefix.contains('.') || found.name.text == segment));
                match named {
                    Some(found) => {
                        let (kind, modifiers) = kinds.of(self, &found);
                        tokens.push(Token { at, length: segment.encode_utf16().count() as u32, kind, modifiers: modifiers | DOCUMENTATION });
                    }
                    None if whole.is_some() => {
                        tokens.push(Token { at, length: segment.encode_utf16().count() as u32, kind: NAMESPACE, modifiers: DOCUMENTATION });
                    }
                    None => {}
                }
                offset += segment.len() + 1;
            }
            // The member, and the types of its parameters.
            if let Some((_, after_hash)) = written.split_once('#') {
                let member: String = after_hash.chars().take_while(|character| character.is_alphanumeric() || *character == '_').collect();
                if !member.is_empty() {
                    for (at, found) in self.javadoc_reference_names(file, comment, &written, start, &member) {
                        if !is_type_declaration(found.node) || found.node.kind == "constructor_declaration" {
                            let (kind, mut modifiers) = kinds.of(self, &found);
                            // A member of a generic type named through that
                            // type itself is its raw member.
                            let own_type_parameters = found.node.child("type_parameters").is_some();
                            let through_declaring_type = type_part.is_empty()
                                || self.type_named(type_part, file).is_some_and(|named| {
                                    path_to(&found.file.tree, found.node.start)
                                        .iter()
                                        .any(|around| std::ptr::eq(*around, named.node))
                                });
                            if through_declaring_type && !own_type_parameters {
                                modifiers &= !GENERIC;
                            }
                            if found.node.kind == "constructor_declaration" {
                                modifiers = CONSTRUCTOR | (modifiers & (PUBLIC | PRIVATE | PROTECTED | DEPRECATED));
                            }
                            tokens.push(Token { at, length: member.encode_utf16().count() as u32, kind, modifiers: modifiers | DOCUMENTATION });
                        }
                    }
                }
                if let Some((_, parameters)) = after_hash.split_once('(') {
                    let parameters = parameters.split(')').next().unwrap_or_default();
                    let opened = written.find('(').unwrap_or_default() + 1;
                    let mut offset = opened;
                    for parameter in parameters.split(',') {
                        let leading = parameter.len() - parameter.trim_start().len();
                        let written_type = parameter.split_whitespace().next().unwrap_or_default();
                        let raw = written_type.split('<').next().unwrap_or_default().trim_end_matches("...").trim_end_matches("[]");
                        let whole = self.type_named(raw, file).filter(|found| is_type_declaration(found.node));
                        let mut within = 0;
                        for segment in raw.split('.') {
                            let prefix = &raw[..within + segment.len()];
                            let at = Position {
                                line: start.line,
                                column: start.column + written[..offset + leading + within].encode_utf16().count() as u32,
                            };
                            let length = segment.encode_utf16().count() as u32;
                            let named = self
                                .type_named(prefix, file)
                                .filter(|found| is_type_declaration(found.node) && (prefix.contains('.') || found.name.text == segment));
                            match named {
                                Some(found) => {
                                    let (kind, modifiers) = kinds.of(self, &found);
                                    tokens.push(Token { at, length, kind, modifiers: modifiers | DOCUMENTATION });
                                }
                                None if whole.is_some() => {
                                    tokens.push(Token { at, length, kind: NAMESPACE, modifiers: DOCUMENTATION });
                                }
                                None => {}
                            }
                            within += segment.len() + 1;
                        }
                        offset += parameter.len() + 1;
                    }
                }
            }
        }
        tokens
    }

    /// The declaration a Javadoc comment documents.
    fn documented_declaration<'a>(&self, file: &'a FileView, comment: &Syn) -> Option<&'a Syn> {
        file.tree
            .walk()
            .into_iter()
            .filter(|node| node.start >= comment.end)
            .find(|node| {
                matches!(
                    node.kind.as_str(),
                    "method_declaration"
                        | "constructor_declaration"
                        | "class_declaration"
                        | "interface_declaration"
                        | "enum_declaration"
                        | "record_declaration"
                        | "field_declaration"
                        | "annotation_type_declaration"
                        | "enum_constant"
                )
            })
    }
}

/// Whether a declaration declares a parameter called `name`.
fn declared_parameter(declaration: &Syn, name: &str) -> bool {
    super::parameters_of(declaration).iter().any(|parameter| {
        parameter
            .child("name")
            .or_else(|| parameter.first("variable_declarator").and_then(|declarator| declarator.child("name")))
            .is_some_and(|named| named.text == name)
    })
}

/// Whether a declaration declares a type parameter called `name`.
fn declared_type_parameter(declaration: &Syn, name: &str) -> bool {
    declaration.child("type_parameters").is_some_and(|parameters| {
        parameters
            .all("type_parameter")
            .any(|parameter| parameter.first("type_identifier").is_some_and(|named| named.text == name))
    })
}

/// Whether a type's name is written with type arguments (`List<String>`,
/// `new ArrayList<>()`): jdtls calls only those, not a raw name, generic.
fn with_type_arguments(path: &[&Syn], name: &Syn) -> bool {
    let mut current = name;
    for around in path.iter().rev().skip(1) {
        match around.kind.as_str() {
            "scoped_type_identifier" if around.children.last().is_some_and(|last| last.start == current.start) => current = around,
            "generic_type" => return around.children.first().is_some_and(|base| base.start == current.start && base.end == current.end),
            _ => return false,
        }
    }
    false
}

fn keyword(word: &Syn, kind: u32) -> Token {
    token(word, kind, 0)
}

fn token(node: &Syn, kind: u32, modifiers: u32) -> Token {
    Token {
        at: node.start,
        length: node.text.encode_utf16().count() as u32,
        kind,
        modifiers,
    }
}

/// The kind and modifiers of declarations, worked out once each.
#[derive(Default)]
struct Kinds {
    known: HashMap<(usize, u32, u32), (u32, u32)>,
}

impl Kinds {
    fn of(&mut self, snapshot: &Snapshot, found: &Found<'_>) -> (u32, u32) {
        let key = (std::ptr::from_ref(found.file) as usize, found.node.start.line, found.node.start.column);
        if let Some(known) = self.known.get(&key) {
            return *known;
        }
        let answer = kind_and_modifiers(snapshot, found);
        self.known.insert(key, answer);
        answer
    }
}

/// What jdtls shows a declaration as.
fn kind_and_modifiers(snapshot: &Snapshot, found: &Found<'_>) -> (u32, u32) {
    let node = found.node;
    let path = path_to(&found.file.tree, node.start);
    // The type a member is declared in, and that type's own path.
    let owner = path.iter().rev().find(|around| is_type_declaration(around) && !std::ptr::eq(**around, node)).copied();
    let in_interface = owner.is_some_and(|owner| matches!(owner.kind.as_str(), "interface_declaration" | "annotation_type_declaration"));
    let says = |declaration: &Syn, word: &str| {
        declaration
            .children
            .iter()
            .find(|child| child.kind == "modifiers")
            .is_some_and(|modifiers| modifiers.children.iter().any(|modifier| modifier.kind == word))
    };
    let visibility = |declaration: &Syn| {
        if says(declaration, "public") {
            PUBLIC
        } else if says(declaration, "private") {
            PRIVATE
        } else if says(declaration, "protected") {
            PROTECTED
        } else {
            0
        }
    };
    let deprecated = |declaration: &Syn| {
        let annotated = declaration.children.iter().find(|child| child.kind == "modifiers").is_some_and(|modifiers| {
            modifiers.children.iter().any(|modifier| {
                matches!(modifier.kind.as_str(), "marker_annotation" | "annotation")
                    && modifier.child("name").is_some_and(|name| matches!(name.text.as_str(), "Deprecated" | "java.lang.Deprecated"))
            })
        });
        let documented = snapshot
            .comment_above(found.file, declaration)
            .is_some_and(|comment| {
                comment.text.lines().any(|line| line.trim_start().trim_start_matches("/**").trim_start_matches('*').trim_start().starts_with("@deprecated"))
            });
        if annotated || documented { DEPRECATED } else { 0 }
    };
    let generic_owner = owner.is_some_and(|owner| owner.child("type_parameters").is_some());
    match node.kind.as_str() {
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration" | "annotation_type_declaration" => {
            let kind = match node.kind.as_str() {
                "class_declaration" => CLASS,
                "interface_declaration" => INTERFACE,
                "enum_declaration" => ENUM,
                "record_declaration" => RECORD,
                _ => DECORATOR,
            };
            let mut modifiers = visibility(node) | deprecated(node);
            if in_interface {
                modifiers |= PUBLIC | STATIC;
            }
            if says(node, "static") || (owner.is_some() && kind != CLASS) {
                modifiers |= STATIC;
            }
            if says(node, "final") || kind == RECORD {
                modifiers |= READONLY;
            }
            if kind == CLASS && says(node, "abstract") {
                modifiers |= ABSTRACT;
            }
            if kind == ENUM {
                let constants_with_bodies = node
                    .child("body")
                    .is_some_and(|body| body.children.iter().any(|constant| constant.kind == "enum_constant" && constant.child("body").is_some()));
                let abstract_methods = node.child("body").is_some_and(|body| {
                    body.walk().into_iter().any(|member| {
                        member.kind == "method_declaration"
                            && member.child("body").is_none()
                            && path_to(body, member.start)
                                .iter()
                                .filter(|around| matches!(around.kind.as_str(), "class_body" | "enum_body"))
                                .count()
                                <= 1
                    })
                });
                if abstract_methods {
                    modifiers |= ABSTRACT;
                } else if !constants_with_bodies {
                    modifiers |= READONLY;
                }
            }
            if node.child("type_parameters").is_some() {
                modifiers |= GENERIC;
            }
            (kind, modifiers)
        }
        "method_declaration" | "annotation_type_element_declaration" => {
            let kind = if node.kind == "method_declaration" { METHOD } else { ANNOTATION_MEMBER };
            let mut modifiers = visibility(node) | deprecated(node);
            if in_interface && !says(node, "private") {
                modifiers |= PUBLIC;
            }
            if says(node, "static") {
                modifiers |= STATIC;
            }
            if says(node, "final") {
                modifiers |= READONLY;
            }
            if says(node, "native") {
                modifiers |= NATIVE;
            }
            let has_body = node.child("body").is_some();
            if says(node, "abstract") || (in_interface && !has_body && !says(node, "static") && !says(node, "default")) {
                modifiers |= ABSTRACT;
            }
            if node.child("type_parameters").is_some() || generic_owner {
                modifiers |= GENERIC;
            }
            (kind, modifiers)
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            // An enum's constructors are private.
            let implied = if owner.is_some_and(|owner| owner.kind == "enum_declaration") { PRIVATE } else { 0 };
            let generic = if generic_owner { GENERIC } else { 0 };
            (CLASS, visibility(node) | implied | deprecated(node) | generic)
        }
        "enum_constant" => (ENUM_MEMBER, PUBLIC | STATIC | READONLY | deprecated(node)),
        "variable_declarator" | "field_declaration" | "constant_declaration" => {
            let declaration = if node.kind == "variable_declarator" {
                path.iter()
                    .position(|around| std::ptr::eq(*around, node))
                    .and_then(|index| index.checked_sub(1))
                    .map_or(node, |index| path[index])
            } else {
                node
            };
            match declaration.kind.as_str() {
                "field_declaration" | "constant_declaration" => {
                    let mut modifiers = visibility(declaration) | deprecated(declaration);
                    if in_interface {
                        modifiers |= PUBLIC | STATIC | READONLY;
                    }
                    if says(declaration, "static") {
                        modifiers |= STATIC;
                    }
                    if says(declaration, "final") {
                        modifiers |= READONLY;
                    }
                    (PROPERTY, modifiers)
                }
                "formal_parameter" | "spread_parameter" => (PARAMETER, if says(declaration, "final") { READONLY } else { 0 }),
                _ => (VARIABLE, if says(declaration, "final") { READONLY } else { 0 }),
            }
        }
        "local_variable_declaration" | "catch_formal_parameter" | "resource" | "enhanced_for_statement" | "type_pattern" => {
            (VARIABLE, if says(node, "final") { READONLY } else { 0 })
        }
        "formal_parameter" | "spread_parameter" | "lambda_expression" | "inferred_parameters" => {
            (PARAMETER, if says(node, "final") { READONLY } else { 0 })
        }
        "type_parameter" => (TYPE_PARAMETER, 0),
        _ => (VARIABLE, 0),
    }
}
