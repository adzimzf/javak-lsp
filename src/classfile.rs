//! A reader of the JVM class file format (JVMS chapter 4): what a library
//! declares, read straight from its jar, without a JVM. Only what a
//! declaration says is kept — names, access, generic signatures, constant
//! values, parameter names, annotations, nesting — never code.

/// Access flags (JVMS tables 4.1-B, 4.5-A, 4.6-A, 4.7.6-A).
pub mod access {
    pub const PUBLIC: u16 = 0x0001;
    pub const PRIVATE: u16 = 0x0002;
    pub const PROTECTED: u16 = 0x0004;
    pub const STATIC: u16 = 0x0008;
    pub const FINAL: u16 = 0x0010;
    pub const SYNCHRONIZED: u16 = 0x0020;
    pub const BRIDGE: u16 = 0x0040;
    pub const VOLATILE: u16 = 0x0040;
    pub const VARARGS: u16 = 0x0080;
    pub const TRANSIENT: u16 = 0x0080;
    pub const NATIVE: u16 = 0x0100;
    pub const INTERFACE: u16 = 0x0200;
    pub const ABSTRACT: u16 = 0x0400;
    pub const SYNTHETIC: u16 = 0x1000;
    pub const ANNOTATION: u16 = 0x2000;
    pub const ENUM: u16 = 0x4000;
    pub const MODULE: u16 = 0x8000;
}

/// A constant a field is initialised with (`ConstantValue`).
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
}

/// An annotation's element value (JVMS 4.7.16.1).
#[derive(Debug, Clone, PartialEq)]
pub enum ElementValue {
    /// A constant, with its tag (`B C D F I J S Z s`).
    Const(u8, Constant),
    /// An enum constant: its type's descriptor and the constant's name.
    Enum(String, String),
    /// A class literal, by its return descriptor (`Ljava/lang/String;`, `V`).
    Class(String),
    Annotation(Annotation),
    Array(Vec<ElementValue>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// Its type's descriptor (`Ljava/lang/Deprecated;`).
    pub descriptor: String,
    pub elements: Vec<(String, ElementValue)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    pub signature: Option<String>,
    pub constant: Option<Constant>,
    pub annotations: Vec<Annotation>,
    pub deprecated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Method {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    pub signature: Option<String>,
    /// `Exceptions`: internal names.
    pub exceptions: Vec<String>,
    /// From `MethodParameters`, else the `LocalVariableTable`: one per
    /// parameter of the descriptor where known.
    pub parameter_names: Option<Vec<String>>,
    /// The parameters `MethodParameters` marks synthetic or mandated (an
    /// inner class's outer instance, an enum's name and ordinal).
    pub implicit_parameters: Vec<bool>,
    pub annotations: Vec<Annotation>,
    /// `Runtime(In)VisibleParameterAnnotations`, by parameter.
    pub parameter_annotations: Vec<Vec<Annotation>>,
    /// `AnnotationDefault`.
    pub default: Option<ElementValue>,
    pub deprecated: bool,
}

/// One `InnerClasses` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct InnerClass {
    pub inner: String,
    /// `None` for a local or anonymous class.
    pub outer: Option<String>,
    /// `None` for an anonymous class.
    pub name: Option<String>,
    pub access: u16,
}

/// One `Record` component.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordComponent {
    pub name: String,
    pub descriptor: String,
    pub signature: Option<String>,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassFile {
    pub access: u16,
    /// Internal name (`java/util/Map$Entry`).
    pub name: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub signature: Option<String>,
    pub fields: Vec<Field>,
    pub methods: Vec<Method>,
    pub inner_classes: Vec<InnerClass>,
    /// `EnclosingMethod`: the class is local or anonymous.
    pub enclosed_in_method: bool,
    pub record: Option<Vec<RecordComponent>>,
    pub permitted: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub deprecated: bool,
}

impl ClassFile {
    /// Whether the Kotlin compiler wrote it (`@kotlin.Metadata`).
    pub fn is_kotlin(&self) -> bool {
        self.annotations.iter().any(|annotation| annotation.descriptor == "Lkotlin/Metadata;")
    }

    /// Its own `InnerClasses` entry, where it is nested.
    pub fn as_inner(&self) -> Option<&InnerClass> {
        self.inner_classes.iter().find(|inner| inner.inner == self.name)
    }
}

/// Whether `bytes` is a Kotlin class (`@kotlin.Metadata`), read cheaply:
/// the descriptor is in the constant pool of every Kotlin class.
pub fn mentions_kotlin_metadata(bytes: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"Lkotlin/Metadata;";
    bytes.windows(NEEDLE.len()).any(|window| window == NEEDLE)
}

#[derive(Clone)]
enum Entry {
    Empty,
    Utf8(String),
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    /// Index of a `Utf8`.
    Class(u16),
    String(u16),
    Other,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

type Result<T> = std::result::Result<T, String>;

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).filter(|end| *end <= self.bytes.len());
        let end = end.ok_or_else(|| "truncated class file".to_owned())?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// Modified UTF-8 (JVMS 4.4.7): `C0 80` for NUL, surrogates encoded one by
/// one.
fn modified_utf8(bytes: &[u8]) -> String {
    if bytes.iter().all(|byte| *byte < 0x80 && *byte != 0) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index] as u16;
        if first < 0x80 {
            units.push(first);
            index += 1;
        } else if first & 0xE0 == 0xC0 && index + 1 < bytes.len() {
            units.push(((first & 0x1F) << 6) | (bytes[index + 1] as u16 & 0x3F));
            index += 2;
        } else if first & 0xF0 == 0xE0 && index + 2 < bytes.len() {
            units.push(((first & 0x0F) << 12) | ((bytes[index + 1] as u16 & 0x3F) << 6) | (bytes[index + 2] as u16 & 0x3F));
            index += 3;
        } else {
            units.push(0xFFFD);
            index += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

struct Pool(Vec<Entry>);

impl Pool {
    fn utf8(&self, index: u16) -> Result<String> {
        match self.0.get(index as usize) {
            Some(Entry::Utf8(text)) => Ok(text.clone()),
            _ => Err(format!("constant {index} is no Utf8")),
        }
    }
    fn class(&self, index: u16) -> Result<String> {
        match self.0.get(index as usize) {
            Some(Entry::Class(name)) => self.utf8(*name),
            _ => Err(format!("constant {index} is no Class")),
        }
    }
    fn optional_class(&self, index: u16) -> Result<Option<String>> {
        if index == 0 {
            Ok(None)
        } else {
            self.class(index).map(Some)
        }
    }
    fn constant(&self, index: u16) -> Result<Constant> {
        Ok(match self.0.get(index as usize) {
            Some(Entry::Int(value)) => Constant::Int(*value),
            Some(Entry::Long(value)) => Constant::Long(*value),
            Some(Entry::Float(value)) => Constant::Float(*value),
            Some(Entry::Double(value)) => Constant::Double(*value),
            Some(Entry::String(text)) => Constant::String(self.utf8(*text)?),
            Some(Entry::Utf8(text)) => Constant::String(text.clone()),
            _ => return Err(format!("constant {index} is no value")),
        })
    }
}

/// Parse a class file.
pub fn parse(bytes: &[u8]) -> Result<ClassFile> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.u32()? != 0xCAFE_BABE {
        return Err("not a class file".to_owned());
    }
    reader.take(4)?; // minor, major
    let pool = read_pool(&mut reader)?;
    let access = reader.u16()?;
    let name = pool.class(reader.u16()?)?;
    let super_class = pool.optional_class(reader.u16()?)?;
    let interface_count = reader.u16()?;
    let mut interfaces = Vec::with_capacity(interface_count as usize);
    for _ in 0..interface_count {
        interfaces.push(pool.class(reader.u16()?)?);
    }
    let mut class = ClassFile {
        access,
        name,
        super_class,
        interfaces,
        signature: None,
        fields: Vec::new(),
        methods: Vec::new(),
        inner_classes: Vec::new(),
        enclosed_in_method: false,
        record: None,
        permitted: Vec::new(),
        annotations: Vec::new(),
        deprecated: false,
    };
    for _ in 0..reader.u16()? {
        class.fields.push(read_field(&mut reader, &pool)?);
    }
    for _ in 0..reader.u16()? {
        class.methods.push(read_method(&mut reader, &pool)?);
    }
    for _ in 0..reader.u16()? {
        let (name, body) = read_attribute(&mut reader, &pool)?;
        let mut body = Reader { bytes: body, at: 0 };
        match name.as_str() {
            "Signature" => class.signature = Some(pool.utf8(body.u16()?)?),
            "InnerClasses" => {
                for _ in 0..body.u16()? {
                    let inner = pool.class(body.u16()?)?;
                    let outer = pool.optional_class(body.u16()?)?;
                    let name_index = body.u16()?;
                    let name = if name_index == 0 { None } else { Some(pool.utf8(name_index)?) };
                    let access = body.u16()?;
                    class.inner_classes.push(InnerClass { inner, outer, name, access });
                }
            }
            "EnclosingMethod" => class.enclosed_in_method = true,
            "Record" => {
                let mut components = Vec::new();
                for _ in 0..body.u16()? {
                    let name = pool.utf8(body.u16()?)?;
                    let descriptor = pool.utf8(body.u16()?)?;
                    let mut component = RecordComponent { name, descriptor, signature: None, annotations: Vec::new() };
                    for _ in 0..body.u16()? {
                        let (attribute, inner) = read_attribute(&mut body, &pool)?;
                        let mut inner = Reader { bytes: inner, at: 0 };
                        match attribute.as_str() {
                            "Signature" => component.signature = Some(pool.utf8(inner.u16()?)?),
                            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                                component.annotations.extend(read_annotations(&mut inner, &pool)?);
                            }
                            _ => {}
                        }
                    }
                    components.push(component);
                }
                class.record = Some(components);
            }
            "PermittedSubclasses" => {
                for _ in 0..body.u16()? {
                    class.permitted.push(pool.class(body.u16()?)?);
                }
            }
            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                class.annotations.extend(read_annotations(&mut body, &pool)?);
            }
            "Deprecated" => class.deprecated = true,
            _ => {}
        }
    }
    Ok(class)
}

fn read_pool(reader: &mut Reader) -> Result<Pool> {
    let count = reader.u16()? as usize;
    let mut entries = vec![Entry::Empty; count.max(1)];
    let mut index = 1;
    while index < count {
        let tag = reader.u8()?;
        let entry = match tag {
            1 => {
                let length = reader.u16()? as usize;
                Entry::Utf8(modified_utf8(reader.take(length)?))
            }
            3 => Entry::Int(reader.u32()? as i32),
            4 => Entry::Float(f32::from_bits(reader.u32()?)),
            5 | 6 => {
                let high = reader.u32()? as u64;
                let low = reader.u32()? as u64;
                let bits = (high << 32) | low;
                entries[index] = if tag == 5 { Entry::Long(bits as i64) } else { Entry::Double(f64::from_bits(bits)) };
                // Eight-byte constants take two slots.
                index += 2;
                continue;
            }
            7 => Entry::Class(reader.u16()?),
            8 => Entry::String(reader.u16()?),
            9..=12 | 17 | 18 => {
                reader.take(4)?;
                Entry::Other
            }
            15 => {
                reader.take(3)?;
                Entry::Other
            }
            16 | 19 | 20 => {
                reader.take(2)?;
                Entry::Other
            }
            _ => return Err(format!("unknown constant tag {tag}")),
        };
        entries[index] = entry;
        index += 1;
    }
    Ok(Pool(entries))
}

fn read_attribute<'a>(reader: &mut Reader<'a>, pool: &Pool) -> Result<(String, &'a [u8])> {
    let name = pool.utf8(reader.u16()?)?;
    let length = reader.u32()? as usize;
    Ok((name, reader.take(length)?))
}

fn read_field(reader: &mut Reader, pool: &Pool) -> Result<Field> {
    let access = reader.u16()?;
    let name = pool.utf8(reader.u16()?)?;
    let descriptor = pool.utf8(reader.u16()?)?;
    let mut field = Field { access, name, descriptor, signature: None, constant: None, annotations: Vec::new(), deprecated: false };
    for _ in 0..reader.u16()? {
        let (attribute, body) = read_attribute(reader, pool)?;
        let mut body = Reader { bytes: body, at: 0 };
        match attribute.as_str() {
            "Signature" => field.signature = Some(pool.utf8(body.u16()?)?),
            "ConstantValue" => field.constant = Some(pool.constant(body.u16()?)?),
            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                field.annotations.extend(read_annotations(&mut body, pool)?);
            }
            "Deprecated" => field.deprecated = true,
            _ => {}
        }
    }
    Ok(field)
}

fn read_method(reader: &mut Reader, pool: &Pool) -> Result<Method> {
    let access = reader.u16()?;
    let name = pool.utf8(reader.u16()?)?;
    let descriptor = pool.utf8(reader.u16()?)?;
    let mut method = Method {
        access,
        name,
        descriptor,
        signature: None,
        exceptions: Vec::new(),
        parameter_names: None,
        implicit_parameters: Vec::new(),
        annotations: Vec::new(),
        parameter_annotations: Vec::new(),
        default: None,
        deprecated: false,
    };
    let mut from_table: Option<Vec<String>> = None;
    for _ in 0..reader.u16()? {
        let (attribute, body) = read_attribute(reader, pool)?;
        let mut body = Reader { bytes: body, at: 0 };
        match attribute.as_str() {
            "Signature" => method.signature = Some(pool.utf8(body.u16()?)?),
            "Exceptions" => {
                for _ in 0..body.u16()? {
                    method.exceptions.push(pool.class(body.u16()?)?);
                }
            }
            "MethodParameters" => {
                let count = body.u8()?;
                let mut names = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let index = body.u16()?;
                    let flags = body.u16()?;
                    // MANDATED shares MODULE's bit.
                    method.implicit_parameters.push(flags & (access::SYNTHETIC | access::MODULE) != 0);
                    names.push(if index == 0 { String::new() } else { pool.utf8(index)? });
                }
                if names.iter().all(|name| !name.is_empty()) {
                    method.parameter_names = Some(names);
                }
            }
            "Code" => from_table = parameters_from_code(body.bytes, pool, &method).ok().flatten(),
            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                method.annotations.extend(read_annotations(&mut body, pool)?);
            }
            "RuntimeVisibleParameterAnnotations" | "RuntimeInvisibleParameterAnnotations" => {
                let count = body.u8()? as usize;
                if method.parameter_annotations.len() < count {
                    method.parameter_annotations.resize(count, Vec::new());
                }
                for parameter in 0..count {
                    let annotations = read_annotations(&mut body, pool)?;
                    method.parameter_annotations[parameter].extend(annotations);
                }
            }
            "AnnotationDefault" => method.default = Some(read_element_value(&mut body, pool)?),
            "Deprecated" => method.deprecated = true,
            _ => {}
        }
    }
    if method.parameter_names.is_none() {
        method.parameter_names = from_table;
    }
    Ok(method)
}

/// The parameters' names from a `Code` attribute's `LocalVariableTable`:
/// each parameter is the local of its slot that is live from the start.
fn parameters_from_code(code: &[u8], pool: &Pool, method: &Method) -> Result<Option<Vec<String>>> {
    let mut reader = Reader { bytes: code, at: 0 };
    reader.take(4)?; // max_stack, max_locals
    let length = reader.u32()? as usize;
    reader.take(length)?;
    let handlers = reader.u16()? as usize;
    reader.take(handlers * 8)?;
    let slots = parameter_slots(&method.descriptor, method.access & access::STATIC != 0);
    for _ in 0..reader.u16()? {
        let (attribute, body) = read_attribute(&mut reader, pool)?;
        if attribute != "LocalVariableTable" {
            continue;
        }
        let mut body = Reader { bytes: body, at: 0 };
        let mut names = vec![String::new(); slots.len()];
        for _ in 0..body.u16()? {
            let start = body.u16()?;
            body.u16()?; // length
            let name = pool.utf8(body.u16()?)?;
            body.u16()?; // descriptor
            let slot = body.u16()?;
            if start == 0
                && let Some(position) = slots.iter().position(|candidate| *candidate == slot)
            {
                names[position] = name;
            }
        }
        if names.iter().all(|name| !name.is_empty()) {
            return Ok(Some(names));
        }
    }
    Ok(None)
}

/// The local slot of each parameter of `descriptor`.
fn parameter_slots(descriptor: &str, is_static: bool) -> Vec<u16> {
    let mut slots = Vec::new();
    let mut slot: u16 = if is_static { 0 } else { 1 };
    for parameter in descriptor_parameters(descriptor) {
        slots.push(slot);
        slot += if parameter == "J" || parameter == "D" { 2 } else { 1 };
    }
    slots
}

/// The parameter descriptors of a method descriptor (`(I[Ljava/lang/String;)V`
/// → `I`, `[Ljava/lang/String;`).
pub fn descriptor_parameters(descriptor: &str) -> Vec<&str> {
    let Some(inner) = descriptor.strip_prefix('(').and_then(|rest| rest.split_once(')')).map(|(inner, _)| inner) else {
        return Vec::new();
    };
    let bytes = inner.as_bytes();
    let mut parameters = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        while index < bytes.len() && bytes[index] == b'[' {
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'L' {
            while index < bytes.len() && bytes[index] != b';' {
                index += 1;
            }
        }
        index += 1;
        parameters.push(&inner[start..index.min(inner.len())]);
    }
    parameters
}

fn read_annotations(reader: &mut Reader, pool: &Pool) -> Result<Vec<Annotation>> {
    let count = reader.u16()?;
    let mut annotations = Vec::with_capacity(count as usize);
    for _ in 0..count {
        annotations.push(read_annotation(reader, pool)?);
    }
    Ok(annotations)
}

fn read_annotation(reader: &mut Reader, pool: &Pool) -> Result<Annotation> {
    let descriptor = pool.utf8(reader.u16()?)?;
    let mut elements = Vec::new();
    for _ in 0..reader.u16()? {
        let name = pool.utf8(reader.u16()?)?;
        elements.push((name, read_element_value(reader, pool)?));
    }
    Ok(Annotation { descriptor, elements })
}

fn read_element_value(reader: &mut Reader, pool: &Pool) -> Result<ElementValue> {
    let tag = reader.u8()?;
    Ok(match tag {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => ElementValue::Const(tag, pool.constant(reader.u16()?)?),
        b'e' => {
            let descriptor = pool.utf8(reader.u16()?)?;
            ElementValue::Enum(descriptor, pool.utf8(reader.u16()?)?)
        }
        b'c' => ElementValue::Class(pool.utf8(reader.u16()?)?),
        b'@' => ElementValue::Annotation(read_annotation(reader, pool)?),
        b'[' => {
            let count = reader.u16()?;
            let mut values = Vec::with_capacity(count as usize);
            for _ in 0..count {
                values.push(read_element_value(reader, pool)?);
            }
            ElementValue::Array(values)
        }
        _ => return Err(format!("unknown element value tag {tag}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_descriptors_split_into_parameters() {
        assert_eq!(descriptor_parameters("(I[[Ljava/lang/String;JLjava/util/List;)V"), vec!["I", "[[Ljava/lang/String;", "J", "Ljava/util/List;"]);
        assert_eq!(parameter_slots("(IJLjava/lang/String;)V", false), vec![1, 2, 4]);
        assert!(descriptor_parameters("()V").is_empty());
    }

    #[test]
    fn modified_utf8_decodes_nul_and_supplementary_characters() {
        assert_eq!(modified_utf8(&[0x61, 0xC0, 0x80]), "a\0");
        // U+1F600 as two encoded surrogates.
        assert_eq!(modified_utf8(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]), "\u{1F600}");
    }
}
