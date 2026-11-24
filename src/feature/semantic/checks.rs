//! What the Eclipse compiler (jdtls) reports of a Java file beyond its
//! syntax, said as it says it: the checks the semantic layer can make
//! without a type checker of its own.
//!
//! A check speaks only where it is sure. Where a type it would have to look
//! into is not known (a library without sources), or the answer would need
//! rules it does not have (generics, overload ranking), it says nothing:
//! jdtls will, and a wrong error is worse than a missing one.

use std::collections::HashMap;

use super::{
    is_anonymous_class, is_type_declaration, parameters, parameters_of, path_to, Bindings,
    FileView, Found, Snapshot,
};
use crate::language::Language;
use crate::parser::{Finding, Syn};

/// Methods every class has from `Object`: the class's supertypes are not
/// all the overloads a call of one of these may pick.
const OBJECT_METHODS: &[&str] = &[
    "clone",
    "equals",
    "finalize",
    "getClass",
    "hashCode",
    "notify",
    "notifyAll",
    "toString",
    "wait",
];

/// The methods a class can call without a receiver, by name.
type Methods<'a> = HashMap<String, Vec<Found<'a>>>;

/// How deep a class hierarchy is followed to check that all of it is known.
const HIERARCHY_DEPTH: usize = 8;

impl Snapshot {
    /// The calls of a Java file that no method of their name can take:
    /// `parse(test)` where the class declares only `parse(String, Parser)`.
    pub fn inapplicable_calls(&self, file: &FileView) -> Vec<Finding> {
        if file.language != Language::Java {
            return Vec::new();
        }
        // A file calls hundreds of methods from a handful of classes: each
        // class's hierarchy is checked, and its methods listed, once.
        let mut callers: HashMap<usize, Option<Methods<'_>>> = HashMap::new();
        file.tree
            .walk()
            .into_iter()
            .filter(|node| node.kind == "method_invocation")
            .filter_map(|call| {
                let name = call.child("name")?;
                let innermost = path_to(&file.tree, name.start)
                    .into_iter()
                    .rev()
                    .find(|node| is_type_declaration(node) || is_anonymous_class(node))?;
                let methods = callers
                    .entry(std::ptr::from_ref(innermost) as usize)
                    .or_insert_with(|| self.methods_of_caller(innermost, file))
                    .as_ref()?;
                self.inapplicable(call, methods.get(&name.text)?, file)
            })
            .collect()
    }

    /// The methods a class and the classes above it declare, by name, for
    /// a class whose whole hierarchy is known. Inside an anonymous class a
    /// name may be the enclosing class's: not followed.
    fn methods_of_caller<'a>(&'a self, class: &'a Syn, file: &'a FileView) -> Option<Methods<'a>> {
        if !is_type_declaration(class) {
            return None;
        }
        let caller = self.declared(file, class)?;
        let mut hierarchy = Vec::new();
        if !self.hierarchy_known(&caller, &mut hierarchy) {
            return None;
        }
        let mut found = Vec::new();
        self.members_where(&caller, super::Wanted::Every, &mut found, &mut 0);
        let mut methods = Methods::new();
        for member in found {
            // A member list holds the methods of the classes nested in it
            // too: those are not the caller's to call.
            let declared_by = declaring_body(member.file, member.name);
            if member.node.kind == "method_declaration" && hierarchy.contains(&declared_by) {
                methods
                    .entry(member.name.text.clone())
                    .or_default()
                    .push(member);
            }
        }
        Some(methods)
    }

    /// A call without a receiver (or on `this`) of a method its class and
    /// the classes above it declare once (`candidates`, the methods of its
    /// name there), with a number of parameters the
    /// call does not pass.
    fn inapplicable(
        &self,
        call: &Syn,
        candidates: &[Found<'_>],
        file: &FileView,
    ) -> Option<Finding> {
        if call
            .child("object")
            .is_some_and(|object| object.kind != "this")
        {
            return None;
        }
        let name = call.child("name")?;
        if OBJECT_METHODS.contains(&name.text.as_str()) {
            return None;
        }
        let passed = arguments_of(call.child("arguments")?);
        let [method] = candidates else {
            return None;
        };
        let declared = parameters_of(method.node);
        let takes_more = declared
            .last()
            .is_some_and(|parameter| parameter.kind == "spread_parameter");
        let fits = if takes_more {
            passed.len() + 1 >= declared.len()
        } else {
            passed.len() == declared.len()
        };
        if fits || method.node.child("type_parameters").is_some() {
            return None;
        }
        let owner = self.type_name_for_message(method)?;
        let wanted: Vec<String> = parameters(method.node, &Bindings::new())
            .into_iter()
            .map(|parameter| {
                if parameter.spread {
                    format!("{}...", parameter.written)
                } else {
                    parameter.written
                }
            })
            .collect();
        let given = passed
            .iter()
            .map(|argument| self.argument_type_for_message(argument, file))
            .collect::<Option<Vec<String>>>()?;
        if wanted
            .iter()
            .any(|written| written.contains('<') || written.is_empty())
        {
            return None;
        }
        Some(Finding {
            at: name.start,
            message: format!(
                "The method {}({}) in the type {owner} is not applicable for the arguments ({})",
                name.text,
                wanted.join(", "),
                given.join(", ")
            ),
        })
    }

    /// Whether every type above `of` is one the sources declare: only
    /// then are the methods found by name all the methods there are.
    /// `hierarchy` gets `of` and every type above it.
    fn hierarchy_known(&self, of: &Found<'_>, hierarchy: &mut Vec<usize>) -> bool {
        if hierarchy.len() > HIERARCHY_DEPTH {
            return false;
        }
        hierarchy.push(std::ptr::from_ref(of.node) as usize);
        let resolved = self.supertypes(of);
        resolved.len() == written_supertypes(of.node)
            && resolved
                .iter()
                .all(|supertype| self.hierarchy_known(supertype, hierarchy))
    }

    /// The type declaring `member` as the compiler names it: `Outer.Inner`
    /// for a member type. `None` for a generic type, whose name the
    /// compiler writes with its parameters.
    fn type_name_for_message(&self, member: &Found<'_>) -> Option<String> {
        let owner = self.owning_type(member)?;
        let path = path_to(&member.file.tree, owner.name.start);
        let names = path
            .iter()
            .filter(|node| is_type_declaration(node))
            .map(|node| {
                let generic = node.child("type_parameters").is_some();
                (!generic).then(|| node.child("name").map(|name| name.text.clone()))?
            })
            .collect::<Option<Vec<String>>>()?;
        Some(names.join("."))
    }

    /// The type of an argument as the compiler writes it in a message, for
    /// the expressions whose type the sources say for certain.
    fn argument_type_for_message(&self, argument: &Syn, file: &FileView) -> Option<String> {
        let certain = matches!(
            argument.kind.as_str(),
            "identifier"
                | "this"
                | "field_access"
                | "object_creation_expression"
                | "cast_expression"
                | "string_literal"
                | "decimal_integer_literal"
                | "hex_integer_literal"
                | "binary_integer_literal"
                | "octal_integer_literal"
                | "decimal_floating_point_literal"
                | "character_literal"
                | "true"
                | "false"
        );
        if argument.kind == "null_literal" {
            return Some("null".to_owned());
        }
        if !certain {
            return None;
        }
        let given = self.type_of_expression(argument, file)?;
        let written = given.written();
        (!written.contains('<') && !written.is_empty()).then_some(written)
    }
}

/// The class (or anonymous class) whose body declares `name`.
fn declaring_body(file: &FileView, name: &Syn) -> usize {
    path_to(&file.tree, name.start)
        .into_iter()
        .rev()
        .find(|node| is_type_declaration(node) || is_anonymous_class(node))
        .map_or(0, |node| std::ptr::from_ref(node) as usize)
}

/// The expressions an argument list passes.
fn arguments_of(arguments: &Syn) -> Vec<&Syn> {
    arguments
        .children
        .iter()
        .filter(|child| {
            !matches!(child.kind.as_str(), "(" | ")" | ",") && !child.kind.ends_with("comment")
        })
        .collect()
}

/// How many supertypes a declaration writes, counted the way
/// [`Snapshot::supertypes`] resolves them.
fn written_supertypes(declaration: &Syn) -> usize {
    if declaration.kind == "object_creation_expression" {
        return usize::from(declaration.child("type").is_some());
    }
    declaration
        .children
        .iter()
        .filter(|child| {
            matches!(
                child.kind.as_str(),
                "superclass" | "super_interfaces" | "extends_interfaces" | "interface_type_list"
            )
        })
        .flat_map(|list| list.walk())
        .filter(|named| {
            matches!(
                named.kind.as_str(),
                "type_identifier" | "generic_type" | "scoped_type_identifier"
            )
        })
        .count()
}
