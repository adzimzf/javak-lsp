//! What organizing imports needs to know of the names a file uses: which
//! of its imports resolve, the imports it lacks, and whether the file has
//! errors JDT would stop a clean-up for.

use crate::feature::semantic::{imports, is_type_declaration, package_of, path_to, FileView, Import, Snapshot};

/// Whether an import names something there is: the type of a single-type
/// import, the type of a static one, the package of an on-demand one.
pub fn resolves(snapshot: &Snapshot, file: &FileView, import: &Import) -> bool {
    if import.is_static {
        let owner = if import.wildcard {
            import.name.as_str()
        } else {
            import.name.rsplit_once('.').map_or(import.name.as_str(), |(owner, _)| owner)
        };
        return snapshot.type_named(owner, file).is_some_and(|found| is_type_declaration(found.node));
    }
    if import.wildcard {
        return snapshot.package_exists(&import.name) || snapshot.type_named(&import.name, file).is_some();
    }
    snapshot.type_named(&import.name, file).is_some()
}

/// The imports a file lacks: each simple type name it writes that nothing
/// visible here declares, and that exactly one type of another package of
/// the workspace declares, sorted. A library's are left alone: a JDK holds
/// several types of many a name (`java.util.List`, `java.awt.List`), and
/// jdtls asks which one is meant.
pub fn missing(snapshot: &Snapshot, file: &FileView) -> Vec<String> {
    let own_package = package_of(snapshot, file).unwrap_or_default();
    let mut missing: Vec<String> = Vec::new();
    let mut asked = std::collections::HashSet::new();
    for node in file.tree.walk() {
        if node.kind != "type_identifier" || !asked.insert(node.text.clone()) {
            continue;
        }
        let path = path_to(&file.tree, node.start);
        let in_header = path
            .iter()
            .any(|around| matches!(around.kind.as_str(), "import_declaration" | "package_declaration" | "scoped_type_identifier"));
        if in_header {
            continue;
        }
        // Found, and visible here: of this file or package, imported, or of
        // `java.lang`. The server finds a type of another package by its
        // simple name too, which the compiler does not without an import.
        if let Some(found) = snapshot.declaration_at(&file.uri, node.start) {
            let visible = !is_type_declaration(found.node) || found.file.uri == file.uri || {
                let qualified = snapshot.qualified_type_name(found.file, found.node);
                let package = package_of(snapshot, found.file).unwrap_or_default();
                package == own_package
                    || package == "java.lang"
                    || !qualified.starts_with(&format!("{package}."))
                    || imports(file).iter().any(|import| {
                        !import.is_static && (import.name == qualified || import.wildcard && import.name == package)
                    })
            };
            if visible {
                continue;
            }
        }
        let mut declaring: Vec<String> = Vec::new();
        for candidate in snapshot.files.iter().filter(|candidate| !candidate.library) {
            let declares = candidate
                .tree
                .children
                .iter()
                .any(|declaration| is_type_declaration(declaration) && declaration.child("name").is_some_and(|name| name.text == node.text));
            if declares && let Some(package) = package_of(snapshot, candidate).filter(|package| *package != own_package) {
                declaring.push(format!("{package}.{}", node.text));
            }
        }
        declaring.sort();
        declaring.dedup();
        if let [only] = declaring.as_slice() {
            missing.push(only.clone());
        }
    }
    missing.sort();
    missing
}

/// Whether the file has an error JDT would see: a syntax error, an import
/// of nothing there is, or a type it writes that nothing declares.
pub fn has_errors(snapshot: &Snapshot, file: &FileView) -> bool {
    let nodes = file.tree.walk();
    if nodes.iter().any(|node| node.kind == "ERROR" || (node.text.is_empty() && node.start == node.end && node.kind != "program")) {
        return true;
    }
    if imports(file).iter().any(|import| !resolves(snapshot, file, import)) {
        return true;
    }
    nodes.iter().any(|node| {
        node.kind == "type_identifier"
            && !path_to(&file.tree, node.start)
                .iter()
                .any(|around| matches!(around.kind.as_str(), "import_declaration" | "package_declaration" | "scoped_type_identifier"))
            && snapshot.declaration_at(&file.uri, node.start).is_none()
    })
}

/// `Add import` on a type named with its package (`java.lang.String`,
/// `new javax.activity`): the simple name written, the import added
/// unless the type is of `java.lang` or already imported.
pub fn add_import(file: &FileView, selection: &super::assists::Selection) -> Option<super::Action> {
    let path = path_to(&file.tree, selection.start);
    // The outermost dotted type name around the cursor.
    let dotted = path.iter().find(|node| node.kind == "scoped_type_identifier")?;
    let segments: Vec<&str> = dotted.text.split('.').map(str::trim).collect();
    let names_package = segments.len() > 1 && segments[0].chars().next().is_some_and(char::is_lowercase);
    if !names_package || path.iter().any(|node| matches!(node.kind.as_str(), "import_declaration" | "package_declaration")) {
        return None;
    }
    let qualified: String = segments.join(".");
    let simple = segments.last()?.to_string();
    let mut edits = vec![super::Edit {
        start: dotted.start,
        end: dotted.end,
        text: simple,
    }];
    let package = segments[..segments.len() - 1].join(".");
    let imported = imports(file).iter().any(|import| !import.is_static && import.name == qualified);
    if package != "java.lang" && !imported {
        let declarations: Vec<&crate::parser::Syn> = file.tree.all("import_declaration").collect();
        let (at, text) = match (declarations.last(), file.tree.first("package_declaration")) {
            (Some(last), _) => (last.end, format!("\nimport {qualified};")),
            (None, Some(package)) => (package.end, format!("\n\nimport {qualified};")),
            (None, None) => (crate::parser::Position { line: 0, column: 0 }, format!("import {qualified};\n\n")),
        };
        edits.insert(0, super::Edit { start: at, end: at, text });
    }
    Some(super::single(file, "Add import", super::REFACTOR, edits))
}
