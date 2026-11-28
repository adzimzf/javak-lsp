//! The quick assists that depend on what the selection covers, as JDT's
//! covering and covered nodes decide them: the field assists of the fields
//! a selection touches, and `final` for several declarations at once.

use super::{accessor_plan, final_candidates, one_per_declaration, single, Action, Edit, QUICK_ASSIST};
use crate::feature::semantic::{is_type_declaration, path_to, FileView, Snapshot};
use crate::parser::{Position, Syn};

/// What a code action is asked for: from `start` to `end`, empty where
/// they are the same.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub start: Position,
    pub end: Position,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Whether `node` is under the selection: around the cursor where it
    /// is empty, overlapping it otherwise.
    pub fn touches(&self, node: &Syn) -> bool {
        if self.is_empty() {
            node.covers(self.start)
        } else {
            node.start < self.end && self.start < node.end
        }
    }
}

/// An action jdtls offers but that needs the client to ask its user
/// something first: offered as jdtls offers it, as the command that asks
/// (`java.action.applyRefactoringCommand` with `refactoring`), no edit.
pub fn needs_input(file: &FileView, title: impl Into<String>, kind: &'static str, refactoring: &'static str) -> Action {
    Action {
        title: title.into(),
        kind,
        edits: vec![(file.uri.clone(), Vec::new())],
        asks: Some(refactoring),
    }
}

/// The declarations of variables the selection touches: fields, locals and
/// parameters, outermost first.
fn touched_declarations<'a>(file: &'a FileView, selection: &Selection) -> Vec<&'a Syn> {
    let mut found = Vec::new();
    for node in file.tree.walk() {
        let declares = matches!(
            node.kind.as_str(),
            "field_declaration" | "local_variable_declaration" | "formal_parameter" | "spread_parameter"
        );
        if declares && selection.touches(node) && !found.iter().any(|seen: &&Syn| seen.start <= node.start && node.end <= seen.end) {
            found.push(node);
        }
    }
    found
}

/// `Change modifiers to final` where a selection spans several
/// declarations: each of them that can take it.
pub fn final_in_selection(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Option<Action> {
    if selection.is_empty() {
        return None;
    }
    let declarations = touched_declarations(file, selection);
    if declarations.len() < 2 {
        return None;
    }
    let mut candidates = Vec::new();
    for declaration in declarations {
        candidates.extend(
            final_candidates(snapshot, file, Some(declaration))
                .into_iter()
                .filter(|candidate| declaration.start <= candidate.name && candidate.name < declaration.end),
        );
    }
    let edits = one_per_declaration(candidates);
    (!edits.is_empty()).then(|| single(file, "Change modifiers to final", QUICK_ASSIST, edits))
}

/// The assists of the fields under the selection: `Add Access Modifier`
/// or `Change modifier for 'x'` for the first of them, and the getter and
/// setter of one alone (`Generate Getter for 'x'`, …). On the header of a
/// class with a single field, that field's getter and setter.
pub fn field_assists(snapshot: &Snapshot, file: &FileView, selection: &Selection) -> Vec<Action> {
    let mut actions = Vec::new();
    let path = path_to(&file.tree, selection.start);
    let Some(class) = path.iter().rev().find(|node| is_type_declaration(node)) else {
        return actions;
    };
    let Some(body) = class.child("body") else {
        return actions;
    };
    // An enum's fields follow its constants, a level down.
    let fields: Vec<&Syn> = body
        .children
        .iter()
        .flat_map(|member| {
            if member.kind == "enum_body_declarations" {
                member.children.iter().collect::<Vec<_>>()
            } else {
                vec![member]
            }
        })
        .filter(|member| matches!(member.kind.as_str(), "field_declaration" | "constant_declaration"))
        .collect();
    let touched: Vec<&Syn> = fields.iter().copied().filter(|field| selection.touches(field)).collect();
    // The header of the class, not its body.
    let on_header = !selection.touches(body) || selection.end <= body.start;
    let first_name = |field: &Syn| -> Option<(Position, String)> {
        let name = field.first("variable_declarator")?.child("name")?;
        Some((name.start, name.text.clone()))
    };
    if let Some(first) = touched.first() {
        let name = first_name(first);
        match (first.first("modifiers"), name) {
            (None, Some(_)) => actions.push(needs_input(file, "Add Access Modifier", QUICK_ASSIST, "changeModifier")),
            (Some(_), Some((_, name))) => {
                actions.push(needs_input(file, format!("Change modifier for '{name}'"), QUICK_ASSIST, "changeModifier"));
            }
            _ => {}
        }
    }
    let alone = match touched.as_slice() {
        [only] => Some(*only),
        [] if on_header && fields.len() == 1 && fields[0].all("variable_declarator").count() == 1 => Some(fields[0]),
        _ => None,
    };
    let Some((at, _)) = alone.and_then(first_name) else {
        return actions;
    };
    if alone.is_some_and(|field| field.all("variable_declarator").count() != 1) {
        return actions;
    }
    let Some((plan, insert)) = accessor_plan(snapshot, file, selection.start) else {
        return actions;
    };
    let Some(field) = plan.iter().find(|field| field.name_at == at) else {
        return actions;
    };
    let edits = |methods: Vec<&String>| -> Vec<Edit> {
        let methods: Vec<String> = methods.into_iter().cloned().collect();
        insert.edits(&methods)
    };
    if let Some(getter) = &field.getter {
        actions.push(single(file, format!("Generate Getter for '{}'", field.name), QUICK_ASSIST, edits(vec![getter])));
    }
    if let Some(setter) = &field.setter {
        actions.push(single(file, format!("Generate Setter for '{}'", field.name), QUICK_ASSIST, edits(vec![setter])));
    }
    if let (Some(getter), Some(setter)) = (&field.getter, &field.setter) {
        actions.push(single(
            file,
            format!("Generate Getter and Setter for '{}'", field.name),
            QUICK_ASSIST,
            edits(vec![getter, setter]),
        ));
    }
    actions
}
