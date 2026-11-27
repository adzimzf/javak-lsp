//! Signature stubs: the Java declarations of a library's class files,
//! written as source, so a library without sources is indexed like one
//! with them — hover, completion, types and checks — without starting a
//! JVM. jdtls and IntelliJ read class files the same way and decompile a
//! class only when it is opened; `decompile::decompile_class` does that
//! part here.
//!
//! A stub declares what the class file declares, as javac would have seen
//! it: modifiers, generic signatures, constant values, parameter names
//! (where the class file kept them), annotations, nested types. Bodies are
//! `{ /* compiled code */ }`. Private and synthetic members are left out:
//! nothing outside the library can name them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::Path;

use crate::classfile::{self, access, Annotation, ClassFile, Constant, ElementValue, Method};

/// The entry of a stubs jar naming the jar its stubs were written from.
pub(crate) const ORIGIN_ENTRY: &str = "META-INF/javak-lsp-stubs-of";

/// A body: no code is read.
const BODY: &str = "{ /* compiled code */ }";

/// What a stubs jar was made of.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stubbed {
    /// The stubs were written, this many files.
    Written(usize),
    /// A class is Kotlin: a Java view of it would mislead Kotlin callers,
    /// so the jar is decompiled instead.
    Kotlin,
}

/// Write the stubs of the classes of `jar` in `packages` (every package
/// where `None`) into the sources jar `destination`.
pub(crate) fn write_stubs(jar: &Path, packages: Option<&[String]>, destination: &Path) -> Result<Stubbed, String> {
    let classes = read_classes(jar, packages)?;
    let Some(classes) = classes else {
        return Ok(Stubbed::Kotlin);
    };
    let files = render_all(&classes);
    if files.is_empty() {
        return Err("the jar has no class to write a stub of".to_owned());
    }
    let temporary = destination.with_extension("jar.tmp");
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(&temporary).map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file(ORIGIN_ENTRY, options).map_err(|error| error.to_string())?;
    zip.write_all(jar.to_string_lossy().as_bytes()).map_err(|error| error.to_string())?;
    let count = files.len();
    for (entry, text) in files {
        zip.start_file(entry, options).map_err(|error| error.to_string())?;
        zip.write_all(text.as_bytes()).map_err(|error| error.to_string())?;
    }
    zip.finish().map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, destination).map_err(|error| error.to_string())?;
    Ok(Stubbed::Written(count))
}

/// The classes of `jar` in `packages`, by internal name; `None` where one
/// is Kotlin.
fn read_classes(jar: &Path, packages: Option<&[String]>) -> Result<Option<HashMap<String, ClassFile>>, String> {
    let file = std::fs::File::open(jar).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut classes = HashMap::new();
    let mut bytes = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().to_owned();
        let Some(stem) = name.strip_suffix(".class") else {
            continue;
        };
        if name.starts_with("META-INF/") || stem.ends_with("module-info") || stem.ends_with("package-info") {
            continue;
        }
        let directory = stem.rsplit_once('/').map_or("", |(directory, _)| directory);
        if packages.is_some_and(|packages| !packages.iter().any(|package| package == directory)) {
            continue;
        }
        bytes.clear();
        entry.read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        if classfile::mentions_kotlin_metadata(&bytes) {
            let class = classfile::parse(&bytes)?;
            if class.is_kotlin() {
                return Ok(None);
            }
        }
        match classfile::parse(&bytes) {
            Ok(class) => {
                classes.insert(class.name.clone(), class);
            }
            Err(error) => log::debug!("stubs: cannot read {name} of {}: {error}", jar.display()),
        }
    }
    Ok(Some(classes))
}

/// Every top-level class of `classes` as `(entry, text)`, sorted.
pub(crate) fn render_all(classes: &HashMap<String, ClassFile>) -> Vec<(String, String)> {
    // Every nested class any class file names, `a/Outer$Inner` → its outer
    // class and simple name.
    let mut nested: HashMap<&str, (&str, &str)> = HashMap::new();
    for class in classes.values() {
        for inner in &class.inner_classes {
            if let (Some(outer), Some(name)) = (&inner.outer, &inner.name) {
                nested.insert(&inner.inner, (outer, name));
            }
        }
    }
    let mut by_package: HashMap<&str, HashSet<&str>> = HashMap::new();
    for name in classes.keys() {
        if !nested.contains_key(name.as_str()) {
            let (package, simple) = split_internal(name);
            by_package.entry(package).or_default().insert(simple);
        }
    }
    let mut files: Vec<(String, String)> = classes
        .values()
        .filter(|class| is_top_level(class, &nested))
        .map(|class| {
            let context = Context { classes, nested: &nested, by_package: &by_package, type_variables: Default::default() };
            (format!("{}.java", class.name), context.file(class))
        })
        .collect();
    files.sort();
    files
}

fn is_top_level(class: &ClassFile, nested: &HashMap<&str, (&str, &str)>) -> bool {
    class.access & access::SYNTHETIC == 0
        && !class.enclosed_in_method
        && !nested.contains_key(class.name.as_str())
        && class.as_inner().is_none_or(|inner| inner.outer.is_none() && inner.name.is_some())
        && !class.name.rsplit('/').next().is_some_and(|simple| simple.is_empty())
        && !is_anonymous_name(&class.name)
}

/// `Foo$1`: an anonymous class some class file did not describe.
fn is_anonymous_name(name: &str) -> bool {
    name.rsplit_once('$')
        .is_some_and(|(_, last)| !last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit()))
}

fn split_internal(name: &str) -> (&str, &str) {
    name.rsplit_once('/').unwrap_or(("", name))
}

struct Context<'a> {
    classes: &'a HashMap<String, ClassFile>,
    nested: &'a HashMap<&'a str, (&'a str, &'a str)>,
    by_package: &'a HashMap<&'a str, HashSet<&'a str>>,
    /// The type variables the file declares: a type of the same simple
    /// name is written qualified.
    type_variables: std::cell::RefCell<HashSet<String>>,
}

/// Marks a reference to a top-level type in rendered text, decided once the
/// whole file is known: `\u{1}java/util/List\u{2}`.
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';

impl Context<'_> {
    fn file(&self, class: &ClassFile) -> String {
        let (package, _) = split_internal(&class.name);
        let mut body = String::new();
        let mut shadowing: HashSet<String> = HashSet::new();
        self.collect_member_names(class, &mut shadowing);
        self.class(class, 0, &mut body);
        shadowing.extend(self.type_variables.borrow().iter().cloned());
        let (resolved, imports) = self.resolve_names(&body, package, class, &shadowing);
        let mut text = String::new();
        if !package.is_empty() {
            let _ = writeln!(text, "package {};\n", package.replace('/', "."));
        }
        if !imports.is_empty() {
            for import in &imports {
                let _ = writeln!(text, "import {import};");
            }
            text.push('\n');
        }
        text.push_str(&resolved);
        text
    }

    /// The simple names of the types declared in `class`, nested ones
    /// included: they shadow a top-level type of the same name.
    fn collect_member_names(&self, class: &ClassFile, names: &mut HashSet<String>) {
        for inner in self.member_classes(class) {
            names.insert(simple_name_of(&inner.name).to_owned());
            self.collect_member_names(inner, names);
        }
    }

    fn member_classes<'c>(&'c self, class: &'c ClassFile) -> impl Iterator<Item = &'c ClassFile> + 'c {
        class
            .inner_classes
            .iter()
            .filter(move |inner| inner.outer.as_deref() == Some(class.name.as_str()) && inner.name.is_some())
            .filter(|inner| inner.access & (access::SYNTHETIC | access::PRIVATE) == 0)
            .filter_map(|inner| self.classes.get(&inner.inner))
    }

    /// Replace the marked references with simple names where that is
    /// unambiguous, and say what to import.
    fn resolve_names(&self, body: &str, package: &str, top: &ClassFile, shadowing: &HashSet<String>) -> (String, BTreeSet<String>) {
        let mut referenced: BTreeSet<&str> = BTreeSet::new();
        referenced.insert(top.name.as_str());
        let mut rest = body;
        while let Some(start) = rest.find(OPEN) {
            let after = &rest[start + OPEN.len_utf8()..];
            let Some(end) = after.find(CLOSE) else { break };
            referenced.insert(&after[..end]);
            rest = &after[end + CLOSE.len_utf8()..];
        }
        let mut by_simple: HashMap<&str, Vec<&str>> = HashMap::new();
        for name in &referenced {
            by_simple.entry(split_internal(name).1).or_default().push(name);
        }
        let same_package = self.by_package.get(package);
        let mut simple_for: HashSet<&str> = HashSet::new();
        let mut imports = BTreeSet::new();
        for (simple, names) in &by_simple {
            if shadowing.contains(*simple) {
                continue;
            }
            // One type per simple name; of several, the package's own.
            let chosen = match names.as_slice() {
                [only] => *only,
                _ => match names.iter().find(|name| split_internal(name).0 == package) {
                    Some(local) => *local,
                    None => continue,
                },
            };
            simple_for.insert(chosen);
            let (chosen_package, _) = split_internal(chosen);
            if chosen_package == package || chosen_package.is_empty() {
                continue;
            }
            // `java.lang` needs no import, unless the package has a type
            // of the same name, which an import then shadows.
            let package_has_one = same_package.is_some_and(|classes| classes.contains(simple));
            if chosen_package != "java/lang" || package_has_one {
                imports.insert(chosen.replace('/', "."));
            }
        }
        let mut out = String::with_capacity(body.len());
        let mut rest = body;
        while let Some(start) = rest.find(OPEN) {
            out.push_str(&rest[..start]);
            let after = &rest[start + OPEN.len_utf8()..];
            let Some(end) = after.find(CLOSE) else { break };
            let name = &after[..end];
            if simple_for.contains(name) {
                out.push_str(split_internal(name).1);
            } else {
                out.push_str(&name.replace('/', "."));
            }
            rest = &after[end + CLOSE.len_utf8()..];
        }
        out.push_str(rest);
        (out, imports)
    }

    /// A reference to the class `internal`: its top-level class marked,
    /// the nested names after it.
    fn class_name(&self, internal: &str) -> String {
        let mut chain = Vec::new();
        let mut current = internal;
        while let Some((outer, simple)) = self.nested.get(current) {
            chain.push(*simple);
            current = outer;
        }
        let mut text = format!("{OPEN}{current}{CLOSE}");
        for simple in chain.iter().rev() {
            text.push('.');
            text.push_str(simple);
        }
        text
    }

    fn class(&self, class: &ClassFile, depth: usize, out: &mut String) {
        let indent = "    ".repeat(depth);
        let access_flags = class.as_inner().map_or(class.access, |inner| inner.access);
        let is_interface = class.access & access::INTERFACE != 0;
        let is_annotation = class.access & access::ANNOTATION != 0;
        let is_enum = class.access & access::ENUM != 0 && class.super_class.as_deref() == Some("java/lang/Enum");
        let is_record = class.record.is_some();
        self.annotations(&class.annotations, class.deprecated, &indent, out);
        out.push_str(&indent);
        out.push_str(&visibility(access_flags));
        if depth > 0 && access_flags & access::STATIC != 0 && !is_interface && !is_enum && !is_record {
            out.push_str("static ");
        }
        if access_flags & access::ABSTRACT != 0 && !is_interface && !is_enum {
            out.push_str("abstract ");
        }
        let sealed = !class.permitted.is_empty();
        if sealed {
            out.push_str("sealed ");
        } else if access_flags & access::FINAL != 0 && !is_enum && !is_record {
            out.push_str("final ");
        }
        out.push_str(if is_annotation {
            "@interface "
        } else if is_interface {
            "interface "
        } else if is_enum {
            "enum "
        } else if is_record {
            "record "
        } else {
            "class "
        });
        out.push_str(simple_name_of(&class.name));
        let (type_parameters, super_class, interfaces) = match class.signature.as_deref().and_then(|signature| self.class_signature(signature)) {
            Some(parsed) => parsed,
            None => (
                String::new(),
                class.super_class.as_deref().map(|name| self.class_name(name)),
                class.interfaces.iter().map(|name| self.class_name(name)).collect(),
            ),
        };
        out.push_str(&type_parameters);
        if let Some(components) = &class.record {
            out.push('(');
            let rendered: Vec<String> = components
                .iter()
                .map(|component| {
                    let mut text = self.inline_annotations(&component.annotations);
                    let kind = component
                        .signature
                        .as_deref()
                        .and_then(|signature| self.field_signature(signature))
                        .unwrap_or_else(|| self.descriptor_type(&component.descriptor));
                    let _ = write!(text, "{kind} {}", component.name);
                    text
                })
                .collect();
            out.push_str(&rendered.join(", "));
            out.push(')');
        }
        let implicit_super = matches!(
            class.super_class.as_deref(),
            None | Some("java/lang/Object") | Some("java/lang/Enum") | Some("java/lang/Record")
        );
        if !is_interface && !is_enum && !is_record && !implicit_super
            && let Some(super_class) = super_class
        {
            let _ = write!(out, " extends {super_class}");
        }
        let interfaces: Vec<String> = if is_annotation {
            Vec::new()
        } else {
            interfaces
        };
        if !interfaces.is_empty() {
            let keyword = if is_interface { "extends" } else { "implements" };
            let _ = write!(out, " {keyword} {}", interfaces.join(", "));
        }
        if sealed {
            let permitted: Vec<String> = class.permitted.iter().map(|name| self.class_name(name)).collect();
            let _ = write!(out, " permits {}", permitted.join(", "));
        }
        out.push_str(" {\n");
        let inner_indent = "    ".repeat(depth + 1);
        if is_enum {
            let constants: Vec<String> = class
                .fields
                .iter()
                .filter(|field| field.access & access::ENUM != 0)
                .map(|field| {
                    let mut text = String::new();
                    self.annotations(&field.annotations, field.deprecated, &inner_indent, &mut text);
                    let _ = write!(text, "{inner_indent}{}", field.name);
                    text
                })
                .collect();
            out.push_str(&constants.join(",\n"));
            out.push_str(";\n");
        }
        for field in &class.fields {
            if field.access & (access::SYNTHETIC | access::PRIVATE) != 0 || (is_enum && field.access & access::ENUM != 0) {
                continue;
            }
            if is_record && field.access & access::STATIC == 0 {
                continue;
            }
            out.push('\n');
            self.annotations(&field.annotations, field.deprecated, &inner_indent, out);
            out.push_str(&inner_indent);
            out.push_str(&visibility(field.access));
            for (flag, word) in [(access::STATIC, "static "), (access::FINAL, "final "), (access::TRANSIENT, "transient "), (access::VOLATILE, "volatile ")] {
                if field.access & flag != 0 {
                    out.push_str(word);
                }
            }
            let kind = field
                .signature
                .as_deref()
                .and_then(|signature| self.field_signature(signature))
                .unwrap_or_else(|| self.descriptor_type(&field.descriptor));
            let _ = write!(out, "{kind} {}", field.name);
            if let Some(constant) = &field.constant {
                let _ = write!(out, " = {}", constant_literal(constant, &field.descriptor));
            }
            out.push_str(";\n");
        }
        for method in &class.methods {
            if !self.shows_method(class, method, is_enum, is_record) {
                continue;
            }
            out.push('\n');
            self.method(class, method, is_interface, is_annotation, &inner_indent, out);
        }
        for inner in self.member_classes(class) {
            out.push('\n');
            self.class(inner, depth + 1, out);
        }
        let _ = writeln!(out, "{indent}}}");
    }

    fn shows_method(&self, class: &ClassFile, method: &Method, is_enum: bool, is_record: bool) -> bool {
        if method.access & (access::SYNTHETIC | access::BRIDGE | access::PRIVATE) != 0 || method.name == "<clinit>" {
            return false;
        }
        if is_enum {
            let name_of_enum = format!("(Ljava/lang/String;)L{};", class.name);
            if method.name == "<init>"
                || (method.name == "values" && method.descriptor.starts_with("()"))
                || (method.name == "valueOf" && method.descriptor == name_of_enum)
            {
                return false;
            }
        }
        if is_record && method.name == "<init>" {
            // The canonical constructor is the record's header.
            let components: String = class
                .record
                .iter()
                .flatten()
                .map(|component| component.descriptor.as_str())
                .collect();
            if method.descriptor == format!("({components})V") {
                return false;
            }
        }
        true
    }

    fn method(&self, class: &ClassFile, method: &Method, is_interface: bool, is_annotation: bool, indent: &str, out: &mut String) {
        self.annotations(&method.annotations, method.deprecated, indent, out);
        out.push_str(indent);
        let is_constructor = method.name == "<init>";
        let is_static = method.access & access::STATIC != 0;
        let is_abstract = method.access & access::ABSTRACT != 0;
        if !is_interface || method.access & access::PUBLIC == 0 {
            out.push_str(&visibility(method.access));
        }
        if is_interface && !is_static && !is_abstract {
            out.push_str("default ");
        }
        for (flag, word) in [
            (access::ABSTRACT, "abstract "),
            (access::STATIC, "static "),
            (access::FINAL, "final "),
            (access::SYNCHRONIZED, "synchronized "),
            (access::NATIVE, "native "),
        ] {
            if method.access & flag != 0 && !(is_interface && flag == access::ABSTRACT) {
                out.push_str(word);
            }
        }
        let descriptor_parameters: Vec<String> = classfile::descriptor_parameters(&method.descriptor)
            .into_iter()
            .map(|parameter| self.descriptor_type(parameter))
            .collect();
        let parsed = method.signature.as_deref().and_then(|signature| self.method_signature(signature));
        let (type_parameters, mut parameters, return_type, mut exceptions) = match parsed {
            Some(parsed) => parsed,
            None => (
                String::new(),
                descriptor_parameters.clone(),
                self.descriptor_type(method.descriptor.rsplit_once(')').map_or("V", |(_, ret)| ret)),
                Vec::new(),
            ),
        };
        if exceptions.is_empty() {
            exceptions = method.exceptions.iter().map(|name| self.class_name(name)).collect();
        }
        // The parameters javac adds (an inner class's outer instance) are
        // in the descriptor, not in the source.
        let mut names: Vec<String> = method.parameter_names.clone().unwrap_or_default();
        let mut parameter_annotations = method.parameter_annotations.clone();
        let descriptor_count = descriptor_parameters.len();
        if method.signature.is_none() {
            let implicit: Vec<bool> = if method.implicit_parameters.len() == descriptor_count {
                method.implicit_parameters.clone()
            } else {
                let outer_instance = is_constructor
                    && class.as_inner().is_some_and(|inner| inner.access & access::STATIC == 0 && inner.outer.is_some())
                    && class.access & access::INTERFACE == 0;
                (0..descriptor_count).map(|index| index == 0 && outer_instance).collect()
            };
            parameters = parameters
                .into_iter()
                .zip(&implicit)
                .filter(|(_, implicit)| !**implicit)
                .map(|(parameter, _)| parameter)
                .collect();
            if names.len() == descriptor_count {
                names = names.into_iter().zip(&implicit).filter(|(_, implicit)| !**implicit).map(|(name, _)| name).collect();
            }
        }
        if names.len() > parameters.len() {
            names.drain(..names.len() - parameters.len());
        }
        if parameter_annotations.len() > parameters.len() {
            parameter_annotations.drain(..parameter_annotations.len() - parameters.len());
        }
        let varargs = method.access & access::VARARGS != 0;
        let count = parameters.len();
        let rendered: Vec<String> = parameters
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let mut text = parameter_annotations
                    .get(index)
                    .map(|annotations| self.inline_annotations(annotations))
                    .unwrap_or_default();
                let kind = if varargs && index + 1 == count {
                    kind.strip_suffix("[]").map_or(kind.clone(), |element| format!("{element}..."))
                } else {
                    kind
                };
                let name = names.get(index).filter(|name| is_identifier(name)).cloned().unwrap_or_else(|| format!("arg{index}"));
                let _ = write!(text, "{kind} {name}");
                text
            })
            .collect();
        if !type_parameters.is_empty() {
            let _ = write!(out, "{type_parameters} ");
        }
        if is_constructor {
            out.push_str(simple_name_of(&class.name));
        } else {
            let _ = write!(out, "{return_type} {}", method.name);
        }
        let _ = write!(out, "({})", rendered.join(", "));
        if is_annotation && let Some(default) = &method.default {
            let _ = write!(out, " default {}", self.element_value(default));
        }
        if !exceptions.is_empty() {
            let _ = write!(out, " throws {}", exceptions.join(", "));
        }
        if is_abstract || method.access & access::NATIVE != 0 {
            out.push_str(";\n");
        } else {
            let _ = writeln!(out, " {BODY}");
        }
    }

    fn annotations(&self, annotations: &[Annotation], deprecated: bool, indent: &str, out: &mut String) {
        let mut wrote_deprecated = false;
        for annotation in annotations {
            if is_hidden_annotation(&annotation.descriptor) {
                continue;
            }
            wrote_deprecated |= annotation.descriptor == "Ljava/lang/Deprecated;";
            let _ = writeln!(out, "{indent}{}", self.annotation(annotation));
        }
        if deprecated && !wrote_deprecated {
            let _ = writeln!(out, "{indent}@{}", self.class_name("java/lang/Deprecated"));
        }
    }

    fn inline_annotations(&self, annotations: &[Annotation]) -> String {
        annotations
            .iter()
            .filter(|annotation| !is_hidden_annotation(&annotation.descriptor))
            .map(|annotation| format!("{} ", self.annotation(annotation)))
            .collect()
    }

    fn annotation(&self, annotation: &Annotation) -> String {
        let name = self.descriptor_type(&annotation.descriptor);
        match annotation.elements.as_slice() {
            [] => format!("@{name}"),
            [(key, value)] if key == "value" => format!("@{name}({})", self.element_value(value)),
            elements => {
                let rendered: Vec<String> = elements
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", self.element_value(value)))
                    .collect();
                format!("@{name}({})", rendered.join(", "))
            }
        }
    }

    fn element_value(&self, value: &ElementValue) -> String {
        match value {
            ElementValue::Const(tag, constant) => {
                let descriptor = match tag {
                    b's' => "Ljava/lang/String;".to_owned(),
                    other => (*other as char).to_string(),
                };
                constant_literal(constant, &descriptor)
            }
            ElementValue::Enum(descriptor, name) => format!("{}.{name}", self.descriptor_type(descriptor)),
            ElementValue::Class(descriptor) => format!("{}.class", self.descriptor_type(descriptor)),
            ElementValue::Annotation(annotation) => self.annotation(annotation),
            ElementValue::Array(values) => {
                let rendered: Vec<String> = values.iter().map(|value| self.element_value(value)).collect();
                format!("{{{}}}", rendered.join(", "))
            }
        }
    }

    /// A field descriptor as a Java type.
    fn descriptor_type(&self, descriptor: &str) -> String {
        let dimensions = descriptor.bytes().take_while(|byte| *byte == b'[').count();
        let element = &descriptor[dimensions..];
        let mut text = match element.as_bytes().first() {
            Some(b'L') => self.class_name(element.trim_start_matches('L').trim_end_matches(';')),
            Some(byte) => primitive(*byte).unwrap_or("Object").to_owned(),
            None => "void".to_owned(),
        };
        for _ in 0..dimensions {
            text.push_str("[]");
        }
        text
    }

    fn class_signature(&self, signature: &str) -> Option<(String, Option<String>, Vec<String>)> {
        let mut parser = SignatureParser { text: signature.as_bytes(), at: 0, context: self };
        let type_parameters = parser.type_parameters()?;
        let super_class = parser.field_type()?;
        let mut interfaces = Vec::new();
        while !parser.done() {
            interfaces.push(parser.field_type()?);
        }
        Some((type_parameters, Some(super_class), interfaces))
    }

    fn field_signature(&self, signature: &str) -> Option<String> {
        let mut parser = SignatureParser { text: signature.as_bytes(), at: 0, context: self };
        let kind = parser.field_type()?;
        parser.done().then_some(kind)
    }

    fn method_signature(&self, signature: &str) -> Option<(String, Vec<String>, String, Vec<String>)> {
        let mut parser = SignatureParser { text: signature.as_bytes(), at: 0, context: self };
        let type_parameters = parser.type_parameters()?;
        parser.expect(b'(')?;
        let mut parameters = Vec::new();
        while parser.peek()? != b')' {
            parameters.push(parser.type_signature()?);
        }
        parser.expect(b')')?;
        let return_type = parser.type_signature()?;
        let mut exceptions = Vec::new();
        while !parser.done() {
            parser.expect(b'^')?;
            exceptions.push(parser.field_type()?);
        }
        Some((type_parameters, parameters, return_type, exceptions))
    }
}

/// A generic signature (JVMS 4.7.9.1) read into Java types.
struct SignatureParser<'a, 'c> {
    text: &'a [u8],
    at: usize,
    context: &'c Context<'c>,
}

impl SignatureParser<'_, '_> {
    fn done(&self) -> bool {
        self.at >= self.text.len()
    }
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }
    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.peek()? == byte).then(|| self.at += 1)
    }
    fn identifier(&mut self, stops: &[u8]) -> Option<String> {
        let start = self.at;
        while self.peek().is_some_and(|byte| !stops.contains(&byte)) {
            self.at += 1;
        }
        (self.at > start).then(|| String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    fn type_parameters(&mut self) -> Option<String> {
        if self.peek() != Some(b'<') {
            return Some(String::new());
        }
        self.at += 1;
        let mut parameters = Vec::new();
        while self.peek()? != b'>' {
            let name = self.identifier(b":")?;
            self.context.type_variables.borrow_mut().insert(name.clone());
            let mut bounds = Vec::new();
            // The class bound, which may be empty, then interface bounds.
            while self.peek() == Some(b':') {
                self.at += 1;
                if matches!(self.peek(), Some(b'L' | b'T' | b'[')) {
                    bounds.push(self.field_type()?);
                }
            }
            bounds.retain(|bound| !is_object(bound));
            parameters.push(if bounds.is_empty() { name } else { format!("{name} extends {}", bounds.join(" & ")) });
        }
        self.at += 1;
        Some(format!("<{}>", parameters.join(", ")))
    }

    fn type_signature(&mut self) -> Option<String> {
        match self.peek()? {
            byte @ (b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V') => {
                self.at += 1;
                primitive(byte).map(str::to_owned)
            }
            _ => self.field_type(),
        }
    }

    fn field_type(&mut self) -> Option<String> {
        match self.peek()? {
            b'L' => {
                self.at += 1;
                let outer = self.identifier(b"<.;")?;
                let mut text = self.context.class_name(&outer);
                text.push_str(&self.type_arguments()?);
                while self.peek() == Some(b'.') {
                    self.at += 1;
                    let inner = self.identifier(b"<.;")?;
                    let _ = write!(text, ".{inner}");
                    text.push_str(&self.type_arguments()?);
                }
                self.expect(b';')?;
                Some(text)
            }
            b'T' => {
                self.at += 1;
                let name = self.identifier(b";")?;
                self.expect(b';')?;
                Some(name)
            }
            b'[' => {
                self.at += 1;
                Some(format!("{}[]", self.type_signature()?))
            }
            _ => None,
        }
    }

    fn type_arguments(&mut self) -> Option<String> {
        if self.peek() != Some(b'<') {
            return Some(String::new());
        }
        self.at += 1;
        let mut arguments = Vec::new();
        while self.peek()? != b'>' {
            arguments.push(match self.peek()? {
                b'*' => {
                    self.at += 1;
                    "?".to_owned()
                }
                b'+' => {
                    self.at += 1;
                    format!("? extends {}", self.field_type()?)
                }
                b'-' => {
                    self.at += 1;
                    format!("? super {}", self.field_type()?)
                }
                _ => self.field_type()?,
            });
        }
        self.at += 1;
        Some(format!("<{}>", arguments.join(", ")))
    }
}

fn is_object(rendered: &str) -> bool {
    rendered == format!("{OPEN}java/lang/Object{CLOSE}")
}

fn simple_name_of(internal: &str) -> &str {
    let simple = split_internal(internal).1;
    simple.rsplit('$').next().filter(|last| !last.is_empty()).unwrap_or(simple)
}

fn primitive(byte: u8) -> Option<&'static str> {
    Some(match byte {
        b'B' => "byte",
        b'C' => "char",
        b'D' => "double",
        b'F' => "float",
        b'I' => "int",
        b'J' => "long",
        b'S' => "short",
        b'Z' => "boolean",
        b'V' => "void",
        _ => return None,
    })
}

fn visibility(flags: u16) -> String {
    if flags & access::PUBLIC != 0 {
        "public ".to_owned()
    } else if flags & access::PROTECTED != 0 {
        "protected ".to_owned()
    } else if flags & access::PRIVATE != 0 {
        "private ".to_owned()
    } else {
        String::new()
    }
}

/// Annotations that say how the class was compiled, not what it declares.
fn is_hidden_annotation(descriptor: &str) -> bool {
    descriptor == "Lkotlin/Metadata;" || descriptor.starts_with("Ljdk/internal/") || descriptor.starts_with("Lsun/")
}

fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && characters.all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

/// A constant as a Java literal of the type `descriptor`.
fn constant_literal(constant: &Constant, descriptor: &str) -> String {
    match constant {
        Constant::Int(value) => match descriptor {
            "Z" => (*value != 0).to_string(),
            "C" => char_literal(*value as u16),
            _ => value.to_string(),
        },
        Constant::Long(value) => format!("{value}L"),
        Constant::Float(value) => {
            if value.is_nan() {
                "0.0f / 0.0f".to_owned()
            } else if value.is_infinite() {
                format!("{}1.0f / 0.0f", if *value < 0.0 { "-" } else { "" })
            } else {
                format!("{value:?}f")
            }
        }
        Constant::Double(value) => {
            if value.is_nan() {
                "0.0 / 0.0".to_owned()
            } else if value.is_infinite() {
                format!("{}1.0 / 0.0", if *value < 0.0 { "-" } else { "" })
            } else {
                format!("{value:?}")
            }
        }
        Constant::String(text) => {
            let mut literal = String::with_capacity(text.len() + 2);
            literal.push('"');
            for unit in text.encode_utf16() {
                literal.push_str(&escaped(unit, '"'));
            }
            literal.push('"');
            // Unpaired surrogates were escaped one by one; paired ones are
            // put back together.
            unescape_pairs(&literal)
        }
    }
}

fn char_literal(unit: u16) -> String {
    format!("'{}'", escaped(unit, '\''))
}

fn escaped(unit: u16, quote: char) -> String {
    match unit {
        0x08 => "\\b".to_owned(),
        0x09 => "\\t".to_owned(),
        0x0A => "\\n".to_owned(),
        0x0C => "\\f".to_owned(),
        0x0D => "\\r".to_owned(),
        0x5C => "\\\\".to_owned(),
        unit if unit == quote as u16 => format!("\\{quote}"),
        0x20..=0x7E => (unit as u8 as char).to_string(),
        unit => match char::from_u32(unit as u32) {
            Some(character) if !character.is_control() && !(0xD800..=0xDFFF).contains(&unit) => character.to_string(),
            _ => format!("\\u{unit:04x}"),
        },
    }
}

/// `😀` → 😀: a surrogate pair escaped unit by unit is written
/// as its character.
fn unescape_pairs(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    let mut rest = literal;
    while let Some(start) = rest.find("\\ud") {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let pair = (candidate.len() >= 12)
            .then(|| (u16::from_str_radix(&candidate[2..6], 16).ok(), &candidate[6..8], u16::from_str_radix(&candidate[8..12], 16).ok()))
            .and_then(|(high, marker, low)| (marker == "\\u").then_some((high?, low?)))
            .and_then(|(high, low)| char::decode_utf16([high, low]).next()?.ok());
        match pair {
            Some(character) => {
                out.push(character);
                rest = &candidate[12..];
            }
            None => {
                out.push_str(&candidate[..3]);
                rest = &candidate[3..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[path = "stubs_tests.rs"]
mod tests;
