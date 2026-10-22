//! The virtual machine: class registry and loading, linking (layout, vtables),
//! symbolic resolution, strings and class mirrors. Nothing here executes Java
//! code; that is the interpreter's job (see `interp.rs`).

use crate::class::{
    Class, ClassKind, ClassRef, Field, FieldKind, FieldRef, InitState, Intrinsic, Method, MethodInit, MethodRef, Resolved,
};
use crate::classpath::{ClassBytes, ClassPath};
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET, HEADER_SIZE, Heap, Ref, RefCell32};
use crate::native::{NativeFn, Natives};
use classfile::{ClassFile, Constant, access};
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

mod gc;

static VERIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RJAVA_GC_VERIFY: poison freed memory and check references when used.
#[inline]
pub fn verify_enabled() -> bool {
    VERIFY.load(Ordering::Relaxed)
}

/// A linkage or loading failure, to be thrown as the named Java exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmError {
    /// Internal name of the Java exception class, e.g. `java/lang/NoClassDefFoundError`.
    pub class: &'static str,
    pub message: String,
}

impl VmError {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        VmError { class, message: message.into() }
    }

    fn no_class_def(name: &str) -> Self {
        VmError::new("java/lang/NoClassDefFoundError", name.replace('/', "."))
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.class.replace('/', "."), self.message)
    }
}

pub type VmResult<T> = Result<T, VmError>;

/// The class table: an append-only array indexed by class id (the id stored
/// in every object header). Lookups are a single atomic load, since they
/// happen on every virtual call and type check.
struct ClassTable {
    slots: Box<[std::sync::atomic::AtomicPtr<Class>]>,
    len: AtomicU32,
    append: Mutex<()>,
}

use crate::memory::MAX_CLASSES;

impl ClassTable {
    fn new() -> Self {
        ClassTable {
            slots: (0..MAX_CLASSES).map(|_| std::sync::atomic::AtomicPtr::new(std::ptr::null_mut())).collect(),
            len: AtomicU32::new(0),
            append: Mutex::new(()),
        }
    }

    #[inline]
    fn get(&self, id: u32) -> Option<ClassRef> {
        let ptr = self.slots.get(id as usize)?.load(Ordering::Acquire);
        // SAFETY: published classes are leaked and never freed.
        (!ptr.is_null()).then(|| unsafe { &*ptr })
    }

    /// Appends the class built by `make(id)` and returns it.
    fn push(&self, make: impl FnOnce(u32) -> ClassRef) -> ClassRef {
        let _guard = self.append.lock().unwrap();
        let id = self.len.load(Ordering::Relaxed);
        assert!((id as usize) < MAX_CLASSES, "too many classes");
        let class = make(id);
        self.slots[id as usize].store(class as *const Class as *mut Class, Ordering::Release);
        self.len.store(id + 1, Ordering::Release);
        class
    }

    fn all(&self) -> Vec<ClassRef> {
        (0..self.len.load(Ordering::Acquire)).filter_map(|id| self.get(id)).collect()
    }
}

/// Methods by global index, readable without locks (compiled code and the
/// interpreter's inline caches look methods up by index).
struct MethodTable {
    slots: Box<[std::sync::atomic::AtomicPtr<Method>]>,
}

const MAX_METHODS: usize = 1 << 22;

impl MethodTable {
    fn new() -> Self {
        // Zeroed (null) pointers, committed lazily by the OS.
        let zeroed: Box<[usize]> = vec![0usize; MAX_METHODS].into_boxed_slice();
        // SAFETY: AtomicPtr has the layout of a pointer; 0 is null.
        MethodTable { slots: unsafe { Box::from_raw(Box::into_raw(zeroed) as *mut [std::sync::atomic::AtomicPtr<Method>]) } }
    }

    #[inline]
    fn get(&self, index: u32) -> Option<MethodRef> {
        let ptr = self.slots.get(index as usize)?.load(Ordering::Acquire);
        // SAFETY: published methods belong to leaked classes.
        (!ptr.is_null()).then(|| unsafe { &*ptr })
    }

    fn set(&self, index: u32, method: MethodRef) {
        assert!((index as usize) < MAX_METHODS, "too many methods");
        self.slots[index as usize].store(method as *const Method as *mut Method, Ordering::Release);
    }
}

/// A linked dynamic call site: the invoker method to call and, if it takes one,
/// the trailing appendix argument (CallSite target, MethodType, ...).
#[derive(Debug, Clone, Copy)]
pub struct LinkedSite {
    pub invoker: MethodRef,
    /// A cell of its own (leaked: sites live as long as the VM), which the
    /// collector updates: compiled code reads it at its address.
    pub appendix: Option<&'static crate::heap::RefCell32>,
}

/// Marks a statics block in its mark word (statics blocks are not Java objects).
pub const STATICS_MARK: u32 = 0xFFFF_FFFF;

pub struct VmOptions {
    pub heap_size: usize,
    /// Linear memory for the heap and thread stacks (e.g. a wasmtime memory);
    /// the VM allocates one when absent.
    pub memory: Option<crate::memory::Region>,
    /// Maximum number of Java threads (each needs a stack in linear memory).
    pub max_threads: usize,
    /// Called by Runtime.halt / System.exit with the status.
    pub exit_handler: Box<dyn Fn(i32) + Send + Sync>,
    pub vfs: crate::fs::Vfs,
    pub config: crate::boot::SystemConfig,
    pub classpath: ClassPath,
    pub stdin: Box<dyn std::io::Read + Send>,
    pub stdout: Box<dyn Write + Send>,
    pub stderr: Box<dyn Write + Send>,
    /// Executes JIT-compiled code; no JIT when absent.
    pub jit: Option<Box<dyn crate::jit::JitBackend>>,
}

impl Default for VmOptions {
    fn default() -> Self {
        let mut vfs = crate::fs::Vfs::default();
        vfs.mount("/", std::sync::Arc::new(crate::fs::HostFs::new("/")));
        VmOptions {
            heap_size: 512 << 20,
            memory: None,
            jit: None,
            max_threads: 64,
            exit_handler: Box::new(|status| std::process::exit(status)),
            vfs,
            config: crate::boot::SystemConfig::from_host(),
            classpath: ClassPath::default(),
            stdin: Box::new(std::io::stdin()),
            stdout: Box::new(std::io::stdout()),
            stderr: Box::new(std::io::stderr()),
        }
    }
}

/// Offsets of VM-known fields, filled in when their classes are defined.
#[derive(Default)]
struct KnownOffsets {
    class_id: AtomicU32,
    class_loader: AtomicU32,
    class_component: AtomicU32,
    class_modifiers: AtomicU32,
    class_primitive: AtomicU32,
    class_module: AtomicU32,
    loader_unnamed_module: AtomicU32,
    string_value: AtomicU32,
    string_coder: AtomicU32,
}

pub struct Vm {
    pub heap: Heap,
    pub memory: crate::memory::LinearMemory,
    pub jit: Option<Box<dyn crate::jit::JitBackend>>,
    pub jit_queue: crate::jit::Queue,
    /// Native libraries and JNI state.
    #[cfg(unix)]
    pub jni: crate::jni::Jni,
    pub threads: crate::threads::Threads,
    self_ref: OnceLock<&'static Vm>,
    exit_handler: Box<dyn Fn(i32) + Send + Sync>,
    pub vfs: crate::fs::Vfs,
    pub fds: crate::fs::FdTable,
    pub sockets: crate::net::Sockets,
    pub config: crate::boot::SystemConfig,
    classpath: ClassPath,
    classes: ClassTable,
    bootstrap: Mutex<HashMap<String, ClassRef>>,
    /// Bootstrap classes being defined, and by which thread.
    loading: Mutex<HashMap<String, u64>>,
    methods: MethodTable,
    next_method_index: AtomicU32,
    primitives: OnceLock<[ClassRef; 9]>,
    mirrors_ready: AtomicBool,
    strings: Mutex<HashMap<Vec<u16>, Ref>>,
    /// Classes defined by non-bootstrap loaders, keyed by (loader, name).
    loader_classes: Mutex<HashMap<(Ref, String), ClassRef>>,
    next_hidden_id: AtomicU32,
    /// Synthetic signature-polymorphic methods, keyed by (class, name, descriptor).
    polymorphic: Mutex<HashMap<(u32, String, String), MethodRef>>,
    /// Linked invokedynamic sites, keyed by (method index, pc).
    pub indy_sites: Mutex<crate::FastMap<(u32, u32), LinkedSite>>,
    /// Linked invokehandle sites (MethodHandle.invoke/VarHandle calls), keyed by
    /// (class id, constant pool index).
    pub handle_sites: Mutex<crate::FastMap<(u32, u16), LinkedSite>>,
    /// Named modules of the bootstrap loader, by name.
    boot_modules: Mutex<HashMap<String, Ref>>,
    /// Module of each package, keyed by (loader, package).
    package_modules: Mutex<HashMap<(Ref, String), Ref>>,
    boot_unnamed_module: RefCell32,
    /// Resolved MethodHandle / MethodType / dynamic constants, keyed by (class id, cp index).
    pub constants: Mutex<crate::FastMap<(u32, u16), Ref>>,
    natives: Natives,
    offsets: KnownOffsets,
    hash_state: AtomicU32,
    gc_count: AtomicU32,
    /// Reads release the GIL (they block); the mutex serialises readers.
    pub stdin: Mutex<Box<dyn std::io::Read + Send>>,
    pub stdout: Mutex<Box<dyn Write + Send>>,
    pub stderr: Mutex<Box<dyn Write + Send>>,
    pub start_time: std::time::Instant,
}

const PRIMITIVES: [Option<FieldKind>; 9] = [
    Some(FieldKind::Boolean),
    Some(FieldKind::Byte),
    Some(FieldKind::Char),
    Some(FieldKind::Short),
    Some(FieldKind::Int),
    Some(FieldKind::Long),
    Some(FieldKind::Float),
    Some(FieldKind::Double),
    None,
];

impl Vm {
    pub fn new(options: VmOptions) -> VmResult<Vm> {
        let memory = match options.memory {
            Some(region) => crate::memory::LinearMemory::new(region, options.heap_size),
            None => crate::memory::LinearMemory::allocate(options.heap_size, options.max_threads),
        };
        let stop_flag = memory.stop_flag();
        let vm = Vm {
            heap: Heap::in_region(memory.heap_base(), options.heap_size, Some(memory.starts_bitmap())),
            memory,
            jit: options.jit.inspect(|_| {
                crate::jit::stats::init();
                crate::memory::SAMPLING.store(std::env::var_os("RJAVA_JSAMPLE").is_some(), std::sync::atomic::Ordering::Relaxed);
            }),
            jit_queue: Default::default(),
            #[cfg(unix)]
            jni: Default::default(),
            threads: crate::threads::Threads::new(stop_flag),
            self_ref: OnceLock::new(),
            exit_handler: options.exit_handler,
            vfs: options.vfs,
            fds: crate::fs::FdTable::default(),
            sockets: crate::net::Sockets::default(),
            config: options.config,
            classpath: options.classpath,
            classes: ClassTable::new(),
            bootstrap: Mutex::new(HashMap::new()),
            loading: Mutex::new(HashMap::new()),
            methods: MethodTable::new(),
            next_method_index: AtomicU32::new(0),
            primitives: OnceLock::new(),
            mirrors_ready: AtomicBool::new(false),
            strings: Mutex::new(HashMap::new()),
            loader_classes: Mutex::new(HashMap::new()),
            next_hidden_id: AtomicU32::new(1),
            polymorphic: Mutex::new(HashMap::new()),
            indy_sites: Mutex::new(Default::default()),
            handle_sites: Mutex::new(Default::default()),
            constants: Mutex::new(Default::default()),
            boot_modules: Mutex::new(HashMap::new()),
            package_modules: Mutex::new(HashMap::new()),
            boot_unnamed_module: RefCell32::default(),
            natives: Natives::new(),
            offsets: KnownOffsets::default(),
            hash_state: AtomicU32::new(0x2545_F491),
            gc_count: AtomicU32::new(0),
            stdin: Mutex::new(options.stdin),
            stdout: Mutex::new(options.stdout),
            stderr: Mutex::new(options.stderr),
            start_time: std::time::Instant::now(),
        };
        VERIFY.store(std::env::var_os("RJAVA_GC_VERIFY").is_some(), Ordering::Relaxed);
        // Class ids start at 1 so that a zeroed header is recognisably invalid.
        vm.classes.push(|_| Box::leak(Box::new(placeholder_class())));
        vm.bootstrap_core()?;
        Ok(vm)
    }

    /// Loads Object and Class, then creates the primitive classes and the
    /// mirrors of everything loaded so far.
    fn bootstrap_core(&self) -> VmResult<()> {
        self.load_class("java/lang/Object")?;
        self.load_class("java/lang/Class")?;
        let primitives = PRIMITIVES.map(|kind| self.define_primitive(kind));
        let _ = self.primitives.set(primitives);
        self.mirrors_ready.store(true, Ordering::Release);
        let loaded: Vec<ClassRef> = self.loaded_classes();
        for class in loaded {
            self.mirror(class);
        }
        self.load_class("java/lang/String")?;
        Ok(())
    }

    /// Moves the VM to the heap for the rest of the process; required for
    /// running Java threads, which may outlive any caller.
    pub fn leak(self) -> &'static Vm {
        let vm: &'static Vm = Box::leak(Box::new(self));
        let _ = vm.self_ref.set(vm);
        vm
    }

    pub fn as_static(&self) -> &'static Vm {
        self.self_ref.get().expect("threads require a VM created with Vm::leak")
    }

    pub fn halt(&self, status: i32) {
        self.print_profile();
        (self.exit_handler)(status)
    }

    /// With RJAVA_PROFILE set, prints the most frequently invoked methods.
    pub fn print_profile(&self) {
        if std::env::var_os("RJAVA_PROFILE").is_none() {
            return;
        }
        let count = self.next_method_index.load(Ordering::Relaxed);
        let mut methods: Vec<(u64, MethodRef)> = (0..count)
            .filter_map(|i| self.methods.get(i))
            .map(|m| (m.invocations.load(Ordering::Relaxed), m))
            .filter(|(n, _)| *n > 0)
            .collect();
        methods.sort_by(|a, b| b.0.cmp(&a.0));
        let total: u64 = methods.iter().map(|(n, _)| n).sum();
        eprintln!("--- {total} Java method invocations; top 40 ---");
        for (count, method) in methods.iter().take(40) {
            eprintln!("{count:>12}  {method:?}");
        }
    }

    // --- registry --------------------------------------------------------------

    pub fn class_by_id(&self, id: u32) -> ClassRef {
        match self.classes.get(id) {
            Some(class) if id != 0 => class,
            _ => panic!("invalid class id {id:#x}: reference to a freed or corrupt object"),
        }
    }

    pub fn class_of(&self, obj: Ref) -> ClassRef {
        self.class_by_id(self.heap.class_id(obj))
    }

    /// The method with global index `index`, if there is one.
    pub fn method_if_any(&self, index: u32) -> Option<MethodRef> {
        self.methods.get(index)
    }

    pub fn method_by_index(&self, index: u32) -> MethodRef {
        self.methods.get(index).expect("valid method index")
    }

    pub fn loaded_classes(&self) -> Vec<ClassRef> {
        self.classes.all().into_iter().skip(1).collect()
    }

    pub fn primitive_class(&self, kind: Option<FieldKind>) -> ClassRef {
        let index = PRIMITIVES.iter().position(|k| *k == kind).unwrap();
        self.primitives.get().expect("primitives not created yet")[index]
    }

    pub fn primitive_class_by_name(&self, name: &str) -> Option<ClassRef> {
        self.primitives.get()?.iter().copied().find(|c| c.name == name)
    }

    pub fn native(&self, key: &str) -> Option<NativeFn> {
        self.natives.get(key)
    }

    pub fn image(&self) -> Option<&jimage::Image> {
        self.classpath.image()
    }

    fn register(&self, class: Class) -> ClassRef {
        let class = self.classes.push(|id| {
            let mut class = Box::new(class);
            class.id = id;
            let (statics, initialized) = self.memory.class_meta(id);
            statics.set(class.statics.get());
            initialized.store(class.initialized.load(Ordering::Relaxed), Ordering::Relaxed);
            (class.statics, class.initialized) = (statics, initialized);
            class.set_method_owners();
            Box::leak(class)
        });
        for method in &class.methods {
            self.register_method(method);
        }
        class
    }

    // --- loading ---------------------------------------------------------------

    /// Loads a class with the bootstrap loader (arrays included).
    pub fn load_class(&self, name: &str) -> VmResult<ClassRef> {
        if let Some(class) = self.bootstrap.lock().unwrap().get(name) {
            return Ok(class);
        }
        if let Some(component) = name.strip_prefix('[') {
            let component = match component.as_bytes().first() {
                Some(b'L') => self.load_class(component.strip_prefix('L').and_then(|c| c.strip_suffix(';')).ok_or_else(|| VmError::no_class_def(name))?)?,
                Some(b'[') => self.load_class(component)?,
                _ => {
                    let kind = match component {
                        "Z" => FieldKind::Boolean,
                        "B" => FieldKind::Byte,
                        "C" => FieldKind::Char,
                        "S" => FieldKind::Short,
                        "I" => FieldKind::Int,
                        "J" => FieldKind::Long,
                        "F" => FieldKind::Float,
                        "D" => FieldKind::Double,
                        _ => return Err(VmError::no_class_def(name)),
                    };
                    self.primitive_class(Some(kind))
                }
            };
            return Ok(self.array_class(component));
        }
        // Another thread defining it: wait for that; this thread: circularity.
        let me = crate::threads::current_id();
        loop {
            if let Some(class) = self.find_loaded(name) {
                return Ok(class);
            }
            let mut loading = self.lock_loading();
            match loading.get(name) {
                None => {
                    loading.insert(name.to_owned(), me);
                    break;
                }
                Some(&owner) if owner == me => {
                    return Err(VmError::new("java/lang/ClassCircularityError", name.replace('/', ".")));
                }
                Some(_) => {
                    drop(loading);
                    crate::threads::blocking_region(&self.threads.safepoints, || {
                        std::thread::sleep(std::time::Duration::from_micros(50))
                    });
                }
            }
        }
        let result = self
            .classpath
            .find_class(name)
            .ok_or_else(|| VmError::no_class_def(name))
            .and_then(|bytes| self.define_class(name, bytes, Ref::NULL));
        // Published before the name stops being "loading": a thread that
        // finds neither would define the class a second time.
        if let Ok(class) = result {
            self.bootstrap.lock().unwrap().insert(name.to_owned(), class);
        }
        self.lock_loading().remove(name);
        result
    }

    /// The `loading` lock, which a thread may hold while defining a class
    /// (allocating, so possibly collecting): wait for it outside Java code,
    /// or the collector would wait for this thread forever.
    fn lock_loading(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        match self.loading.try_lock() {
            Ok(guard) => guard,
            Err(_) => crate::threads::blocking_region(&self.threads.safepoints, || self.loading.lock().unwrap()),
        }
    }

    pub fn find_loaded(&self, name: &str) -> Option<ClassRef> {
        self.bootstrap.lock().unwrap().get(name).copied()
    }

    pub fn array_class(&self, component: ClassRef) -> ClassRef {
        if let Some(array) = component.array_class.get() {
            return array;
        }
        let _guard = self.lock_loading();
        if let Some(array) = component.array_class.get() {
            return array;
        }
        let name = match &component.kind {
            ClassKind::Instance => format!("[L{};", component.name),
            ClassKind::Array { .. } => format!("[{}", component.name),
            ClassKind::Primitive(Some(kind)) => format!("[{}", kind.descriptor_char()),
            ClassKind::Primitive(None) => panic!("array of void"),
        };
        let object = self.find_loaded("java/lang/Object").expect("Object loaded first");
        let interfaces = ["java/lang/Cloneable", "java/io/Serializable"]
            .iter()
            .filter_map(|n| self.find_loaded(n))
            .collect();
        let visibility = component.access_flags & (access::PUBLIC | access::PRIVATE | access::PROTECTED);
        let class = self.register(Class {
            access_flags: visibility | access::FINAL | access::ABSTRACT,
            super_class: Some(object),
            interfaces,
            loader: component.loader,
            module: component.module.clone(),
            vtable: object.vtable.clone(),
            init_state: Mutex::new(InitState::Initialized),
            ..empty_class(name.clone(), ClassKind::Array { component })
        });
        let _ = component.array_class.set(class);
        drop(_guard);
        self.bootstrap.lock().unwrap().insert(name, class);
        if self.mirrors_ready.load(Ordering::Acquire) {
            self.mirror(class);
        }
        class
    }

    fn define_primitive(&self, kind: Option<FieldKind>) -> ClassRef {
        let name = kind.map_or("void", FieldKind::primitive_name).to_owned();
        self.register(Class {
            access_flags: access::PUBLIC | access::FINAL | access::ABSTRACT,
            init_state: Mutex::new(InitState::Initialized),
            ..empty_class(name, ClassKind::Primitive(kind))
        })
    }

    /// Parses, links and registers a class. Superclasses and interfaces are
    /// loaded first.
    fn define_class(&self, expected_name: &str, bytes: ClassBytes, loader: Ref) -> VmResult<ClassRef> {
        let file = ClassFile::parse(&bytes.bytes).map_err(|e| VmError::new("java/lang/ClassFormatError", format!("{expected_name}: {e}")))?;
        if file.this_class != expected_name {
            return Err(VmError::new(
                "java/lang/NoClassDefFoundError",
                format!("{expected_name} (wrong name: {})", file.this_class),
            ));
        }
        self.define_parsed(file, bytes.module, loader, false, &mut |name| self.load_class(name))
    }

    /// Defines a class from bytes on behalf of a class loader (ClassLoader.defineClass*),
    /// or a hidden class (Lookup.defineHiddenClass). `name` is the expected internal name.
    pub fn define_class_bytes(
        &self,
        name: Option<&str>,
        bytes: &[u8],
        loader: Ref,
        module: Option<String>,
        hidden: bool,
        load_super: &mut dyn FnMut(&str) -> VmResult<ClassRef>,
    ) -> VmResult<ClassRef> {
        let mut file = ClassFile::parse(bytes).map_err(|e| VmError::new("java/lang/ClassFormatError", e.to_string()))?;
        if let Some(name) = name {
            // A hidden class takes the name it is defined under (e.g. the
            // JDK's InjectedInvoker template becomes Caller$$InjectedInvoker).
            if hidden {
                file.this_class = name.to_owned();
            } else if file.this_class != name {
                return Err(VmError::new("java/lang/NoClassDefFoundError", format!("{name} (wrong name: {})", file.this_class)));
            }
        }
        if !hidden {
            let key = (loader, file.this_class.clone());
            let exists = if loader.is_null() { self.find_loaded(&key.1).is_some() } else { self.loader_classes.lock().unwrap().contains_key(&key) };
            if exists {
                return Err(VmError::new("java/lang/LinkageError", format!("duplicate class definition: {}", key.1)));
            }
        }
        let this_class = file.this_class.clone();
        let class = self.define_parsed(file, module, loader, hidden, load_super)?;
        if !hidden {
            if loader.is_null() {
                self.bootstrap.lock().unwrap().insert(this_class, class);
            } else {
                self.loader_classes.lock().unwrap().insert((loader, this_class), class);
            }
        }
        Ok(class)
    }

    /// Records `loader` as an initiating loader of `class` (JVMS §5.3).
    pub fn record_initiating_loader(&self, loader: Ref, name: &str, class: ClassRef) {
        if !loader.is_null() {
            self.loader_classes.lock().unwrap().entry((loader, name.to_owned())).or_insert(class);
        }
    }

    /// A class defined by `loader`, or for which it is a recorded initiating loader
    /// (the bootstrap loader for null).
    pub fn find_loaded_by(&self, loader: Ref, name: &str) -> Option<ClassRef> {
        if loader.is_null() {
            self.find_loaded(name)
        } else {
            self.loader_classes.lock().unwrap().get(&(loader, name.to_owned())).copied()
        }
    }

    fn define_parsed(
        &self,
        mut file: ClassFile,
        module: Option<String>,
        loader: Ref,
        hidden: bool,
        load_super: &mut dyn FnMut(&str) -> VmResult<ClassRef>,
    ) -> VmResult<ClassRef> {
        crate::records::rewrite_object_methods(&mut file);
        let super_class = match &file.super_class {
            Some(name) => {
                let class = load_super(name)?;
                if class.is_interface() || class.access_flags & access::FINAL != 0 {
                    return Err(VmError::new(
                        "java/lang/IncompatibleClassChangeError",
                        format!("class {} has interface or final class {} as super class", file.this_class, name),
                    ));
                }
                Some(class)
            }
            None if file.this_class == "java/lang/Object" => None,
            None => return Err(VmError::new("java/lang/ClassFormatError", "missing super class")),
        };
        let interfaces = file
            .interfaces
            .iter()
            .map(|name| {
                let iface = load_super(name)?;
                if !iface.is_interface() {
                    return Err(VmError::new(
                        "java/lang/IncompatibleClassChangeError",
                        format!("{} is not an interface", name),
                    ));
                }
                Ok(iface)
            })
            .collect::<VmResult<Vec<_>>>()?;

        // Fields and layout.
        let mut fields: Vec<Field> = file
            .fields
            .iter()
            .map(|f| Field {
                name: f.name.clone(),
                descriptor: f.descriptor.clone(),
                kind: FieldKind::from_descriptor(&f.descriptor),
                access_flags: f.access_flags,
                offset: 0,
                constant_value: f.constant_value,
                injected: false,
            })
            .collect();
        for (name, descriptor) in injected_fields(&file.this_class) {
            fields.push(Field {
                name: (*name).to_owned(),
                descriptor: (*descriptor).to_owned(),
                kind: FieldKind::from_descriptor(descriptor),
                access_flags: access::PRIVATE,
                offset: 0,
                constant_value: None,
                injected: true,
            });
        }
        let instance_start = super_class.map_or(HEADER_SIZE, |s| s.instance_size);
        let (instance_end, own_refs) = layout(&mut fields, instance_start, false);
        let mut ref_offsets = super_class.map_or_else(Vec::new, |s| s.ref_offsets.clone());
        ref_offsets.extend(own_refs);
        let (statics_end, static_ref_offsets) = layout(&mut fields, HEADER_SIZE, true);

        // Methods.
        let mut methods: Vec<Method> = file
            .methods
            .iter()
            .map(|m| {
                Method::new(MethodInit {
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                    access_flags: m.access_flags,
                    code: m.code.clone(),
                    global_index: self.next_method_index.fetch_add(1, Ordering::Relaxed),
                })
            })
            .collect();

        let name = if hidden {
            format!("{}/0x{:08x}", file.this_class, self.next_hidden_id.fetch_add(1, Ordering::Relaxed))
        } else {
            file.this_class.clone()
        };
        let this_class_index = file.this_class_index;
        let access_flags = file.access_flags;
        let vtable = if access_flags & access::INTERFACE != 0 {
            Vec::new()
        } else {
            build_vtable(super_class, &name, loader, &mut methods)
        };
        let cp_len = file.constant_pool.len();
        let mut class = Class {
            access_flags,
            super_class,
            interfaces,
            loader,
            module,
            hidden,
            fields,
            instance_size: instance_end,
            ref_offsets,
            statics_size: statics_end,
            static_ref_offsets,
            cp_cache: (0..cp_len).map(|_| OnceLock::new()).collect(),
            call_caches: (0..cp_len).map(|_| crate::class::CallCache::default()).collect(),
            file: Some(Box::new(file)),
            vtable,
            ..empty_class(name.clone(), ClassKind::Instance)
        };
        // Moving the Vec keeps its buffer, so the vtable's pointers stay valid.
        class.methods = methods;
        let class = self.register(class);
        // A class's own Class entry always resolves to itself (essential for
        // hidden classes, whose real name differs from the one in the bytes).
        let _ = class.cp_cache[this_class_index as usize].set(Resolved::Class(class));
        self.record_known_offsets(class);

        let statics = self.allocate(HEADER_SIZE + class.statics_size)?;
        self.heap.write(statics, 0, class.id);
        self.heap.write(statics, 4, STATICS_MARK);
        class.statics.set(statics);
        if self.mirrors_ready.load(Ordering::Acquire) {
            self.mirror(class);
        }
        Ok(class)
    }

    fn record_known_offsets(&self, class: ClassRef) {
        let offset = |name: &str| class.find_field_by_name(name).expect("known field").offset;
        let o = &self.offsets;
        match class.name.as_str() {
            "java/lang/Class" => {
                o.class_id.store(offset("vmClassId"), Ordering::Relaxed);
                o.class_loader.store(offset("classLoader"), Ordering::Relaxed);
                o.class_component.store(offset("componentType"), Ordering::Relaxed);
                o.class_modifiers.store(offset("modifiers"), Ordering::Relaxed);
                o.class_primitive.store(offset("primitive"), Ordering::Relaxed);
                o.class_module.store(offset("module"), Ordering::Relaxed);
            }
            "java/lang/ClassLoader" => {
                o.loader_unnamed_module.store(offset("unnamedModule"), Ordering::Relaxed);
            }
            "java/lang/String" => {
                o.string_value.store(offset("value"), Ordering::Relaxed);
                o.string_coder.store(offset("coder"), Ordering::Relaxed);
            }
            _ => {}
        }
    }

    // --- objects -----------------------------------------------------------------

    /// Allocates zeroed heap memory, collecting garbage if the heap is full.
    /// The new object is registered as a handle of the current thread so that
    /// a collection before it is stored somewhere reachable keeps it alive.
    fn allocate(&self, size: u32) -> VmResult<Ref> {
        let size = size.max(8).next_multiple_of(8);
        static NO_TLAB: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let no_tlab = *NO_TLAB.get_or_init(|| std::env::var_os("RJAVA_NO_TLAB").is_some());
        if let Some(obj) = (!no_tlab).then(|| crate::threads::tlab_alloc(&self.heap, size)).flatten() {
            crate::threads::push_handle(obj);
            return Ok(obj);
        }
        let obj = match self.heap.alloc(size) {
            Some(obj) => obj,
            None if crate::threads::on_vm_thread() => {
                self.collect();
                match self.heap.alloc(size) {
                    Some(obj) => obj,
                    None => {
                        // Free space may be there but fragmented.
                        self.heap.force_compaction();
                        self.collect();
                        self.heap.alloc(size).ok_or_else(out_of_memory)?
                    }
                }
            }
            None => return Err(out_of_memory()),
        };
        crate::threads::push_handle(obj);
        Ok(obj)
    }

    pub fn new_object(&self, class: ClassRef) -> VmResult<Ref> {
        let obj = self.allocate(class.instance_size)?;
        self.heap.write(obj, 0, class.id);
        Ok(obj)
    }

    pub fn new_array(&self, array_class: ClassRef, length: i32) -> VmResult<Ref> {
        if length < 0 {
            return Err(VmError::new("java/lang/NegativeArraySizeException", length.to_string()));
        }
        let element_size = array_class.element_kind().expect("array class").size() as u64;
        let size = ARRAY_BASE_OFFSET as u64 + element_size * length as u64;
        let size = u32::try_from(size).map_err(|_| out_of_memory())?;
        let array = self.allocate(size)?;
        self.heap.write(array, 0, array_class.id);
        self.heap.write(array, ARRAY_LENGTH_OFFSET, length);
        Ok(array)
    }

    pub fn new_primitive_array(&self, kind: FieldKind, length: i32) -> VmResult<Ref> {
        self.new_array(self.array_class(self.primitive_class(Some(kind))), length)
    }

    /// Shallow copy (Object.clone).
    pub fn clone_object(&self, obj: Ref) -> VmResult<Ref> {
        let class = self.class_of(obj);
        let size = match class.element_kind() {
            Some(kind) => ARRAY_BASE_OFFSET + kind.size() * self.heap.array_length(obj) as u32,
            None => class.instance_size,
        };
        let copy = self.allocate(size)?;
        self.heap.copy(obj, 0, copy, 0, size as usize);
        self.heap.write::<u32>(copy, 4, 0); // fresh identity hash
        Ok(copy)
    }

    pub fn identity_hash(&self, obj: Ref) -> i32 {
        let mark = self.heap.atomic_u32(obj, 4);
        let current = mark.load(Ordering::Relaxed);
        if current != 0 {
            return current as i32;
        }
        let hash = loop {
            // xorshift32; Java hash codes are 31-bit positive in HotSpot too.
            let mut x = self.hash_state.load(Ordering::Relaxed);
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.hash_state.store(x, Ordering::Relaxed);
            let h = x & 0x7FFF_FFFF;
            if h != 0 {
                break h;
            }
        };
        match mark.compare_exchange(0, hash, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => hash as i32,
            Err(existing) => existing as i32,
        }
    }

    // --- mirrors -----------------------------------------------------------------

    /// The `java.lang.Class` object for a class, created on first use.
    /// (Registered as a handle, so callers may allocate while holding it.)
    pub fn mirror(&self, class: ClassRef) -> Ref {
        let existing = class.mirror.get();
        if !existing.is_null() {
            crate::threads::push_handle(existing);
            return existing;
        }
        let class_class = self.find_loaded("java/lang/Class").expect("java.lang.Class not loaded");
        let mirror = self.new_object(class_class).expect("out of memory creating mirror");
        let o = &self.offsets;
        self.heap.write(mirror, o.class_id.load(Ordering::Relaxed), class.id);
        self.heap.write(mirror, o.class_loader.load(Ordering::Relaxed), class.loader.0);
        self.heap.write(mirror, o.class_modifiers.load(Ordering::Relaxed), class_modifiers(class));
        if let ClassKind::Array { component } = class.kind {
            self.heap.write(mirror, o.class_component.load(Ordering::Relaxed), self.mirror(component).0);
        }
        if class.is_primitive() {
            self.heap.write::<u8>(mirror, o.class_primitive.load(Ordering::Relaxed), 1);
        }
        let module = self.module_of(class);
        self.heap.write(mirror, o.class_module.load(Ordering::Relaxed), module.0);
        // Another thread may have made one meanwhile: there must be only one.
        class.mirror.publish(mirror)
    }

    // --- modules -------------------------------------------------------------------

    /// The Module a class belongs to, or null before the module system is up.
    pub fn module_of(&self, class: ClassRef) -> Ref {
        match class.kind {
            ClassKind::Array { component } => return self.module_of(component),
            ClassKind::Primitive(_) => return self.boot_modules.lock().unwrap().get("java.base").copied().unwrap_or_default(),
            ClassKind::Instance => {}
        }
        if let Some(module) = self.package_modules.lock().unwrap().get(&(class.loader, class.package().to_owned())) {
            return *module;
        }
        if class.loader.is_null() {
            if let Some(name) = &class.module {
                if let Some(&module) = self.boot_modules.lock().unwrap().get(name) {
                    return module;
                }
            }
            return self.boot_unnamed_module.get();
        }
        Ref(self.heap.read(class.loader, self.offsets.loader_unnamed_module.load(Ordering::Relaxed)))
    }

    /// Records a module's packages (Module.defineModule0) and patches the
    /// `module` field of already-created mirrors.
    pub fn define_module(&self, module: Ref, loader: Ref, name: Option<&str>, packages: &[String]) {
        {
            let mut map = self.package_modules.lock().unwrap();
            for package in packages {
                map.insert((loader, package.clone()), module);
            }
        }
        if loader.is_null() {
            if let Some(name) = name {
                self.boot_modules.lock().unwrap().insert(name.to_owned(), module);
            }
        }
        self.refresh_mirror_modules();
    }

    pub fn set_boot_unnamed_module(&self, module: Ref) {
        self.boot_unnamed_module.set(module);
        self.refresh_mirror_modules();
    }

    fn refresh_mirror_modules(&self) {
        let offset = self.offsets.class_module.load(Ordering::Relaxed);
        for class in self.loaded_classes() {
            let mirror = class.mirror.get();
            if !mirror.is_null() {
                let module = self.module_of(class);
                self.heap.write(mirror, offset, module.0);
            }
        }
    }

    /// The class a `java.lang.Class` object stands for.
    /// Offset of the class id in a java.lang.Class mirror.
    pub(crate) fn mirror_class_id_offset(&self) -> u32 {
        self.offsets.class_id.load(Ordering::Relaxed)
    }

    pub fn class_from_mirror(&self, mirror: Ref) -> ClassRef {
        self.class_by_id(self.heap.read(mirror, self.offsets.class_id.load(Ordering::Relaxed)))
    }

    // --- strings -----------------------------------------------------------------

    pub fn new_string(&self, s: &str) -> VmResult<Ref> {
        self.new_string_utf16(&s.encode_utf16().collect::<Vec<_>>())
    }

    /// Creates a `java.lang.String`, LATIN1-coded when possible (compact strings).
    pub fn new_string_utf16(&self, units: &[u16]) -> VmResult<Ref> {
        let string_class = self.load_class("java/lang/String")?;
        let latin1 = units.iter().all(|&u| u <= 0xFF);
        let byte_len = if latin1 { units.len() } else { units.len() * 2 };
        let value = self.new_primitive_array(FieldKind::Byte, byte_len as i32)?;
        let bytes = self.heap.bytes_mut(value, ARRAY_BASE_OFFSET, byte_len);
        if latin1 {
            for (b, &u) in bytes.iter_mut().zip(units) {
                *b = u as u8;
            }
        } else {
            // StringUTF16 uses the platform byte order (StringUTF16.isBigEndian()).
            for (chunk, &u) in bytes.chunks_exact_mut(2).zip(units) {
                chunk.copy_from_slice(&u.to_le_bytes());
            }
        }
        let string = self.new_object(string_class)?;
        self.heap.write(string, self.offsets.string_value.load(Ordering::Relaxed), value.0);
        self.heap.write::<u8>(string, self.offsets.string_coder.load(Ordering::Relaxed), if latin1 { 0 } else { 1 });
        Ok(string)
    }

    pub fn string_utf16(&self, string: Ref) -> Vec<u16> {
        let value = Ref(self.heap.read(string, self.offsets.string_value.load(Ordering::Relaxed)));
        let coder: u8 = self.heap.read(string, self.offsets.string_coder.load(Ordering::Relaxed));
        let len = self.heap.array_length(value) as usize;
        let bytes = self.heap.bytes(value, ARRAY_BASE_OFFSET, len);
        if coder == 0 {
            bytes.iter().map(|&b| b as u16).collect()
        } else {
            bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
        }
    }

    pub fn string_value(&self, string: Ref) -> String {
        String::from_utf16_lossy(&self.string_utf16(string))
    }

    pub fn intern(&self, string: Ref) -> Ref {
        let units = self.string_utf16(string);
        *self.strings.lock().unwrap().entry(units).or_insert(string)
    }

    pub fn intern_utf16(&self, units: &[u16]) -> VmResult<Ref> {
        if let Some(&s) = self.strings.lock().unwrap().get(units) {
            return Ok(s);
        }
        let string = self.new_string_utf16(units)?;
        Ok(*self.strings.lock().unwrap().entry(units.to_vec()).or_insert(string))
    }

    // --- resolution (JVMS §5.4.3) ------------------------------------------------

    pub fn resolve_class(&self, from: ClassRef, index: u16) -> VmResult<ClassRef> {
        if let Some(Resolved::Class(c)) = from.cp_cache[index as usize].get() {
            return Ok(c);
        }
        let name = from.cp().class_name(index).map_err(format_error)?;
        let class = self.load_class(name)?;
        check_access(from, class)?;
        let _ = from.cp_cache[index as usize].set(Resolved::Class(class));
        Ok(class)
    }

    pub fn resolve_field(&self, from: ClassRef, index: u16) -> VmResult<FieldRef> {
        if let Some(Resolved::Field(f)) = from.cp_cache[index as usize].get() {
            return Ok(*f);
        }
        let member = from.cp().member_ref(index).map_err(format_error)?;
        let class_index = match from.cp().get(index).map_err(format_error)? {
            Constant::FieldRef(m) => m.class,
            _ => return Err(format_error("not a field reference")),
        };
        let owner = self.resolve_class(from, class_index)?;
        let (holder, field) = find_field(owner, member.name, member.descriptor).ok_or_else(|| {
            VmError::new("java/lang/NoSuchFieldError", format!("{}.{}", owner.java_name(), member.name))
        })?;
        let volatile = field.access_flags & classfile::access::VOLATILE != 0;
        let resolved = FieldRef { holder, offset: field.offset, kind: field.kind, is_static: field.is_static(), volatile };
        let _ = from.cp_cache[index as usize].set(Resolved::Field(resolved));
        Ok(resolved)
    }

    pub fn resolve_method(&self, from: ClassRef, index: u16) -> VmResult<MethodRef> {
        if let Some(Resolved::Method(m)) = from.cp_cache[index as usize].get() {
            return Ok(m);
        }
        let cp = from.cp();
        let (class_index, interface) = match cp.get(index).map_err(format_error)? {
            Constant::MethodRef(m) => (m.class, false),
            Constant::InterfaceMethodRef(m) => (m.class, true),
            _ => return Err(format_error("not a method reference")),
        };
        let member = cp.member_ref(index).map_err(format_error)?;
        let owner = self.resolve_class(from, class_index)?;
        let mut method = self.lookup_method(owner, member.name, member.descriptor, interface)?;
        if method.is_signature_polymorphic() {
            method = self.signature_polymorphic(method, member.descriptor);
        }
        let _ = from.cp_cache[index as usize].set(Resolved::Method(method));
        Ok(method)
    }

    /// Method resolution (JVMS §5.4.3.3) and interface method resolution (§5.4.3.4).
    pub fn lookup_method(&self, owner: ClassRef, name: &str, descriptor: &str, interface: bool) -> VmResult<MethodRef> {
        if owner.is_interface() != interface {
            return Err(VmError::new(
                "java/lang/IncompatibleClassChangeError",
                format!("{} {} an interface", owner.java_name(), if interface { "is not" } else { "is" }),
            ));
        }
        let found = if interface {
            owner
                .find_method(name, descriptor)
                .or_else(|| {
                    let object = self.find_loaded("java/lang/Object")?;
                    object.find_method(name, descriptor).filter(|m| m.access_flags & access::PUBLIC != 0 && !m.is_static())
                })
                .or_else(|| find_in_superinterfaces(owner, name, descriptor))
        } else {
            owner
                .super_classes()
                .find_map(|c| {
                    c.find_method(name, descriptor).or_else(|| {
                        // Signature-polymorphic methods match any descriptor.
                        c.methods
                            .iter()
                            .find(|m| m.name == name && m.is_signature_polymorphic())
                            .map(|m| unsafe { &*(m as *const Method) })
                    })
                })
                .or_else(|| find_in_superinterfaces(owner, name, descriptor))
        };
        found.ok_or_else(|| {
            VmError::new("java/lang/NoSuchMethodError", format!("'{} {}.{}'", descriptor, owner.java_name(), name))
        })
    }

    /// The synthetic method implementing a signature-polymorphic method for one
    /// call-site descriptor.
    pub fn signature_polymorphic(&self, generic: MethodRef, descriptor: &str) -> MethodRef {
        let key = (generic.class().id, generic.name.clone(), descriptor.to_owned());
        if let Some(&m) = self.polymorphic.lock().unwrap().get(&key) {
            return m;
        }
        let intrinsic = match (generic.class().name.as_str(), generic.name.as_str()) {
            ("java/lang/invoke/MethodHandle", "invokeBasic") => Intrinsic::InvokeBasic,
            ("java/lang/invoke/MethodHandle", "linkToStatic") => Intrinsic::LinkToStatic,
            ("java/lang/invoke/MethodHandle", "linkToSpecial") => Intrinsic::LinkToSpecial,
            ("java/lang/invoke/MethodHandle", "linkToVirtual") => Intrinsic::LinkToVirtual,
            ("java/lang/invoke/MethodHandle", "linkToInterface") => Intrinsic::LinkToInterface,
            _ => Intrinsic::Generic,
        };
        let mut method = Method::new(MethodInit {
            name: generic.name.clone(),
            descriptor: descriptor.to_owned(),
            access_flags: generic.access_flags,
            code: None,
            global_index: self.next_method_index.fetch_add(1, Ordering::Relaxed),
        });
        method.intrinsic = Some(intrinsic);
        method.set_class(generic.class());
        let method: MethodRef = Box::leak(Box::new(method));
        self.register_method(method);
        *self.polymorphic.lock().unwrap().entry(key).or_insert(method)
    }

    fn register_method(&self, method: MethodRef) {
        self.methods.set(method.global_index, method);
    }

    /// Method selection for invokevirtual / invokeinterface (JVMS §5.4.6).
    pub fn select_method(&self, receiver: ClassRef, resolved: MethodRef) -> VmResult<MethodRef> {
        if resolved.is_private() {
            return Ok(resolved);
        }
        if let Some(index) = resolved.vtable_index {
            if let Some(&m) = receiver.vtable.get(index as usize) {
                return self.check_selected(m);
            }
        }
        let key = resolved as *const Method as usize;
        if let Some(&m) = receiver.selection_cache.lock().unwrap().get(&key) {
            return Ok(m);
        }
        let selected = self.select_by_name(receiver, &resolved.name, &resolved.descriptor)?;
        receiver.selection_cache.lock().unwrap().insert(key, selected);
        Ok(selected)
    }

    fn select_by_name(&self, receiver: ClassRef, name: &str, descriptor: &str) -> VmResult<MethodRef> {
        for class in receiver.super_classes() {
            if let Some(m) = class.find_method(name, descriptor) {
                if !m.is_static() && !m.is_private() {
                    return self.check_selected(m);
                }
            }
        }
        // Maximally-specific superinterface methods.
        let candidates: Vec<MethodRef> = receiver
            .all_interfaces()
            .into_iter()
            .filter_map(|i| i.find_method(name, descriptor))
            .filter(|m| !m.is_static() && !m.is_private())
            .collect();
        let specific: Vec<MethodRef> = candidates
            .iter()
            .copied()
            .filter(|m| {
                !candidates.iter().any(|other| {
                    !std::ptr::eq(other.class(), m.class()) && other.class().is_subclass_of(m.class())
                })
            })
            .collect();
        let concrete: Vec<MethodRef> = specific.iter().copied().filter(|m| !m.is_abstract()).collect();
        match concrete.as_slice() {
            [m] => Ok(m),
            [] => Err(VmError::new(
                "java/lang/AbstractMethodError",
                format!("Receiver class {} does not define or inherit an implementation of {}{}", receiver.java_name(), name, descriptor),
            )),
            _ => Err(VmError::new(
                "java/lang/IncompatibleClassChangeError",
                format!("Conflicting default methods: {name}{descriptor}"),
            )),
        }
    }

    fn check_selected(&self, method: MethodRef) -> VmResult<MethodRef> {
        if method.is_abstract() {
            return Err(VmError::new("java/lang/AbstractMethodError", format!("{method:?}")));
        }
        Ok(method)
    }

    /// Initialisation state transitions are driven by the interpreter; this
    /// only reads it.
    pub fn is_initialized(&self, class: ClassRef) -> bool {
        *class.init_state.lock().unwrap() == InitState::Initialized
    }
}

fn out_of_memory() -> VmError {
    VmError::new("java/lang/OutOfMemoryError", "Java heap space")
}

fn format_error(e: impl std::fmt::Display) -> VmError {
    VmError::new("java/lang/ClassFormatError", e.to_string())
}

fn placeholder_class() -> Class {
    empty_class("<invalid>".into(), ClassKind::Primitive(None))
}

fn empty_class(name: String, kind: ClassKind) -> Class {
    Class {
        id: 0,
        name,
        kind,
        access_flags: 0,
        super_class: None,
        interfaces: Vec::new(),
        loader: Ref::NULL,
        module: None,
        hidden: false,
        dynamic_nest_host: OnceLock::new(),
        file: None,
        fields: Vec::new(),
        methods: Vec::new(),
        instance_size: HEADER_SIZE,
        ref_offsets: Vec::new(),
        statics_size: 0,
        static_ref_offsets: Vec::new(),
        // Placeholders until `register` assigns the class its metadata.
        statics: Box::leak(Box::default()),
        vtable: Vec::new(),
        mirror: RefCell32::default(),
        init_state: Mutex::new(InitState::Uninitialized),
        init_thread: std::sync::atomic::AtomicU64::new(0),
        initialized: Box::leak(Box::default()),
        cp_cache: Box::new([]),
        call_caches: Box::new([]),
        array_class: OnceLock::new(),
        interface_closure: OnceLock::new(),
        selection_cache: Mutex::new(HashMap::new()),
    }
}

/// Hidden fields the VM adds to some classes (HotSpot's "injected fields").
fn injected_fields(class: &str) -> &'static [(&'static str, &'static str)] {
    match class {
        "java/lang/Class" => &[("vmClassId", "I")],
        "java/lang/invoke/MemberName" => &[("vmindex", "J")],
        "java/lang/invoke/ResolvedMethodName" => &[("vmtarget", "J")],
        _ => &[],
    }
}

/// Assigns offsets to the static or instance fields, largest first so that
/// everything is naturally aligned. Returns the end offset and the offsets of
/// reference fields.
fn layout(fields: &mut [Field], start: u32, statics: bool) -> (u32, Vec<u32>) {
    let mut offset = start;
    let mut refs = Vec::new();
    for size in [8, 4, 2, 1] {
        for field in fields.iter_mut().filter(|f| f.is_static() == statics && f.kind.size() == size) {
            offset = offset.next_multiple_of(size);
            field.offset = offset;
            if field.kind == FieldKind::Ref {
                refs.push(offset);
            }
            offset += size;
        }
    }
    (offset, refs)
}

/// Builds the vtable from the superclass's and records each method's index.
/// Entries for the class's own methods point into `methods`' heap buffer.
fn build_vtable(super_class: Option<ClassRef>, name: &str, loader: Ref, methods: &mut [Method]) -> Vec<MethodRef> {
    let mut vtable: Vec<MethodRef> = super_class.map_or_else(Vec::new, |s| s.vtable.clone());
    let package = name.rsplit_once('/').map_or("", |(p, _)| p);
    for method in methods.iter_mut() {
        if method.is_static() || method.is_private() || method.is_initializer() {
            continue;
        }
        let overridden = vtable.iter().position(|m| {
            m.name == method.name && m.descriptor == method.descriptor && can_override(package, loader, m)
        });
        // SAFETY: the method lives in the class's (immortal) method buffer.
        let entry: MethodRef = unsafe { &*(method as *const Method) };
        let index = match overridden {
            Some(index) => {
                vtable[index] = entry;
                index
            }
            None => {
                vtable.push(entry);
                vtable.len() - 1
            }
        };
        method.vtable_index = Some(index as u32);
    }
    vtable
}

/// Does a method declared in `package` (of `loader`) override `inherited` (JVMS §5.4.5)?
fn can_override(package: &str, loader: Ref, inherited: MethodRef) -> bool {
    if inherited.is_private() {
        return false;
    }
    if inherited.access_flags & (access::PUBLIC | access::PROTECTED) != 0 {
        return true;
    }
    // Package-private: same runtime package.
    inherited.class().loader == loader && inherited.class().package() == package
}

fn find_field<'a>(class: ClassRef, name: &str, descriptor: &str) -> Option<(ClassRef, &'a Field)> {
    if let Some(field) = class.find_field(name, descriptor) {
        // SAFETY: fields live as long as the immortal class.
        return Some((class, unsafe { &*(field as *const Field) }));
    }
    for &iface in &class.interfaces {
        if let Some(found) = find_field(iface, name, descriptor) {
            return Some(found);
        }
    }
    find_field(class.super_class?, name, descriptor)
}

fn find_in_superinterfaces(class: ClassRef, name: &str, descriptor: &str) -> Option<MethodRef> {
    let methods: Vec<MethodRef> = class
        .all_interfaces()
        .into_iter()
        .filter_map(|i| i.find_method(name, descriptor))
        .filter(|m| !m.is_static() && !m.is_private())
        .collect();
    methods.iter().copied().find(|m| !m.is_abstract()).or_else(|| methods.first().copied())
}

fn check_access(_from: ClassRef, _to: ClassRef) -> VmResult<()> {
    // Access checking is deferred until the module system is wired up.
    Ok(())
}

/// `Class.getModifiers()` bits: inner classes take their flags from the
/// InnerClasses attribute; arrays are abstract final with the component's
/// visibility.
fn class_modifiers(class: ClassRef) -> u16 {
    if let Some(file) = &class.file {
        if let Some(inner) = file.inner_classes.iter().find(|ic| ic.inner_class == class.name) {
            return inner.access_flags & !access::SYNCHRONIZED;
        }
    }
    class.access_flags & !(access::SYNCHRONIZED | access::MODULE)
}
