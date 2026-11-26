//! What the Eclipse compiler (jdtls) says of a Java file that the file's
//! own text settles, said as it says it: task tags, a public type in a
//! file of another name, duplicate methods, `this` where there is no
//! instance, dead branches, and the private fields and imports nothing
//! reads.
//!
//! As in [`super::checks`], a check speaks only where it is sure: a
//! duplicate whose parameter types it would have to resolve, or a field a
//! name of another file could read, is left to jdtls.

use std::collections::{HashMap, HashSet};

use super::{imports, is_type_declaration, FileView};
use crate::language::Language;
use crate::parser::{Finding, Position, Syn};

/// How much a report weighs, in the words of the LSP's severities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Information,
}

/// One thing the compiler says of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub finding: Finding,
    pub level: Level,
    /// Code that can go (an unused import, a dead branch), which an editor
    /// greys out rather than underlines.
    pub unnecessary: bool,
}

/// The task tags Eclipse looks for in comments by default.
const TASK_TAGS: &[&str] = &["TODO", "FIXME", "XXX"];

/// The bodies a type declaration holds its members in.
const TYPE_BODIES: &[&str] = &[
    "class_body",
    "interface_body",
    "enum_body",
    "enum_body_declarations",
    "annotation_type_body",
];

/// The comments of a Java file that carry a task tag: `// TODO task`.
/// A tag stands alone (`aTODO` and `TODOx` are not tags), and its message
/// runs to the end of the line, the end of the comment or the next tag.
fn tasks(file: &FileView) -> Vec<Report> {
    let mut reports = Vec::new();
    for comment in file.tree.walk() {
        if !matches!(comment.kind.as_str(), "line_comment" | "block_comment") {
            continue;
        }
        let text = comment.text.strip_suffix("*/").unwrap_or(&comment.text);
        let tags = task_tags_in(text);
        for (index, &(start, _)) in tags.iter().enumerate() {
            let line_end = text[start..]
                .find('\n')
                .map_or(text.len(), |end| start + end);
            let end = tags
                .get(index + 1)
                .map_or(line_end, |&(next, _)| next.min(line_end));
            let message = text[start..end].trim_end();
            reports.push(Report {
                finding: Finding { end: None,
                    at: offset_from(comment.start, text, start),
                    message: message.to_owned(),
                },
                level: Level::Information,
                unnecessary: false,
            });
        }
    }
    reports
}

/// Where each task tag of a comment starts, with the tag.
fn task_tags_in(text: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for tag in TASK_TAGS {
        for (start, _) in text.match_indices(tag) {
            let before = text[..start].chars().next_back();
            let after = text[start + tag.len()..].chars().next();
            if !before.is_some_and(is_identifier_part) && !after.is_some_and(is_identifier_part) {
                found.push((start, *tag));
            }
        }
    }
    found.sort_unstable();
    found
}

fn is_identifier_part(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// The position `offset` bytes into `text`, which starts at `start`.
fn offset_from(start: Position, text: &str, offset: usize) -> Position {
    let mut at = start;
    for character in text[..offset].chars() {
        if character == '\n' {
            at.line += 1;
            at.column = 0;
        } else {
            at.column += character.len_utf16() as u32;
        }
    }
    at
}

/// What the compiler says of a Java file beyond its syntax.
///
/// Off a source root Eclipse compiles nothing but the syntax: only the
/// task tags are said. On one, the errors its text alone settles (a public
/// type the file is not named after, a method declared twice, `this` in a
/// static context) and the warnings: dead branches, private fields nothing
/// reads, imports nothing uses. Eclipse leaves out the unused fields when
/// the file declares another package than its folder names or a static
/// field's initializer or a static block has an error, and every unused
/// import when the file has an error, though it still says a repeated
/// import is never used.
pub fn reports(file: &FileView, on_source_root: bool, elsewhere: Errors) -> Vec<Report> {
    if file.language != Language::Java {
        return Vec::new();
    }
    let mut reports = tasks(file);
    if !on_source_root {
        return reports;
    }
    let mut errors = types_in_another_file(file);
    errors.extend(duplicate_methods(file));
    let (in_methods, in_initializers) = this_without_instance(file);
    let initializer_fails = !in_initializers.is_empty();
    errors.extend(in_methods);
    errors.extend(in_initializers);
    let has_errors = elsewhere.any || !errors.is_empty();
    reports.extend(errors.into_iter().map(|finding| Report {
        finding,
        level: Level::Error,
        unnecessary: false,
    }));
    reports.extend(dead_branches(file));
    if !initializer_fails && !elsewhere.wrong_package {
        reports.extend(unread_fields(file));
    }
    reports.extend(unused_imports(file, has_errors));
    reports.sort_by_key(|report| report.finding.at);
    reports
}

/// The errors other checks found in the file.
#[derive(Clone, Copy, Debug, Default)]
pub struct Errors {
    /// Any error at all.
    pub any: bool,
    /// The file declares another package than its folder names.
    pub wrong_package: bool,
}

fn unnecessary(at: Position, message: String) -> Report {
    Report {
        finding: Finding { end: None, at, message },
        level: Level::Warning,
        unnecessary: true,
    }
}

/// Calls `visit` with every node of `node` and the nodes around it,
/// outermost first.
fn each_with_ancestors<'a>(
    node: &'a Syn,
    ancestors: &mut Vec<&'a Syn>,
    visit: &mut impl FnMut(&'a Syn, &[&'a Syn]),
) {
    visit(node, ancestors);
    ancestors.push(node);
    for child in &node.children {
        each_with_ancestors(child, ancestors, visit);
    }
    ancestors.pop();
}

/// The name Eclipse gives the type whose body a member sits in, as
/// `Outer.Inner`: `None` inside an anonymous or a local class.
fn owner_of(ancestors: &[&Syn], body: &Syn) -> Option<String> {
    if !TYPE_BODIES.contains(&body.kind.as_str()) {
        return None;
    }
    let mut names = Vec::new();
    for node in ancestors {
        if is_type_declaration(node) {
            names.push(node.child("name")?.text.as_str());
        } else if node.kind != "program" && !TYPE_BODIES.contains(&node.kind.as_str()) {
            return None;
        }
    }
    (!names.is_empty()).then(|| names.join("."))
}

fn has_modifier(declaration: &Syn, modifier: &str) -> bool {
    declaration.first("modifiers").is_some_and(|modifiers| {
        modifiers
            .children
            .iter()
            .any(|child| child.kind == modifier)
    })
}

/// `public class Foo` written in `Other.java`.
fn types_in_another_file(file: &FileView) -> Vec<Finding> {
    let Some(stem) = file_stem(file) else {
        return Vec::new();
    };
    file.tree
        .children
        .iter()
        .filter(|node| is_type_declaration(node) && has_modifier(node, "public"))
        .filter_map(|node| node.child("name"))
        .filter(|name| name.text != stem)
        .map(|name| Finding { end: None,
            at: name.start,
            message: format!(
                "The public type {} must be defined in its own file",
                name.text
            ),
        })
        .collect()
}

fn file_stem(file: &FileView) -> Option<String> {
    let path = file.uri.to_file_path().ok()?;
    Some(path.file_stem()?.to_str()?.to_owned())
}

/// Two methods, or two constructors, of one type with the same name and
/// the same parameter types: each is reported. Types are compared as
/// written, so only a type with no `.` and no second type argument takes
/// part: `java.util.List` and `List` may be one type, and Eclipse words
/// `Map<K,V>` in a way not checked here.
fn duplicate_methods(file: &FileView) -> Vec<Finding> {
    let mut findings = Vec::new();
    each_with_ancestors(&file.tree, &mut Vec::new(), &mut |body, ancestors| {
        let Some(owner) = owner_of(ancestors, body) else {
            return;
        };
        // By name and parameter types, varargs read as arrays: each
        // declaration's name, with its parameter types as written.
        let mut seen: HashMap<(String, Vec<String>), Vec<Declared<'_>>> = HashMap::new();
        for member in &body.children {
            if !matches!(
                member.kind.as_str(),
                "method_declaration" | "constructor_declaration"
            ) {
                continue;
            }
            let Some(name) = member.child("name") else {
                continue;
            };
            let Some(written) = parameter_types(member) else {
                continue;
            };
            let key = written
                .iter()
                .map(|written| written.replace("...", "[]"))
                .collect();
            seen.entry((name.text.clone(), key))
                .or_default()
                .push((name, written));
        }
        for declarations in seen
            .into_values()
            .filter(|declarations| declarations.len() > 1)
        {
            for (name, written) in declarations {
                findings.push(Finding { end: None,
                    at: name.start,
                    message: format!(
                        "Duplicate method {}({}) in type {owner}",
                        name.text,
                        written.join(", ")
                    ),
                });
            }
        }
    });
    findings
}

/// A method's name, and its parameter types as written.
type Declared<'a> = (&'a Syn, Vec<String>);

/// The parameter types of a method as written, `String...` for varargs:
/// `None` where one is not simple enough to compare as text.
fn parameter_types(method: &Syn) -> Option<Vec<String>> {
    let parameters = method.child("parameters")?;
    let mut types = Vec::new();
    for parameter in &parameters.children {
        let written = match parameter.kind.as_str() {
            "formal_parameter" => {
                if parameter.child("dimensions").is_some() {
                    return None;
                }
                parameter.child("type")?.text.clone()
            }
            "spread_parameter" => {
                let declared = parameter.children.iter().find(|child| {
                    !matches!(
                        child.kind.as_str(),
                        "modifiers" | "..." | "variable_declarator"
                    )
                })?;
                format!("{}...", declared.text)
            }
            "receiver_parameter" => return None,
            _ => continue,
        };
        let written: String = written.chars().filter(|c| !c.is_whitespace()).collect();
        if written.contains(',')
            || written.trim_end_matches("...").contains('.')
            || written.contains('@')
        {
            return None;
        }
        types.push(written);
    }
    Some(types)
}

/// `this` in a static method, a static field's initializer or a static
/// block, lambdas inside them included; a class declared in between has
/// instances of its own. Those in methods, then those in initializers.
fn this_without_instance(file: &FileView) -> (Vec<Finding>, Vec<Finding>) {
    let mut in_methods = Vec::new();
    let mut in_initializers = Vec::new();
    each_with_ancestors(&file.tree, &mut Vec::new(), &mut |node, ancestors| {
        if node.kind != "this" {
            return;
        }
        let parent = ancestors.last();
        let qualified = parent.is_some_and(|parent| {
            parent.kind == "field_access" && node.field.as_deref() == Some("field")
        });
        let explicit_call =
            parent.is_some_and(|parent| parent.kind == "explicit_constructor_invocation");
        if qualified || explicit_call {
            return;
        }
        let finding = || Finding { end: None,
            at: node.start,
            message: "Cannot use this in a static context".to_owned(),
        };
        match static_member(ancestors) {
            Some(StaticMember::Method) => in_methods.push(finding()),
            Some(StaticMember::Initializer) => in_initializers.push(finding()),
            None => {}
        }
    });
    (in_methods, in_initializers)
}

enum StaticMember {
    Method,
    /// A static field's initializer, or a static block.
    Initializer,
}

/// The static member the innermost class around a node runs it in.
fn static_member(ancestors: &[&Syn]) -> Option<StaticMember> {
    for node in ancestors.iter().rev() {
        match node.kind.as_str() {
            "method_declaration" => {
                return has_modifier(node, "static").then_some(StaticMember::Method);
            }
            "field_declaration" => {
                return has_modifier(node, "static").then_some(StaticMember::Initializer);
            }
            "static_initializer" => return Some(StaticMember::Initializer),
            kind if TYPE_BODIES.contains(&kind) => return None,
            "constructor_declaration" | "compact_constructor_declaration" => return None,
            _ => {}
        }
    }
    None
}

/// `if (false) { … }`: the branch a constant condition never takes.
fn dead_branches(file: &FileView) -> Vec<Report> {
    let mut reports = Vec::new();
    for statement in file.tree.walk() {
        if statement.kind != "if_statement" {
            continue;
        }
        let Some(condition) = statement.child("condition").and_then(constant) else {
            continue;
        };
        let dead = if condition {
            statement.child("alternative")
        } else {
            statement.child("consequence")
        };
        if let Some(dead) = dead {
            reports.push(unnecessary(dead.start, "Dead code".to_owned()));
        }
    }
    reports
}

/// The value of a condition written with `true`, `false`, `!` and
/// parentheses alone.
fn constant(condition: &Syn) -> Option<bool> {
    match condition.kind.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        "parenthesized_expression" => {
            let inner = condition
                .children
                .iter()
                .filter(|child| !matches!(child.kind.as_str(), "(" | ")"));
            let mut inner = inner.collect::<Vec<_>>();
            (inner.len() == 1)
                .then(|| constant(inner.remove(0)))
                .flatten()
        }
        "unary_expression" => {
            let operator = condition.child("operator")?;
            if operator.kind != "!" {
                return None;
            }
            constant(condition.child("operand")?).map(|value| !value)
        }
        _ => None,
    }
}

/// The private fields of a file nothing reads: their name is written
/// nowhere else, or only as the target of `name = …` or `this.name = …`
/// statements. A file that names Lombok, or a field or type that
/// suppresses warnings, is left alone: either may read them unseen.
fn unread_fields(file: &FileView) -> Vec<Report> {
    if file.text.contains("lombok") {
        return Vec::new();
    }
    let mut read: HashSet<&str> = HashSet::new();
    let mut declared: Vec<(&Syn, String)> = Vec::new();
    each_with_ancestors(&file.tree, &mut Vec::new(), &mut |node, ancestors| {
        if node.kind != "identifier" {
            return;
        }
        let parent = ancestors.last();
        let declares_field = parent.is_some_and(|declarator| {
            declarator.kind == "variable_declarator" && node.field.as_deref() == Some("name")
        }) && ancestors
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|field| field.kind == "field_declaration");
        if declares_field {
            let field = ancestors[ancestors.len() - 2];
            let owner = ancestors[..ancestors.len() - 2]
                .split_last()
                .and_then(|(body, outer)| owner_of(outer, body));
            let suppressed = ancestors.iter().any(|around| {
                around
                    .first("modifiers")
                    .is_some_and(|modifiers| modifiers.text.contains("SuppressWarnings"))
            });
            if let Some(owner) = owner.filter(|_| has_modifier(field, "private") && !suppressed) {
                declared.push((node, owner));
            }
            return;
        }
        if !only_written(node, ancestors) {
            read.insert(node.text.as_str());
        }
    });
    declared
        .into_iter()
        .filter(|(name, _)| !read.contains(name.text.as_str()))
        .map(|(name, owner)| {
            unnecessary(
                name.start,
                format!("The value of the field {owner}.{} is not used", name.text),
            )
        })
        .collect()
}

/// `name = …;` or `this.name = …;`: the name is written, not read.
fn only_written(name: &Syn, ancestors: &[&Syn]) -> bool {
    let mut target = name;
    let mut above = ancestors.iter().rev();
    let mut parent = above.next();
    if let Some(access) = parent.filter(|access| access.kind == "field_access") {
        let on_this = access
            .child("object")
            .is_some_and(|object| object.kind == "this");
        if !on_this || name.field.as_deref() != Some("field") {
            return false;
        }
        target = access;
        parent = above.next();
    }
    let Some(assignment) = parent.filter(|assignment| assignment.kind == "assignment_expression")
    else {
        return false;
    };
    let assigned = assignment
        .child("left")
        .is_some_and(|left| std::ptr::eq(left, target));
    let plain = assignment
        .child("operator")
        .is_some_and(|operator| operator.kind == "=");
    let statement = above
        .next()
        .is_some_and(|statement| statement.kind == "expression_statement");
    assigned && plain && statement
}

/// The imports nothing in the file names, and every repeat of an import.
/// A name mentioned in a comment counts as used: a Javadoc `{@link}` is.
/// An on-demand import (`java.util.*`) is not checked, nor an import of
/// the file's own package.
fn unused_imports(file: &FileView, has_errors: bool) -> Vec<Report> {
    let declarations: Vec<&Syn> = file.tree.all("import_declaration").collect();
    let written = imports(file);
    let package = file
        .tree
        .first("package_declaration")
        .and_then(|package| {
            package
                .children
                .iter()
                .find(|child| matches!(child.kind.as_str(), "scoped_identifier" | "identifier"))
        })
        .map(|name| name.text.clone())
        .unwrap_or_default();
    let mentioned = mentioned_names(file);
    let mut seen = HashSet::new();
    let mut reports = Vec::new();
    for (declaration, import) in declarations.into_iter().zip(written) {
        let Some(name) = declaration
            .children
            .iter()
            .find(|child| matches!(child.kind.as_str(), "scoped_identifier" | "identifier"))
        else {
            continue;
        };
        if import.wildcard {
            continue;
        }
        let repeated = !seen.insert((import.name.clone(), import.is_static));
        let unused = !has_errors
            && !mentioned.contains(import.last_segment())
            && (import.is_static || import.qualifier() != package);
        if repeated || unused {
            reports.push(unnecessary(
                name.start,
                format!("The import {} is never used", import.name),
            ));
        }
    }
    reports
}

/// Every name the file writes outside its package and import
/// declarations, and every word of its comments.
fn mentioned_names(file: &FileView) -> HashSet<&str> {
    let mut names = HashSet::new();
    for node in &file.tree.children {
        if matches!(
            node.kind.as_str(),
            "package_declaration" | "import_declaration"
        ) {
            continue;
        }
        for inner in node.walk() {
            match inner.kind.as_str() {
                "identifier" | "type_identifier" => {
                    names.insert(inner.text.as_str());
                }
                "line_comment" | "block_comment" => {
                    names.extend(inner.text.split(|c: char| !is_identifier_part(c)));
                }
                _ => {}
            }
        }
    }
    names
}
