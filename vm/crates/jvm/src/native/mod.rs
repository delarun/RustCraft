//! Native method implementations, keyed by `class.name(descriptor)`.
//!
//! A native receives the argument slots exactly as laid out on the operand
//! stack (`this` first for instance methods; long/double take two slots) and
//! returns the result slot.

mod intrinsics;
mod invoke;
mod io;
mod lang;
mod loader;
mod management;
mod module;
mod net;
mod nio;
mod misc;
mod props;
mod reflect;
mod socket;
mod stackwalk;
mod thread;
mod unsafe_;
mod zip;

pub(crate) use invoke::{member_name_method, unbox_value};
pub(crate) use lang::arraycopy_unchecked;
pub(crate) use thread::THREAD_STATUS_RUNNABLE;

use crate::class::ClassRef;
use crate::heap::Ref;
use crate::interp::{JResult, Thread};
use std::collections::HashMap;

/// This process's id (WASI has none: a fixed one).
pub(crate) fn process_id() -> u32 {
    if cfg!(target_family = "wasm") { 42 } else { std::process::id() }
}

pub type NativeFn = fn(&mut Thread<'_>, &[u64]) -> JResult<Option<u64>>;

pub struct Natives {
    map: HashMap<&'static str, NativeFn>,
}

impl Natives {
    pub fn new() -> Self {
        let mut natives = Natives { map: HashMap::new() };
        invoke::register(&mut natives);
        intrinsics::register(&mut natives);
        io::register(&mut natives);
        lang::register(&mut natives);
        loader::register(&mut natives);
        management::register(&mut natives);
        module::register(&mut natives);
        net::register(&mut natives);
        nio::register(&mut natives);
        zip::register(&mut natives);
        misc::register(&mut natives);
        props::register(&mut natives);
        thread::register(&mut natives);
        thread::register_shutdown(&mut natives);
        reflect::register(&mut natives);
        reflect::register_members(&mut natives);
        reflect::register_constant_pool(&mut natives);
        stackwalk::register(&mut natives);
        socket::register(&mut natives);
        unsafe_::register(&mut natives);
        #[cfg(target_family = "wasm")]
        crate::web::register(&mut natives);
        natives
    }

    pub fn add(&mut self, key: &'static str, f: NativeFn) {
        let previous = self.map.insert(key, f);
        debug_assert!(previous.is_none(), "native {key} registered twice");
    }

    pub fn get(&self, key: &str) -> Option<NativeFn> {
        if let Some(f) = self.map.get(key) {
            return Some(*f);
        }
        // AWT and Java2D classes cache JNI field IDs for their C code in
        // initIDs: nothing to do here (headless; image code is Java).
        let awt = ["java/awt/", "sun/awt/", "sun/java2d/", "javax/imageio/"].iter().any(|p| key.starts_with(p));
        (awt && key.ends_with(".initIDs()V")).then_some(no_op as NativeFn)
    }
}

impl Default for Natives {
    fn default() -> Self {
        Self::new()
    }
}

/// VM fix-ups that must run right after a class's static initialiser.
pub(crate) fn class_initialized(thread: &mut Thread<'_>, class: ClassRef) {
    if class.name == "jdk/internal/misc/UnsafeConstants" {
        unsafe_::set_unsafe_constants(thread, class);
    }
}

// --- argument and result helpers ---------------------------------------------

#[inline]
pub(crate) fn arg_ref(args: &[u64], i: usize) -> Ref {
    Ref::from_slot(args[i])
}
#[inline]
pub(crate) fn arg_i(args: &[u64], i: usize) -> i32 {
    args[i] as i32
}
#[inline]
pub(crate) fn arg_j(args: &[u64], i: usize) -> i64 {
    args[i] as i64
}
#[inline]
pub(crate) fn arg_z(args: &[u64], i: usize) -> bool {
    args[i] as i32 != 0
}

#[inline]
pub(crate) fn ret_void() -> JResult<Option<u64>> {
    Ok(None)
}
#[inline]
pub(crate) fn ret_i(v: i32) -> JResult<Option<u64>> {
    Ok(Some(v as u32 as u64))
}
#[inline]
pub(crate) fn ret_z(v: bool) -> JResult<Option<u64>> {
    ret_i(v as i32)
}
#[inline]
pub(crate) fn ret_j(v: i64) -> JResult<Option<u64>> {
    Ok(Some(v as u64))
}
#[inline]
pub(crate) fn ret_ref(r: Ref) -> JResult<Option<u64>> {
    Ok(Some(r.slot()))
}

pub(crate) fn no_op(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_void()
}

pub(crate) fn return_false(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_z(false)
}

pub(crate) fn return_zero_int(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_i(0)
}

pub(crate) fn return_zero_long(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_j(0)
}

pub(crate) fn return_null(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    ret_ref(Ref::NULL)
}

impl Thread<'_> {
    /// Reads a named instance field (natives use this for JDK objects whose
    /// layout the VM does not track explicitly).
    pub fn field(&self, obj: Ref, class: &str, name: &str) -> u64 {
        let (offset, kind) = self.field_info(obj, class, name);
        crate::interp::read_field(&self.vm.heap, obj, offset, kind)
    }

    pub fn set_field(&self, obj: Ref, class: &str, name: &str, value: u64) {
        let (offset, kind) = self.field_info(obj, class, name);
        crate::interp::write_field(&self.vm.heap, obj, offset, kind, value)
    }

    /// Reads a reference field. The result is registered as a handle: the
    /// object stays put even if the caller allocates before using it.
    pub fn field_ref(&self, obj: Ref, class: &str, name: &str) -> Ref {
        let r = Ref::from_slot(self.field(obj, class, name));
        crate::threads::push_handle(r);
        r
    }

    fn field_info(&self, obj: Ref, class: &str, name: &str) -> (u32, crate::class::FieldKind) {
        // The class among the object's own (it may not be the boot loader's).
        let own = (!obj.is_null()).then(|| self.vm.class_of(obj)).and_then(|c| c.super_classes().find(|s| s.name == class));
        let class = own.or_else(|| self.vm.find_loaded(class)).unwrap_or_else(|| panic!("{class} not loaded"));
        let field = class
            .super_classes()
            .find_map(|c| c.find_field_by_name(name).filter(|f| !f.is_static()))
            .unwrap_or_else(|| panic!("{}.{name} not found", class.name));
        (field.offset, field.kind)
    }

    pub fn set_static(&self, class: ClassRef, name: &str, value: u64) {
        let field = class.fields.iter().find(|f| f.name == name && f.is_static()).unwrap_or_else(|| panic!("static {}.{name}", class.name));
        crate::interp::write_field(&self.vm.heap, class.statics.get(), field.offset, field.kind, value)
    }

    pub fn new_string(&mut self, s: &str) -> JResult<Ref> {
        self.vm.new_string(s).map_err(|e| self.throw_vm_error(e))
    }

    pub fn new_object_array(&mut self, component: &str, items: &[Ref]) -> JResult<Ref> {
        let component = self.load_class(component)?;
        let array_class = self.vm.array_class(component);
        let array = self.vm.new_array(array_class, items.len() as i32).map_err(|e| self.throw_vm_error(e))?;
        for (i, item) in items.iter().enumerate() {
            self.vm.heap.write(array, crate::heap::ARRAY_BASE_OFFSET + i as u32 * 4, item.0);
        }
        Ok(array)
    }

    pub fn null_check(&mut self, r: Ref) -> JResult<Ref> {
        if r.is_null() { Err(self.throw_new("java/lang/NullPointerException", None)) } else { Ok(r) }
    }
}
