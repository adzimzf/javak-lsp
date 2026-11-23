//! What the server asks of a parser, and nothing more: the interface is
//! phrased in positions and names, never in tree-sitter's types, so the
//! implementation (`treesitter`) is replaceable.

pub mod treesitter;

/// A zero-based position: LSP's, with the column in UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub at: Position,
    pub message: String,
}

/// One entry of a file's outline, in source order.
#[derive(Clone, Debug)]
pub struct Outline {
    pub name: String,
    pub kind: Kind,
    pub selection: Position,
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Class,
    Interface,
    Enum,
    EnumMember,
    Field,
    Constant,
    Method,
    Constructor,
}

/// A foldable region, by zero-based line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fold {
    pub first_line: u32,
    pub last_line: u32,
}

/// An identifier in a file, and what it sits in.
#[derive(Clone, Debug)]
pub struct Name {
    pub text: String,
    pub start: Position,
    pub end: Position,
    /// Whether it is part of a `package` or `import` declaration: those
    /// name things outside the file, which the server does not rename.
    pub in_package_or_import: bool,
}

/// A parsed file, as the semantic layer reads it: a tree of nodes with a
/// kind, the field its parent holds it under, its text and its span. It is
/// a snapshot and owns its text, so nothing above the parser has to know
/// what parsed it.
#[derive(Clone, Debug)]
pub struct Syn {
    pub kind: String,
    /// The field name the parent gives this child, where it gives one.
    pub field: Option<String>,
    /// The source this node spans.
    pub text: String,
    pub start: Position,
    pub end: Position,
    pub children: Vec<Syn>,
}

impl Syn {
    pub fn child(&self, field: &str) -> Option<&Syn> {
        self.children
            .iter()
            .find(|child| child.field.as_deref() == Some(field))
    }

    pub fn first(&self, kind: &str) -> Option<&Syn> {
        self.children.iter().find(|child| child.kind == kind)
    }

    pub fn all(&self, kind: &'static str) -> impl Iterator<Item = &Syn> {
        self.children.iter().filter(move |child| child.kind == kind)
    }

    pub fn covers(&self, at: Position) -> bool {
        self.start <= at && at <= self.end
    }

    /// Every node of the tree, parents before children.
    pub fn walk(&self) -> Vec<&Syn> {
        let mut nodes = vec![self];
        for child in &self.children {
            nodes.extend(child.walk());
        }
        nodes
    }
}

pub trait Syntax: Send + Sync {
    /// Where the parser cannot make sense of the text.
    fn syntax_errors(&self, text: &str) -> Vec<Finding>;
    /// The package the file declares, if it declares one.
    fn package(&self, text: &str) -> Option<String>;
    fn outline(&self, text: &str) -> Vec<Outline>;
    /// The identifier at `at`, or the one ending there.
    fn name_at(&self, text: &str, at: Position) -> Option<Name>;
    /// Every name the file declares: types, members, type parameters,
    /// parameters, locals and the variables a pattern or a `catch` binds.
    fn declared_names(&self, text: &str) -> Vec<String>;
    fn folds(&self, text: &str) -> Vec<Fold>;
    /// The file as a tree the semantic layer can read.
    fn tree(&self, text: &str) -> Syn;
}

/// The position `offset` bytes into `text`.
pub fn position_of(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position {
        line: before.matches('\n').count() as u32,
        column: before[line_start..].encode_utf16().count() as u32,
    }
}
