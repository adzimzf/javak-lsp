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
//! - everything `@Builder` makes — `builder()`, `toBuilder()`, the builder
//!   class and all of its members — sits at the annotation, from its `@`
//!   to the end of its name;
//! - the types a member mentions are the annotation's name (`Data`),
//!   except `toString`'s `String`, the whole annotation (`@Data`), and a
//!   setter's parameter, which is the field's own type where it is
//!   written;
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
    /// The field's written type stands here: in a getter's return type or
    /// a constructor's parameter.
    Type,
    /// `java.lang.String` stands here, `toString`'s return type; `field`
    /// is the annotation's start.
    String,
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
            if matches!(member.kind, MemberKind::Method | MemberKind::Constructor) {
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
    "Builder",
    "Builder.Default",
    "Singular",
    "experimental.SuperBuilder",
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
    /// A nested class, every member of it generated as well.
    Type,
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
    // `@Builder.Default`: a member of an annotation of Lombok's.
    if let Some((outer, inner)) = written.split_once('.') {
        if !outer.starts_with(char::is_uppercase) {
            return None;
        }
        return Some(format!("{}.{inner}", lombok_name(outer, imports)?));
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
    let builder = on_class("Builder");
    let super_builder = on_class("experimental.SuperBuilder");
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
                // The parameter's type is the field's own type node,
                // copied with its place: no use apart from the field's.
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
    // `None` for the access level the annotation's arguments ask for.
    let mut constructors: Vec<(&Annotation, Vec<&Field>, Option<&'static str>)> = Vec::new();
    if let Some(annotation) = on_class("NoArgsConstructor") {
        constructors.push((annotation, Vec::new(), None));
    }
    if let Some(annotation) = on_class("RequiredArgsConstructor") {
        constructors.push((annotation, required.clone(), None));
    }
    if let Some(annotation) = on_class("AllArgsConstructor") {
        constructors.push((annotation, all.clone(), None));
    }
    if let Some(annotation) = super_builder {
        // What the builder's `build()` calls: the builder itself.
        constructors.push((annotation, Vec::new(), Some("protected ")));
    } else if !has_constructor && !any_explicit {
        if let Some(annotation) = value {
            constructors.push((annotation, all.clone(), None));
        } else if let Some(annotation) = builder {
            // What `build()` calls: every field, package-private.
            constructors.push((annotation, all.clone(), Some("")));
        } else if let Some(annotation) = data {
            constructors.push((annotation, required.clone(), None));
        }
    }
    let mut arities: Vec<usize> = Vec::new();
    for (annotation, parameters, modifier) in constructors {
        if super_builder.is_some_and(|written| std::ptr::eq(written, annotation)) {
            let declared = class.child("type_parameters").map(|written| written.text.clone()).unwrap_or_default();
            let arguments = wildcard_builder_arguments(&declared);
            let builder_class = builder_argument(&annotation.arguments, "builderClassName")
                .unwrap_or_else(|| format!("{}Builder", class_name.text));
            plan.members.push(Member {
                kind: MemberKind::Constructor,
                source: format!("{}{}({builder_class}{arguments} b) {{}}", modifier.unwrap_or_default(), class_name.text),
                whole: (annotation.whole.0, annotation.name_at.1),
                name_at: (annotation.whole.0, annotation.name_at.1),
                types_at: (annotation.whole.0, annotation.name_at.1),
                parameters_at: (annotation.whole.0, annotation.name_at.1),
            });
            for field in &instance {
                plan.uses.push(Use {
                    field: field.name_at.0,
                    start: annotation.whole.0,
                    end: annotation.whole.1,
                    access: Access::Write,
                });
            }
            continue;
        }
        // Two constructors with the same parameters would be one.
        if arities.contains(&parameters.len()) && parameters.is_empty() {
            continue;
        }
        arities.push(parameters.len());
        let Some(modifier) = modifier.or_else(|| access(&annotation.arguments)) else {
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

    if let Some(annotation) = builder {
        plan_builder(&mut plan, annotation, class, &class_name.text, &instance, &taken, false);
    } else if let Some(annotation) = super_builder {
        plan_builder(&mut plan, annotation, class, &class_name.text, &instance, &taken, true);
    }

    let to_string = data.or(value).or_else(|| on_class("ToString"));
    if let Some(annotation) = to_string.filter(|_| !taken.iter().any(|name| name == "toString")) {
        plan.members.push(method(annotation, "public String toString() {}"));
        plan.uses.push(Use {
            field: annotation.whole.0,
            start: annotation.whole.0,
            end: annotation.whole.1,
            access: Access::String,
        });
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

/// What `@Builder` on a class generates: `builder()`, `toBuilder()` where
/// asked for, and the builder class, with a field and a setter named as
/// each field is (a `@Singular` collection's adders and `clear…`, a
/// `@Builder.Default` field's value and whether it was set), `build()` and
/// `toString()`. jdtls puts all of them at the annotation, from its `@` to
/// the end of its name.
///
/// `@SuperBuilder` (`hierarchical`) makes the builder an abstract class
/// `XBuilder<C extends X, B extends XBuilder<C, B>>` that extends the
/// superclass's builder, its setters returning `B`, with `self()`, an
/// abstract `build()` and the `XBuilderImpl` that `builder()` returns.
#[allow(clippy::too_many_arguments)]
fn plan_builder(
    plan: &mut Plan,
    annotation: &Annotation,
    class: &Syn,
    class_name: &str,
    instance: &[&Field],
    taken: &[String],
    hierarchical: bool,
) {
    let at = (annotation.whole.0, annotation.name_at.1);
    let argument = |name: &str| builder_argument(&annotation.arguments, name);
    let builder_class = argument("builderClassName").unwrap_or_else(|| format!("{class_name}Builder"));
    let builder_method = argument("builderMethodName").unwrap_or_else(|| "builder".to_owned());
    let build_method = argument("buildMethodName").unwrap_or_else(|| "build".to_owned());
    let to_builder = argument("toBuilder").is_some_and(|value| value == "true");
    let declared = class.child("type_parameters").map(|written| written.text.clone()).unwrap_or_default();
    let arguments = type_parameter_names(&declared);
    // What `builder()` and `toBuilder()` return, and what a setter does.
    let (made_type, builder_type) = if hierarchical {
        (format!("{builder_class}{}", wildcard_builder_arguments(&declared)), "B".to_owned())
    } else {
        (format!("{builder_class}{arguments}"), format!("{builder_class}{arguments}"))
    };
    let generic = if declared.is_empty() { String::new() } else { format!("{declared} ") };
    let member = |kind: MemberKind, source: String| Member {
        kind,
        source,
        whole: at,
        name_at: at,
        types_at: at,
        parameters_at: at,
    };
    if !builder_method.is_empty() && !taken.contains(&builder_method) {
        plan.members.push(member(
            MemberKind::Method,
            format!("public static {generic}{made_type} {builder_method}() {{}}"),
        ));
    }
    if to_builder && !taken.iter().any(|name| name == "toBuilder") {
        plan.members.push(member(MemberKind::Method, format!("public {made_type} toBuilder() {{}}")));
        for field in instance {
            plan.uses.push(read(field, annotation.whole));
        }
    }
    let mut fields = String::new();
    let mut setters = String::new();
    for field in instance.iter().filter(|field| !(field.is_final && field.initialized) || has(field, "Builder.Default")) {
        let (name, written) = (&field.name, &field.written_type);
        let singular = field.annotations.iter().find(|annotation| annotation.name == "Singular");
        if let Some(singular) = singular {
            let written_name = if singular.arguments.contains('=') {
                builder_argument(&singular.arguments, "value")
            } else {
                Some(singular.arguments.trim_matches(|c: char| matches!(c, '(' | ')' | '"') || c.is_whitespace()).to_owned())
            };
            let one = written_name.filter(|one| !one.is_empty()).or_else(|| singular_of(name));
            let Some(one) = one else {
                continue;
            };
            let elements = type_arguments(written);
            let clear = format!("clear{}", capitalized(name));
            if written.split('<').next().is_some_and(|raw| raw.trim().ends_with("Map")) {
                let key = elements.first().cloned().unwrap_or_else(|| "Object".to_owned());
                let value = elements.get(1).cloned().unwrap_or_else(|| "Object".to_owned());
                fields.push_str(&format!("private java.util.ArrayList<{key}> {name}$key;\nprivate java.util.ArrayList<{value}> {name}$value;\n"));
                setters.push_str(&format!(
                    "public {builder_type} {one}({key} {one}Key, {value} {one}Value) {{}}\n\
                     public {builder_type} {name}(java.util.Map<? extends {key}, ? extends {value}> {name}) {{}}\n\
                     public {builder_type} {clear}() {{}}\n"
                ));
            } else {
                let element = elements.first().cloned().unwrap_or_else(|| "Object".to_owned());
                fields.push_str(&format!("private java.util.ArrayList<{element}> {name};\n"));
                setters.push_str(&format!(
                    "public {builder_type} {one}({element} {one}) {{}}\n\
                     public {builder_type} {name}(java.util.Collection<? extends {element}> {name}) {{}}\n\
                     public {builder_type} {clear}() {{}}\n"
                ));
            }
        } else {
            if has(field, "Builder.Default") {
                fields.push_str(&format!("private {written} {name}$value;\nprivate boolean {name}$set;\n"));
            } else {
                fields.push_str(&format!("private {written} {name};\n"));
            }
            setters.push_str(&format!("public {builder_type} {name}({written} {name}) {{}}\n"));
        }
    }
    if hierarchical {
        let names = type_parameter_names(&declared);
        let own = names.trim_start_matches('<').trim_end_matches('>');
        let leading = if own.is_empty() { String::new() } else { format!("{own}, ") };
        let parent = class
            .child("superclass")
            .and_then(|superclass| superclass.children.iter().find(|child| child.kind != "extends"))
            .map(|written| {
                let (raw, given) = match written.text.split_once('<') {
                    Some((raw, rest)) => (raw.trim(), format!("{}, ", rest.trim_end().trim_end_matches('>'))),
                    None => (written.text.trim(), String::new()),
                };
                let simple = raw.rsplit('.').next().unwrap_or(raw);
                format!(" extends {raw}.{simple}Builder<{given}C, B>")
            })
            .unwrap_or_default();
        let bounds = declared.trim().trim_start_matches('<').trim_end_matches('>').trim();
        let bounds = if bounds.is_empty() { String::new() } else { format!("{bounds}, ") };
        let source = format!(
            "public static abstract class {builder_class}<{bounds}C extends {class_name}{arguments}, \
             B extends {builder_class}<{leading}C, B>>{parent} {{\n{fields}public {builder_class}() {{}}\n{setters}\
             protected abstract B self();\npublic abstract C {build_method}();\npublic String toString() {{}}\n}}"
        );
        plan.members.push(member(MemberKind::Type, source));
        let own_type = format!("{class_name}{arguments}");
        let implementation = format!("{builder_class}Impl");
        let source = format!(
            "private static final class {implementation}{declared} extends {builder_class}<{leading}{own_type}, {implementation}{arguments}> {{\n\
             private {implementation}() {{}}\nprotected {implementation}{arguments} self() {{}}\npublic {own_type} {build_method}() {{}}\n}}"
        );
        plan.members.push(member(MemberKind::Type, source));
        return;
    }
    let source = format!(
        "public static class {builder_class}{declared} {{\n{fields}{builder_class}() {{}}\n{setters}\
         public {class_name}{arguments} {build_method}() {{}}\npublic String toString() {{}}\n}}"
    );
    plan.members.push(member(MemberKind::Type, source));
}

fn has(field: &Field, annotation: &str) -> bool {
    field.annotations.iter().any(|written| written.name == annotation)
}

/// The value `name = …` is given in an annotation's arguments, quotes
/// left out.
fn builder_argument(arguments: &str, name: &str) -> Option<String> {
    let inner = arguments.trim().strip_prefix('(')?.strip_suffix(')')?;
    inner.split(',').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().trim_matches('"').to_owned())
    })
}

/// The arguments of the builder `@SuperBuilder`'s `builder()` returns:
/// `<?, ?>`, after the class's own type parameters (`<K, V, ?, ?>`).
fn wildcard_builder_arguments(declared: &str) -> String {
    let names = type_parameter_names(declared);
    let own = names.trim_start_matches('<').trim_end_matches('>');
    if own.is_empty() {
        "<?, ?>".to_owned()
    } else {
        format!("<{own}, ?, ?>")
    }
}

/// `<K, V>` of a class's `<K extends Comparable<K>, V>`.
fn type_parameter_names(declared: &str) -> String {
    let inner = declared.trim().trim_start_matches('<').trim_end_matches('>');
    if inner.trim().is_empty() {
        return String::new();
    }
    let names: Vec<&str> = split_top_level(inner)
        .into_iter()
        .filter_map(|parameter| parameter.split_whitespace().next())
        .collect();
    format!("<{}>", names.join(", "))
}

/// The type arguments of `Map<String, List<Integer>>`, as written.
fn type_arguments(written: &str) -> Vec<String> {
    let Some((_, after)) = written.split_once('<') else {
        return Vec::new();
    };
    let inner = after.trim_end().strip_suffix('>').unwrap_or(after);
    split_top_level(inner).into_iter().map(|argument| argument.trim().to_owned()).collect()
}

/// `text` split at the commas no `<…>` holds.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (index, character) in text.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The name of one element of a `@Singular` collection called `plural`,
/// as Lombok makes it of an English plural; `None` where it cannot.
fn singular_of(plural: &str) -> Option<String> {
    if let Some(stem) = plural.strip_suffix("ies") {
        return Some(format!("{stem}y"));
    }
    for ending in ["sses", "shes", "ches", "xes"] {
        if plural.ends_with(ending) {
            return Some(plural[..plural.len() - 2].to_owned());
        }
    }
    plural
        .strip_suffix('s')
        .filter(|stem| !stem.is_empty() && !stem.ends_with('s'))
        .map(str::to_owned)
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
            if member.kind == MemberKind::Type {
                // The builder's own members are generated too.
                for body in node.children.iter_mut().filter(|child| child.kind == "class_body") {
                    for inner in body.children.iter_mut().filter(|inner| !matches!(inner.kind.as_str(), "{" | "}")) {
                        inner.field = Some(GENERATED.to_owned());
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
    fn builder_generates_a_builder_class_with_a_setter_per_field() {
        let members = generated(
            "import java.util.List;\nimport lombok.Builder;\nimport lombok.Singular;\n@Builder(toBuilder = true)\nclass Order {\n  private String customer;\n  @Builder.Default\n  private boolean express = false;\n  @Singular\n  private List<String> items;\n}\n",
        );
        assert_eq!(members[0], "Order(String customer, boolean express, List<String> items) {}");
        assert_eq!(members[1], "public static OrderBuilder builder() {}");
        assert_eq!(members[2], "public OrderBuilder toBuilder() {}");
        let builder = &members[3];
        for expected in [
            "public static class OrderBuilder {",
            "private boolean express$value;",
            "private boolean express$set;",
            "private java.util.ArrayList<String> items;",
            "public OrderBuilder customer(String customer) {}",
            "public OrderBuilder item(String item) {}",
            "public OrderBuilder items(java.util.Collection<? extends String> items) {}",
            "public OrderBuilder clearItems() {}",
            "public Order build() {}",
        ] {
            assert!(builder.contains(expected), "{expected} in {builder}");
        }
    }

    #[test]
    fn a_generic_class_has_a_generic_builder() {
        let members = generated("import lombok.Builder;\n@Builder\nclass Pair<K extends Comparable<K>, V> {\n  private K key;\n  private V value;\n}\n");
        assert!(members.contains(&"public static <K extends Comparable<K>, V> PairBuilder<K, V> builder() {}".to_owned()), "{members:?}");
        assert!(members.iter().any(|member| member.contains("public Pair<K, V> build() {}")), "{members:?}");
    }

    #[test]
    fn super_builder_extends_the_superclass_s_builder() {
        let members = generated(
            "import lombok.experimental.SuperBuilder;\n@SuperBuilder(toBuilder = true)\nclass Customer extends Person {\n  private int points;\n}\n",
        );
        assert!(members.contains(&"public static CustomerBuilder<?, ?> builder() {}".to_owned()), "{members:?}");
        assert!(members.contains(&"public CustomerBuilder<?, ?> toBuilder() {}".to_owned()), "{members:?}");
        assert!(members.contains(&"protected Customer(CustomerBuilder<?, ?> b) {}".to_owned()), "{members:?}");
        let builder = members.iter().find(|member| member.contains("abstract class CustomerBuilder")).expect("the builder");
        for expected in [
            "public static abstract class CustomerBuilder<C extends Customer, B extends CustomerBuilder<C, B>> extends Person.PersonBuilder<C, B> {",
            "public B points(int points) {}",
            "protected abstract B self();",
            "public abstract C build();",
        ] {
            assert!(builder.contains(expected), "{expected} in {builder}");
        }
        assert!(members.iter().any(|member| member.starts_with(
            "private static final class CustomerBuilderImpl extends CustomerBuilder<Customer, CustomerBuilderImpl> {"
        )), "{members:?}");
    }

    #[test]
    fn singular_names_one_element_of_a_plural() {
        assert_eq!(singular_of("items").as_deref(), Some("item"));
        assert_eq!(singular_of("entries").as_deref(), Some("entry"));
        assert_eq!(singular_of("boxes").as_deref(), Some("box"));
        assert_eq!(singular_of("data"), None);
    }

    #[test]
    fn slf4j_adds_a_logger_field_at_the_annotation() {
        let tree = super::super::Java.tree("import lombok.extern.slf4j.Slf4j;\n@Slf4j\nclass S {\n}\n");
        let log = tree.children[1].first("class_body").unwrap().children.iter().find(|child| child.is_generated()).unwrap();
        assert_eq!(log.text, "private static final org.slf4j.Logger log = null;");
        assert_eq!((log.start, log.end), (Position { line: 1, column: 0 }, Position { line: 1, column: 6 }));
    }
}
