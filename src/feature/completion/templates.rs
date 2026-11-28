//! jdtls's code templates, offered beside the names: the statements it
//! writes for a key (`sysout`, `foreach`, `ifnull`), the members of a
//! type's body (`main`, `ctor`) and the types a file or a body declares
//! (`class`, `interface`, `enum`, `record`), and the postfix templates of
//! an expression (`a.var`, `a.not`).
//!
//! Each is a snippet item with jdtls's description as its label's
//! description and its detail, written as jdtls writes it. They rank after
//! every other offer.

use super::java::{Context, starts_with};
use super::{ItemKind, Offer, OfferKind, Shown};
use crate::feature::semantic::{Type, is_type_declaration, types_of};
use crate::parser::{Position, Syn};

/// Where templates rank: before keywords, as jdtls lists them.
const TEMPLATE: &str = "8";

/// A statement template: its key, its description, its body and whether
/// jdtls documents it with what it writes.
struct Statement {
    key: &'static str,
    description: &'static str,
    body: &'static str,
    documented: bool,
}

const SELECTED: &str = "$TM_SELECTED_TEXT";

const STATEMENTS: &[Statement] = &[
    Statement { key: "sysout", description: "print to standard out", body: "System.out.println(${0});", documented: false },
    Statement { key: "sout", description: "print to standard out", body: "System.out.println(${0});", documented: false },
    Statement { key: "syserr", description: "print to standard err", body: "System.err.println(${0});", documented: false },
    Statement { key: "serr", description: "print to standard err", body: "System.err.println(${0});", documented: false },
    Statement { key: "if", description: "if statement", body: "if (${1:con}) {\n\t$TM_SELECTED_TEXT${0}\n}", documented: true },
    Statement { key: "ifelse", description: "if-else statement", body: "if (${1:con}) {\n\t${2}\n} else {\n\t${0}\n}", documented: true },
    Statement { key: "ifnull", description: "if statement checking for null", body: "if (${1:obj} == null) {\n\t$TM_SELECTED_TEXT${0}\n}", documented: false },
    Statement { key: "ifnotnull", description: "if statement checking for not null", body: "if (${1:obj} != null) {\n\t$TM_SELECTED_TEXT${0}\n}", documented: false },
    Statement { key: "while", description: "while statement", body: "while (${1:con}) {\n\t$TM_SELECTED_TEXT${0}\n}", documented: false },
    Statement { key: "dowhile", description: "do-while statement", body: "do {\n\t$TM_SELECTED_TEXT${0}\n} while (${1:con});", documented: true },
];

/// The templates a statement may start with, for a key begun.
pub(super) fn statements(context: &Context<'_>) -> Vec<Offer> {
    if context.prefix.is_empty() {
        return Vec::new();
    }
    let mut offers: Vec<Offer> = STATEMENTS
        .iter()
        .map(|template| {
            let doc = template.documented.then(|| expanded(template.body));
            template_offer(template.key, Some(template.description), template.body.to_owned(), doc)
        })
        .collect();
    // `soutm` and `systrace`: the method being written, by name.
    if let Some(method) = enclosing_method(context) {
        let text = format!("System.out.println(\"{method}\");");
        for key in ["soutm", "systrace"] {
            offers.push(template_offer(key, Some("print current method to standard out"), text.clone(), None));
        }
    }
    // `foreach`, `iter` and `fori` go over the nearest array or Iterable.
    let iterated = iterable_in_scope(context);
    let (element, name, iterable) = iterated
        .as_ref()
        .map_or(("iterable_type".to_owned(), "iterable_element".to_owned(), "iterable".to_owned()), |(element, name)| {
            (element.clone(), decapitalized(element), name.clone())
        });
    let foreach = format!("for (${{1:{element}}} ${{2:{name}}} : ${{3:{iterable}}}) {{\n\t{SELECTED}${{0}}\n}}");
    for key in ["foreach", "iter"] {
        offers.push(template_offer(key, Some("iterate over an array or Iterable"), foreach.clone(), None));
    }
    let array = iterated.map_or("array".to_owned(), |(_, name)| name);
    offers.push(template_offer(
        "fori",
        Some("iterate over array"),
        format!("for (${{1:int}} ${{2:i}} = ${{3:0}}; ${{2:i}} < ${{4:{array}.length}}; ${{2:i}}++) {{\n\t{SELECTED}${{0}}\n}}"),
        None,
    ));
    offers.retain(|offer| starts_with(&offer.name, context.prefix));
    offers
}

/// The templates of a type's body: `main` (by its key and by what it
/// writes), `ctor` in a class, `method` in an interface, and the types a
/// body may declare.
pub(super) fn members(context: &Context<'_>) -> Vec<Offer> {
    let main = "public static void main(String[] args) {\n\t${0}\n}";
    // jdtls ranks these before the modifiers, but after one begun.
    let after_keywords = !context.prefix.is_empty();
    let mut offers = vec![
        template_offer("main", Some("public static main method"), main.to_owned(), None),
        Offer {
            alias: Some("main".to_owned()),
            ..template_offer("public static void main(String[] args)", Some("public static main method"), main.to_owned(), None)
        },
    ];
    let declaration = context.path.iter().rev().find(|node| is_type_declaration(node)).copied();
    let class_name = declaration.and_then(|declaration| declaration.child("name")).map(|name| name.text.clone());
    match declaration.map(|declaration| declaration.kind.as_str()) {
        Some("interface_declaration") => offers.push(template_offer(
            "method",
            Some("method"),
            "${1|public,private|} ${2:void} ${3:name}(${4});".to_owned(),
            None,
        )),
        Some("class_declaration") => {
            if let Some(class_name) = &class_name {
                offers.push(template_offer(
                    "ctor",
                    Some("constructor"),
                    format!("${{1|public,protected,private|}} {class_name}(${{2}}) {{\n\t${{3:super();}}${{0}}\n}}"),
                    None,
                ));
            }
        }
        _ => {}
    }
    // A record's body declares no type by template.
    if declaration.is_none_or(|declaration| declaration.kind != "record_declaration") {
        offers.extend(types(context, &["class", "interface", "enum"], true));
    }
    if after_keywords {
        for offer in &mut offers {
            offer.rank = format!("9~{}", offer.name);
        }
    }
    offers
}

/// The types a file may declare: each with its Javadoc, public.
pub(super) fn unit(context: &Context<'_>) -> Vec<Offer> {
    types(context, &["class", "interface", "enum", "record"], true)
}

/// A class declared in a block: no Javadoc, no modifier; jdtls offers it
/// only where nothing follows the cursor on its line.
pub(super) fn local_class(context: &Context<'_>) -> Vec<Offer> {
    let line = context.written.text.lines().nth(context.name.start.line as usize).unwrap_or_default();
    let rest: String = line.chars().skip(context.name.start.column as usize + context.prefix.chars().count()).collect();
    if !rest.trim().is_empty() {
        return Vec::new();
    }
    plain_class(context)
}

/// The class template without Javadoc or modifier, wherever it is asked
/// for (jdtls offers it in an annotation's value too).
pub(super) fn plain_class(context: &Context<'_>) -> Vec<Offer> {
    types(context, &["class"], false)
}

/// The type templates `kinds`, named `Inner` and the file's name (`_1`
/// after it where a type is called that already).
fn types(context: &Context<'_>, kinds: &[&str], documented: bool) -> Vec<Offer> {
    let stem = context
        .written
        .uri
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|file| file.strip_suffix(".java"))
        .unwrap_or("Type")
        .to_owned();
    let declared: Vec<&str> = types_of(context.written).into_iter().map(|(name, _)| name).collect();
    let mut name = format!("Inner{stem}");
    if declared.contains(&name.as_str()) {
        name.push_str("_1");
    }
    kinds
        .iter()
        .map(|kind| {
            let body = if *kind == "record" {
                format!("{kind} ${{1:{name}}}(${{0}}) {{\n}}")
            } else {
                format!("{kind} ${{1:{name}}} {{\n\n\t${{0}}\n}}")
            };
            let text = if documented {
                format!("/**\n * ${{1:{name}}}\n */\npublic {body}")
            } else {
                body
            };
            let doc = documented.then(|| expanded(&text));
            template_offer(kind, None, text, doc)
        })
        .collect()
}

/// A postfix template: its key, its description, what it writes of the
/// expression `{e}`, what the expression must be, and whether jdtls
/// documents it with what it writes.
struct Postfix {
    key: &'static str,
    description: &'static str,
    body: &'static str,
    wants: Wants,
    documented: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wants {
    Anything,
    Boolean,
    Reference,
    Text,
    Array,
    Iterable,
    Throwable,
}

const POSTFIX: &[Postfix] = &[
    Postfix { key: "assert", description: "Creates an assert statement", body: "assert {e};", wants: Wants::Boolean, documented: false },
    Postfix { key: "cast", description: "Casts the expression to a new type", body: "((${1}){e})${0}", wants: Wants::Anything, documented: true },
    Postfix { key: "else", description: "Creates a negated if statement", body: "if (!{e}) {\n\t${0}\n}", wants: Wants::Boolean, documented: true },
    Postfix { key: "if", description: "Creates a if statement", body: "if ({e}) {\n\t${0}\n}", wants: Wants::Boolean, documented: true },
    Postfix { key: "not", description: "Negates the expression", body: "!{e}", wants: Wants::Boolean, documented: true },
    Postfix { key: "while", description: "Creates a while loop", body: "while ({e}) {\n\t${0}\n}", wants: Wants::Boolean, documented: false },
    Postfix { key: "nnull", description: "Creates an if statement and checks if the expression does not resolve to null", body: "if ({e} != null) {\n\t${0}\n}", wants: Wants::Reference, documented: false },
    Postfix { key: "null", description: "Creates an if statement which checks if expression resolves to null", body: "if ({e} == null) {\n\t${0}\n}", wants: Wants::Reference, documented: false },
    Postfix { key: "opt", description: "Creates an Optional.ofNullable(..) call", body: "Optional.ofNullable({e})", wants: Wants::Reference, documented: false },
    Postfix { key: "par", description: "Places the expression in parentheses", body: "({e})", wants: Wants::Anything, documented: true },
    Postfix { key: "format", description: "Sends the affected object to the String.format(..) method", body: "String.format({e}, ${0});", wants: Wants::Text, documented: false },
    Postfix { key: "sysout", description: "Sends the affected object to a System.out.println(..) call", body: "System.out.println({e});${0}", wants: Wants::Anything, documented: false },
    Postfix { key: "sysoutv", description: "Sends the affected object to a System.out.println(..) call", body: "System.out.println(\"{e} = \" + {e});${0}", wants: Wants::Anything, documented: false },
    Postfix { key: "sysouf", description: "Sends the affected object to a System.out.printf(..) call", body: "System.out.printf(\"\", {e});${0}", wants: Wants::Anything, documented: false },
    Postfix { key: "syserr", description: "Sends the affected object to a System.err.println(..) call", body: "System.err.println({e});${0}", wants: Wants::Anything, documented: false },
    Postfix { key: "throw", description: "Throws the given Exception", body: "throw {e};", wants: Wants::Throwable, documented: false },
    Postfix { key: "fori", description: "Creates a for statement which iterates over an array", body: "for (int ${1:{n}} = 0; ${1:{n}} < {e}.length; ${1:{n}}++) {\n\t${0}\n}", wants: Wants::Array, documented: false },
    Postfix { key: "forr", description: "Creates a for statement which iterates over an array in reverse order", body: "for (int ${1:{n}} = {e}.length - 1; ${1:{n}} >= 0; ${1:{n}}--) {\n\t${0}\n}", wants: Wants::Array, documented: false },
    Postfix { key: "for", description: "Creates a for statement", body: "for ({t} ${1:{n}} : {e}) {\n\t${0}\n}", wants: Wants::Iterable, documented: false },
    Postfix { key: "var", description: "Creates a new variable", body: "{v} ${1:{n}} = {e};${0}", wants: Wants::Anything, documented: false },
];

/// The postfix templates of `receiver.key|`, for a key begun: what fits
/// the expression's type, written over the expression and the key.
pub(super) fn postfix(context: &Context<'_>, receiver: &Syn, written_type: Option<&Type>, from: Position) -> Vec<Offer> {
    if context.prefix.is_empty() || context.path.iter().any(|node| node.kind == "import_declaration") {
        return Vec::new();
    }
    let expression = receiver.text.trim();
    let name = suggested_name(receiver);
    let fits = |wants: Wants| -> bool {
        let Some(written) = written_type else {
            return wants == Wants::Anything || wants == Wants::Reference;
        };
        let simple = written.name.rsplit('.').next().unwrap_or(&written.name);
        match wants {
            Wants::Anything => true,
            Wants::Boolean => written.dimensions == 0 && matches!(simple, "boolean" | "Boolean"),
            Wants::Reference => written.dimensions > 0 || !simple.starts_with(char::is_lowercase),
            Wants::Text => written.dimensions == 0 && simple == "String",
            Wants::Array => written.dimensions > 0,
            Wants::Iterable => {
                written.dimensions > 0 || matches!(simple, "Iterable" | "Collection" | "List" | "Set" | "ArrayList" | "LinkedList" | "HashSet" | "TreeSet" | "Queue" | "Deque")
            }
            Wants::Throwable => written.dimensions == 0 && (simple.ends_with("Exception") || simple.ends_with("Error") || simple == "Throwable"),
        }
    };
    let element = written_type
        .map(|written| {
            if written.dimensions > 0 {
                Type { dimensions: written.dimensions - 1, ..written.clone() }.written()
            } else {
                written.arguments.first().map_or("Object".to_owned(), Type::written)
            }
        })
        .unwrap_or_else(|| "Object".to_owned());
    let variable_type = written_type.map_or("Object".to_owned(), |written| raw_variables_as_object(written).written());
    POSTFIX
        .iter()
        .filter(|template| fits(template.wants) && starts_with(template.key, context.prefix))
        .map(|template| {
            let text = template
                .body
                .replace("{e}", expression)
                .replace("{n}", &name)
                .replace("{t}", &element)
                .replace("{v}", &variable_type);
            let doc = template.documented.then(|| expanded(&text));
            let mut offer = template_offer(template.key, Some(template.description), text, doc);
            offer.shown.replaces = Some(from);
            offer.postfix = true;
            offer
        })
        .collect()
}

/// The name a variable taking `expression` is given: a call's method
/// name, a variable's name with `2` after it.
fn suggested_name(expression: &Syn) -> String {
    match expression.kind.as_str() {
        "method_invocation" => expression
            .child("name")
            .map_or_else(|| "value".to_owned(), |name| name.text.clone()),
        _ if expression.text.chars().all(|character| character.is_alphanumeric() || character == '_') => {
            format!("{}2", expression.text)
        }
        "field_access" => expression
            .child("field")
            .map_or_else(|| "value".to_owned(), |field| format!("{}2", field.text)),
        _ => "value".to_owned(),
    }
}

/// `List<T>` with its unbound type variable as `Object`, as JDT infers
/// what nothing binds.
fn raw_variables_as_object(written: &Type) -> Type {
    let arguments = written
        .arguments
        .iter()
        .map(|argument| {
            if argument.name.len() == 1 && argument.name.starts_with(char::is_uppercase) && argument.arguments.is_empty() {
                Type { name: "Object".to_owned(), ..argument.clone() }
            } else {
                raw_variables_as_object(argument)
            }
        })
        .collect();
    Type { arguments, ..written.clone() }
}

/// A template as an offer: a snippet, ranked after the other offers.
fn template_offer(key: &str, description: Option<&str>, text: String, documentation: Option<String>) -> Offer {
    Offer {
        name: key.to_owned(),
        rank: format!("{TEMPLATE}{key}"),
        kind: OfferKind::Keyword,
        shown: Shown {
            kind: Some(ItemKind::Snippet),
            description: description.map(str::to_owned),
            detail: description.map(str::to_owned),
            documentation: documentation.map(|text| format!("```java\n{text}\n```")),
            insert: Some(text),
            snippet: true,
            no_import: true,
            ..Shown::default()
        },
        ..Offer::default()
    }
}

/// What a snippet writes once its placeholders keep their defaults.
fn expanded(snippet: &str) -> String {
    let mut out = String::new();
    let mut rest = snippet.replace(SELECTED, "");
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let inside = &after[..end];
        if let Some((_, default)) = inside.split_once(':') {
            out.push_str(default);
        } else if let Some((_, choices)) = inside.split_once('|') {
            out.push_str(choices.split([',', '|']).next().unwrap_or_default());
        }
        rest = after[end + 1..].to_owned();
    }
    out.push_str(&rest);
    out
}

/// `Type.method()` of the method the cursor is in.
fn enclosing_method(context: &Context<'_>) -> Option<String> {
    let method = context.path.iter().rev().find(|node| node.kind == "method_declaration")?;
    let owner = context
        .path
        .iter()
        .rev()
        .find(|node| is_type_declaration(node) && node.start <= method.start)?;
    Some(format!("{}.{}()", owner.child("name")?.text, method.child("name")?.text))
}

/// The nearest variable in scope that is an array or an `Iterable`, with
/// the type of its elements.
fn iterable_in_scope(context: &Context<'_>) -> Option<(String, String)> {
    let at = context.name.start;
    for name in context.snapshot.names_in_scope(context.file, at) {
        let Some(found) = context.snapshot.name_in_scope(&name, context.file, at) else {
            continue;
        };
        let Some(written) = context.snapshot.type_of_declaration(&found) else {
            continue;
        };
        if written.dimensions > 0 {
            return Some((Type { dimensions: written.dimensions - 1, ..written }.written(), name));
        }
        let iterable = matches!(written.name.as_str(), "Iterable" | "Collection" | "List" | "Set" | "ArrayList");
        if iterable && let Some(element) = written.arguments.first() {
            return Some((element.written(), name));
        }
    }
    None
}

fn decapitalized(name: &str) -> String {
    let simple: String = name.chars().take_while(|character| character.is_alphanumeric() || *character == '_').collect();
    let mut characters = simple.chars();
    characters
        .next()
        .map(|first| first.to_lowercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
}

/// Whether the cursor is in a block, where a statement may start.
pub(super) fn in_a_block(path: &[&Syn]) -> bool {
    path.iter().any(|node| matches!(node.kind.as_str(), "block" | "constructor_body"))
}
