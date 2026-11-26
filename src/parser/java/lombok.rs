//! Lombok, the way jdtls sees it when it is started with Lombok (as
//! vscode-java starts it): the members Lombok generates are part of the
//! class, written nowhere.
//!
//! Lombok changes the compiler's tree while it is built, so jdtls finds a
//! generated getter like any method. Each generated member gets a place in
//! the source from the annotation that made it, and jdtls answers with
//! those places:
//!
//! - a getter's or setter's name is the field's name (definition, call
//!   hierarchy), a constructor's or a logger field's the whole annotation
//!   (`@RequiredArgsConstructor`, `@Slf4j`);
//! - the types a member mentions are the annotation's name (`Data`),
//!   except a setter's parameter, which is the whole annotation (`@Data`);
//! - what a member's body reads or writes of a field is the whole
//!   annotation.
//!
//! The members are written as Java, parsed with the same grammar and moved
//! to those places, then added to the class body under the field name
//! [`GENERATED`]. The tree's walks ([`Syn::walk`]) and the descent to a
//! cursor leave them out, so nothing searches, renames or colours them;
//! looking a member up by name finds them. Where jdtls shows what a body
//! does at the annotation (highlights), [`uses`] says so.
//!
//! Only Lombok's defaults are followed: no `lombok.config`, and of the
//! annotations, the ones that only add members from the class's own
//! fields.

use super::GRAMMAR;
use crate::parser::{Position, Syn, GENERATED};

/// What a generated member's body does with a field, at the annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    /// The field's written type stands here: in a getter's return type, a
    /// setter's or a constructor's parameter.
    Type,
}

/// A place where a generated member uses a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
    /// The start of the field's name where it is declared, which says
    /// which field.
    pub field: Position,
    pub start: Position,
    pub end: Position,
    pub access: Access,
}

/// One-line folds jdtls reports for the generated methods and
/// constructors: the lines of the annotation that made each.
pub fn folds(tree: &Syn) -> Vec<(u32, u32)> {
    let mut folds = Vec::new();
    for class in classes(tree) {
        for member in plan(tree, class).members {
            if member.kind != MemberKind::Field {
                folds.push((member.whole.0.line, member.whole.1.line));
            }
        }
    }
    folds
}

/// Every place a generated member uses a field of `tree`'s classes.
pub fn uses(tree: &Syn) -> Vec<Use> {
    classes(tree)
        .into_iter()
        .flat_map(|class| plan(tree, class).uses)
        .collect()
}

/// The annotations of Lombok's this module knows what to generate for.
const MODELLED: &[&str] = &[
    "Data",
    "Value",
    "Getter",
    "Setter",
    "NonNull",
    "ToString",
    "EqualsAndHashCode",
    "NoArgsConstructor",
    "RequiredArgsConstructor",
    "AllArgsConstructor",
    "extern.slf4j.Slf4j",
    "extern.slf4j.XSlf4j",
    "extern.log4j.Log4j2",
    "extern.log4j.Log4j",
    "extern.java.Log",
    "extern.apachecommons.CommonsLog",
    "extern.jbosslog.JBossLog",
];

/// Whether every annotation of Lombok's in `tree` is one this module
/// generates the members of: otherwise what Lombok generates is not all
/// in the tree.
pub fn understood(tree: &Syn) -> bool {
    let imports = imports(tree);
    tree.walk()
        .into_iter()
        .filter(|node| matches!(node.kind.as_str(), "marker_annotation" | "annotation"))
        .filter_map(|annotation| lombok_name(&annotation.child("name")?.text, &imports))
        .all(|name| MODELLED.contains(&name.as_str()))
        && !imports.iter().any(|import| import.starts_with("lombok.") && {
            let name = &import["lombok.".len()..];
            name != "*" && !MODELLED.contains(&name) && name != "AccessLevel"
        })
}

/// Adds the members Lombok generates to every class of `tree`.
pub fn expand(tree: &mut Syn) {
    if !tree.text.contains("lombok") {
        return;
    }
    let imports = imports(tree);
    expand_in(tree, &imports);
}

fn expand_in(node: &mut Syn, imports: &[String]) {
    for child in &mut node.children {
        expand_in(child, imports);
    }
    if node.kind != "class_declaration" {
        return;
    }
    let plan = plan_with(node, imports);
    if plan.members.is_empty() {
        return;
    }
    let generated = build(&plan.members);
    let Some(body) = node.children.iter_mut().find(|child| child.kind == "class_body") else {
        return;
    };
    let closing = body.children.iter().rposition(|child| child.kind == "}");
    let at = closing.unwrap_or(body.children.len());
    body.children.splice(at..at, generated);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberKind {
    Method,
    Constructor,
    Field,
}

/// One member Lombok generates: its source, and where jdtls puts it.
struct Member {
    kind: MemberKind,
    source: String,
    /// The annotation that made it, `@` included.
    whole: (Position, Position),
    /// Where its name is.
    name_at: (Position, Position),
    /// Where the types it mentions are.
    types_at: (Position, Position),
    /// Where its parameters are.
    parameters_at: (Position, Position),
}

struct Plan {
    members: Vec<Member>,
    uses: Vec<Use>,
}

/// An annotation of Lombok's on a class or a field.
#[derive(Clone)]
struct Annotation {
    /// Its name below `lombok.`: `Data`, `extern.slf4j.Slf4j`.
    name: String,
    /// What it is given, as written: `(AccessLevel.NONE)`.
    arguments: String,
    whole: (Position, Position),
    name_at: (Position, Position),
}

struct Field {
    name: String,
    name_at: (Position, Position),
    written_type: String,
    is_static: bool,
    is_final: bool,
    initialized: bool,
    /// `@NonNull` and the other annotations Lombok copies onto a
    /// parameter, as written.
    non_null: Option<String>,
    annotations: Vec<Annotation>,
}

fn classes(tree: &Syn) -> Vec<&Syn> {
    if !tree.text.contains("lombok") {
        return Vec::new();
    }
    tree.walk()
        .into_iter()
        .filter(|node| node.kind == "class_declaration")
        .collect()
}

fn plan(tree: &Syn, class: &Syn) -> Plan {
    plan_with(class, &imports(tree))
}

fn imports(tree: &Syn) -> Vec<String> {
    tree.children
        .iter()
        .filter(|child| child.kind == "import_declaration")
        .map(|import| {
            import
                .text
                .trim_start_matches("import")
                .trim_end_matches(';')
                .split_whitespace()
                .collect::<String>()
        })
        .collect()
}

/// Lombok's name for an annotation written `written`, where the imports
/// make it one of Lombok's.
fn lombok_name(written: &str, imports: &[String]) -> Option<String> {
    if let Some(name) = written.strip_prefix("lombok.") {
        return Some(name.to_owned());
    }
    if written.contains('.') {
        return None;
    }
    imports.iter().find_map(|import| {
        let name = import.strip_prefix("lombok.")?;
        if name.rsplit('.').next() == Some(written) {
            return Some(name.to_owned());
        }
        // `import lombok.*;` brings the top-level ones only.
        (name == "*").then(|| written.to_owned())
    })
}

fn annotations_of(declaration: &Syn, imports: &[String]) -> Vec<Annotation> {
    let Some(modifiers) = declaration.first("modifiers") else {
        return Vec::new();
    };
    modifiers
        .children
        .iter()
        .filter(|child| matches!(child.kind.as_str(), "marker_annotation" | "annotation"))
        .filter_map(|annotation| {
            let named = annotation.child("name")?;
            Some(Annotation {
                name: lombok_name(&named.text, imports)?,
                arguments: annotation
                    .child("arguments")
                    .map(|arguments| arguments.text.clone())
                    .unwrap_or_default(),
                whole: (annotation.start, annotation.end),
                name_at: (named.start, named.end),
            })
        })
        .collect()
}

fn has_modifier(declaration: &Syn, modifier: &str) -> bool {
    declaration
        .first("modifiers")
        .is_some_and(|modifiers| modifiers.children.iter().any(|child| child.kind == modifier))
}

fn fields_of(body: &Syn, imports: &[String]) -> Vec<Field> {
    let mut fields = Vec::new();
    for declaration in body
        .children
        .iter()
        .filter(|child| child.kind == "field_declaration" && !child.is_generated())
    {
        let Some(written_type) = declaration.child("type") else {
            continue;
        };
        let annotations = annotations_of(declaration, imports);
        let non_null = declaration.first("modifiers").and_then(|modifiers| {
            modifiers
                .children
                .iter()
                .filter(|child| matches!(child.kind.as_str(), "marker_annotation" | "annotation"))
                .find(|annotation| {
                    annotation
                        .child("name")
                        .is_some_and(|name| name.text.rsplit('.').next() == Some("NonNull"))
                })
                .map(|annotation| annotation.text.clone())
        });
        for declarator in declaration.all("variable_declarator") {
            let Some(name) = declarator.child("name") else {
                continue;
            };
            if name.text.starts_with('$') {
                continue;
            }
            fields.push(Field {
                name: name.text.clone(),
                name_at: (name.start, name.end),
                written_type: written_type.text.clone(),
                is_static: has_modifier(declaration, "static"),
                is_final: has_modifier(declaration, "final"),
                initialized: declarator.child("value").is_some(),
                non_null: non_null.clone(),
                annotations: annotations.clone(),
            });
        }
    }
    fields
}

fn capitalized(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The getter and setter names of a field: `getName`/`setName`, and for a
/// `boolean` `isActive`/`setActive`, also for one already called
/// `isActive`. Lombok and Eclipse's accessor search name them alike.
pub fn accessor_names(field: &str, written_type: &str) -> (String, String) {
    if written_type == "boolean" {
        if let Some(rest) = field.strip_prefix("is") {
            if rest.starts_with(|character: char| character.is_uppercase()) {
                return (field.to_owned(), format!("set{rest}"));
            }
        }
        return (format!("is{}", capitalized(field)), format!("set{}", capitalized(field)));
    }
    (format!("get{}", capitalized(field)), format!("set{}", capitalized(field)))
}

fn getter_name(field: &Field) -> String {
    accessor_names(&field.name, &field.written_type).0
}

fn setter_name(field: &Field) -> String {
    accessor_names(&field.name, &field.written_type).1
}

/// The modifier an access level written in an annotation's arguments asks
/// for; `None` for `AccessLevel.NONE`, which asks for no member.
fn access(arguments: &str) -> Option<&'static str> {
    if arguments.contains("NONE") {
        None
    } else if arguments.contains("PROTECTED") {
        Some("protected ")
    } else if arguments.contains("PRIVATE") {
        Some("private ")
    } else if arguments.contains("PACKAGE") {
        Some("")
    } else {
        Some("public ")
    }
}

fn plan_with(class: &Syn, imports: &[String]) -> Plan {
    let mut plan = Plan {
        members: Vec::new(),
        uses: Vec::new(),
    };
    let Some(body) = class.first("class_body") else {
        return plan;
    };
    let annotations = annotations_of(class, imports);
    let fields = fields_of(body, imports);
    if annotations.is_empty() && fields.iter().all(|field| field.annotations.is_empty()) {
        return plan;
    }
    let Some(class_name) = class.child("name") else {
        return plan;
    };
    let written_methods: Vec<&str> = body
        .children
        .iter()
        .filter(|child| child.kind == "method_declaration" && !child.is_generated())
        .filter_map(|method| method.child("name").map(|name| name.text.as_str()))
        .collect();
    let has_constructor = body
        .children
        .iter()
        .any(|child| child.kind == "constructor_declaration" && !child.is_generated());
    let written_fields: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    let on_class = |name: &str| annotations.iter().find(|annotation| annotation.name == name);
    let data = on_class("Data");
    let value = on_class("Value");
    let mut taken: Vec<String> = written_methods.iter().map(|name| (*name).to_owned()).collect();

    // Loggers first: they are fields, and the order of members is the
    // order jdtls lists them in.
    const LOGGERS: &[(&str, &str)] = &[
        ("extern.slf4j.Slf4j", "org.slf4j.Logger"),
        ("extern.slf4j.XSlf4j", "org.slf4j.ext.XLogger"),
        ("extern.log4j.Log4j2", "org.apache.logging.log4j.Logger"),
        ("extern.log4j.Log4j", "org.apache.log4j.Logger"),
        ("extern.java.Log", "java.util.logging.Logger"),
        ("extern.apachecommons.CommonsLog", "org.apache.commons.logging.Log"),
        ("extern.jbosslog.JBossLog", "org.jboss.logging.Logger"),
    ];
    for (annotation_name, logger) in LOGGERS {
        if let Some(annotation) = on_class(annotation_name) {
            if !written_fields.contains(&"log") {
                plan.members.push(Member {
                    kind: MemberKind::Field,
                    source: format!("private static final {logger} log = null;"),
                    whole: annotation.whole,
                    name_at: annotation.whole,
                    types_at: annotation.whole,
                    parameters_at: annotation.whole,
                });
            }
        }
    }

    let class_getter = data.or(value).or_else(|| on_class("Getter"));
    let class_setter = data.or_else(|| on_class("Setter")).filter(|_| value.is_none());
    for field in fields.iter().filter(|field| !field.is_static) {
        let field_getter = field.annotations.iter().find(|annotation| annotation.name == "Getter");
        if let Some(annotation) = field_getter.or(class_getter) {
            let name = getter_name(field);
            if let Some(modifier) = access(&annotation.arguments).filter(|_| !taken.contains(&name)) {
                plan.members.push(Member {
                    kind: MemberKind::Method,
                    source: format!("{modifier}{} {name}() {{}}", field.written_type),
                    whole: annotation.whole,
                    name_at: field.name_at,
                    types_at: annotation.name_at,
                    parameters_at: annotation.whole,
                });
                plan.uses.push(read(field, annotation.whole));
                plan.uses.push(type_use(field, annotation.name_at));
                taken.push(name);
            }
        }
        let field_setter = field.annotations.iter().find(|annotation| annotation.name == "Setter");
        if let Some(annotation) = field_setter.or(class_setter).filter(|_| !field.is_final) {
            let name = setter_name(field);
            if let Some(modifier) = access(&annotation.arguments).filter(|_| !taken.contains(&name)) {
                let annotated = field.non_null.as_deref().map(|written| format!("{written} ")).unwrap_or_default();
                plan.members.push(Member {
                    kind: MemberKind::Method,
                    source: format!(
                        "{modifier}void {name}({annotated}{} {}) {{}}",
                        field.written_type, field.name
                    ),
                    whole: annotation.whole,
                    name_at: field.name_at,
                    types_at: annotation.name_at,
                    parameters_at: annotation.whole,
                });
                plan.uses.push(Use {
                    field: field.name_at.0,
                    start: annotation.whole.0,
                    end: annotation.whole.1,
                    access: Access::Write,
                });
                plan.uses.push(type_use(field, annotation.whole));
                if field.non_null.is_some() {
                    plan.uses.push(read(field, annotation.whole));
                }
                taken.push(name);
            }
        }
    }

    // Constructors, in the order Lombok handles their annotations.
    let instance: Vec<&Field> = fields.iter().filter(|field| !field.is_static).collect();
    let required: Vec<&Field> = instance
        .iter()
        .copied()
        .filter(|field| !field.initialized && (field.is_final || value.is_some() || field.non_null.is_some()))
        .collect();
    let all: Vec<&Field> = instance
        .iter()
        .copied()
        .filter(|field| !(field.initialized && (field.is_final || value.is_some())))
        .collect();
    let explicit = ["NoArgsConstructor", "RequiredArgsConstructor", "AllArgsConstructor"];
    let any_explicit = explicit.iter().any(|name| on_class(name).is_some());
    let mut constructors: Vec<(&Annotation, Vec<&Field>)> = Vec::new();
    if let Some(annotation) = on_class("NoArgsConstructor") {
        constructors.push((annotation, Vec::new()));
    }
    if let Some(annotation) = on_class("RequiredArgsConstructor") {
        constructors.push((annotation, required.clone()));
    }
    if let Some(annotation) = on_class("AllArgsConstructor") {
        constructors.push((annotation, all.clone()));
    }
    if !has_constructor && !any_explicit {
        if let Some(annotation) = value {
            constructors.push((annotation, all.clone()));
        } else if let Some(annotation) = data {
            constructors.push((annotation, required.clone()));
        }
    }
    let mut arities: Vec<usize> = Vec::new();
    for (annotation, parameters) in constructors {
        // Two constructors with the same parameters would be one.
        if arities.contains(&parameters.len()) && parameters.is_empty() {
            continue;
        }
        arities.push(parameters.len());
        let Some(modifier) = access(&annotation.arguments) else {
            continue;
        };
        let written: Vec<String> = parameters
            .iter()
            .map(|field| {
                let annotated = field.non_null.as_deref().map(|written| format!("{written} ")).unwrap_or_default();
                format!("{annotated}{} {}", field.written_type, field.name)
            })
            .collect();
        plan.members.push(Member {
            kind: MemberKind::Constructor,
            source: format!("{modifier}{}({}) {{}}", class_name.text, written.join(", ")),
            whole: annotation.whole,
            name_at: annotation.whole,
            types_at: annotation.name_at,
            parameters_at: annotation.name_at,
        });
        for field in parameters {
            plan.uses.push(Use {
                field: field.name_at.0,
                start: annotation.whole.0,
                end: annotation.whole.1,
                access: Access::Write,
            });
            plan.uses.push(type_use(field, annotation.name_at));
            if field.non_null.is_some() {
                plan.uses.push(read(field, annotation.whole));
            }
        }
    }

    let to_string = data.or(value).or_else(|| on_class("ToString"));
    if let Some(annotation) = to_string.filter(|_| !taken.iter().any(|name| name == "toString")) {
        plan.members.push(method(annotation, "public String toString() {}"));
        for field in &instance {
            plan.uses.push(read(field, annotation.whole));
        }
    }
    let equality = data.or(value).or_else(|| on_class("EqualsAndHashCode"));
    if let Some(annotation) = equality.filter(|_| !taken.iter().any(|name| name == "equals" || name == "hashCode")) {
        plan.members.push(method(annotation, "public boolean equals(final Object o) {}"));
        plan.members.push(method(annotation, "protected boolean canEqual(final Object other) {}"));
        plan.members.push(method(annotation, "public int hashCode() {}"));
        for field in &instance {
            plan.uses.push(read(field, annotation.whole));
        }
    }
    plan
}

fn method(annotation: &Annotation, source: &str) -> Member {
    Member {
        kind: MemberKind::Method,
        source: source.to_owned(),
        whole: annotation.whole,
        name_at: annotation.name_at,
        types_at: annotation.name_at,
        parameters_at: annotation.whole,
    }
}

fn read(field: &Field, at: (Position, Position)) -> Use {
    Use {
        field: field.name_at.0,
        start: at.0,
        end: at.1,
        access: Access::Read,
    }
}

fn type_use(field: &Field, at: (Position, Position)) -> Use {
    Use {
        field: field.name_at.0,
        start: at.0,
        end: at.1,
        access: Access::Type,
    }
}

/// The members, parsed and moved to their places.
fn build(members: &[Member]) -> Vec<Syn> {
    let mut source = String::from("class Lombok {\n");
    for member in members {
        source.push_str(&member.source);
        source.push('\n');
    }
    source.push('}');
    let Some(tree) = GRAMMAR.snapshot(&source) else {
        return Vec::new();
    };
    let Some(body) = tree
        .children
        .into_iter()
        .find(|child| child.kind == "class_declaration")
        .and_then(|class| class.children.into_iter().find(|child| child.kind == "class_body"))
    else {
        return Vec::new();
    };
    body.children
        .into_iter()
        .filter(|child| !matches!(child.kind.as_str(), "{" | "}"))
        .zip(members)
        .map(|(mut node, member)| {
            place(&mut node, member.whole);
            for child in &mut node.children {
                match child.field.as_deref() {
                    Some("name") => place(child, member.name_at),
                    Some("type") => place(child, member.types_at),
                    Some("parameters") => {
                        place(child, member.parameters_at);
                        for parameter in &mut child.children {
                            if let Some(written) = parameter.children.iter_mut().find(|part| part.field.as_deref() == Some("type")) {
                                place(written, member.types_at);
                            }
                        }
                    }
                    _ => {}
                }
                if child.kind == "variable_declarator" {
                    for part in &mut child.children {
                        if part.field.as_deref() == Some("name") {
                            place(part, member.name_at);
                        }
                    }
                }
            }
            node.field = Some(GENERATED.to_owned());
            node
        })
        .collect()
}

/// Moves a node and everything in it to `at`.
fn place(node: &mut Syn, at: (Position, Position)) {
    node.start = at.0;
    node.end = at.1;
    for child in &mut node.children {
        place(child, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Syntax;

    fn generated(source: &str) -> Vec<String> {
        let tree = super::super::Java.tree(source);
        tree.walk()
            .into_iter()
            .filter_map(|node| node.first("class_body"))
            .flat_map(|body| body.children.iter().filter(|child| child.is_generated()))
            .map(|member| member.text.clone())
            .collect()
    }

    #[test]
    fn accessors_are_named_as_lombok_names_them() {
        assert_eq!(accessor_names("name", "String"), ("getName".to_owned(), "setName".to_owned()));
        assert_eq!(accessor_names("active", "boolean"), ("isActive".to_owned(), "setActive".to_owned()));
        assert_eq!(accessor_names("isActive", "boolean"), ("isActive".to_owned(), "setActive".to_owned()));
        assert_eq!(accessor_names("isolated", "boolean"), ("isIsolated".to_owned(), "setIsolated".to_owned()));
        assert_eq!(accessor_names("active", "Boolean"), ("getActive".to_owned(), "setActive".to_owned()));
    }

    #[test]
    fn data_generates_accessors_a_constructor_and_the_object_methods() {
        let members = generated(
            "import lombok.Data;\n@Data\nclass P {\n  private String name;\n  private final Long id;\n  private final int n = 1;\n  private static int count;\n  public String getName() { return name; }\n}\n",
        );
        assert_eq!(
            members,
            [
                "public void setName(String name) {}",
                "public Long getId() {}",
                "public int getN() {}",
                "public P(Long id) {}",
                "public String toString() {}",
                "public boolean equals(final Object o) {}",
                "protected boolean canEqual(final Object other) {}",
                "public int hashCode() {}",
            ]
        );
    }

    #[test]
    fn an_annotation_of_another_library_generates_nothing() {
        let members = generated(
            "import org.springframework.beans.factory.annotation.Value;\nimport lombok.Getter;\nclass C {\n  @Value(\"${x}\")\n  private String x;\n}\n",
        );
        assert!(members.is_empty(), "{members:?}");
    }

    #[test]
    fn a_written_constructor_stops_the_one_data_would_add() {
        let members = generated("import lombok.*;\n@Data\nclass P {\n  private final Long id;\n  P() { id = 1L; }\n}\n");
        assert!(!members.iter().any(|member| member.starts_with("public P(")), "{members:?}");
    }

    #[test]
    fn slf4j_adds_a_logger_field_at_the_annotation() {
        let tree = super::super::Java.tree("import lombok.extern.slf4j.Slf4j;\n@Slf4j\nclass S {\n}\n");
        let log = tree.children[1].first("class_body").unwrap().children.iter().find(|child| child.is_generated()).unwrap();
        assert_eq!(log.text, "private static final org.slf4j.Logger log = null;");
        assert_eq!((log.start, log.end), (Position { line: 1, column: 0 }, Position { line: 1, column: 6 }));
    }
}
