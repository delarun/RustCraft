//! Parser for Java class files (JVMS chapter 4), up to class file version 69 (Java 25).

mod constant_pool;
pub mod descriptor;
pub mod mutf8;
pub mod opcodes;
mod reader;

pub use constant_pool::{Constant, ConstantPool, MemberRef, MemberRefInfo, ReferenceKind, Utf8};
pub use descriptor::{DescriptorError, FieldType, MethodDescriptor};

use reader::Reader;
use std::fmt;

pub const MAGIC: u32 = 0xCAFE_BABE;
pub const MAX_MAJOR_VERSION: u16 = 69;

pub mod access {
    pub const PUBLIC: u16 = 0x0001;
    pub const PRIVATE: u16 = 0x0002;
    pub const PROTECTED: u16 = 0x0004;
    pub const STATIC: u16 = 0x0008;
    pub const FINAL: u16 = 0x0010;
    /// ACC_SUPER on classes, ACC_SYNCHRONIZED on methods.
    pub const SYNCHRONIZED: u16 = 0x0020;
    pub const VOLATILE: u16 = 0x0040;
    pub const BRIDGE: u16 = 0x0040;
    pub const TRANSIENT: u16 = 0x0080;
    pub const VARARGS: u16 = 0x0080;
    pub const NATIVE: u16 = 0x0100;
    pub const INTERFACE: u16 = 0x0200;
    pub const ABSTRACT: u16 = 0x0400;
    pub const STRICT: u16 = 0x0800;
    pub const SYNTHETIC: u16 = 0x1000;
    pub const ANNOTATION: u16 = 0x2000;
    pub const ENUM: u16 = 0x4000;
    pub const MODULE: u16 = 0x8000;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassFormatError(pub String);

impl ClassFormatError {
    pub fn new(message: impl Into<String>) -> Self {
        ClassFormatError(message.into())
    }
}

impl fmt::Display for ClassFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClassFormatError {}

pub type Result<T> = std::result::Result<T, ClassFormatError>;

/// An attribute the parser does not interpret, kept raw for reflection
/// (annotations, MethodParameters, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: Box<str>,
    pub data: Box<[u8]>,
}

#[derive(Debug, Clone)]
pub struct ClassFile {
    pub minor_version: u16,
    pub major_version: u16,
    pub constant_pool: ConstantPool,
    pub access_flags: u16,
    pub this_class: String,
    /// Constant pool index of this class's own Class entry.
    pub this_class_index: u16,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub source_file: Option<String>,
    pub signature: Option<String>,
    pub bootstrap_methods: Vec<BootstrapMethod>,
    pub inner_classes: Vec<InnerClass>,
    pub enclosing_method: Option<EnclosingMethod>,
    pub nest_host: Option<String>,
    pub nest_members: Vec<String>,
    pub permitted_subclasses: Vec<String>,
    pub record_components: Option<Vec<RecordComponent>>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    /// Constant pool index of the ConstantValue, for static final fields.
    pub constant_value: Option<u16>,
    pub signature: Option<String>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    pub code: Option<Code>,
    pub exceptions: Vec<String>,
    pub signature: Option<String>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub struct Code {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    pub exception_table: Vec<ExceptionHandler>,
    /// (start_pc, line_number), as in the LineNumberTable.
    pub line_numbers: Vec<(u16, u16)>,
    /// Raw StackMapTable body; decoded on demand by the verifier / JIT.
    pub stack_map_table: Option<Box<[u8]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionHandler {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    /// Constant pool Class index, or 0 for "any" (finally).
    pub catch_type: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapMethod {
    /// Constant pool index of a MethodHandle.
    pub method_ref: u16,
    pub arguments: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InnerClass {
    pub inner_class: String,
    pub outer_class: Option<String>,
    pub inner_name: Option<String>,
    pub access_flags: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnclosingMethod {
    pub class: String,
    /// (name, descriptor), absent for initializers of fields/blocks.
    pub method: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordComponent {
    pub name: String,
    pub descriptor: String,
    pub signature: Option<String>,
    pub attributes: Vec<Attribute>,
}

impl ClassFile {
    pub fn parse(data: &[u8]) -> Result<ClassFile> {
        let mut r = Reader::new(data);
        let magic = r.u4()?;
        if magic != MAGIC {
            return Err(ClassFormatError::new(format!("bad magic {magic:#010x}")));
        }
        let minor_version = r.u2()?;
        let major_version = r.u2()?;
        if !(45..=MAX_MAJOR_VERSION).contains(&major_version) {
            return Err(ClassFormatError::new(format!(
                "unsupported class file version {major_version}.{minor_version}"
            )));
        }
        let cp = ConstantPool::parse(&mut r)?;
        let access_flags = r.u2()?;
        let this_class_index = r.u2()?;
        let this_class = cp.class_name(this_class_index)?.to_owned();
        let super_index = r.u2()?;
        let super_class = if super_index == 0 { None } else { Some(cp.class_name(super_index)?.to_owned()) };
        let interfaces = r.u2_list()?.into_iter().map(|i| cp.class_name(i).map(str::to_owned)).collect::<Result<_>>()?;

        let field_count = r.u2()?;
        let fields = (0..field_count).map(|_| parse_field(&mut r, &cp)).collect::<Result<_>>()?;
        let method_count = r.u2()?;
        let methods = (0..method_count).map(|_| parse_method(&mut r, &cp)).collect::<Result<_>>()?;

        let mut class = ClassFile {
            minor_version,
            major_version,
            constant_pool: ConstantPool::default(),
            access_flags,
            this_class,
            this_class_index,
            super_class,
            interfaces,
            fields,
            methods,
            source_file: None,
            signature: None,
            bootstrap_methods: Vec::new(),
            inner_classes: Vec::new(),
            enclosing_method: None,
            nest_host: None,
            nest_members: Vec::new(),
            permitted_subclasses: Vec::new(),
            record_components: None,
            attributes: Vec::new(),
        };
        for (name, data) in parse_attributes(&mut r, &cp)? {
            let mut a = Reader::new(data);
            match name {
                "SourceFile" => class.source_file = Some(cp.utf8(a.u2()?)?.to_owned()),
                "Signature" => class.signature = Some(cp.utf8(a.u2()?)?.to_owned()),
                "BootstrapMethods" => {
                    let count = a.u2()?;
                    for _ in 0..count {
                        let method_ref = a.u2()?;
                        let arguments = a.u2_list()?;
                        class.bootstrap_methods.push(BootstrapMethod { method_ref, arguments });
                    }
                }
                "InnerClasses" => {
                    let count = a.u2()?;
                    for _ in 0..count {
                        let inner = a.u2()?;
                        let outer = a.u2()?;
                        let inner_name = a.u2()?;
                        let access_flags = a.u2()?;
                        class.inner_classes.push(InnerClass {
                            inner_class: cp.class_name(inner)?.to_owned(),
                            outer_class: optional(outer, |i| cp.class_name(i))?,
                            inner_name: optional(inner_name, |i| cp.utf8(i))?,
                            access_flags,
                        });
                    }
                }
                "EnclosingMethod" => {
                    let class_index = a.u2()?;
                    let method = a.u2()?;
                    class.enclosing_method = Some(EnclosingMethod {
                        class: cp.class_name(class_index)?.to_owned(),
                        method: if method == 0 {
                            None
                        } else {
                            let (n, d) = cp.name_and_type(method)?;
                            Some((n.to_owned(), d.to_owned()))
                        },
                    });
                }
                "NestHost" => class.nest_host = Some(cp.class_name(a.u2()?)?.to_owned()),
                "NestMembers" => class.nest_members = class_names(&mut a, &cp)?,
                "PermittedSubclasses" => class.permitted_subclasses = class_names(&mut a, &cp)?,
                "Record" => {
                    let count = a.u2()?;
                    let mut components = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        let name = cp.utf8(a.u2()?)?.to_owned();
                        let descriptor = cp.utf8(a.u2()?)?.to_owned();
                        let mut signature = None;
                        let mut attributes = Vec::new();
                        for (attr_name, attr_data) in parse_attributes(&mut a, &cp)? {
                            if attr_name == "Signature" {
                                signature = Some(cp.utf8(Reader::new(attr_data).u2()?)?.to_owned());
                            } else {
                                attributes.push(raw(attr_name, attr_data));
                            }
                        }
                        components.push(RecordComponent { name, descriptor, signature, attributes });
                    }
                    class.record_components = Some(components);
                }
                _ => {
                    class.attributes.push(raw(name, data));
                    continue;
                }
            }
            expect_consumed(&a, name)?;
        }
        if !r.is_empty() {
            return Err(ClassFormatError::new("trailing bytes after class file"));
        }
        class.constant_pool = cp;
        Ok(class)
    }

    pub fn is_interface(&self) -> bool {
        self.access_flags & access::INTERFACE != 0
    }

    pub fn attribute(&self, name: &str) -> Option<&[u8]> {
        find_attribute(&self.attributes, name)
    }

    pub fn method(&self, name: &str, descriptor: &str) -> Option<&MethodInfo> {
        self.methods.iter().find(|m| m.name == name && m.descriptor == descriptor)
    }
}

impl FieldInfo {
    pub fn is_static(&self) -> bool {
        self.access_flags & access::STATIC != 0
    }

    pub fn attribute(&self, name: &str) -> Option<&[u8]> {
        find_attribute(&self.attributes, name)
    }
}

impl MethodInfo {
    pub fn is_static(&self) -> bool {
        self.access_flags & access::STATIC != 0
    }

    pub fn is_native(&self) -> bool {
        self.access_flags & access::NATIVE != 0
    }

    pub fn is_abstract(&self) -> bool {
        self.access_flags & access::ABSTRACT != 0
    }

    pub fn attribute(&self, name: &str) -> Option<&[u8]> {
        find_attribute(&self.attributes, name)
    }
}

impl Code {
    /// Source line for a bytecode offset, if a LineNumberTable is present.
    pub fn line_number(&self, pc: u16) -> Option<u16> {
        self.line_numbers.iter().filter(|(start, _)| *start <= pc).max_by_key(|(start, _)| *start).map(|(_, l)| *l)
    }
}

fn parse_field(r: &mut Reader, cp: &ConstantPool) -> Result<FieldInfo> {
    let access_flags = r.u2()?;
    let name = cp.utf8(r.u2()?)?.to_owned();
    let descriptor = cp.utf8(r.u2()?)?.to_owned();
    FieldType::parse(&descriptor).map_err(|e| ClassFormatError::new(format!("field {name}: {e}")))?;
    let mut field = FieldInfo { access_flags, name, descriptor, constant_value: None, signature: None, attributes: Vec::new() };
    for (attr_name, data) in parse_attributes(r, cp)? {
        let mut a = Reader::new(data);
        match attr_name {
            "ConstantValue" => field.constant_value = Some(a.u2()?),
            "Signature" => field.signature = Some(cp.utf8(a.u2()?)?.to_owned()),
            _ => {
                field.attributes.push(raw(attr_name, data));
                continue;
            }
        }
        expect_consumed(&a, attr_name)?;
    }
    Ok(field)
}

fn parse_method(r: &mut Reader, cp: &ConstantPool) -> Result<MethodInfo> {
    let access_flags = r.u2()?;
    let name = cp.utf8(r.u2()?)?.to_owned();
    let descriptor = cp.utf8(r.u2()?)?.to_owned();
    MethodDescriptor::parse(&descriptor).map_err(|e| ClassFormatError::new(format!("method {name}: {e}")))?;
    let mut method = MethodInfo {
        access_flags,
        name,
        descriptor,
        code: None,
        exceptions: Vec::new(),
        signature: None,
        attributes: Vec::new(),
    };
    for (attr_name, data) in parse_attributes(r, cp)? {
        let mut a = Reader::new(data);
        match attr_name {
            "Code" => method.code = Some(parse_code(&mut a, cp)?),
            "Exceptions" => method.exceptions = class_names(&mut a, cp)?,
            "Signature" => method.signature = Some(cp.utf8(a.u2()?)?.to_owned()),
            _ => {
                method.attributes.push(raw(attr_name, data));
                continue;
            }
        }
        expect_consumed(&a, attr_name)?;
    }
    Ok(method)
}

fn parse_code(r: &mut Reader, cp: &ConstantPool) -> Result<Code> {
    let max_stack = r.u2()?;
    let max_locals = r.u2()?;
    let code_length = r.u4()? as usize;
    if code_length == 0 || code_length > 65535 {
        return Err(ClassFormatError::new(format!("bad code length {code_length}")));
    }
    let code = r.bytes(code_length)?.to_vec();
    let handler_count = r.u2()?;
    let mut exception_table = Vec::with_capacity(handler_count as usize);
    for _ in 0..handler_count {
        let handler = ExceptionHandler { start_pc: r.u2()?, end_pc: r.u2()?, handler_pc: r.u2()?, catch_type: r.u2()? };
        if handler.catch_type != 0 {
            cp.class_name(handler.catch_type)?;
        }
        exception_table.push(handler);
    }
    let mut line_numbers = Vec::new();
    let mut stack_map_table = None;
    for (name, data) in parse_attributes(r, cp)? {
        match name {
            "LineNumberTable" => {
                let mut a = Reader::new(data);
                let count = a.u2()?;
                for _ in 0..count {
                    line_numbers.push((a.u2()?, a.u2()?));
                }
            }
            "StackMapTable" => stack_map_table = Some(data.into()),
            // Debug-only tables (LocalVariableTable, ...) aren't needed at runtime.
            _ => {}
        }
    }
    Ok(Code { max_stack, max_locals, code, exception_table, line_numbers, stack_map_table })
}

fn parse_attributes<'a, 'c>(r: &mut Reader<'a>, cp: &'c ConstantPool) -> Result<Vec<(&'c str, &'a [u8])>> {
    let count = r.u2()?;
    let mut attributes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = cp.utf8(r.u2()?)?;
        let len = r.u4()? as usize;
        attributes.push((name, r.bytes(len)?));
    }
    Ok(attributes)
}

fn class_names(r: &mut Reader, cp: &ConstantPool) -> Result<Vec<String>> {
    r.u2_list()?.into_iter().map(|i| cp.class_name(i).map(str::to_owned)).collect()
}

fn optional<'a>(index: u16, get: impl FnOnce(u16) -> Result<&'a str>) -> Result<Option<String>> {
    if index == 0 { Ok(None) } else { get(index).map(|s| Some(s.to_owned())) }
}

fn raw(name: &str, data: &[u8]) -> Attribute {
    Attribute { name: name.into(), data: data.into() }
}

fn find_attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a [u8]> {
    attributes.iter().find(|a| &*a.name == name).map(|a| &*a.data)
}

fn expect_consumed(r: &Reader, name: &str) -> Result<()> {
    if r.is_empty() { Ok(()) } else { Err(ClassFormatError::new(format!("{name} attribute has trailing bytes"))) }
}
