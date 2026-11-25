//! What a name means: the layer between the parser's trees and the
//! requests. It reads `Syn` trees only, so it holds for any parser that
//! names its nodes the way the Java grammar does.

mod checks;
pub mod compiler;

use std::collections::HashMap;
use std::sync::Arc;

use tower_lsp::lsp_types::Url;

use crate::feature::{features, MethodShape, Parameter};
use crate::indexer::kotlin::extension_key;
use crate::indexer::library::{Libraries, LibraryFile};
use crate::language::Language;
use crate::parser::{Position, Syn};

/// How many library files declaring one name are read to find it.
const LIBRARY_FILES_PER_NAME: usize = 4;

/// The workspace's source files, each parsed — the open ones as the editor
/// has them, the others as they are on disk — and the library files the
/// open ones name.
///
/// A library file nothing named when the snapshot was taken is read when
/// an answer needs it — the `PrintStream` of `System.out`, the `String` a
/// literal is — and kept for the rest of the request.
pub struct Snapshot {
    pub files: Vec<FileView>,
    libraries: Option<Arc<Libraries>>,
    /// Library files read while answering.
    read: elsa::sync::FrozenVec<Box<FileView>>,
    /// The types each file declares, by the address of its tree, listed
    /// once per snapshot: a library file is large, and asked often.
    declared_types: elsa::sync::FrozenMap<usize, Vec<TypePath>>,
    /// The members of each type declaration asked about, by the address of
    /// its node: [`members_of`] walks the whole body, and resolving one
    /// name asks for the members of every class around it.
    members: elsa::sync::FrozenMap<usize, Box<Members>>,
    /// What [`Self::type_named`] answered, by the address of the file asked
    /// from and the name: a name is resolved again for every class above
    /// every use of it. The answer is the address of the declaring file
    /// and the child indices leading to the declaration.
    types_named: elsa::sync::FrozenMap<(usize, String), Box<Option<TypeAt>>>,
    /// Every type above a type declaration, by the address of its node:
    /// finding the subtypes of one type asks it of every type there is.
    ancestors: elsa::sync::FrozenMap<usize, Vec<(Url, Position)>>,
    /// The workspace's types that name a supertype, anonymous classes
    /// among them, as the index of their file and the child indices to
    /// their node: what [`Self::subtypes_of`] looks through, listed once.
    extending: std::sync::OnceLock<Vec<(usize, Vec<u32>)>>,
}

/// Where a type is declared: the address of its file, and the child
/// indices from the file's root to the declaration.
type TypeAt = (usize, Vec<u32>);

/// Which members of a declaration are asked for.
#[derive(Clone, Copy)]
enum Wanted<'n> {
    Named(&'n str),
    Every,
}

/// A member of a declaration: its name, and the child indices that lead
/// from the declaration to the member and to its name.
type MemberPath = (String, Vec<u32>, Vec<u32>);

/// The members of a declaration, in order, and where each name is among
/// them.
struct Members {
    all: Vec<MemberPath>,
    by_name: HashMap<String, Vec<usize>>,
}

/// A type a file declares: its name, and the child indices that lead to
/// its declaration from the file's root.
type TypePath = (String, Vec<u32>);

/// One parsed file. Text and tree are shared, so a snapshot of many files
/// is cheap to take.
#[derive(Clone)]
pub struct FileView {
    pub uri: Url,
    pub language: Language,
    pub text: Arc<String>,
    pub tree: Arc<Syn>,
    /// A source of a library: read to resolve names, never searched for
    /// uses or renamed.
    pub library: bool,
}

/// A type as it is written: a name, its arguments and its dimensions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Type {
    pub name: String,
    pub arguments: Vec<Type>,
    pub dimensions: usize,
    /// `? extends ` or `? super ` in front of a type argument that is a
    /// bounded wildcard, the type being its bound; a bare `?` is named `?`.
    pub wildcard: Option<&'static str>,
    /// What the name is qualified with where it is written so
    /// (`org.demo` of `org.demo.Shape`, `Outer` of `Outer.Inner`).
    pub qualifier: Option<String>,
}

impl Type {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    pub fn written(&self) -> String {
        let arguments = if self.arguments.is_empty() {
            String::new()
        } else {
            let inner: Vec<String> = self.arguments.iter().map(Type::written).collect();
            format!("<{}>", inner.join(", "))
        };
        format!(
            "{}{}{arguments}{}",
            self.wildcard.unwrap_or_default(),
            self.name,
            "[]".repeat(self.dimensions)
        )
    }
}

/// What the type parameters of a declaration stand for here.
pub type Bindings = HashMap<String, Type>;

/// A declaration the server found, and where.
#[derive(Clone)]
pub struct Found<'a> {
    pub file: &'a FileView,
    /// The declaration itself.
    pub node: &'a Syn,
    /// The identifier a client is sent to.
    pub name: &'a Syn,
    pub bindings: Bindings,
}

impl Found<'_> {
    pub fn selection(&self) -> Position {
        self.name.start
    }
}

impl Snapshot {
    /// These files, and no library to read more from.
    pub fn new(files: Vec<FileView>) -> Self {
        Self {
            files,
            libraries: None,
            read: elsa::sync::FrozenVec::new(),
            declared_types: elsa::sync::FrozenMap::new(),
            members: elsa::sync::FrozenMap::new(),
            types_named: elsa::sync::FrozenMap::new(),
            ancestors: elsa::sync::FrozenMap::new(),
            extending: std::sync::OnceLock::new(),
        }
    }

    /// The type declared in `file` called `name`, if any: [`types_of`],
    /// listed once.
    fn declared_type<'a>(&self, file: &'a FileView, name: &str) -> Option<&'a Syn> {
        let key = Arc::as_ptr(&file.tree) as usize;
        let listed = match self.declared_types.get(&key) {
            Some(listed) => listed,
            None => {
                let mut listed = Vec::new();
                type_paths(&file.tree, &mut Vec::new(), &mut listed);
                self.declared_types.insert(key, listed)
            }
        };
        let (_, path) = listed.iter().find(|(declared, _)| declared == name)?;
        let mut node: &'a Syn = &file.tree;
        for index in path {
            node = node.children.get(*index as usize)?;
        }
        Some(node)
    }

    /// The same, reading what else it needs from `libraries`.
    /// The members of `declaration` ([`members_of`]) that `wanted` asks
    /// for, listed once per snapshot.
    fn members_named_by<'a>(&self, declaration: &'a Syn, wanted: Wanted<'_>) -> Vec<(&'a Syn, &'a Syn)> {
        let key = std::ptr::from_ref(declaration) as usize;
        let listed = match self.members.get(&key) {
            Some(listed) => listed,
            None => {
                let all = member_paths(declaration);
                let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
                for (index, (name, _, _)) in all.iter().enumerate() {
                    by_name.entry(name.clone()).or_default().push(index);
                }
                self.members.insert(key, Box::new(Members { all, by_name }))
            }
        };
        let located = |(_, member, name): &MemberPath| {
            Some((at_path(declaration, member)?, at_path(declaration, name)?))
        };
        match wanted {
            Wanted::Named(name) => listed
                .by_name
                .get(name)
                .into_iter()
                .flatten()
                .filter_map(|index| located(&listed.all[*index]))
                .collect(),
            Wanted::Every => listed.all.iter().filter_map(located).collect(),
        }
    }

    pub fn reading(mut self, libraries: Option<Arc<Libraries>>) -> Self {
        self.libraries = libraries;
        self
    }

    /// The libraries this snapshot reads from.
    pub fn libraries(&self) -> Option<Arc<Libraries>> {
        self.libraries.clone()
    }

    pub fn file(&self, uri: &Url) -> Option<&FileView> {
        self.every_file().find(|file| &file.uri == uri)
    }

    /// The files the snapshot was taken with, then the library files read
    /// since.
    fn every_file(&self) -> impl Iterator<Item = &FileView> {
        self.files.iter().chain(self.read.iter())
    }

    /// A library file, read now unless it already is.
    fn library_file(&self, file: &LibraryFile) -> Option<&FileView> {
        let loaded = self.libraries.as_ref()?.load(file)?;
        if let Some(known) = self.every_file().find(|known| known.uri == loaded.uri) {
            return Some(known);
        }
        Some(self.read.push_get(Box::new(FileView {
            uri: loaded.uri.clone(),
            language: loaded.language,
            text: loaded.text.clone(),
            tree: loaded.tree.clone(),
            library: true,
        })))
    }

    /// Library files declaring `name` in `package` (in any package when
    /// none is asked for), files of `prefer`'s language first, each read.
    fn library_files<'a>(
        &'a self,
        mut candidates: Vec<LibraryFile>,
        package: Option<&str>,
        prefer: Language,
    ) -> impl Iterator<Item = &'a FileView> {
        candidates.retain(|candidate| package.is_none_or(|wanted| candidate.package == wanted));
        candidates.sort_by_key(|candidate| (candidate.language != prefer, candidate.rank));
        candidates
            .into_iter()
            .take(LIBRARY_FILES_PER_NAME)
            .filter_map(move |candidate| self.library_file(&candidate))
    }

    /// The package a file declares.
    pub(crate) fn package(&self, file: &FileView) -> Option<String> {
        let declaration = file.tree.first("package_declaration")?;
        Some(
            declaration
                .children
                .iter()
                .find(|child| matches!(child.kind.as_str(), "scoped_identifier" | "identifier"))?
                .text
                .clone(),
        )
    }

    /// The declaration of the type called `name`, as the file at `from`
    /// sees it: its own types first, then what it imports by name, then
    /// its package, what it imports on demand, what its language imports
    /// on its own (`java.lang`, `kotlin.collections`), then anything else
    /// the workspace or its libraries declare.
    pub fn type_named<'a>(&'a self, name: &str, from: &'a FileView) -> Option<Found<'a>> {
        let key = (std::ptr::from_ref(from) as usize, name.to_owned());
        let answer = match self.types_named.get(&key) {
            Some(answer) => answer,
            None => {
                let found = self.type_named_uncached(name, from).and_then(|found| {
                    let path = indices_to(&found.file.tree, found.node)?;
                    Some((std::ptr::from_ref(found.file) as usize, path))
                });
                self.types_named.insert(key, Box::new(found))
            }
        };
        let (file, path) = answer.as_ref()?;
        let file = self
            .every_file()
            .find(|known| std::ptr::from_ref(*known) as usize == *file)?;
        self.declared(file, at_path(&file.tree, path)?)
    }

    fn type_named_uncached<'a>(&'a self, name: &str, from: &'a FileView) -> Option<Found<'a>> {
        if let Some((qualifier, last)) = name.rsplit_once('.') {
            return self.type_qualified(qualifier, last, from);
        }
        if let Some(declaration) = self.declared_type(from, name) {
            return self.declared(from, declaration);
        }
        let imported = imports(from)
            .into_iter()
            .find(|import| !import.wildcard && import.last_segment() == name);
        let prefer = from.language;
        if let Some(import) = imported {
            // A single-type import decides what the name is: where it names
            // nothing, the name is nothing, not a type of the same name in
            // some other package.
            return self
                .type_in_package(name, Some(import.qualifier()), prefer)
                .or_else(|| self.type_qualified_without_fallback(import.qualifier(), name, from));
        }
        let package = self.package(from);
        let own = package.as_deref().unwrap_or("");
        if let Some(found) = self.type_in_package(name, Some(own), prefer) {
            return Some(found);
        }
        let on_demand = imports(from).into_iter().filter(|import| import.wildcard);
        for import in on_demand {
            if let Some(found) = self.type_in_package(name, Some(&import.name), prefer) {
                return Some(found);
            }
        }
        for implicit in features(prefer).implicit_packages() {
            if let Some(found) = self.type_in_package(name, Some(implicit), prefer) {
                return Some(found);
            }
        }
        self.type_in_package(name, None, prefer)
    }

    /// `com.demo.shapes.Circle` is the `Circle` of that package;
    /// `Outer.Inner` is the `Inner` declared inside `Outer`.
    fn type_qualified<'a>(
        &'a self,
        qualifier: &str,
        name: &str,
        from: &'a FileView,
    ) -> Option<Found<'a>> {
        if let Some(found) = self.type_in_package(name, Some(qualifier), from.language) {
            return Some(found);
        }
        let outer = self.type_named(qualifier, from)?;
        let nested = outer
            .node
            .walk()
            .into_iter()
            .filter(|node| is_type_declaration(node))
            .find(|node| node.child("name").is_some_and(|named| named.text == name))?;
        self.declared(outer.file, nested)
    }

    /// `Outer.Inner` of an import, where `Outer` is itself named in full.
    fn type_qualified_without_fallback<'a>(
        &'a self,
        qualifier: &str,
        name: &str,
        from: &'a FileView,
    ) -> Option<Found<'a>> {
        let (package, outer) = qualifier.rsplit_once('.').unwrap_or(("", qualifier));
        let outer = self.type_in_package(outer, Some(package), from.language)?;
        let nested = outer
            .node
            .walk()
            .into_iter()
            .filter(|node| is_type_declaration(node))
            .find(|node| node.child("name").is_some_and(|named| named.text == name))?;
        self.declared(outer.file, nested)
    }

    /// The type called `name` declared in `package` (`""` for the unnamed
    /// one), or in any package when none is asked for: in the snapshot's
    /// files, else in a library file of `prefer`'s language, else of the
    /// other.
    fn type_in_package<'a>(
        &'a self,
        name: &str,
        package: Option<&str>,
        prefer: Language,
    ) -> Option<Found<'a>> {
        let declaring = |file: &'a FileView| {
            let declaration = self.declared_type(file, name)?;
            self.declared(file, declaration)
        };
        let in_package = |file: &&FileView| {
            package.is_none_or(|wanted| self.package(file).as_deref().unwrap_or("") == wanted)
        };
        if let Some(found) = self.every_file().filter(in_package).find_map(declaring) {
            return Some(found);
        }
        let candidates = self.libraries.as_ref()?.declaring(name);
        self.library_files(candidates, package, prefer)
            .filter(in_package)
            .find_map(declaring)
    }

    fn declared<'a>(&self, file: &'a FileView, node: &'a Syn) -> Option<Found<'a>> {
        Some(Found {
            file,
            node,
            name: node.child("name")?,
            bindings: Bindings::new(),
        })
    }

    /// The supertypes of a declaration, resolved.
    fn supertypes<'a>(&'a self, of: &Found<'a>) -> Vec<Found<'a>> {
        let mut found = Vec::new();
        if of.node.kind == "object_creation_expression" {
            let created = of.node.child("type").map(type_of_node);
            if let Some(written) = created {
                found.extend(self.of_type(&written, of.file));
            }
            return found;
        }
        for list in of.node.children.iter().filter(|child| {
            matches!(
                child.kind.as_str(),
                "superclass" | "super_interfaces" | "extends_interfaces" | "interface_type_list"
            )
        }) {
            // Each type listed, whole: `Outer.Base<T>` is one supertype, not
            // `Outer`, `Base` and `T`.
            for named in outermost_types(list) {
                let written = substituted(type_of_node(named), &of.bindings);
                if let Some(supertype) = self.of_type(&written, of.file) {
                    found.push(supertype);
                }
            }
        }
        found
    }

    /// A member called `name` of a declaration or of anything above it,
    /// with the arguments a call passes it, and the file the call is written
    /// in, where there is a choice.
    pub fn member<'a>(
        &'a self,
        of: &Found<'a>,
        name: &str,
        call: Option<(&Syn, &FileView)>,
    ) -> Option<Found<'a>> {
        let mut candidates = Vec::new();
        self.members_named(of, name, &mut candidates, &mut 0);
        // A call means a method, not a field of the same name.
        if call.is_some() && candidates.iter().any(|candidate| is_callable(candidate.node)) {
            candidates.retain(|candidate| is_callable(candidate.node));
        }
        if candidates.len() > 1
            && let Some((arguments, from)) = call
        {
            // The arguments are typed where the call is written, not where
            // the member is declared.
            if let Some(chosen) = self.overload(&candidates, Some(arguments), from) {
                return Some(chosen);
            }
        }
        candidates.into_iter().next()
    }

    /// Every member of this type and its supertypes called `name`,
    /// nearest first.
    fn members_named<'a>(
        &'a self,
        of: &Found<'a>,
        name: &str,
        found: &mut Vec<Found<'a>>,
        depth: &mut usize,
    ) {
        self.members_where(of, Wanted::Named(name), found, depth);
    }

    /// Every member of this type and its supertypes that `wanted` asks
    /// for, nearest first.
    fn members_where<'a>(
        &'a self,
        of: &Found<'a>,
        wanted: Wanted<'_>,
        found: &mut Vec<Found<'a>>,
        depth: &mut usize,
    ) {
        if *depth > 8 {
            return;
        }
        *depth += 1;
        for (member, identifier) in self.members_named_by(of.node, wanted) {
            found.push(Found {
                file: of.file,
                node: member,
                name: identifier,
                bindings: of.bindings.clone(),
            });
        }
        for supertype in self.supertypes(of) {
            self.members_where(&supertype, wanted, found, depth);
        }
    }

    /// The overload a call picks: the one with as many parameters as the
    /// call has arguments, and whose parameter types the arguments are —
    /// else the one whose parameter types the arguments' types extend
    /// (`println(Any?)` for a `String`).
    fn overload<'a>(
        &'a self,
        candidates: &[Found<'a>],
        arguments: Option<&Syn>,
        from: &FileView,
    ) -> Option<Found<'a>> {
        let arguments = arguments?;
        let passed: Vec<&Syn> = arguments
            .children
            .iter()
            .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ","))
            .collect();
        let same_count: Vec<&Found<'a>> = candidates
            .iter()
            .filter(|candidate| parameters_of(candidate.node).len() == passed.len())
            .collect();
        // Each argument is typed once, however many overloads there are.
        let given: Vec<std::cell::OnceCell<Option<Type>>> =
            passed.iter().map(|_| std::cell::OnceCell::new()).collect();
        let given_at = |index: usize| {
            given[index]
                .get_or_init(|| self.type_of_expression(passed[index], from))
                .as_ref()
        };
        let accepted: Vec<std::cell::OnceCell<Vec<String>>> =
            passed.iter().map(|_| std::cell::OnceCell::new()).collect();
        let accepted_at = |index: usize| {
            accepted[index].get_or_init(|| {
                let mut names = vec!["Any".to_owned(), "Object".to_owned()];
                let family = given_at(index)
                    .and_then(|given| self.of_type(given, from))
                    .map(|declaration| self.with_supertypes(&declaration))
                    .unwrap_or_default();
                names.extend(family.iter().map(|member| member.name.text.clone()));
                names
            })
        };
        let wanted = |candidate: &Found<'_>| -> Vec<Option<Type>> {
            parameters_of(candidate.node)
                .iter()
                .map(|parameter| parameter.child("type").map(type_of_node))
                .collect()
        };
        let exact = same_count.iter().find(|candidate| {
            wanted(candidate).iter().enumerate().all(|(index, wanted)| {
                match (wanted, given_at(index)) {
                    (Some(wanted), Some(given)) => wanted.name == given.name,
                    _ => false,
                }
            })
        });
        let wider = || {
            same_count.iter().find(|candidate| {
                wanted(candidate).iter().enumerate().all(|(index, wanted)| {
                    let Some(wanted) = wanted else {
                        return false;
                    };
                    // A lambda or a method reference is a functional
                    // interface: any interface fits it, a class does not.
                    if matches!(passed[index].kind.as_str(), "lambda_expression" | "method_reference") {
                        return self
                            .of_type(wanted, candidate.file)
                            .is_some_and(|declaration| declaration.node.kind == "interface_declaration");
                    }
                    let exactly = given_at(index).is_some_and(|given| given.name == wanted.name);
                    exactly || accepted_at(index).contains(&wanted.name)
                })
            })
        };
        let fits = exact.or_else(wider);
        // Short of a fit, an overload taking as many arguments as are
        // passed, before one that cannot take them.
        let chosen = fits.or_else(|| same_count.first())?;
        Some(Found {
            file: chosen.file,
            node: chosen.node,
            name: chosen.name,
            bindings: chosen.bindings.clone(),
        })
    }

    /// The type an expression has, as far as the sources say.
    pub fn type_of_expression(&self, expression: &Syn, from: &FileView) -> Option<Type> {
        let at = expression.start;
        match expression.kind.as_str() {
            "identifier" => {
                let declaration = self.name_in_scope(&expression.text, from, at)?;
                self.type_of_declaration(&declaration)
            }
            "this" => Some(Type::named(&enclosing_type(&from.tree, at)?.0)),
            "field_access" => {
                let object = expression.child("object")?;
                let field = expression.child("field")?;
                if field.kind == "this" {
                    return Some(type_of_node(object));
                }
                let receiver = self.type_of_expression(object, from)?;
                let owner = self.of_type(&receiver, from)?;
                let member = self.member(&owner, &field.text, None)?;
                self.type_of_declaration(&member)
            }
            "method_invocation" => {
                let method = self.invoked(expression, from)?;
                if is_type_declaration(method.node) || is_constructor(method.node) {
                    return Some(Type::named(&method.name.text));
                }
                let returned = self.type_of_declaration(&method)?;
                Some(
                    self.inferred_from_arguments(&method, &returned, expression, from)
                        .unwrap_or(returned),
                )
            }
            "object_creation_expression" => expression.child("type").map(type_of_node),
            "cast_expression" => expression.child("type").map(type_of_node),
            "parenthesized_expression" => {
                let inner = first_expression(expression)?;
                self.type_of_expression(inner, from)
            }
            "ternary_expression" => {
                let consequence = expression.child("consequence")?;
                self.type_of_expression(consequence, from)
            }
            "array_access" => {
                let array = expression.child("array")?;
                let mut element = self.type_of_expression(array, from)?;
                element.dimensions = element.dimensions.saturating_sub(1);
                Some(element)
            }
            "string_literal" => Some(Type::named("String")),
            "decimal_integer_literal" | "hex_integer_literal" | "binary_integer_literal"
            | "octal_integer_literal" | "decimal_floating_point_literal" | "character_literal"
            | "true" | "false" => Some(Type::named(features(from.language).literal_type(
                &expression.kind,
                &expression.text,
            ))),
            "binary_expression" => {
                let left = expression.child("left")?;
                self.type_of_expression(left, from)
            }
            "super" => {
                let (name, _) = enclosing_type(&from.tree, at)?;
                let declaration = self.type_named(&name, from)?;
                let supertype = self.supertypes(&declaration).into_iter().next()?;
                Some(Type::named(&supertype.name.text))
            }
            _ => None,
        }
    }

    /// What a generic method returns once its type parameters are bound
    /// by the arguments of the call: `<T> T unwrap(T value)` called with a
    /// `Dog` returns a `Dog`, `listOf("a")` a `List<String>`.
    fn inferred_from_arguments(
        &self,
        method: &Found<'_>,
        returned: &Type,
        call: &Syn,
        from: &FileView,
    ) -> Option<Type> {
        let introduced = type_parameter_names(method.node);
        if !mentions(returned, &introduced) {
            return None;
        }
        let passed: Vec<&Syn> = call
            .child("arguments")?
            .children
            .iter()
            .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ","))
            .collect();
        let mut bindings = Bindings::new();
        for (parameter, argument) in parameters_of(method.node).iter().zip(&passed) {
            let Some(written) = parameter.child("type").map(type_of_node) else {
                continue;
            };
            if !mentions(&written, &introduced) {
                continue;
            }
            if let Some(given) = self.type_of_expression(argument, from) {
                unify(&written, &given, &introduced, &mut bindings);
            }
        }
        if bindings.is_empty() {
            return None;
        }
        Some(substituted(returned.clone(), &bindings))
    }

    /// The type of what a declaration declares: a field's or a local's
    /// written type, a method's return type, a record component's type,
    /// with the type parameters of its class filled in.
    pub fn type_of_declaration(&self, declaration: &Found<'_>) -> Option<Type> {
        let node = declaration.node;
        let written = match node.kind.as_str() {
            "method_declaration" => node.child("type").map(type_of_node),
            "catch_formal_parameter" => node
                .child("type")
                .or_else(|| node.first("catch_type").and_then(|caught| caught.children.first()))
                .map(type_of_node),
            "formal_parameter" | "spread_parameter" | "field_declaration"
            | "local_variable_declaration" | "resource" | "type_pattern" => {
                node.child("type").map(type_of_node)
            }
            "instanceof_expression" | "lambda_expression" => self.inferred(declaration),
            "enum_constant" => Some(Type::named(&enclosing_type(&declaration.file.tree, node.start)?.0)),
            "class_declaration" | "record_declaration" | "interface_declaration"
            | "enum_declaration" => Some(Type::named(&node.child("name")?.text)),
            _ => node.child("type").map(type_of_node),
        }?;
        let written = if written.name == "var" {
            self.inferred(declaration).unwrap_or(written)
        } else if declaration.file.language == Language::Java {
            // Read where it is written: `Node` in a file importing
            // `org.demo.Node` is that one, wherever the declaration is used.
            let mut variables = type_parameter_names(node);
            for enclosing in path_to(&declaration.file.tree, node.start) {
                variables.extend(type_parameter_names(enclosing));
            }
            self.anchored(written, declaration.file, &variables)
        } else {
            written
        };
        Some(substituted(written, &declaration.bindings))
    }

    /// `written` with the qualifier of the type each name means in `file`,
    /// type variables left as they are.
    fn anchored(&self, mut written: Type, file: &FileView, variables: &[String]) -> Type {
        written.arguments = std::mem::take(&mut written.arguments)
            .into_iter()
            .map(|argument| self.anchored(argument, file, variables))
            .collect();
        if written.qualifier.is_some() || written.name == "?" || variables.contains(&written.name) {
            return written;
        }
        if let Some(found) = self.type_named(&written.name, file) {
            let qualified = self.qualified_type_name(found.file, found.node);
            written.qualifier = qualified.rsplit_once('.').map(|(qualifier, _)| qualifier.to_owned());
        }
        written
    }

    /// What a `var` is: the type of the expression it is given. An
    /// `instanceof` pattern is the type it tests for, and a lambda
    /// parameter the type its functional interface gives it.
    fn inferred(&self, declaration: &Found<'_>) -> Option<Type> {
        if declaration.node.kind == "instanceof_expression" {
            return declaration.node.child("right").map(type_of_node);
        }
        if declaration.node.kind == "lambda_expression" {
            return self.lambda_parameter_type(declaration);
        }
        let declarator = declaration
            .node
            .all("variable_declarator")
            .find(|declarator| {
                declarator
                    .child("name")
                    .is_some_and(|name| name.text == declaration.name.text)
            })?;
        let value = declarator.child("value")?;
        self.type_of_expression(value, declaration.file)
    }

    /// The type the functional interface of a lambda gives the parameter
    /// at `declaration`: the single method's parameter in that position.
    fn lambda_parameter_type(&self, declaration: &Found<'_>) -> Option<Type> {
        let lambda = declaration.node;
        let parameters = lambda.child("parameters")?;
        let names: Vec<&Syn> = parameters
            .walk()
            .into_iter()
            .filter(|node| is_identifier(node))
            .collect();
        let at = names
            .iter()
            .position(|named| named.start == declaration.name.start)?;
        let expected = self.expected_type_of(lambda, declaration.file)?;
        let interface = self.of_type(&expected, declaration.file)?;
        let (method, _) = members_of(interface.node)
            .into_iter()
            .find(|(member, _)| member.kind == "method_declaration")?;
        let parameter = parameters_of(method).get(at).copied()?;
        let written = parameter.child("type").map(type_of_node)?;
        Some(substituted(written, &interface.bindings))
    }

    /// The method a lambda implements: the abstract method of the
    /// functional interface it is wanted as (`run` of a `Runnable`).
    pub fn lambda_method<'a>(&'a self, lambda: &Syn, file: &'a FileView) -> Option<Found<'a>> {
        let expected = self.expected_type_of(lambda, file)?;
        let interface = self.of_type(&expected, file)?;
        for owner in self.with_supertypes(&interface) {
            let abstract_method = members_of(owner.node).into_iter().find(|(member, _)| {
                let modifiers = member.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
                member.kind == "method_declaration"
                    && member.child("body").is_none()
                    && !modifiers.split_whitespace().any(|word| matches!(word, "static" | "default" | "private"))
            });
            if let Some((member, name)) = abstract_method {
                return Some(Found {
                    file: owner.file,
                    node: member,
                    name,
                    bindings: owner.bindings.clone(),
                });
            }
        }
        None
    }

    /// The type an expression is wanted as: what the method it is returned
    /// from returns, or what the variable it is assigned to is declared as.
    fn expected_type_of(&self, expression: &Syn, file: &FileView) -> Option<Type> {
        let path = path_to(&file.tree, expression.start);
        for (index, ancestor) in path.iter().enumerate().rev() {
            match ancestor.kind.as_str() {
                // An argument is wanted as the parameter it is passed to.
                "argument_list" => {
                    let arguments: Vec<&Syn> = ancestor
                        .children
                        .iter()
                        .filter(|child| !matches!(child.kind.as_str(), "(" | ")" | ","))
                        .collect();
                    let at = arguments
                        .iter()
                        .position(|argument| argument.start == expression.start && argument.end == expression.end)?;
                    let call = *path.get(index.checked_sub(1)?)?;
                    let callee = match call.kind.as_str() {
                        "method_invocation" => self.invoked(call, file),
                        "object_creation_expression" => self.created(call, file),
                        "explicit_constructor_invocation" => self.chained(call, file),
                        _ => None,
                    }?;
                    let parameter = *parameters_of(callee.node).get(at)?;
                    let written = parameter.child("type").map(type_of_node)?;
                    let mut variables = type_parameter_names(callee.node);
                    for enclosing in path_to(&callee.file.tree, callee.node.start) {
                        variables.extend(type_parameter_names(enclosing));
                    }
                    let written = self.anchored(written, callee.file, &variables);
                    return Some(substituted(written, &callee.bindings));
                }
                "return_statement" => {
                    let method = path
                        .iter()
                        .rev()
                        .find(|node| node.kind == "method_declaration")?;
                    return method.child("type").map(type_of_node);
                }
                "variable_declarator" => {
                    let declaration = path
                        .iter()
                        .rev()
                        .find(|node| {
                            matches!(
                                node.kind.as_str(),
                                "local_variable_declaration" | "field_declaration"
                            )
                        })?;
                    return declaration.child("type").map(type_of_node);
                }
                _ => {}
            }
        }
        None
    }

    /// The declaration of a type, by the type as written.
    pub fn of_type<'a>(&'a self, written: &Type, from: &'a FileView) -> Option<Found<'a>> {
        let qualified = written
            .qualifier
            .as_ref()
            .and_then(|qualifier| self.type_named(&format!("{qualifier}.{}", written.name), from));
        let mut declaration = match qualified {
            Some(declaration) => declaration,
            None => self.type_named(&written.name, from)?,
        };
        declaration.bindings = bound(declaration.node, &written.arguments);
        Some(declaration)
    }

    /// The type the cursor's own class is, as a declaration.
    pub fn enclosing<'a>(&'a self, file: &'a FileView, at: Position) -> Option<Found<'a>> {
        let path = path_to(&file.tree, at);
        let innermost = path
            .iter()
            .rev()
            .find(|node| is_type_declaration(node) || is_anonymous_class(node))?;
        if is_anonymous_class(innermost) {
            return Some(Found {
                file,
                node: innermost,
                name: innermost.child("type")?,
                bindings: Bindings::new(),
            });
        }
        self.type_named(&innermost.child("name")?.text, file)
    }

    /// What `name` is where the cursor is: the nearest local, parameter or
    /// pattern variable declared before it, else a field of the class or
    /// of a class above it, else a type.
    pub fn name_in_scope<'a>(
        &'a self,
        name: &str,
        file: &'a FileView,
        at: Position,
    ) -> Option<Found<'a>> {
        let path = path_to(&file.tree, at);
        for ancestor in path.iter().rev() {
            for (declaration, identifier) in locals_of(ancestor) {
                let before_the_cursor = identifier.start <= at;
                if identifier.text == name && before_the_cursor {
                    return Some(Found {
                        file,
                        node: declaration,
                        name: identifier,
                        bindings: Bindings::new(),
                    });
                }
            }
        }
        for ancestor in path.iter().rev() {
            let parameters = ancestor
                .child("type_parameters")
                .map(|parameters| parameters.all("type_parameter").collect::<Vec<_>>())
                .unwrap_or_default();
            for parameter in parameters {
                let named = parameter.first("type_identifier");
                if let Some(named) = named.filter(|named| named.text == name) {
                    return Some(Found {
                        file,
                        node: parameter,
                        name: named,
                        bindings: Bindings::new(),
                    });
                }
            }
        }
        for ancestor in path.iter().rev() {
            let declares_a_type = matches!(
                ancestor.kind.as_str(),
                "class_declaration"
                    | "record_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
            );
            if !declares_a_type {
                continue;
            }
            let of = self.declared(file, ancestor)?;
            // A constructor is called by `new` or `this(…)`, never named:
            // the class's own name there is the class.
            let member = self
                .member(&of, name, None)
                .filter(|member| !is_constructor(member.node));
            if let Some(member) = member {
                return Some(member);
            }
        }
        if let Some(found) = self.statically_imported(name, file) {
            return Some(found);
        }
        self.type_named(name, file)
    }

    /// A member a `import static` names, by the type it is imported from.
    fn statically_imported<'a>(&'a self, name: &str, from: &'a FileView) -> Option<Found<'a>> {
        imports(from)
            .into_iter()
            .filter(|import| import.is_static)
            .find_map(|import| {
                let (owner, member) = if import.wildcard {
                    (import.name.clone(), name.to_owned())
                } else {
                    (import.qualifier().to_owned(), import.last_segment().to_owned())
                };
                if member != name {
                    return None;
                }
                let declaration = self.type_named(&owner, from)?;
                self.member(&declaration, name, None)
            })
    }

    /// What a call calls. With a receiver, a member of its type or, where
    /// the language has them, an extension of it (`list.first()`); without
    /// one, a member of the class around it, else a top-level function
    /// (`println(…)`), else — where the language calls a class to create
    /// one — that class's constructor.
    pub fn invoked<'a>(&'a self, call: &'a Syn, file: &'a FileView) -> Option<Found<'a>> {
        let name = call.child("name")?;
        let arguments = call.child("arguments");
        let passed = arguments.map(|at| (at, file));
        if let Some(object) = call.child("object") {
            let receiver = self.type_of_expression(object, file)?;
            let member = self
                .of_type(&receiver, file)
                .and_then(|owner| self.member(&owner, &name.text, passed));
            return member.or_else(|| self.extension(&receiver, &name.text, arguments, file));
        }
        let member = self
            .enclosing(file, name.start)
            .and_then(|owner| self.member(&owner, &name.text, passed));
        if member.is_some() || !features(file.language).has_top_level_functions() {
            return member;
        }
        let candidates = self.functions_named(&name.text, file, None);
        if let Some(function) = self.chosen(candidates, arguments, file) {
            return Some(function);
        }
        let class = self.type_named(&name.text, file)?;
        let constructor = self.member(&class, &class.name.text, passed);
        constructor.or(Some(class))
    }

    /// The constructor `new Circle(…)` calls, else — for a class that
    /// declares none — the class itself, standing for its default one.
    pub fn created<'a>(&'a self, creation: &'a Syn, file: &'a FileView) -> Option<Found<'a>> {
        let created = creation.child("type")?;
        let declaration = self.type_named(&type_of_node(created).name, file)?;
        let arguments = creation.child("arguments").map(|at| (at, file));
        let constructor = self.member(&declaration, &declaration.name.text, arguments);
        constructor.or(Some(declaration))
    }

    /// The constructor `super(…)` or `this(…)` calls, else the class it
    /// would be in.
    pub fn chained<'a>(&'a self, invocation: &'a Syn, file: &'a FileView) -> Option<Found<'a>> {
        let enclosing = self.enclosing(file, invocation.start)?;
        let declaration = if invocation.child("constructor")?.kind == "super" {
            self.supertype_of(&enclosing)?
        } else {
            enclosing
        };
        let arguments = invocation.child("arguments").map(|at| (at, file));
        let constructor = self.member(&declaration, &declaration.name.text, arguments);
        constructor.or(Some(declaration))
    }

    /// The one of several declarations a call's arguments pick, else the
    /// first.
    fn chosen<'a>(
        &'a self,
        candidates: Vec<Found<'a>>,
        arguments: Option<&Syn>,
        from: &FileView,
    ) -> Option<Found<'a>> {
        let mut candidates = candidates;
        // A Kotlin `expect` declaration stands for the platform's `actual`.
        candidates.sort_by_key(|candidate| is_expected(candidate.node));
        if candidates.len() > 1
            && let Some(chosen) = self.overload(&candidates, arguments, from)
        {
            return Some(chosen);
        }
        candidates.into_iter().next()
    }

    /// The extension called `name` a value of type `receiver` has: one
    /// declared on its type before one declared on a supertype, and that
    /// before one declared on any type (`fun <T> T.also(…)`). Its type
    /// parameters are bound by the receiver: `first` of a `List<String>`
    /// returns a `String`.
    fn extension<'a>(
        &'a self,
        receiver: &Type,
        name: &str,
        arguments: Option<&Syn>,
        from: &'a FileView,
    ) -> Option<Found<'a>> {
        if !features(from.language).has_top_level_functions() {
            return None;
        }
        let family = self
            .of_type(receiver, from)
            .map(|owner| self.with_supertypes(&owner))
            .unwrap_or_default();
        let anywhere = family.len() + 1;
        let mut receivers: Vec<String> = family.iter().map(|member| member.name.text.clone()).collect();
        receivers.extend([receiver.name.clone(), "Any".to_owned(), "*".to_owned()]);
        let mut ranked: Vec<(usize, Found<'a>)> = Vec::new();
        for candidate in self.functions_named(name, from, Some(&receivers)) {
            let Some(written) = candidate.node.child("receiver").map(type_of_node) else {
                continue;
            };
            let introduced = type_parameter_names(candidate.node);
            let mut bindings = Bindings::new();
            let distance = if introduced.contains(&written.name) {
                bindings.insert(written.name.clone(), receiver.clone());
                anywhere
            } else if written.name == "Any" {
                anywhere
            } else if let Some(at) = family
                .iter()
                .position(|member| member.name.text == written.name)
            {
                let declared = type_parameter_names(family[at].node);
                for (index, argument) in written.arguments.iter().enumerate() {
                    let bound = declared
                        .get(index)
                        .and_then(|parameter| family[at].bindings.get(parameter));
                    if let Some(bound) = bound.filter(|_| introduced.contains(&argument.name)) {
                        bindings.insert(argument.name.clone(), bound.clone());
                    }
                }
                at
            } else if family.is_empty() && written.name == receiver.name {
                0
            } else {
                continue;
            };
            ranked.push((distance, Found { bindings, ..candidate }));
        }
        let nearest = ranked.iter().map(|(distance, _)| *distance).min()?;
        let candidates = ranked
            .into_iter()
            .filter(|(distance, _)| *distance == nearest)
            .map(|(_, found)| found)
            .collect();
        self.chosen(candidates, arguments, from)
    }

    /// The top-level functions called `name` the file at `from` sees: its
    /// own, then those of its package, of what it imports and of its
    /// language's default imports, in the workspace and in its libraries.
    /// With `receivers`, the extensions of those types (`*` for one
    /// declared on its own type parameter), else the functions that extend
    /// nothing.
    fn functions_named<'a>(
        &'a self,
        name: &str,
        from: &'a FileView,
        receivers: Option<&[String]>,
    ) -> Vec<Found<'a>> {
        let extensions = receivers.is_some();
        let packages = self.packages_seen(from, Some(name));
        let mut found = Vec::new();
        let mut read: Vec<&Url> = Vec::new();
        let mut collect = |file: &'a FileView, found: &mut Vec<Found<'a>>| {
            if read.contains(&&file.uri) {
                return;
            }
            read.push(&file.uri);
            for function in top_level_functions(file) {
                if function.child("receiver").is_some() != extensions {
                    continue;
                }
                if let Some(named) = function.child("name").filter(|named| named.text == name) {
                    found.push(Found {
                        file,
                        node: function,
                        name: named,
                        bindings: Bindings::new(),
                    });
                }
            }
        };
        collect(from, &mut found);
        for file in self.every_file() {
            let package = self.package(file).unwrap_or_default();
            if file.language == from.language && packages.contains(&package) {
                collect(file, &mut found);
            }
        }
        if let Some(libraries) = &self.libraries {
            let candidates: Vec<LibraryFile> = match receivers {
                Some(receivers) => receivers
                    .iter()
                    .flat_map(|receiver| libraries.declaring_function(&extension_key(receiver, name)))
                    .collect(),
                None => libraries.declaring_function(name),
            };
            for package in &packages {
                let wanted: Vec<LibraryFile> = candidates
                    .iter()
                    .filter(|candidate| candidate.package == *package)
                    .cloned()
                    .collect();
                for candidate in wanted {
                    if let Some(file) = self.library_file(&candidate) {
                        collect(file, &mut found);
                    }
                }
            }
        }
        found
    }
}

impl Snapshot {
    /// The packages whose top-level declarations the file at `from` sees
    /// without qualifying them: its own, those it imports on demand, its
    /// language's default imports and, for `name`, the package an import
    /// of that name imports it from. Every import counts when no name is
    /// asked for.
    fn packages_seen(&self, from: &FileView, name: Option<&str>) -> Vec<String> {
        let mut packages = vec![self.package(from).unwrap_or_default()];
        for import in imports(from) {
            let names_it = name.is_none_or(|name| import.last_segment() == name);
            if import.wildcard {
                packages.push(import.name.clone());
            } else if names_it {
                packages.push(import.qualifier().to_owned());
            }
        }
        packages.extend(
            features(from.language)
                .implicit_packages()
                .iter()
                .map(|package| (*package).to_owned()),
        );
        packages
    }

    /// The names of the extensions starting with `prefix` a value of type
    /// `receiver` has, where the file at `from` sees them: `first`, `map`
    /// of a `List`.
    pub fn extension_names(&self, receiver: &Type, prefix: &str, from: &FileView) -> Vec<String> {
        if !features(from.language).has_top_level_functions() {
            return Vec::new();
        }
        let mut receivers: Vec<String> = self
            .of_type(receiver, from)
            .map(|owner| {
                self.with_supertypes(&owner)
                    .iter()
                    .map(|member| member.name.text.clone())
                    .collect()
            })
            .unwrap_or_default();
        receivers.extend([receiver.name.clone(), "Any".to_owned(), "*".to_owned()]);
        let packages = self.packages_seen(from, None);
        let mut names: Vec<String> = Vec::new();
        for file in self.every_file() {
            let seen = file.uri == from.uri
                || packages.contains(&self.package(file).unwrap_or_default());
            if !seen || file.library {
                continue;
            }
            for function in top_level_functions(file) {
                let (Some(written), Some(named)) = (function.child("receiver"), function.child("name"))
                else {
                    continue;
                };
                let written = type_of_node(written);
                let key = if type_parameter_names(function).contains(&written.name) {
                    "*".to_owned()
                } else {
                    written.name
                };
                if receivers.contains(&key) && named.text.starts_with(prefix) && !names.contains(&named.text) {
                    names.push(named.text.clone());
                }
            }
        }
        if let Some(libraries) = &self.libraries {
            for receiver in &receivers {
                for (key, files) in libraries.functions_starting(&extension_key(receiver, prefix)) {
                    let name = key.split_once('.').map_or(key.as_str(), |(_, name)| name);
                    let seen = files.iter().any(|file| packages.contains(&file.package));
                    if seen && !names.iter().any(|known| known == name) {
                        names.push(name.to_owned());
                    }
                }
            }
        }
        names
    }

    /// The names of the top-level functions starting with `prefix` the
    /// file at `from` sees in its libraries: `println` of `prin`. Nothing
    /// is offered before a name is begun.
    pub fn function_names(&self, prefix: &str, from: &FileView) -> Vec<String> {
        let libraries = self.libraries.as_ref();
        if prefix.is_empty() || !features(from.language).has_top_level_functions() {
            return Vec::new();
        }
        let packages = self.packages_seen(from, None);
        let mut names: Vec<String> = Vec::new();
        for file in self.every_file().filter(|file| !file.library) {
            if !packages.contains(&self.package(file).unwrap_or_default()) {
                continue;
            }
            for function in top_level_functions(file).filter(|function| function.child("receiver").is_none()) {
                if let Some(named) = function.child("name").filter(|named| named.text.starts_with(prefix))
                    && !names.contains(&named.text)
                {
                    names.push(named.text.clone());
                }
            }
        }
        for (key, files) in libraries.map(|libraries| libraries.functions_starting(prefix)).unwrap_or_default() {
            let seen = files.iter().any(|file| packages.contains(&file.package));
            if !key.contains('.') && seen && !names.contains(&key) {
                names.push(key);
            }
        }
        names
    }
}

/// Whether a declaration is a Kotlin `expect` one, whose body is its
/// platform's `actual` declaration.
fn is_expected(declaration: &Syn) -> bool {
    declaration
        .children
        .iter()
        .filter(|child| child.kind == "modifiers")
        .any(|modifiers| modifiers.text.split_whitespace().any(|word| word == "expect"))
}

/// The functions declared at the top of a file, outside any type.
fn top_level_functions(file: &FileView) -> impl Iterator<Item = &Syn> {
    file.tree
        .children
        .iter()
        .filter(|node| node.kind == "method_declaration")
}

/// Whether a type is or has among its arguments one of `parameters`.
fn mentions(written: &Type, parameters: &[String]) -> bool {
    parameters.contains(&written.name)
        || written
            .arguments
            .iter()
            .any(|argument| mentions(argument, parameters))
}

/// Binds the `parameters` a written type has to what stands in their place
/// in the type given for it: `List<T>` given `List<String>` binds `T`.
fn unify(written: &Type, given: &Type, parameters: &[String], bindings: &mut Bindings) {
    if parameters.contains(&written.name) {
        if given.dimensions >= written.dimensions && !bindings.contains_key(&written.name) {
            let mut bound = given.clone();
            bound.dimensions -= written.dimensions;
            bindings.insert(written.name.clone(), bound);
        }
        return;
    }
    if written.name == given.name {
        for (written, given) in written.arguments.iter().zip(&given.arguments) {
            unify(written, given, parameters, bindings);
        }
    }
}

/// `T` of `Box<T>` bound to `String` of `Box<String>`.
fn bound(declaration: &Syn, arguments: &[Type]) -> Bindings {
    let parameters = declaration
        .child("type_parameters")
        .map(|parameters| {
            parameters
                .all("type_parameter")
                .filter_map(|parameter| parameter.first("type_identifier"))
                .map(|name| name.text.clone())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    parameters
        .into_iter()
        .zip(arguments.iter().cloned())
        .collect()
}

fn substituted(written: Type, bindings: &Bindings) -> Type {
    if let Some(bound) = bindings.get(&written.name) {
        let mut bound = bound.clone();
        bound.dimensions += written.dimensions;
        bound.wildcard = bound.wildcard.or(written.wildcard);
        return bound;
    }
    Type {
        name: written.name,
        arguments: written
            .arguments
            .into_iter()
            .map(|argument| substituted(argument, bindings))
            .collect(),
        dimensions: written.dimensions,
        wildcard: written.wildcard,
        qualifier: written.qualifier,
    }
}

/// The type a type node writes.
pub fn type_of_node(node: &Syn) -> Type {
    match node.kind.as_str() {
        "generic_type" => {
            let name = node
                .children
                .first()
                .map(|child| last_segment(&child.text).to_owned())
                .unwrap_or_default();
            let qualifier = node
                .children
                .first()
                .filter(|child| child.kind == "scoped_type_identifier")
                .and_then(|child| qualifier_of(&child.text));
            let arguments = node
                .first("type_arguments")
                .map(|arguments| {
                    arguments
                        .children
                        .iter()
                        .filter(|child| !matches!(child.kind.as_str(), "<" | ">" | ","))
                        .map(type_of_node)
                        .collect()
                })
                .unwrap_or_default();
            Type {
                name,
                arguments,
                qualifier,
                ..Type::default()
            }
        }
        "array_type" => {
            let mut element = node
                .child("element")
                .map(type_of_node)
                .unwrap_or_default();
            element.dimensions += 1;
            element
        }
        "wildcard" => {
            let bound = node
                .children
                .iter()
                .find(|child| child.kind.ends_with("type") || child.kind.ends_with("identifier"));
            match bound {
                Some(bound) => {
                    let mut written = type_of_node(bound);
                    written.wildcard = Some(if node.children.iter().any(|child| child.kind == "super") {
                        "? super "
                    } else {
                        "? extends "
                    });
                    written
                }
                None => Type::named("?"),
            }
        }
        _ => {
            let mut written = Type::named(last_segment(node.text.trim()));
            if node.kind == "scoped_type_identifier" {
                written.qualifier = qualifier_of(node.text.trim());
            }
            written
        }
    }
}

/// `org.demo` of `org.demo.Shape`: what a dotted name is qualified with.
fn qualifier_of(name: &str) -> Option<String> {
    let name: String = name.chars().filter(|character| !character.is_whitespace()).collect();
    name.rsplit_once('.').map(|(qualifier, _)| qualifier.to_owned())
}

fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Every type below `node`, nested ones included, by name and by the
/// child indices that lead to it, in the order [`types_of`] lists them.
fn type_paths(node: &Syn, path: &mut Vec<u32>, found: &mut Vec<TypePath>) {
    if is_type_declaration(node)
        && let Some(name) = node.child("name")
    {
        found.push((name.text.clone(), path.clone()));
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index as u32);
        type_paths(child, path, found);
        path.pop();
    }
}

/// Every type `file` declares, nested ones included, by name.
pub fn types_of(file: &FileView) -> Vec<(&str, &Syn)> {
    file.tree
        .walk()
        .into_iter()
        .filter(|node| is_type_declaration(node))
        .filter_map(|node| Some((node.child("name")?.text.as_str(), node)))
        .collect()
}

/// An anonymous class: `new Section() { … }` declares a type of its own.
pub fn is_anonymous_class(node: &Syn) -> bool {
    node.kind == "object_creation_expression" && node.first("class_body").is_some()
}

/// The paths to the type declarations below `node` that name a supertype,
/// and to its anonymous classes.
fn extending_paths(node: &Syn, path: &mut Vec<u32>, found: &mut dyn FnMut(Vec<u32>)) {
    let names_a_supertype = is_type_declaration(node)
        && node.children.iter().any(|child| {
            matches!(
                child.kind.as_str(),
                "superclass" | "super_interfaces" | "extends_interfaces" | "interface_type_list"
            )
        });
    if names_a_supertype || is_anonymous_class(node) {
        found(path.clone());
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index as u32);
        extending_paths(child, path, found);
        path.pop();
    }
}

/// The type nodes below `node` that no other type node holds.
fn outermost_types(node: &Syn) -> Vec<&Syn> {
    let mut found = Vec::new();
    for child in &node.children {
        if matches!(
            child.kind.as_str(),
            "type_identifier" | "generic_type" | "scoped_type_identifier"
        ) {
            found.push(child);
        } else {
            found.extend(outermost_types(child));
        }
    }
    found
}

/// A declaration a call can mean.
fn is_callable(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "function_declaration"
    )
}

pub fn is_type_declaration(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "class_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration"
    )
}

/// One `import` of a file.
pub struct Import {
    /// The dotted name, without the `*` of an on-demand import.
    pub name: String,
    pub wildcard: bool,
    pub is_static: bool,
}

impl Import {
    pub(crate) fn last_segment(&self) -> &str {
        last_segment(&self.name)
    }

    /// Everything before the last segment: the package of a type import,
    /// the type of a static member import.
    pub(crate) fn qualifier(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(before, _)| before)
    }
}

pub(crate) fn imports(file: &FileView) -> Vec<Import> {
    file.tree
        .all("import_declaration")
        .map(|declaration| {
            let written = declaration.text.trim_start_matches("import").trim();
            let is_static = written.starts_with("static");
            let name = written
                .trim_start_matches("static")
                .trim()
                .trim_end_matches(';')
                .trim();
            Import {
                wildcard: name.ends_with(".*"),
                name: name.trim_end_matches(".*").trim_end_matches('.').to_owned(),
                is_static,
            }
        })
        .collect()
}

/// The members a type declaration holds, each with the identifier that
/// names it: a field declaration is one member per declarator.
pub fn members_of(declaration: &Syn) -> Vec<(&Syn, &Syn)> {
    let Some(body) = declaration.children.iter().find(|child| {
        matches!(
            child.kind.as_str(),
            "class_body" | "interface_body" | "enum_body" | "annotation_type_body"
        )
    }) else {
        return components_of(declaration);
    };
    let mut members = components_of(declaration);
    for child in own_members(body) {
        match child.kind.as_str() {
            "method_declaration" | "constructor_declaration" | "compact_constructor_declaration"
            | "enum_constant" | "annotation_type_element_declaration" => {
                if let Some(name) = child.child("name") {
                    members.push((child, name));
                }
            }
            "field_declaration" | "constant_declaration" => {
                for declarator in child.all("variable_declarator") {
                    if let Some(name) = declarator.child("name") {
                        members.push((child, name));
                    }
                }
            }
            _ => {}
        }
    }
    members.extend(recovered_members(body));
    members
}

/// The nodes of a type's body, not looking into the bodies it holds: a
/// nested class's, an enum constant's or an anonymous class's members, and
/// what a method declares, are not the type's.
fn own_members(body: &Syn) -> Vec<&Syn> {
    let mut found = Vec::new();
    let mut pending: Vec<&Syn> = body.children.iter().rev().collect();
    while let Some(node) = pending.pop() {
        found.push(node);
        let another_body = matches!(
            node.kind.as_str(),
            "class_body"
                | "interface_body"
                | "enum_body"
                | "annotation_type_body"
                | "block"
                | "constructor_body"
                | "lambda_expression"
        );
        if !another_body {
            pending.extend(node.children.iter().rev());
        }
    }
    found
}

/// [`members_of`] as paths from `declaration`, so a snapshot can keep them.
fn member_paths(declaration: &Syn) -> Vec<MemberPath> {
    let mut paths: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut pending = vec![(declaration, Vec::new())];
    while let Some((node, path)) = pending.pop() {
        for (index, child) in node.children.iter().enumerate() {
            let mut below = path.clone();
            below.push(index as u32);
            pending.push((child, below));
        }
        paths.insert(std::ptr::from_ref(node) as usize, path);
    }
    let path_of = |node: &Syn| paths.get(&(std::ptr::from_ref(node) as usize)).cloned();
    members_of(declaration)
        .into_iter()
        .filter_map(|(member, name)| Some((name.text.clone(), path_of(member)?, path_of(name)?)))
        .collect()
}

/// The child indices that lead from `root` to `target`, a node of its tree:
/// only the children spanning `target` are entered.
fn indices_to(root: &Syn, target: &Syn) -> Option<Vec<u32>> {
    if std::ptr::eq(root, target) {
        return Some(Vec::new());
    }
    root.children.iter().enumerate().find_map(|(index, child)| {
        if child.start > target.start || target.end > child.end {
            return None;
        }
        let mut path = indices_to(child, target)?;
        path.insert(0, index as u32);
        Some(path)
    })
}

/// The node the child indices `path` lead to from `root`.
fn at_path<'a>(root: &'a Syn, path: &[u32]) -> Option<&'a Syn> {
    path.iter()
        .try_fold(root, |node, index| node.children.get(*index as usize))
}

/// A member the parser could not finish: where a call is still being
/// typed, the declaration below it ends up inside an error — a parameter
/// list with the method's name somewhere in front of it.
fn recovered_members(body: &Syn) -> Vec<(&Syn, &Syn)> {
    let nodes = body.walk();
    // Where each finished declaration's parameters start, looked up once
    // per parameter list: a large class has thousands of nodes.
    let owned: std::collections::BTreeSet<Position> = nodes
        .iter()
        .filter(|node| {
            // A lambda's or a record's parameters are not a method's.
            matches!(
                node.kind.as_str(),
                "method_declaration" | "constructor_declaration" | "lambda_expression" | "record_declaration"
            )
        })
        .filter_map(|node| node.child("parameters"))
        .map(|own| own.start)
        .collect();
    let orphaned = nodes
        .iter()
        .filter(|node| node.kind == "formal_parameters")
        .filter(|parameters| !owned.contains(&parameters.start));
    orphaned
        .filter_map(|parameters| {
            let name = nodes
                .iter()
                .filter(|node| is_identifier(node) && node.end <= parameters.start)
                .max_by_key(|node| node.end)?;
            Some((*parameters, *name))
        })
        .collect()
}

/// The components a record declares: each is a field and an accessor.
pub fn components_of(declaration: &Syn) -> Vec<(&Syn, &Syn)> {
    declaration
        .child("parameters")
        .map(|parameters| {
            parameters
                .children
                .iter()
                .filter(|child| child.kind == "formal_parameter")
                .filter_map(|component| Some((component, component.child("name")?)))
                .collect()
        })
        .unwrap_or_default()
}

/// The parameters a method or constructor declares.
pub fn parameters_of(declaration: &Syn) -> Vec<&Syn> {
    if declaration.kind == "formal_parameters" {
        return declaration
            .children
            .iter()
            .filter(|child| matches!(child.kind.as_str(), "formal_parameter" | "spread_parameter"))
            .collect();
    }
    declaration
        .child("parameters")
        .map(|parameters| {
            parameters
                .children
                .iter()
                .filter(|child| {
                    matches!(child.kind.as_str(), "formal_parameter" | "spread_parameter")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What `node` declares in its own right: a local, a parameter, a pattern
/// variable, a loop variable, a caught exception, a resource.
fn locals_of(node: &Syn) -> Vec<(&Syn, &Syn)> {
    let mut declarations = Vec::new();
    match node.kind.as_str() {
        "method_declaration" | "constructor_declaration" | "lambda_expression" => {
            for parameter in parameters_of(node) {
                if let Some(name) = parameter.child("name") {
                    declarations.push((parameter, name));
                }
            }
            // A lambda's untyped parameters: `x -> …` or `(a, b) -> …`. Only
            // the names themselves: an identifier deeper in a parameter list
            // is an annotation's or a type's name, not a declaration.
            match node.child("parameters") {
                Some(name) if name.kind == "identifier" => declarations.push((node, name)),
                Some(inferred) if inferred.kind == "inferred_parameters" => {
                    for name in &inferred.children {
                        if name.kind == "identifier" {
                            declarations.push((node, name));
                        }
                    }
                }
                _ => {}
            }
            if node.child("parameters").is_none() {
                if let Some(name) = node.children.iter().find(|child| child.kind == "identifier") {
                    declarations.push((node, name));
                }
            }
        }
        "enhanced_for_statement" => {
            if let Some(name) = node.child("name") {
                declarations.push((node, name));
            }
        }
        "catch_clause" => {
            for parameter in node.walk() {
                if parameter.kind == "catch_formal_parameter" {
                    if let Some(name) = parameter.child("name") {
                        declarations.push((parameter, name));
                    }
                }
            }
        }
        _ => {}
    }
    for child in &node.children {
        match child.kind.as_str() {
            "local_variable_declaration" => {
                for declarator in child.all("variable_declarator") {
                    if let Some(name) = declarator.child("name") {
                        declarations.push((child, name));
                    }
                }
            }
            "expression_statement" | "if_statement" | "while_statement" | "for_statement"
            | "return_statement" => {
                for pattern in child.walk() {
                    match pattern.kind.as_str() {
                        "type_pattern" => {
                            let name = pattern.child("name").or_else(|| pattern.children.last());
                            declarations.extend(name.map(|name| (pattern, name)));
                        }
                        "instanceof_expression" => {
                            let name = pattern.child("name");
                            declarations.extend(name.map(|name| (pattern, name)));
                        }
                        "record_pattern_component" => {
                            let name = pattern
                                .children
                                .last()
                                .filter(|name| name.kind == "identifier");
                            declarations.extend(name.map(|name| (pattern, name)));
                        }
                        _ => {}
                    }
                }
            }
            "resource_specification" => {
                for resource in child.walk() {
                    if resource.kind == "resource" {
                        if let Some(name) = resource.child("name") {
                            declarations.push((resource, name));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    declarations
}

/// The name of the type whose body `at` is in, innermost first, and its
/// declaration.
pub fn enclosing_type(tree: &Syn, at: Position) -> Option<(String, &Syn)> {
    let path = path_to(tree, at);
    path.into_iter()
        .rev()
        .find(|node| is_type_declaration(node))
        .and_then(|node| Some((node.child("name")?.text.clone(), node)))
}

/// Every node covering `at`, outermost first.
pub fn path_to(tree: &Syn, at: Position) -> Vec<&Syn> {
    let mut path = vec![tree];
    let mut current = tree;
    while let Some(child) = next_child(current, at) {
        path.push(child);
        current = child;
    }
    path
}

/// The child to follow towards `at`: the one that holds it outright, and
/// only failing that the one that ends there — a position at the start of
/// a name is in the name, not in the `.` in front of it.
fn next_child<'a>(node: &'a Syn, at: Position) -> Option<&'a Syn> {
    node.children
        .iter()
        .find(|child| child.start <= at && at < child.end)
        .or_else(|| node.children.iter().find(|child| child.covers(at)))
}

/// The first expression among a node's children.
fn first_expression(node: &Syn) -> Option<&Syn> {
    node.children
        .iter()
        .find(|child| !matches!(child.kind.as_str(), "(" | ")" | ";"))
}

/// What the name at the cursor means: its declaration, wherever it is.
impl Snapshot {
    pub fn declaration_at(&self, uri: &Url, at: Position) -> Option<Found<'_>> {
        let file = self.file(uri)?;
        // `break`, `continue` and a `{@link}` are not identifiers.
        let at_the_cursor = path_to(&file.tree, at);
        if let Some(found) = self.label_at(&at_the_cursor, file) {
            return Some(found);
        }
        if let Some(found) = self.javadoc_reference_at(&at_the_cursor, file, at) {
            return Some(found);
        }
        if let Some(found) = self.superclass_at(&at_the_cursor, file) {
            return Some(found);
        }
        let name = identifier_at(&file.tree, at)?;
        if let Some(declared) = self.declares(file, name) {
            return Some(declared);
        }
        let path = path_to(&file.tree, name.start);
        // `com.demo.shapes.Circle` names one type, not four.
        let dotted = path
            .iter()
            .rev()
            .find(|node| node.kind == "scoped_type_identifier");
        if let Some(dotted) = dotted {
            let ends_there = dotted
                .children
                .last()
                .is_some_and(|last| last.start == name.start);
            if ends_there {
                return self.type_named(&dotted.text, file);
            }
        }
        let parent = parent_of(&path, name);
        self.meaning_of(name, parent, file)
    }

    /// Whether the identifier is the name of a declaration: then it means
    /// that declaration, whatever else carries the same name.
    fn declares<'a>(&'a self, file: &'a FileView, name: &'a Syn) -> Option<Found<'a>> {
        let path = path_to(&file.tree, name.start);
        let declaration = path.iter().rev().find(|node| {
            declared_name(node).is_some_and(|declared| declared.start == name.start)
        })?;
        Some(Found {
            file,
            node: declaration,
            name,
            bindings: Bindings::new(),
        })
    }

    /// The cursor on `super`: the class it stands for.
    fn superclass_at<'a>(&'a self, path: &[&'a Syn], file: &'a FileView) -> Option<Found<'a>> {
        let at = path.last()?.start;
        let on_super = path
            .iter()
            .flat_map(|node| node.children.iter())
            .any(|child| child.kind == "super" && (child.covers(at) || child.end == at));
        if !on_super {
            return None;
        }
        let class = path.iter().rev().find(|node| is_type_declaration(node))?;
        let of = Found {
            file,
            node: class,
            name: class.child("name")?,
            bindings: Bindings::new(),
        };
        self.supertypes(&of).into_iter().next()
    }

    /// `break`/`continue`, with a label or without: the loop they leave.
    fn label_at<'a>(&'a self, path: &[&'a Syn], file: &'a FileView) -> Option<Found<'a>> {
        let jump = path
            .iter()
            .rev()
            .find(|node| matches!(node.kind.as_str(), "break_statement" | "continue_statement"))?;
        let label = jump.children.iter().find(|child| is_identifier(child));
        let loop_or_label = match label {
            Some(label) => path.iter().find(|node| {
                node.kind == "labeled_statement"
                    && node.children.first().is_some_and(|named| named.text == label.text)
            })?,
            None => path
                .iter()
                .rev()
                .find(|node| is_loop_or_switch(node))?,
        };
        let named = match label {
            Some(_) => loop_or_label.children.first()?,
            None => loop_or_label.children.first()?,
        };
        Some(Found {
            file,
            node: loop_or_label,
            name: named,
            bindings: Bindings::new(),
        })
    }

    /// `{@link #area()}` in a javadoc comment: the member it names.
    fn javadoc_reference_at<'a>(
        &'a self,
        path: &[&'a Syn],
        file: &'a FileView,
        at: Position,
    ) -> Option<Found<'a>> {
        let comment = path.iter().rev().find(|node| node.kind == "block_comment")?;
        let line = comment.text.lines().nth((at.line - comment.start.line) as usize)?;
        let linked = line.split_once("{@link")?.1;
        let name: String = linked
            .trim()
            .trim_start_matches('#')
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        let of = self.enclosing(file, comment.start)?;
        self.member(&of, &name, None)
    }

    /// The declaration the identifier `name` refers to, read from what it
    /// sits in: a call's name is a method of its receiver, a field access
    /// is a member of its object, anything else is resolved by scope.
    fn meaning_of<'a>(
        &'a self,
        name: &Syn,
        parent: Option<&'a Syn>,
        file: &'a FileView,
    ) -> Option<Found<'a>> {
        let parent = parent?;
        let names_it = |field: &str| {
            parent
                .child(field)
                .is_some_and(|child| child.start == name.start)
        };
        match parent.kind.as_str() {
            "method_invocation" if names_it("name") => self.invoked(parent, file),
            "field_access" if names_it("field") => {
                let object = parent.child("object")?;
                let member = self
                    .type_of_expression(object, file)
                    .and_then(|receiver| self.of_type(&receiver, file))
                    .and_then(|owner| self.member(&owner, &name.text, None));
                // `org.demo.Shape.ORIGIN`: a type named in full.
                member
                    .or_else(|| self.dotted_type(parent, file))
                    .or_else(|| self.name_in_scope(&name.text, file, name.start))
            }
            // `import static org.demo.Shape.Kind.ROUND;`: the types on the
            // way are named in full.
            "scoped_identifier" if names_it("name") => self
                .dotted_type(parent, file)
                .or_else(|| self.name_in_scope(&name.text, file, name.start)),
            "object_creation_expression" => self.created(parent, file),
            "method_reference" => {
                let receiver = parent.children.first()?;
                let owner = if receiver.start == name.start {
                    return self.name_in_scope(&name.text, file, name.start);
                } else if is_identifier(receiver) {
                    self.type_named(&receiver.text, file).or_else(|| {
                        let written = self.type_of_expression(receiver, file)?;
                        self.of_type(&written, file)
                    })?
                } else {
                    let written = self.type_of_expression(receiver, file)?;
                    self.of_type(&written, file)?
                };
                self.member(&owner, &name.text, None)
            }
            "element_value_pair" if names_it("key") => {
                let path = path_to(&file.tree, name.start);
                let annotation = path
                    .iter()
                    .rev()
                    .find(|node| matches!(node.kind.as_str(), "annotation" | "marker_annotation"))?;
                let declaration = self.type_named(&annotation.child("name")?.text, file)?;
                self.member(&declaration, &name.text, None)
            }
            "switch_label" => {
                let switch = self.switch_of(file, name.start)?;
                self.member(&switch, &name.text, None)
            }
            _ => self.name_in_scope(&name.text, file, name.start),
        }
    }

    /// The type a dotted name of identifiers names in full
    /// (`org.demo.Outer.Inner`), if it names one.
    fn dotted_type<'a>(&'a self, dotted: &Syn, file: &'a FileView) -> Option<Found<'a>> {
        let written: String = dotted.text.chars().filter(|character| !character.is_whitespace()).collect();
        let is_dotted = written.contains('.')
            && written
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|character| character.is_alphanumeric() || matches!(character, '_' | '$')));
        if !is_dotted {
            return None;
        }
        self.type_named(&written, file)
    }

    /// The type a `switch` switches on: a `case` label names its members.
    pub(crate) fn switch_of<'a>(&'a self, file: &'a FileView, at: Position) -> Option<Found<'a>> {
        let path = path_to(&file.tree, at);
        let switch = path
            .iter()
            .rev()
            .find(|node| node.kind == "switch_expression")?;
        let condition = switch.child("condition")?;
        let inner = condition
            .children
            .iter()
            .find(|child| !matches!(child.kind.as_str(), "(" | ")"))?;
        let written = self.type_of_expression(inner, file)?;
        self.of_type(&written, file)
    }
}

/// What a `break` or `continue` with no label leaves.
fn is_loop_or_switch(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "while_statement"
            | "for_statement"
            | "enhanced_for_statement"
            | "do_statement"
            | "switch_expression"
    )
}

/// The identifier `at` is in, or the one that ends there.
fn identifier_at(tree: &Syn, at: Position) -> Option<&Syn> {
    first_identifier(tree, at, &|node| node.start <= at && at < node.end)
        .or_else(|| first_identifier(tree, at, &|node| node.end == at))
}

/// The first identifier, in tree order, that `accepts`: only the nodes
/// covering `at` are entered, where every identifier asked for lies.
fn first_identifier<'a>(node: &'a Syn, at: Position, accepts: &dyn Fn(&Syn) -> bool) -> Option<&'a Syn> {
    if is_identifier(node) && accepts(node) {
        return Some(node);
    }
    node.children
        .iter()
        .filter(|child| child.covers(at))
        .find_map(|child| first_identifier(child, at, accepts))
}

/// The identifier a declaration names, where it names one. A field or a
/// local declares one name per declarator, so it is not one of these.
fn declared_name(node: &Syn) -> Option<&Syn> {
    let names_itself = matches!(
        node.kind.as_str(),
        "class_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration"
            | "annotation_type_element_declaration"
            | "enum_constant"
            | "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "variable_declarator"
            | "formal_parameter"
            | "catch_formal_parameter"
            | "resource"
            | "type_pattern"
            | "enhanced_for_statement"
    );
    if names_itself {
        return node.child("name");
    }
    if node.kind == "type_parameter" {
        return node.first("type_identifier");
    }
    if node.kind == "record_pattern_component" {
        return node
            .children
            .last()
            .filter(|name| name.kind == "identifier");
    }
    if node.kind == "spread_parameter" {
        return node
            .all("variable_declarator")
            .next()
            .and_then(|declarator| declarator.child("name"));
    }
    None
}

pub fn is_identifier(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "identifier" | "type_identifier" | "field_identifier"
    )
}

/// `text` on one line, every run of whitespace a single space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The node whose child `name` is, in a path that ends at it.
fn parent_of<'a>(path: &[&'a Syn], name: &Syn) -> Option<&'a Syn> {
    let at = path
        .iter()
        .rposition(|node| node.start == name.start && node.kind == name.kind)?;
    path.get(at.checked_sub(1)?).copied()
}

/// What hover shows of the name at a place.
pub struct HoverText<'a> {
    pub found: Found<'a>,
    pub signature: String,
    /// The declaration's javadoc as markdown, if it has one.
    pub documentation: Option<String>,
}

impl Snapshot {
    /// The declaration of the name at `at`, its signature as jdtls shows
    /// it there and its documentation.
    pub fn hover_at(&self, uri: &Url, at: Position) -> Option<HoverText<'_>> {
        let found = self.declaration_at(uri, at)?;
        let mut signature = self.signature_of(&found)?;
        // jdtls names a Java type with its package and the types it is in,
        // and where it is used, with the type arguments written there.
        if found.file.language == Language::Java && is_type_declaration(found.node) {
            let name = self.qualified_type_name(found.file, found.node);
            let arguments = self
                .file(uri)
                .and_then(|file| type_arguments_at_use(file, at, &found));
            signature = match arguments {
                Some(arguments) => format!("{name}{arguments}"),
                None => format!("{name}{}", &signature[found.name.text.len()..]),
            };
        }
        let documentation = match found.file.language {
            Language::Java => self
                .comment_above(found.file, found.node)
                .filter(|comment| comment.text.starts_with("/**"))
                .map(|comment| javadoc_markdown(&comment.text))
                .filter(|markdown| !markdown.is_empty()),
            _ => None,
        };
        Some(HoverText {
            found,
            signature,
            documentation,
        })
    }
}

/// How a generic type is shown where it is used rather than declared: with
/// the arguments written there (`java.util.Map<String, String>`), or none
/// where it is used raw. `None` where the declaration's own form stands: at
/// the declaration, and where the name is not a type (`Outer.make()`).
fn type_arguments_at_use(file: &FileView, at: Position, found: &Found<'_>) -> Option<String> {
    let name = identifier_at(&file.tree, at)?;
    if file.uri == found.file.uri && name.start == found.name.start {
        return None;
    }
    if name.kind != "type_identifier" {
        return None;
    }
    let path = path_to(&file.tree, name.start);
    let mut at_name = path.iter().rposition(|node| node.start == name.start && node.kind == name.kind)?;
    // `Outer.Inner<T>`: the arguments belong to the whole dotted name.
    while at_name > 0 && path[at_name - 1].kind == "scoped_type_identifier" {
        if path[at_name - 1].children.last().is_none_or(|last| last.start != path[at_name].start) {
            return None;
        }
        at_name -= 1;
    }
    let parent = at_name.checked_sub(1).map(|index| path[index]);
    match parent {
        Some(generic) if generic.kind == "generic_type" => {
            let arguments = generic.child("type_arguments").or_else(|| generic.first("type_arguments"))?;
            Some(type_arguments_text(&arguments.text))
        }
        _ => Some(String::new()),
    }
}

/// `<String,List<String>>` as jdtls prints it: `<String, List<String>>`.
fn type_arguments_text(written: &str) -> String {
    let collapsed = collapse_whitespace(written);
    let mut text = String::new();
    for piece in collapsed.split(',') {
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(piece.trim());
    }
    text.replace("< ", "<").replace(" >", ">")
}

/// A javadoc comment as markdown, the way jdtls renders one: the text with
/// its `*` margins gone, inline tags as code or plain names, and the block
/// tags (`@param`, `@return`, …) as lists under a heading each.
pub fn javadoc_markdown(comment: &str) -> String {
    let body = comment
        .trim_start_matches("/**")
        .trim_end_matches("*/");
    let lines: Vec<&str> = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let trimmed = trimmed.strip_prefix('*').unwrap_or(trimmed);
            trimmed.strip_prefix(' ').unwrap_or(trimmed).trim_end()
        })
        .collect();
    let mut description = String::new();
    let mut tags: Vec<(String, String)> = Vec::new();
    for line in lines {
        let starts_tag = line.trim_start().starts_with('@')
            && line.trim_start()[1..].starts_with(|character: char| character.is_ascii_alphabetic());
        if starts_tag {
            let line = line.trim_start();
            let (tag, rest) = line[1..].split_once(char::is_whitespace).unwrap_or((&line[1..], ""));
            tags.push((tag.to_owned(), rest.trim().to_owned()));
        } else if let Some((_, text)) = tags.last_mut() {
            if !line.trim().is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(line.trim());
            }
        } else {
            description.push_str(line);
            description.push('\n');
        }
    }
    let mut markdown = inline_tags(&html_to_markdown(description.trim()));
    // jdtls's headings, in its order; `@exception` is `@throws`.
    const HEADINGS: &[(&[&str], &str)] = &[
        (&["param"], "Parameters:"),
        (&["return"], "Returns:"),
        (&["throws", "exception"], "Throws:"),
        (&["since"], "Since:"),
        (&["see"], "See Also:"),
        (&["author"], "Author:"),
        (&["deprecated"], "Deprecated."),
    ];
    for (names, heading) in HEADINGS {
        let entries: Vec<String> = tags
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, text)| {
                let text = inline_tags(&html_to_markdown(text));
                match name.as_str() {
                    "param" | "throws" | "exception" => match text.split_once(' ') {
                        Some((first, rest)) => format!("**{first}** {rest}"),
                        None => format!("**{text}**"),
                    },
                    _ => text,
                }
            })
            .collect();
        if entries.is_empty() {
            continue;
        }
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(&format!("* **{heading}**"));
        for entry in entries {
            markdown.push_str(&format!("\n  * {entry}"));
        }
    }
    markdown.trim().to_owned()
}

/// The few HTML tags javadoc is written with, as markdown.
fn html_to_markdown(text: &str) -> String {
    let mut out = text.to_owned();
    for (tag, replacement) in [
        ("<p>", "\n\n"),
        ("</p>", "\n\n"),
        ("<P>", "\n\n"),
        ("<br>", "\n"),
        ("<br/>", "\n"),
        ("<code>", "`"),
        ("</code>", "`"),
        ("<b>", "**"),
        ("</b>", "**"),
        ("<strong>", "**"),
        ("</strong>", "**"),
        ("<i>", "*"),
        ("</i>", "*"),
        ("<em>", "*"),
        ("</em>", "*"),
        ("<ul>", "\n"),
        ("</ul>", "\n"),
        ("<li>", "\n* "),
        ("</li>", ""),
        ("<pre>", "\n```\n"),
        ("</pre>", "\n```\n"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
    ] {
        out = out.replace(tag, replacement);
    }
    let mut paragraphs: Vec<String> = Vec::new();
    for paragraph in out.split("\n\n") {
        let joined = paragraph
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            paragraphs.push(joined);
        }
    }
    paragraphs.join("\n\n")
}

/// `{@code x}` and `{@literal x}` as code, `{@link Type#member label}` as
/// its label or the name it links to.
fn inline_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{@") {
        out.push_str(&rest[..start]);
        let inside = &rest[start + 2..];
        // Braces nest in `{@code Map<K, V> {}}`.
        let mut depth = 1;
        let mut end = None;
        for (index, character) in inside.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag = &inside[..end];
        let (name, argument) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let argument = argument.trim();
        match name {
            "code" | "literal" | "value" => out.push_str(&format!("`{argument}`")),
            "link" | "linkplain" => {
                let (target, label) = argument.split_once(char::is_whitespace).unwrap_or((argument, ""));
                if label.trim().is_empty() {
                    let target = target.trim_start_matches('#').replace('#', ".");
                    out.push_str(&format!("`{target}`"));
                } else {
                    out.push_str(label.trim());
                }
            }
            _ => out.push_str(argument),
        }
        rest = &inside[end + 1..];
    }
    out.push_str(rest);
    out
}

/// How a declaration reads on hover, as jdtls words it.
impl Snapshot {
    pub fn signature_of(&self, found: &Found<'_>) -> Option<String> {
        let node = found.node;
        match node.kind.as_str() {
            "method_declaration" | "annotation_type_element_declaration" => {
                Some(self.method_signature(found))
            }
            "constructor_declaration" | "compact_constructor_declaration" => {
                let language = features(found.file.language);
                let parameters: Vec<String> = parameters(node, &Bindings::new())
                    .iter()
                    .map(|parameter| format!("{}{}", parameter.annotations, language.parameter(parameter)))
                    .collect();
                // jdtls names a constructor after its class, qualified:
                // `com.demo.Point.Point(int x)`.
                let owner = self
                    .java_owner(found)
                    .map(|owner| format!("{owner}."))
                    .unwrap_or_default();
                Some(format!("{owner}{}({})", found.name.text, parameters.join(", ")))
            }
            "class_declaration" | "record_declaration" | "interface_declaration"
            | "enum_declaration" | "annotation_type_declaration" => {
                let parameters = type_parameters_of(node);
                let written = if parameters.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", parameters.join(", "))
                };
                Some(format!("{}{written}", found.name.text))
            }
            "enum_constant" => Some(found.name.text.clone()),
            // jdtls shows a type parameter with its bounds: `T extends Node`.
            "type_parameter" => match found.file.language {
                Language::Java => Some(collapse_whitespace(&node.text)),
                _ => Some(found.name.text.clone()),
            },
            "field_declaration" | "constant_declaration" => {
                let written = self.type_of_declaration(found)?.written();
                let value = assigned_value(node, &found.name.text);
                let is_constant = is_compile_time_constant(node, &found.name.text, &written);
                match value.filter(|_| is_constant) {
                    Some(value) => Some(format!("{written} {} = {value}", found.name.text)),
                    None => Some(format!("{written} {}", found.name.text)),
                }
            }
            // A local or a parameter is shown with the method it is in.
            _ => {
                let written = self.type_of_declaration(found)?.written();
                let owner = self.owning_method(found)?;
                Some(format!("{written} {} - {owner}", found.name.text))
            }
        }
    }

    fn method_signature(&self, found: &Found<'_>) -> String {
        let node = found.node;
        // jdtls qualifies a Java method with the type that declares it:
        // `int com.demo.Main.sum(int a, int b)`.
        let name = match self.java_owner(found) {
            Some(owner) => format!("{owner}.{}", found.name.text),
            None => found.name.text.clone(),
        };
        let shape = MethodShape {
            name,
            type_parameters: type_parameters_of(node),
            returns: node.child("type").map(|written| type_of_node(written).written()),
            parameters: parameters(node, &Bindings::new()),
            thrown: node
                .first("throws")
                .map(|throws| throws.text.split_whitespace().collect::<Vec<_>>().join(" ")),
        };
        features(found.file.language).method_signature(&shape)
    }

    /// The type declaring a Java member, qualified (`com.demo.Outer.Inner`);
    /// `None` for Kotlin, whose hover does not qualify.
    fn java_owner(&self, found: &Found<'_>) -> Option<String> {
        if found.file.language != Language::Java {
            return None;
        }
        let owner = path_to(&found.file.tree, found.node.start)
            .into_iter()
            .rev()
            .filter(|node| node.start != found.node.start || node.kind != found.node.kind)
            .find(|node| is_type_declaration(node))?;
        Some(self.qualified_type_name(found.file, owner))
    }

    /// A type declaration's name with its package and the types it is
    /// nested in: `com.demo.Outer.Inner`.
    pub fn qualified_type_name(&self, file: &FileView, declaration: &Syn) -> String {
        let mut names: Vec<String> = path_to(&file.tree, declaration.start)
            .into_iter()
            .filter(|node| is_type_declaration(node) && node.start < declaration.start)
            .filter_map(|node| node.child("name").map(|name| name.text.clone()))
            .collect();
        if let Some(name) = declaration.child("name") {
            names.push(name.text.clone());
        }
        if let Some(package) = self.package(file) {
            names.insert(0, package);
        }
        names.join(".")
    }

    /// `com.demo.Main.sum(int, int)`: the method a local belongs to.
    fn owning_method(&self, found: &Found<'_>) -> Option<String> {
        let path = path_to(&found.file.tree, found.name.start);
        let method = path.iter().rev().find(|node| {
            matches!(
                node.kind.as_str(),
                "method_declaration" | "constructor_declaration"
            )
        })?;
        let (owner, declaration) = enclosing_type(&found.file.tree, method.start)?;
        // Java names every type it is nested in; Kotlin only the innermost.
        let (package, owner) = match found.file.language {
            Language::Java => (String::new(), self.qualified_type_name(found.file, declaration)),
            _ => (
                self.package(found.file)
                    .map(|package| format!("{package}."))
                    .unwrap_or_default(),
                owner,
            ),
        };
        let types: Vec<String> = parameters_of(method)
            .iter()
            .map(|parameter| {
                let written = parameter.child("type").map(type_of_node).unwrap_or_default();
                written.written()
            })
            .collect();
        Some(format!(
            "{package}{owner}.{}({})",
            method.child("name")?.text,
            types.join(", ")
        ))
    }
}

/// The parameters a declaration takes, each with its type as written
/// (the type parameters in `bindings` filled in) and its name. A record
/// takes its components.
pub fn parameters(declaration: &Syn, bindings: &Bindings) -> Vec<Parameter> {
    let source = if declaration.kind == "record_declaration" {
        components_of(declaration)
            .into_iter()
            .map(|(component, _)| component)
            .collect::<Vec<_>>()
    } else {
        parameters_of(declaration)
    };
    source
        .iter()
        .map(|parameter| {
            // A spread parameter writes its type by position, not under a
            // `type` field.
            let written = parameter
                .child("type")
                .or_else(|| parameter.children.first())
                .map(type_of_node)
                .unwrap_or_default();
            let name = declared_name(parameter)
                .or_else(|| parameter.child("name"))
                .map(|name| name.text.clone())
                .unwrap_or_default();
            let annotations = parameter
                .first("modifiers")
                .map(|modifiers| {
                    modifiers
                        .children
                        .iter()
                        .filter(|modifier| modifier.kind.ends_with("annotation"))
                        .map(|annotation| format!("{} ", collapse_whitespace(&annotation.text)))
                        .collect()
                })
                .unwrap_or_default();
            Parameter {
                written: substituted(written, bindings).written(),
                name,
                spread: parameter.kind == "spread_parameter",
                annotations,
            }
        })
        .collect()
}

/// The names of the type parameters a declaration introduces.
fn type_parameter_names(declaration: &Syn) -> Vec<String> {
    declaration
        .child("type_parameters")
        .map(|parameters| {
            parameters
                .all("type_parameter")
                .filter_map(|parameter| parameter.first("type_identifier"))
                .map(|name| name.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn type_parameters_of(declaration: &Syn) -> Vec<String> {
    declaration
        .child("type_parameters")
        .map(|parameters| {
            parameters
                .all("type_parameter")
                .map(|parameter| parameter.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a field is a constant whose value jdtls shows on hover: final
/// (an interface's fields are), of a primitive type or `String`, and set to
/// an expression of literals and names — no call, no `new`.
fn is_compile_time_constant(declaration: &Syn, name: &str, written: &str) -> bool {
    let is_final = declaration.kind == "constant_declaration"
        || declaration.first("modifiers").is_some_and(|modifiers| {
            modifiers.children.iter().any(|modifier| modifier.kind == "final")
        });
    let constant_type = matches!(
        written,
        "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double" | "String"
    );
    let Some(value) = declaration
        .all("variable_declarator")
        .find(|declarator| declarator.child("name").is_some_and(|named| named.text == name))
        .and_then(|declarator| declarator.child("value"))
    else {
        return false;
    };
    let computed = value.walk().into_iter().any(|node| {
        matches!(
            node.kind.as_str(),
            "method_invocation"
                | "object_creation_expression"
                | "array_creation_expression"
                | "array_initializer"
                | "lambda_expression"
                | "method_reference"
                | "assignment_expression"
                | "update_expression"
                | "instanceof_expression"
                | "switch_expression"
        )
    });
    is_final && constant_type && !computed
}

/// What a field declaration assigns `name`, as written.
fn assigned_value<'a>(declaration: &'a Syn, name: &str) -> Option<&'a str> {
    declaration
        .all("variable_declarator")
        .find(|declarator| declarator.child("name").is_some_and(|named| named.text == name))?
        .child("value")
        .map(|value| value.text.as_str())
}

/// Where a declaration is used: every identifier of the workspace that
/// means the same declaration. A use is found by resolving it, so a
/// comment, a string and a name of something else are left out by
/// construction.
impl Snapshot {
    pub fn uses_of(&self, declaration: &Found<'_>) -> Vec<(&Url, Position)> {
        self.uses_in(declaration, None)
    }

    /// [`Self::uses_of`] in one file, or in every file when none is given.
    /// Each file that spells the name is searched on a core of its own
    /// share: a name is resolved where it is written, and a workspace
    /// writes a popular type's name hundreds of times.
    pub fn uses_in<'a>(
        &'a self,
        declaration: &Found<'_>,
        only: Option<&Url>,
    ) -> Vec<(&'a Url, Position)> {
        let wanted = (&declaration.file.uri, declaration.selection());
        let name = declaration.name.text.as_str();
        // A library is read, not searched: its uses are not the workspace's.
        let files: Vec<&'a FileView> = self
            .files
            .iter()
            .filter(|file| !file.library && only.is_none_or(|only| file.uri == *only))
            .filter(|file| file.text.contains(name))
            .collect();
        let uses_in_file = |file: &&'a FileView| self.uses_in_file(file, name, wanted);
        // The largest files first, each taken by whichever core is free:
        // a few files hold most of the uses.
        let mut files = files;
        files.sort_by_key(|file| std::cmp::Reverse(file.text.len()));
        let threads = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .min(files.len());
        let mut places: Vec<(&'a Url, Position)> = if threads <= 1 {
            files.iter().flat_map(uses_in_file).collect()
        } else {
            let next = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                let searches: Vec<_> = (0..threads)
                    .map(|_| {
                        scope.spawn(|| {
                            let mut places = Vec::new();
                            loop {
                                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(file) = files.get(index) else {
                                    break places;
                                };
                                places.extend(uses_in_file(file));
                            }
                        })
                    })
                    .collect();
                searches
                    .into_iter()
                    .flat_map(|search| search.join().unwrap_or_default())
                    .collect()
            })
        };
        places.sort_by(|left, right| (left.0.as_str(), left.1).cmp(&(right.0.as_str(), right.1)));
        places.dedup();
        places
    }

    /// Where `file` writes `name` meaning the declaration at `wanted`.
    fn uses_in_file<'a>(
        &'a self,
        file: &'a FileView,
        name: &str,
        wanted: (&Url, Position),
    ) -> Vec<(&'a Url, Position)> {
        let mut places = Vec::new();
        for node in file.tree.walk() {
            if !is_identifier(node) || node.text != name {
                continue;
            }
            let Some(found) = self.declaration_at(&file.uri, node.start) else {
                continue;
            };
            if (&found.file.uri, found.selection()) == wanted {
                places.push((&file.uri, node.start));
            }
        }
        places
    }
}

/// Where javadoc comments name a declaration — `{@link Type#member(String)}`,
/// `{@linkplain …}`, `{@value …}`, `@see`, `@throws` — which jdtls lists
/// among its references and renames with it.
impl Snapshot {
    pub fn javadoc_uses_of<'a>(&'a self, declaration: &Found<'_>) -> Vec<(&'a Url, Position)> {
        if declaration.file.language != Language::Java {
            return Vec::new();
        }
        let wanted = (&declaration.file.uri, declaration.selection());
        let name = declaration.name.text.as_str();
        let mut places = Vec::new();
        for file in self.files.iter().filter(|file| !file.library && file.text.contains(name)) {
            if file.language != Language::Java {
                continue;
            }
            for comment in file.tree.walk() {
                if comment.kind != "block_comment"
                    || !comment.text.starts_with("/**")
                    || !comment.text.contains(name)
                {
                    continue;
                }
                for (written, start) in javadoc_references(comment) {
                    for (at, found) in self.javadoc_reference_names(file, comment, &written, start, name) {
                        if (&found.file.uri, found.selection()) == wanted {
                            places.push((&file.uri, at));
                        }
                    }
                }
            }
        }
        places
    }

    /// The names called `name` in a javadoc reference written at `start`,
    /// each with what it means: the types of `pkg.Outer.Inner`, and the
    /// member after `#`, its overload picked by the parameter types written.
    fn javadoc_reference_names<'a>(
        &'a self,
        file: &'a FileView,
        comment: &Syn,
        written: &str,
        start: Position,
        name: &str,
    ) -> Vec<(Position, Found<'a>)> {
        let mut named = Vec::new();
        let (type_part, member_part) = match written.split_once('#') {
            Some((type_part, member)) => (type_part, Some(member)),
            None => (written, None),
        };
        let column_of = |offset: usize| Position {
            line: start.line,
            column: start.column + written[..offset].encode_utf16().count() as u32,
        };
        // A type named with its qualifier is reported where the qualifier
        // starts, as jdtls reports `org.demo.Outer.Inner`.
        let mut offset = 0;
        for segment in type_part.split('.') {
            if segment == name
                && let Some(found) = self.type_named(&type_part[..offset + segment.len()], file)
            {
                named.push((column_of(0), found));
            }
            offset += segment.len() + 1;
        }
        let Some(member) = member_part else {
            return named;
        };
        let (member_name, written_parameters) = match member.split_once('(') {
            Some((member_name, rest)) => (member_name, Some(rest.trim_end_matches(')'))),
            None => (member, None),
        };
        // The types of `#member(Document, int)` are references too.
        if let Some(written_parameters) = written_parameters {
            let mut offset = type_part.len() + 1 + member_name.len() + 1;
            for parameter in written_parameters.split(',') {
                let leading = parameter.len() - parameter.trim_start().len();
                let type_written = parameter.split_whitespace().next().unwrap_or_default();
                let raw = type_written.split('<').next().unwrap_or_default().trim_end_matches("...");
                let raw = raw.trim_end_matches("[]");
                if raw.rsplit('.').next() == Some(name)
                    && let Some(found) = self.type_named(raw, file)
                {
                    named.push((column_of(offset + leading), found));
                }
                offset += parameter.len() + 1;
            }
        }
        if member_name != name {
            return named;
        }
        let owner = if type_part.is_empty() {
            self.documented_type(file, comment)
        } else {
            self.type_named(type_part, file)
        };
        let Some(owner) = owner else {
            return named;
        };
        let candidates = self.members_called(&owner, name);
        let wanted_types: Option<Vec<String>> = written_parameters.map(|written_parameters| {
            written_parameters
                .split(',')
                .map(str::trim)
                .filter(|parameter| !parameter.is_empty())
                .map(|parameter| {
                    // `String name` or `java.lang.String`: the type, simply named.
                    let written = parameter.split_whitespace().next().unwrap_or(parameter);
                    let raw = written.split('<').next().unwrap_or(written);
                    let simple = raw.rsplit('.').next().unwrap_or(raw);
                    format!("{simple}{}", if written.ends_with("...") { "..." } else { "" })
                })
                .collect()
        });
        let chosen = candidates.into_iter().find(|candidate| {
            let Some(wanted_types) = &wanted_types else {
                return true;
            };
            if !matches!(
                candidate.node.kind.as_str(),
                "method_declaration" | "constructor_declaration"
            ) {
                return false;
            }
            let declared: Vec<String> = parameters(candidate.node, &Bindings::new())
                .into_iter()
                .map(|parameter| {
                    let raw = parameter.written.split('<').next().unwrap_or_default().to_owned();
                    let raw = raw.rsplit('.').next().unwrap_or_default().to_owned();
                    let dimensions = "[]".repeat(parameter.written.matches("[]").count());
                    let raw = raw.trim_end_matches("[]").to_owned();
                    if parameter.spread {
                        format!("{raw}{dimensions}...")
                    } else {
                        format!("{raw}{dimensions}")
                    }
                })
                .collect();
            declared == *wanted_types
        });
        if let Some(chosen) = chosen {
            named.push((column_of(type_part.len() + 1), chosen));
        }
        named
    }

    /// The type a javadoc's `#member` belongs to: the type it documents,
    /// else the one it is written in.
    fn documented_type<'a>(&'a self, file: &'a FileView, comment: &Syn) -> Option<Found<'a>> {
        let after = path_to(&file.tree, comment.start)
            .into_iter()
            .rev()
            .find_map(|parent| {
                let index = parent
                    .children
                    .iter()
                    .position(|child| child.start == comment.start && child.kind == comment.kind)?;
                parent.children[index + 1..]
                    .iter()
                    .find(|next| !matches!(next.kind.as_str(), "line_comment" | "block_comment"))
            });
        if let Some(documented) = after.filter(|node| is_type_declaration(node)) {
            return self.enclosing(file, documented.child("name")?.start);
        }
        self.enclosing(file, comment.start)
    }
}

/// The references a javadoc comment writes, each with where it starts:
/// what follows `{@link`, `{@linkplain`, `{@value`, `@see`, `@throws` and
/// `@exception`, on the same line or the next.
fn javadoc_references(comment: &Syn) -> Vec<(String, Position)> {
    const TAGS: &[&str] = &["{@link", "{@linkplain", "{@value", "@see", "@throws", "@exception"];
    let text = &comment.text;
    let mut found = Vec::new();
    let position_of = |offset: usize| {
        let before = &text[..offset];
        let line = before.matches('\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |at| at + 1);
        let column = before[line_start..].encode_utf16().count() as u32;
        if line == 0 {
            Position {
                line: comment.start.line,
                column: comment.start.column + column,
            }
        } else {
            Position {
                line: comment.start.line + line,
                column,
            }
        }
    };
    for tag in TAGS {
        let mut from = 0;
        while let Some(at) = text[from..].find(tag) {
            let after = from + at + tag.len();
            from = after;
            // `{@link` must not be the start of `{@linkplain`.
            if text[after..].starts_with(|character: char| character.is_alphanumeric()) {
                continue;
            }
            // Spaces, and a line break with the next line's margin.
            let mut start = after;
            let bytes = text.as_bytes();
            let mut crossed = false;
            while start < text.len() {
                match bytes[start] {
                    b' ' | b'\t' | b'\r' => start += 1,
                    b'\n' if !crossed => {
                        crossed = true;
                        start += 1;
                        while start < text.len() && matches!(bytes[start], b' ' | b'\t') {
                            start += 1;
                        }
                        if text[start..].starts_with('*') && !text[start..].starts_with("*/") {
                            start += 1;
                        }
                    }
                    _ => break,
                }
            }
            let mut end = start;
            let mut depth = 0usize;
            for (offset, character) in text[start..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    '}' if depth == 0 => break,
                    _ if character.is_whitespace() && depth == 0 => break,
                    _ => {}
                }
                end = start + offset + character.len_utf8();
            }
            let written = text[start..end].replace(['\n', '\r', '*'], " ");
            let written: String = written.split_whitespace().collect::<Vec<_>>().join(" ");
            if !written.is_empty() && !written.starts_with('"') && !written.starts_with('<') {
                found.push((written, position_of(start)));
            }
        }
    }
    found
}

/// A rename changes one name, but a name can be two declarations tied
/// together: a class and its constructors, a record component and its
/// accessor, a method and the ones it overrides or is overridden by.
impl Snapshot {
    pub fn tied_to<'a>(&'a self, found: &Found<'a>) -> Vec<Found<'a>> {
        let mut tied = vec![Found {
            file: found.file,
            node: found.node,
            name: found.name,
            bindings: Bindings::new(),
        }];
        match found.node.kind.as_str() {
            "class_declaration" | "record_declaration" => {
                let constructors = members_of(found.node)
                    .into_iter()
                    .filter(|(member, named)| {
                        is_constructor(member) && named.text == found.name.text
                    })
                    .map(|(member, named)| Found {
                        file: found.file,
                        node: member,
                        name: named,
                        bindings: Bindings::new(),
                    });
                tied.extend(constructors);
            }
            "constructor_declaration" | "compact_constructor_declaration" => {
                let class = self.owning_type(found);
                tied.extend(class);
            }
            "method_declaration" if found.file.language == Language::Java => {
                // Every method an override ties it to, however far: renaming
                // one renames them all, as jdtls renames the ripple.
                let mut queue = vec![found.clone()];
                while let Some(method) = queue.pop() {
                    for related in self.dispatching_to(&method) {
                        let place = (&related.file.uri, related.selection());
                        if tied.iter().any(|seen| (&seen.file.uri, seen.selection()) == place) {
                            continue;
                        }
                        tied.push(related.clone());
                        queue.push(related);
                    }
                    if tied.len() > 256 {
                        break;
                    }
                }
                tied.extend(self.accessed_component(found));
            }
            "method_declaration" => {
                tied.extend(self.overrides_of(found));
                tied.extend(self.accessed_component(found));
            }
            "formal_parameter" => {
                if self.is_record_component(found) {
                    tied.extend(self.accessor_of(found));
                } else if let Some(component) = self.component_named(found) {
                    tied.extend(self.accessor_of(&component));
                    tied.push(component);
                }
            }
            _ => {}
        }
        tied
    }

    /// The type a member is declared in.
    fn owning_type<'a>(&'a self, member: &Found<'a>) -> Option<Found<'a>> {
        let path = path_to(&member.file.tree, member.name.start);
        let declaration = path.iter().rev().find(|node| is_type_declaration(node))?;
        Some(Found {
            file: member.file,
            node: declaration,
            name: declaration.child("name")?,
            bindings: Bindings::new(),
        })
    }

    /// Whether a parameter is a record's component, not a method's.
    fn is_record_component(&self, parameter: &Found<'_>) -> bool {
        self.owning_type(parameter).is_some_and(|owner| {
            owner.node.kind == "record_declaration"
                && components_of(owner.node)
                    .iter()
                    .any(|(_, named)| named.start == parameter.name.start)
        })
    }

    /// The component a canonical constructor's parameter stands for: a
    /// record's own constructor names its components.
    fn component_named<'a>(&'a self, parameter: &Found<'a>) -> Option<Found<'a>> {
        let path = path_to(&parameter.file.tree, parameter.name.start);
        let in_a_constructor = path.iter().rev().any(|node| is_constructor(node));
        if !in_a_constructor {
            return None;
        }
        let owner = self.owning_type(parameter)?;
        if owner.node.kind != "record_declaration" {
            return None;
        }
        let (component, named) = components_of(owner.node)
            .into_iter()
            .find(|(_, named)| named.text == parameter.name.text)?;
        Some(Found {
            file: owner.file,
            node: component,
            name: named,
            bindings: Bindings::new(),
        })
    }

    /// The accessor a record component is read through, where the record
    /// writes one out.
    fn accessor_of<'a>(&'a self, component: &Found<'a>) -> Option<Found<'a>> {
        let owner = self.owning_type(component)?;
        let (member, named) = members_of(owner.node).into_iter().find(|(member, named)| {
            member.kind == "method_declaration"
                && named.text == component.name.text
                && parameters_of(member).is_empty()
        })?;
        Some(Found {
            file: owner.file,
            node: member,
            name: named,
            bindings: Bindings::new(),
        })
    }

    /// The component an accessor reads, where the method is one.
    fn accessed_component<'a>(&'a self, accessor: &Found<'a>) -> Option<Found<'a>> {
        if !parameters_of(accessor.node).is_empty() {
            return None;
        }
        let owner = self.owning_type(accessor)?;
        if owner.node.kind != "record_declaration" {
            return None;
        }
        let (component, named) = components_of(owner.node)
            .into_iter()
            .find(|(_, named)| named.text == accessor.name.text)?;
        Some(Found {
            file: owner.file,
            node: component,
            name: named,
            bindings: Bindings::new(),
        })
    }

    /// Every method of the same name and arity declared by a type tied to
    /// this one by inheritance — what jdtls renames along with an override.
    pub fn overrides_of<'a>(&'a self, method: &Found<'a>) -> Vec<Found<'a>> {
        let Some(owner) = self.owning_type(method) else {
            return Vec::new();
        };
        let arity = parameters_of(method.node).len();
        let family = self.hierarchy_of(&owner);
        let mut found = Vec::new();
        for relative in family {
            for (member, named) in members_of(relative.node) {
                let same = member.kind == "method_declaration"
                    && named.text == method.name.text
                    && parameters_of(member).len() == arity
                    && named.start != method.name.start;
                if same {
                    found.push(Found {
                        file: relative.file,
                        node: member,
                        name: named,
                        bindings: Bindings::new(),
                    });
                }
            }
        }
        found
    }

    /// The methods whose calls may run `method`, as jdtls counts them among
    /// its references: the ones it overrides up its supertypes and the ones
    /// overriding it down its subtypes, with the same parameter types — not
    /// a sibling's, and none for a static or private method.
    pub fn dispatching_to<'a>(&'a self, method: &Found<'a>) -> Vec<Found<'a>> {
        if method.node.kind != "method_declaration" {
            return Vec::new();
        }
        let modifiers = method.node.first("modifiers").map(|modifiers| modifiers.text.as_str()).unwrap_or_default();
        if modifiers.split_whitespace().any(|word| word == "static" || word == "private") {
            return Vec::new();
        }
        let Some(owner) = self.owning_type(method) else {
            return Vec::new();
        };
        let mut related: Vec<Found<'a>> = Vec::new();
        let mut seen = vec![(owner.file.uri.clone(), owner.name.start)];
        let mut queue: std::collections::VecDeque<Found<'a>> = self.supertypes(&owner).into();
        while let Some(supertype) = queue.pop_front() {
            let place = (supertype.file.uri.clone(), supertype.name.start);
            if seen.contains(&place) || seen.len() > 64 {
                continue;
            }
            seen.push(place);
            queue.extend(self.supertypes(&supertype));
            related.push(supertype);
        }
        related.extend(self.subtypes_of(&owner));
        let wanted = self.erased_parameter_types(method);
        let mut found = Vec::new();
        for relative in related {
            for (member, named) in members_of(relative.node) {
                if member.kind != "method_declaration" || named.text != method.name.text {
                    continue;
                }
                let candidate = Found {
                    file: relative.file,
                    node: member,
                    name: named,
                    bindings: Bindings::new(),
                };
                if self.erased_parameter_types(&candidate) == wanted {
                    found.push(candidate);
                }
            }
        }
        found
    }

    /// Whether `method` overrides a method of a library type, however far
    /// up: then it cannot be renamed.
    pub fn overrides_a_library_method(&self, method: &Found<'_>) -> bool {
        if method.node.kind != "method_declaration" {
            return false;
        }
        let Some(owner) = self.owning_type(method) else {
            return false;
        };
        let wanted = self.erased_parameter_types(method);
        let mut seen = 0;
        let mut queue: std::collections::VecDeque<Found<'_>> = self.supertypes(&owner).into();
        while let Some(supertype) = queue.pop_front() {
            seen += 1;
            if seen > 64 {
                break;
            }
            if supertype.file.library {
                let overridden = members_of(supertype.node).into_iter().any(|(member, named)| {
                    member.kind == "method_declaration"
                        && named.text == method.name.text
                        && self.erased_parameter_types(&Found {
                            file: supertype.file,
                            node: member,
                            name: named,
                            bindings: Bindings::new(),
                        }) == wanted
                });
                if overridden {
                    return true;
                }
            }
            queue.extend(self.supertypes(&supertype));
        }
        false
    }

    /// A method's parameter types with their arguments dropped, and a type
    /// variable of the method or its type as `?`: what two overriding
    /// declarations agree on however each names its type parameters.
    fn erased_parameter_types(&self, method: &Found<'_>) -> Vec<String> {
        let mut variables = type_parameter_names(method.node);
        if let Some(owner) = self.owning_type(method) {
            variables.extend(type_parameter_names(owner.node));
        }
        parameters(method.node, &Bindings::new())
            .into_iter()
            .map(|parameter| {
                let raw = parameter.written.split('<').next().unwrap_or_default();
                let dimensions = parameter.written.matches("[]").count();
                let raw = raw.trim_end_matches("[]");
                let raw = raw.rsplit('.').next().unwrap_or(raw);
                let name = if variables.iter().any(|variable| variable == raw) { "?" } else { raw };
                format!("{name}{}{}", "[]".repeat(dimensions), if parameter.spread { "..." } else { "" })
            })
            .collect()
    }

    /// The method `method` overrides or implements: the nearest one of the
    /// same name and arity up its supertypes, the superclass's side first —
    /// what jdtls answers a declaration request with.
    pub fn overridden<'a>(&'a self, method: &Found<'a>) -> Option<Found<'a>> {
        if method.node.kind != "method_declaration" {
            return None;
        }
        let owner = self.owning_type(method)?;
        let arity = parameters_of(method.node).len();
        let mut seen = vec![(owner.file.uri.clone(), owner.name.start)];
        let mut queue: std::collections::VecDeque<Found<'a>> = self.supertypes(&owner).into();
        while let Some(supertype) = queue.pop_front() {
            let place = (supertype.file.uri.clone(), supertype.name.start);
            if seen.contains(&place) {
                continue;
            }
            seen.push(place);
            let same = members_of(supertype.node)
                .into_iter()
                .find(|(member, named)| {
                    member.kind == "method_declaration"
                        && named.text == method.name.text
                        && parameters_of(member).len() == arity
                });
            if let Some((member, named)) = same {
                return Some(Found {
                    file: supertype.file,
                    node: member,
                    name: named,
                    bindings: Bindings::new(),
                });
            }
            queue.extend(self.supertypes(&supertype));
        }
        None
    }

    /// Every type reachable from this one by `extends`/`implements`, up or
    /// down, however far.
    fn hierarchy_of<'a>(&'a self, of: &Found<'a>) -> Vec<Found<'a>> {
        let mut family: Vec<(&Url, Position)> = vec![(&of.file.uri, of.name.start)];
        let mut grew = true;
        while grew {
            grew = false;
            for file in &self.files {
                for (_, declaration) in types_of(file) {
                    let Some(named) = declaration.child("name") else {
                        continue;
                    };
                    let here = (&file.uri, named.start);
                    let found = Found {
                        file,
                        node: declaration,
                        name: named,
                        bindings: Bindings::new(),
                    };
                    let supertypes: Vec<(&Url, Position)> = self
                        .supertypes(&found)
                        .iter()
                        .map(|supertype| (&supertype.file.uri, supertype.name.start))
                        .collect();
                    let touches = family.contains(&here)
                        || supertypes.iter().any(|edge| family.contains(edge));
                    if !touches {
                        continue;
                    }
                    for place in std::iter::once(here).chain(supertypes) {
                        if !family.contains(&place) {
                            family.push(place);
                            grew = true;
                        }
                    }
                }
            }
        }
        family
            .into_iter()
            .filter_map(|(uri, at)| {
                let file = self.file(uri)?;
                let declaration = types_of(file)
                    .into_iter()
                    .map(|(_, declaration)| declaration)
                    .find(|declaration| {
                        declaration.child("name").is_some_and(|named| named.start == at)
                    })?;
                Some(Found {
                    file,
                    node: declaration,
                    name: declaration.child("name")?,
                    bindings: Bindings::new(),
                })
            })
            .collect()
    }
}

fn is_constructor(node: &Syn) -> bool {
    matches!(
        node.kind.as_str(),
        "constructor_declaration" | "compact_constructor_declaration"
    )
}

impl Snapshot {
    /// Where a javadoc `@param` names a parameter: a rename changes the
    /// tag with it.
    pub fn javadoc_tags_of(&self, found: &Found<'_>) -> Vec<(&Url, Position)> {
        let is_parameter = matches!(
            found.node.kind.as_str(),
            "formal_parameter" | "spread_parameter"
        );
        if !is_parameter {
            return Vec::new();
        }
        let Some(method) = path_to(&found.file.tree, found.name.start)
            .into_iter()
            .rev()
            .find(|node| {
                matches!(
                    node.kind.as_str(),
                    "method_declaration" | "constructor_declaration"
                )
            })
        else {
            return Vec::new();
        };
        let Some(comment) = self.comment_above(found.file, method) else {
            return Vec::new();
        };
        let wanted = format!("@param {}", found.name.text);
        comment
            .text
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let tag = line.find(&wanted)?;
                let is_the_whole_word = line[tag + wanted.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric() && next != '_');
                is_the_whole_word.then(|| {
                    let column = line[..tag].encode_utf16().count() as u32
                        + "@param ".encode_utf16().count() as u32;
                    (
                        &self.file(&found.file.uri).expect("the file is open").uri,
                        Position {
                            line: comment.start.line + index as u32,
                            column: if index == 0 { comment.start.column + column } else { column },
                        },
                    )
                })
            })
            .collect()
    }

    /// The block comment directly above a declaration.
    /// The block comment right above a declaration: the last one before
    /// it with nothing but blank space between.
    fn comment_above<'a>(&self, file: &'a FileView, declaration: &Syn) -> Option<&'a Syn> {
        let comment = file
            .tree
            .walk()
            .into_iter()
            .filter(|node| node.kind == "block_comment" && node.end <= declaration.start)
            .max_by_key(|comment| comment.end)?;
        let lines: Vec<&str> = file.text.lines().collect();
        let between = (comment.end.line..=declaration.start.line).all(|line| {
            let Some(text) = lines.get(line as usize) else {
                return true;
            };
            let from = if line == comment.end.line { comment.end.column as usize } else { 0 };
            let to = if line == declaration.start.line {
                declaration.start.column as usize
            } else {
                usize::MAX
            };
            text.chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .all(char::is_whitespace)
        });
        between.then_some(comment)
    }
}

/// What implements what: the subtypes of a type, and the methods that
/// implement a method.
impl Snapshot {
    /// Every type declared below `of`, however deep.
    pub fn subtypes_of<'a>(&'a self, of: &Found<'a>) -> Vec<Found<'a>> {
        let wanted = (&of.file.uri, of.name.start);
        let extending = self.extending.get_or_init(|| {
            let mut listed = Vec::new();
            for (index, file) in self.files.iter().enumerate() {
                if !file.library {
                    extending_paths(&file.tree, &mut Vec::new(), &mut |path| listed.push((index, path)));
                }
            }
            listed
        });
        let mut below = Vec::new();
        for (index, path) in extending {
            let file = &self.files[*index];
            let Some(declaration) = at_path(&file.tree, path) else {
                continue;
            };
            let named = if is_anonymous_class(declaration) {
                declaration.child("type")
            } else {
                declaration.child("name")
            };
            let Some(named) = named else {
                continue;
            };
            if (&file.uri, named.start) == wanted {
                continue;
            }
            let candidate = Found {
                file,
                node: declaration,
                name: named,
                bindings: Bindings::new(),
            };
            // Through a library type too: `extends ArrayList` is a
            // `Collection`.
            if self
                .ancestors(&candidate)
                .iter()
                .any(|(uri, at)| (uri, *at) == (wanted.0, wanted.1))
            {
                below.push(candidate);
            }
        }
        below
    }

    /// Every type above `of`, however far, as places; worked out once per
    /// snapshot.
    fn ancestors(&self, of: &Found<'_>) -> &[(Url, Position)] {
        let key = std::ptr::from_ref(of.node) as usize;
        if let Some(known) = self.ancestors.get(&key) {
            return known;
        }
        let mut seen: Vec<(Url, Position)> = Vec::new();
        let mut queue: std::collections::VecDeque<Found<'_>> = self.supertypes(of).into();
        while let Some(supertype) = queue.pop_front() {
            let place = (supertype.file.uri.clone(), supertype.name.start);
            if seen.contains(&place) || seen.len() > 64 {
                continue;
            }
            seen.push(place);
            queue.extend(self.supertypes(&supertype));
        }
        self.ancestors.insert(key, seen)
    }

    /// The declarations `textDocument/implementation` answers with: the
    /// subtypes of a type, the methods implementing a method. A use of a
    /// type names the type too; its declaration does not.
    pub fn implementations_at(&self, uri: &Url, at: Position) -> Vec<Found<'_>> {
        let Some(found) = self.declaration_at(uri, at) else {
            return Vec::new();
        };
        let Some(file) = self.file(uri) else {
            return Vec::new();
        };
        let at_its_declaration = found.file.uri == *uri && (found.name.covers(at) || found.name.end == at);
        let itself = || Found {
            file: found.file,
            node: found.node,
            name: found.name,
            bindings: Bindings::new(),
        };
        if is_type_declaration(found.node) {
            // A use of a class names the class too; an interface or an
            // abstract class only when nothing is below it.
            let mut answer = Vec::new();
            let concrete = found.node.kind != "interface_declaration"
                && found.node.kind != "annotation_type_declaration"
                && !found
                    .node
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.text.split_whitespace().any(|word| word == "abstract"));
            if !at_its_declaration && concrete {
                answer.push(itself());
            }
            answer.extend(self.subtypes_of(&found));
            if answer.is_empty() && !at_its_declaration {
                answer.push(itself());
            }
            return answer;
        }
        if found.node.kind != "method_declaration" {
            return Vec::new();
        }
        // `super.m()` names one method: the one it calls.
        let through_super = path_to(&file.tree, at)
            .iter()
            .any(|node| node.children.iter().any(|child| child.kind == "super"));
        let mut answer = Vec::new();
        if !is_abstract(found.node) && !at_its_declaration {
            answer.push(itself());
        }
        if through_super {
            return answer;
        }
        let Some(owner) = self.owning_type(&found) else {
            return answer;
        };
        let wanted = self.erased_parameter_types(&found);
        for subtype in self.subtypes_of(&owner) {
            let implementing = members_of(subtype.node).into_iter().find(|(member, named)| {
                member.kind == "method_declaration"
                    && named.text == found.name.text
                    && !is_abstract(member)
                    && self.erased_parameter_types(&Found {
                        file: subtype.file,
                        node: member,
                        name: named,
                        bindings: Bindings::new(),
                    }) == wanted
            });
            if let Some((member, named)) = implementing {
                answer.push(Found {
                    file: subtype.file,
                    node: member,
                    name: named,
                    bindings: Bindings::new(),
                });
            }
        }
        if answer.is_empty() && !at_its_declaration {
            answer.push(found);
        }
        answer
    }
}

/// A method with no body of its own.
fn is_abstract(method: &Syn) -> bool {
    method.child("body").is_none()
}

/// What a call could mean, for signature help.
impl Snapshot {
    /// Every method of a type and its supertypes called `name`.
    pub fn members_called<'a>(&'a self, of: &Found<'a>, name: &str) -> Vec<Found<'a>> {
        let mut found = Vec::new();
        self.members_named(of, name, &mut found, &mut 0);
        found
    }

    /// The constructors of a type: the ones it declares, the canonical one
    /// of a record, or the default one. A constructor another class cannot
    /// call is left out.
    pub fn constructors_of<'a>(&'a self, of: &Found<'a>) -> Vec<Found<'a>> {
        let declared: Vec<Found<'a>> = members_of(of.node)
            .into_iter()
            .filter(|(member, _)| member.kind == "constructor_declaration")
            .filter(|(member, _)| !member.text.contains("private"))
            .map(|(member, named)| Found {
                file: of.file,
                node: member,
                name: named,
                bindings: of.bindings.clone(),
            })
            .collect();
        if !declared.is_empty() {
            return declared;
        }
        // A record's components are its canonical constructor; a class
        // with none gets the default one — the declaration itself stands
        // for both, as it has the parameters each would take.
        vec![Found {
            file: of.file,
            node: of.node,
            name: of.name,
            bindings: of.bindings.clone(),
        }]
    }

    pub fn supertype_of<'a>(&'a self, of: &Found<'a>) -> Option<Found<'a>> {
        self.supertypes(of).into_iter().next()
    }

    /// The parameters of a declaration as its language writes them, with
    /// the bindings of its receiver filled in.
    /// The types of a declaration's parameters, with the bindings of its
    /// receiver filled in: what tells one overload from another.
    pub fn parameter_types(&self, found: &Found<'_>) -> Vec<String> {
        parameters(found.node, &found.bindings)
            .into_iter()
            .map(|parameter| parameter.written)
            .collect()
    }

    /// A method's return type as written, with the bindings of its
    /// receiver filled in; `None` for a constructor.
    pub fn return_written(&self, found: &Found<'_>) -> Option<String> {
        if found.node.kind != "method_declaration" {
            return None;
        }
        let written = found.node.child("type").map(type_of_node)?;
        Some(substituted(written, &found.bindings).written())
    }

    pub fn parameters_written(&self, found: &Found<'_>) -> Vec<String> {
        let language = features(found.file.language);
        parameters(found.node, &found.bindings)
            .iter()
            .map(|parameter| language.parameter(parameter))
            .collect()
    }
}

/// What completion asks of the semantic layer.
impl Snapshot {
    /// A type and its supertypes, nearest first.
    pub fn with_supertypes<'a>(&'a self, of: &Found<'a>) -> Vec<Found<'a>> {
        let mut family = vec![Found {
            file: of.file,
            node: of.node,
            name: of.name,
            bindings: of.bindings.clone(),
        }];
        let mut at = 0;
        while at < family.len() && at < 8 {
            for supertype in self.supertypes(&family[at]) {
                let known = family.iter().any(|found| {
                    found.file.uri == supertype.file.uri
                        && found.name.start == supertype.name.start
                });
                if !known {
                    family.push(supertype);
                }
            }
            at += 1;
        }
        family
    }

    /// The type a `switch` around `at` switches on.
    pub fn switched_type<'a>(&'a self, file: &'a FileView, at: Position) -> Option<Found<'a>> {
        self.switch_of(file, at)
    }

    /// The locals and parameters in scope at `at`, nearest first.
    pub fn names_in_scope(&self, file: &FileView, at: Position) -> Vec<String> {
        let mut names = Vec::new();
        for ancestor in path_to(&file.tree, at).iter().rev() {
            let mut here: Vec<(Position, String)> = locals_of(ancestor)
                .into_iter()
                .filter(|(_, named)| named.start <= at)
                .map(|(_, named)| (named.start, named.text.clone()))
                .collect();
            here.sort_by(|left, right| right.0.cmp(&left.0));
            for (_, name) in here {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }
}

/// The package a file declares.
pub fn package_of(snapshot: &Snapshot, file: &FileView) -> Option<String> {
    snapshot.package(file)
}
