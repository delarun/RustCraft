//! Runtime representation of loaded classes, fields and methods.
//!
//! Classes are allocated once and never freed (`&'static Class`); Minecraft
//! never unloads classes, and this keeps method and class references in hot
//! paths free of reference counting.

use crate::heap::{RefCell32, Ref};
use crate::native::NativeFn;
use classfile::{ClassFile, Code, access};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub type ClassRef = &'static Class;
pub type MethodRef = &'static Method;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Byte,
    Boolean,
    Char,
    Short,
    Int,
    Float,
    Long,
    Double,
    Ref,
}

impl FieldKind {
    /// From the first character of a field descriptor.
    pub fn from_descriptor(descriptor: &str) -> FieldKind {
        match descriptor.as_bytes()[0] {
            b'B' => FieldKind::Byte,
            b'Z' => FieldKind::Boolean,
            b'C' => FieldKind::Char,
            b'S' => FieldKind::Short,
            b'I' => FieldKind::Int,
            b'F' => FieldKind::Float,
            b'J' => FieldKind::Long,
            b'D' => FieldKind::Double,
            _ => FieldKind::Ref,
        }
    }

    pub fn size(self) -> u32 {
        match self {
            FieldKind::Byte | FieldKind::Boolean => 1,
            FieldKind::Char | FieldKind::Short => 2,
            FieldKind::Int | FieldKind::Float | FieldKind::Ref => 4,
            FieldKind::Long | FieldKind::Double => 8,
        }
    }

    /// Takes two local-variable / operand-stack slots.
    pub fn is_wide(self) -> bool {
        matches!(self, FieldKind::Long | FieldKind::Double)
    }

    pub fn descriptor_char(self) -> char {
        match self {
            FieldKind::Byte => 'B',
            FieldKind::Boolean => 'Z',
            FieldKind::Char => 'C',
            FieldKind::Short => 'S',
            FieldKind::Int => 'I',
            FieldKind::Float => 'F',
            FieldKind::Long => 'J',
            FieldKind::Double => 'D',
            FieldKind::Ref => 'L',
        }
    }

    pub fn primitive_name(self) -> &'static str {
        match self {
            FieldKind::Byte => "byte",
            FieldKind::Boolean => "boolean",
            FieldKind::Char => "char",
            FieldKind::Short => "short",
            FieldKind::Int => "int",
            FieldKind::Float => "float",
            FieldKind::Long => "long",
            FieldKind::Double => "double",
            FieldKind::Ref => panic!("not a primitive"),
        }
    }
}

#[derive(Debug)]
pub enum ClassKind {
    Instance,
    Array { component: ClassRef },
    /// `None` is `void`.
    Primitive(Option<FieldKind>),
}

/// A trivial method executed without an interpreter frame (see
/// `Thread::fast_path`): HotSpot's "fast accessor" idea.
#[derive(Debug, Clone, Copy)]
pub enum FastPath {
    /// No fast path; interpret normally.
    None,
    /// `return` (possibly after calling a trivial super constructor).
    Empty,
    /// `aload_0; getfield f; xreturn`
    Getter(FieldRef),
    /// `aload_0; xload_1; putfield f; return`
    Setter(FieldRef),
    /// `<constant>; xreturn` for an int-like or null constant.
    Constant(u64),
}

/// VM-implemented signature-polymorphic methods (JVMS §2.9.3). One synthetic
/// `Method` exists per (method, call-site descriptor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    /// `MethodHandle.invokeBasic`: jump to `mh.form.vmentry`.
    InvokeBasic,
    /// `MethodHandle.linkTo*`: the trailing MemberName argument names the target.
    LinkToStatic,
    LinkToSpecial,
    LinkToVirtual,
    LinkToInterface,
    /// `MethodHandle.invokeExact/invoke` and the VarHandle access modes:
    /// linked per call site through `MethodHandleNatives.linkMethod`.
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitState {
    Uninitialized,
    InProgress,
    Initialized,
    Erroneous,
}

/// A monomorphic inline cache: the receiver class id and the method selected
/// for it, in one word so threads never see a torn pair (0: empty).
#[derive(Default)]
pub struct CallCache(std::sync::atomic::AtomicU64);

impl CallCache {
    #[inline]
    pub fn get(&self) -> Option<(u32, u32)> {
        let entry = self.0.load(std::sync::atomic::Ordering::Relaxed);
        (entry != 0).then_some(((entry >> 32) as u32, entry as u32))
    }

    #[inline]
    pub fn set(&self, class_id: u32, method_index: u32) {
        self.0.store((class_id as u64) << 32 | method_index as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A resolved constant pool entry (JVMS §5.4.3), cached per class.
#[derive(Debug, Clone, Copy)]
pub enum Resolved {
    Class(ClassRef),
    Field(FieldRef),
    Method(MethodRef),
    String(Ref),
}

#[derive(Debug, Clone, Copy)]
pub struct FieldRef {
    pub holder: ClassRef,
    pub offset: u32,
    pub kind: FieldKind,
    pub is_static: bool,
    pub volatile: bool,
}

pub struct Field {
    pub name: String,
    pub descriptor: String,
    pub kind: FieldKind,
    pub access_flags: u16,
    /// Byte offset within the object (instance) or the statics block (static).
    pub offset: u32,
    pub constant_value: Option<u16>,
    /// Hidden VM fields (e.g. the class id in `java.lang.Class`) are not
    /// visible to reflection.
    pub injected: bool,
}

impl Field {
    pub fn is_static(&self) -> bool {
        self.access_flags & access::STATIC != 0
    }
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}@{}", self.name, self.descriptor, self.offset)
    }
}

pub struct Method {
    class: *const Class,
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
    pub params: Vec<FieldKind>,
    /// `None` for void.
    pub ret: Option<FieldKind>,
    /// Argument slots including `this`.
    pub arg_slots: u16,
    pub code: Option<Code>,
    pub vtable_index: Option<u32>,
    /// Index into the VM-wide method table (used in stack traces).
    pub global_index: u32,
    /// Native implementation or VM override, bound on first call.
    pub native: OnceLock<Option<NativeFn>>,
    pub intrinsic: Option<Intrinsic>,
    /// Number of times the method was entered (hotness for profiling / JIT).
    pub invocations: std::sync::atomic::AtomicU64,
    pub fast_path: OnceLock<FastPath>,
    pub jit: crate::jit::MethodJit,
    /// The JNI function a native method is bound to (a loaded library's).
    #[cfg(unix)]
    pub jni: OnceLock<Option<&'static crate::jni::JniMethod>>,
}

// SAFETY: `class` points at the owning, immortal Class.
unsafe impl Send for Method {}
unsafe impl Sync for Method {}

impl Method {
    pub(crate) fn set_class(&mut self, class: ClassRef) {
        self.class = class;
    }

    pub fn class(&self) -> ClassRef {
        // SAFETY: set before the class is published; classes are immortal.
        unsafe { &*self.class }
    }

    pub fn is_static(&self) -> bool {
        self.access_flags & access::STATIC != 0
    }

    pub fn is_native(&self) -> bool {
        self.access_flags & access::NATIVE != 0
    }

    pub fn is_abstract(&self) -> bool {
        self.access_flags & access::ABSTRACT != 0
    }

    pub fn is_private(&self) -> bool {
        self.access_flags & access::PRIVATE != 0
    }

    pub fn is_initializer(&self) -> bool {
        self.name == "<init>" || self.name == "<clinit>"
    }

    pub fn is_signature_polymorphic(&self) -> bool {
        self.is_native()
            && self.access_flags & access::VARARGS != 0
            && matches!(self.class().name.as_str(), "java/lang/invoke/MethodHandle" | "java/lang/invoke/VarHandle")
    }

    pub fn native_key(&self) -> String {
        format!("{}.{}{}", self.class().name, self.name, self.descriptor)
    }

    pub fn line_number(&self, pc: u32) -> Option<u16> {
        self.code.as_ref()?.line_number(pc as u16)
    }
}

impl std::fmt::Debug for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}{}", self.class().name, self.name, self.descriptor)
    }
}

pub struct Class {
    pub id: u32,
    /// Internal name (`java/lang/String`, `[I`, `int`).
    pub name: String,
    pub kind: ClassKind,
    pub access_flags: u16,
    pub super_class: Option<ClassRef>,
    pub interfaces: Vec<ClassRef>,
    /// Defining loader; null for the bootstrap loader.
    pub loader: Ref,
    /// Module from the runtime image, for bootstrap classes.
    pub module: Option<String>,
    /// Defined through Lookup.defineHiddenClass; not findable by name.
    pub hidden: bool,
    /// Nest host assigned at definition (hidden classes defined as nestmates).
    pub dynamic_nest_host: OnceLock<ClassRef>,
    pub file: Option<Box<ClassFile>>,
    pub fields: Vec<Field>,
    pub methods: Vec<Method>,
    pub instance_size: u32,
    /// Offsets of all reference fields of an instance, inherited included.
    pub ref_offsets: Vec<u32>,
    pub statics_size: u32,
    pub static_ref_offsets: Vec<u32>,
    /// In the linear-memory class metadata (compiled code reads it).
    pub statics: &'static RefCell32,
    pub vtable: Vec<MethodRef>,
    pub mirror: RefCell32,
    pub init_state: Mutex<InitState>,
    /// Fast-path flag for `InitState::Initialized` (in the class metadata).
    pub initialized: &'static std::sync::atomic::AtomicBool,
    /// Id of the thread running the static initialiser.
    pub init_thread: std::sync::atomic::AtomicU64,
    pub cp_cache: Box<[OnceLock<Resolved>]>,
    /// Monomorphic inline caches for virtual/interface call sites, indexed by
    /// constant pool index: last receiver class id and the method selected.
    pub call_caches: Box<[CallCache]>,
    pub array_class: OnceLock<ClassRef>,
    /// `all_interfaces`, computed on first use.
    pub interface_closure: OnceLock<Box<[ClassRef]>>,
    /// Virtual/interface method selection cache: resolved method → selected.
    pub selection_cache: Mutex<HashMap<usize, MethodRef>>,
}

impl Class {
    pub fn is_interface(&self) -> bool {
        self.access_flags & access::INTERFACE != 0
    }

    pub fn is_array(&self) -> bool {
        matches!(self.kind, ClassKind::Array { .. })
    }

    pub fn is_primitive(&self) -> bool {
        matches!(self.kind, ClassKind::Primitive(_))
    }

    pub fn component(&self) -> Option<ClassRef> {
        match self.kind {
            ClassKind::Array { component } => Some(component),
            _ => None,
        }
    }

    /// Element kind and size for arrays.
    pub fn element_kind(&self) -> Option<FieldKind> {
        let component = self.component()?;
        Some(match component.kind {
            ClassKind::Primitive(Some(kind)) => kind,
            _ => FieldKind::Ref,
        })
    }

    /// Java's binary name: `java.lang.String`, `[Ljava.lang.String;`. Hidden
    /// classes keep the `/` before their suffix (`Foo$$Lambda/0x...`).
    pub fn java_name(&self) -> String {
        if self.hidden {
            if let Some((base, suffix)) = self.name.rsplit_once('/') {
                return format!("{}/{suffix}", base.replace('/', "."));
            }
        }
        self.name.replace('/', ".")
    }

    pub fn package(&self) -> &str {
        match &self.kind {
            ClassKind::Instance => {
                // A hidden class's name has an extra `/0x...` suffix.
                let name = if self.hidden { self.name.rsplit_once('/').map_or("", |(n, _)| n) } else { &self.name };
                name.rsplit_once('/').map_or("", |(p, _)| p)
            }
            ClassKind::Array { component } => component.package(),
            ClassKind::Primitive(_) => "java/lang",
        }
    }

    pub fn find_field(&self, name: &str, descriptor: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name && f.descriptor == descriptor)
    }

    pub fn find_field_by_name(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn find_method(&self, name: &str, descriptor: &str) -> Option<MethodRef> {
        self.methods
            .iter()
            .find(|m| m.name == name && m.descriptor == descriptor)
            // SAFETY: methods live as long as the immortal class.
            .map(|m| unsafe { &*(m as *const Method) })
    }

    /// A method by its reflection slot (index into `methods`).
    pub fn find_method_by_slot(&self, slot: usize) -> MethodRef {
        // SAFETY: methods live as long as the immortal class.
        unsafe { &*(&self.methods[slot] as *const Method) }
    }

    pub fn super_classes(&'static self) -> impl Iterator<Item = ClassRef> {
        std::iter::successors(Some(self), |c| c.super_class)
    }

    /// All superinterfaces, transitively, of this class and its superclasses.
    pub fn all_interfaces(&'static self) -> &'static [ClassRef] {
        self.interface_closure.get_or_init(|| self.collect_interfaces().into_boxed_slice())
    }

    fn collect_interfaces(&'static self) -> Vec<ClassRef> {
        let mut out: Vec<ClassRef> = Vec::new();
        fn visit(class: ClassRef, out: &mut Vec<ClassRef>) {
            for &iface in &class.interfaces {
                if !out.iter().any(|c| std::ptr::eq(*c, iface)) {
                    out.push(iface);
                    visit(iface, out);
                }
            }
        }
        for class in self.super_classes() {
            visit(class, &mut out);
        }
        out
    }

    /// Is a value of class `self` assignable to `target` (JVMS §6.5 checkcast)?
    pub fn is_subclass_of(&'static self, target: ClassRef) -> bool {
        if std::ptr::eq(self, target) {
            return true;
        }
        if let ClassKind::Array { component } = self.kind {
            return match target.kind {
                ClassKind::Array { component: target_component } => match (&component.kind, &target_component.kind) {
                    (ClassKind::Primitive(_), _) | (_, ClassKind::Primitive(_)) => std::ptr::eq(component, target_component),
                    _ => component.is_subclass_of(target_component),
                },
                _ => matches!(target.name.as_str(), "java/lang/Object" | "java/lang/Cloneable" | "java/io/Serializable"),
            };
        }
        if target.is_interface() {
            return self.all_interfaces().iter().any(|i| std::ptr::eq(*i, target));
        }
        self.super_classes().any(|c| std::ptr::eq(c, target))
    }

    pub fn cp(&self) -> &classfile::ConstantPool {
        &self.file.as_ref().expect("class has no constant pool").constant_pool
    }

    pub(crate) fn set_method_owners(&mut self) {
        let ptr = self as *const Class;
        for method in &mut self.methods {
            method.class = ptr;
        }
    }
}

impl std::fmt::Debug for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Class({})", self.name)
    }
}

pub(crate) struct MethodInit {
    pub name: String,
    pub descriptor: String,
    pub access_flags: u16,
    pub code: Option<Code>,
    pub global_index: u32,
}

impl Method {
    pub(crate) fn new(init: MethodInit) -> Method {
        let parsed = classfile::MethodDescriptor::parse(&init.descriptor).expect("descriptor validated by parser");
        let params: Vec<FieldKind> = parsed.params.iter().map(|p| FieldKind::from_descriptor(&p.descriptor())).collect();
        let ret = parsed.ret.as_ref().map(|r| FieldKind::from_descriptor(&r.descriptor()));
        let this_slot = if init.access_flags & access::STATIC != 0 { 0 } else { 1 };
        let arg_slots = this_slot + params.iter().map(|k| if k.is_wide() { 2 } else { 1 }).sum::<u16>();
        Method {
            class: std::ptr::null(),
            name: init.name,
            descriptor: init.descriptor,
            access_flags: init.access_flags,
            params,
            ret,
            arg_slots,
            code: init.code,
            vtable_index: None,
            global_index: init.global_index,
            native: OnceLock::new(),
            intrinsic: None,
            invocations: std::sync::atomic::AtomicU64::new(0),
            fast_path: OnceLock::new(),
            jit: Default::default(),
            #[cfg(unix)]
            jni: OnceLock::new(),
        }
    }
}
