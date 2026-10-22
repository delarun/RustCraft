//! The bytecode interpreter.
//!
//! All frames of a thread share one value stack of 64-bit slots. A frame's
//! locals are followed by its operand stack; on a call the arguments already
//! on the caller's operand stack become the callee's first locals, so nothing
//! is copied. Long and double values take two slots (value in the first), as
//! in the JVM specification, which keeps `dup2`/`pop2` semantics exact.

use crate::class::{ClassKind, ClassRef, FastPath, FieldKind, InitState, Intrinsic, MethodRef};
use crate::heap::{ARRAY_BASE_OFFSET, Ref};
use crate::memory::{Frame, FrameStack};
use crate::native::NativeFn;
use crate::vm::{Vm, VmError};
use classfile::{Constant, opcodes::*};

/// A thrown Java exception (the Throwable object).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throw(pub Ref);

pub type JResult<T> = Result<T, Throw>;

pub struct Thread<'vm> {
    pub vm: &'vm Vm,
    pub(crate) shared: std::sync::Arc<crate::threads::ThreadShared>,
    /// Instructions until the next safepoint check.
    ticks: u32,
    /// Handles below this index belong to Rust code that called into the
    /// interpreter; those above may be dropped at a safepoint.
    handle_base: usize,
    stack: crate::memory::ValueStack,
    frames: FrameStack,
    /// The java.lang.Thread object, once created.
    pub java_thread: Ref,
    /// Thread.scopedValueCache().
    pub scoped_value_cache: Ref,
    /// Classes this thread is currently initialising (JVMS §5.5 recursion).
    initializing: Vec<ClassRef>,
    /// Active StackWalker walks: anchor → (frames, frames consumed).
    stack_walks: std::collections::HashMap<i64, (Vec<(MethodRef, u32)>, usize)>,
    next_walk_anchor: i64,
    /// Exception thrown by an import, until compiled code returns.
    jit_exception: Ref,
    /// Frame count at which a return instruction stops the interpreter
    /// instead of popping the frame (`run_to_return`); 0 if none.
    jit_stop: usize,
    /// Constructing a StackOverflowError (see `stack_overflow`).
    overflowing: bool,
    /// JNI state, once this thread called native library code.
    #[cfg(unix)]
    jni: Option<Box<crate::jni::JniThread>>,
}

/// What `execute` stopped for.
enum Flow {
    /// The frame set changed (call or return); re-dispatch on the top frame.
    Continue,
    /// The `jit_stop` frame executed a return instruction (see `run_to_return`).
    Return,
}

/// Operand stack slots reserved beyond max_stack (appendix arguments, wide results).
const STACK_SLACK: usize = 4;
/// Calls and backward branches between collection checks (the stop flag is
/// checked at every one).
const SAFEPOINT_INTERVAL: u32 = 10_000;

impl<'vm> Thread<'vm> {
    /// A new thread; the caller must `Safepoints::enter` to run Java code.
    pub fn new(vm: &'vm Vm) -> Self {
        let shared = vm.threads.register();
        let stack = vm.memory.take_stack().expect("no free thread stack");
        Self::with_shared(vm, shared, stack)
    }

    pub(crate) fn with_shared(
        vm: &'vm Vm,
        shared: std::sync::Arc<crate::threads::ThreadShared>,
        stack: crate::memory::ValueStack,
    ) -> Self {
        let frames = stack.frame_stack();
        shared.tlab.store(stack.thread_block() as *const _ as *mut _, std::sync::atomic::Ordering::Relaxed);
        Thread {
            vm,
            shared,
            ticks: SAFEPOINT_INTERVAL,
            handle_base: 0,
            stack,
            frames,
            java_thread: Ref::NULL,
            scoped_value_cache: Ref::NULL,
            initializing: Vec::new(),
            stack_walks: std::collections::HashMap::new(),
            next_walk_anchor: 0,
            jit_exception: Ref::NULL,
            jit_stop: 0,
            overflowing: false,
            #[cfg(unix)]
            jni: None,
        }
    }

    pub fn id(&self) -> u64 {
        self.shared.id
    }

    // --- public entry points ---------------------------------------------------

    /// Invokes a method with the given argument slots (including `this`).
    /// Returns the result slot for non-void methods.
    pub fn invoke(&mut self, method: MethodRef, args: &[u64]) -> JResult<Option<u64>> {
        debug_assert_eq!(args.len(), method.arg_slots as usize, "{method:?}");
        if let Some(intrinsic) = method.intrinsic {
            let (target, drop_trailing) = self.intrinsic_target(method, intrinsic, args)?;
            let args = if drop_trailing { &args[..args.len() - 1] } else { args };
            return self.invoke(target, args);
        }
        let result = if let Some(native) = self.bind_native(method)? {
            let base = self.shared.handles_len();
            let result = self.call_native(method, native, args);
            self.shared.truncate_handles(base);
            result
        } else if method.is_abstract() {
            Err(self.throw_vm_error(VmError::new("java/lang/AbstractMethodError", format!("{method:?}"))))
        } else {
            if !self.stack.has_room(args.len()) {
                return Err(self.stack_overflow());
            }
            self.stack.extend_from_slice(args);
            let depth = self.frames.len();
            let saved_base = std::mem::replace(&mut self.handle_base, self.shared.handles_len());
            let result = match self.push_frame(method, true) {
                Ok(()) => self.run(depth).map(|()| method.ret.map(|kind| self.pop_value(kind))),
                Err(throw) => {
                    self.stack.truncate(self.stack.len() - args.len());
                    Err(throw)
                }
            };
            self.handle_base = saved_base;
            result
        };
        // The caller may hold on to a returned reference or exception.
        match &result {
            Ok(Some(value)) if method.ret == Some(FieldKind::Ref) => crate::threads::push_handle(Ref::from_slot(*value)),
            Err(Throw(exception)) => crate::threads::push_handle(*exception),
            _ => {}
        }
        result
    }

    /// GC roots of this thread's interpreter: stack slots (conservatively) and
    /// the references the Thread struct holds.
    pub(crate) fn visit_stack_roots(&self, f: &mut dyn FnMut(u64, bool)) {
        for &slot in self.stack.iter() {
            f(slot, false);
        }
        for frame in self.frames.iter() {
            f(frame.monitor.slot(), true);
        }
        f(self.java_thread.slot(), true);
        f(self.scoped_value_cache.slot(), true);
        #[cfg(unix)]
        if let Some(jni) = &self.jni {
            jni.visit(f);
        }
    }

    /// This thread's JNI state (created on first use).
    #[cfg(unix)]
    pub(crate) fn jni_state(&mut self) -> &mut crate::jni::JniThread {
        if self.jni.is_none() {
            let me = self as *mut Thread<'vm> as *mut Thread<'static>;
            self.jni = Some(crate::jni::JniThread::new(me));
        }
        self.jni.as_mut().unwrap()
    }

    /// Finds and invokes a static method by name.
    pub fn invoke_static(&mut self, class: &str, name: &str, descriptor: &str, args: &[u64]) -> JResult<Option<u64>> {
        let class = self.load_class(class)?;
        self.ensure_initialized(class)?;
        let method = class.find_method(name, descriptor).ok_or_else(|| {
            self.throw_vm_error(VmError::new("java/lang/NoSuchMethodError", format!("{}.{}{}", class.name, name, descriptor)))
        })?;
        self.invoke(method, args)
    }

    /// Invokes a virtual method on `receiver`, selected by name and descriptor.
    pub fn invoke_virtual(&mut self, receiver: Ref, name: &str, descriptor: &str, args: &[u64]) -> JResult<Option<u64>> {
        if receiver.is_null() {
            return Err(self.throw_new("java/lang/NullPointerException", None));
        }
        let class = self.vm.class_of(receiver);
        let resolved = class
            .super_classes()
            .find_map(|c| c.find_method(name, descriptor))
            .or_else(|| class.all_interfaces().into_iter().find_map(|i| i.find_method(name, descriptor)))
            .ok_or_else(|| {
                self.throw_vm_error(VmError::new("java/lang/NoSuchMethodError", format!("{}.{}{}", class.name, name, descriptor)))
            })?;
        let method = self.vm.select_method(class, resolved).map_err(|e| self.throw_vm_error(e))?;
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(receiver.slot());
        all.extend_from_slice(args);
        self.invoke(method, &all)
    }

    pub fn load_class(&mut self, name: &str) -> JResult<ClassRef> {
        self.vm.load_class(name).map_err(|e| self.throw_vm_error(e))
    }

    /// Java frames, innermost first: (method, pc).
    /// The methods on this thread's stack, compiled frames included (for
    /// the sampler, see `threads::start_sampler`).
    pub(crate) fn sample_stack(&self, out: &mut Vec<MethodRef>) {
        self.frames.sample(out)
    }

    pub fn stack_trace(&self) -> Vec<(MethodRef, u32)> {
        self.frames.iter().rev().map(|f| (f.method, f.pc)).collect()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub(crate) fn begin_stack_walk(&mut self, frames: Vec<(MethodRef, u32)>, consumed: usize) -> i64 {
        self.next_walk_anchor += 1;
        self.stack_walks.insert(self.next_walk_anchor, (frames, consumed));
        self.next_walk_anchor
    }

    pub(crate) fn stack_walk_remaining(&self, anchor: i64) -> Option<Vec<(MethodRef, u32)>> {
        self.stack_walks.get(&anchor).map(|(frames, consumed)| frames[*consumed..].to_vec())
    }

    pub(crate) fn advance_stack_walk(&mut self, anchor: i64, count: usize) {
        if let Some((_, consumed)) = self.stack_walks.get_mut(&anchor) {
            *consumed += count;
        }
    }

    pub(crate) fn end_stack_walk(&mut self, anchor: i64) {
        self.stack_walks.remove(&anchor);
    }

    // --- exceptions --------------------------------------------------------------

    /// Creates and returns (for throwing) an exception with an optional message.
    pub fn throw_new(&mut self, class: &str, message: Option<&str>) -> Throw {
        match self.new_exception(class, message) {
            Ok(exception) => Throw(exception),
            Err(throw) => throw,
        }
    }

    /// StackOverflowError. Constructing it may itself need frames: those may
    /// use the reserve, and an overflow while constructing one gives a bare
    /// (unconstructed) error rather than recursing.
    fn stack_overflow(&mut self) -> Throw {
        if self.overflowing {
            let error = self.load_class("java/lang/StackOverflowError").and_then(|class| {
                self.vm.new_object(class).map_err(|e| self.fatal_oom(e))
            });
            return match error {
                Ok(error) => Throw(error),
                Err(throw) => throw,
            };
        }
        self.overflowing = true;
        let error = self.throw_new("java/lang/StackOverflowError", None);
        self.overflowing = false;
        error
    }

    pub fn throw_vm_error(&mut self, error: VmError) -> Throw {
        let message = error.message;
        self.throw_new(error.class, Some(&message))
    }

    fn new_exception(&mut self, class: &str, message: Option<&str>) -> JResult<Ref> {
        let class = self.load_class(class)?;
        self.ensure_initialized(class)?;
        let exception = self.vm.new_object(class).map_err(|e| self.fatal_oom(e))?;
        match message {
            Some(message) => {
                let message = self.vm.new_string(message).map_err(|e| self.fatal_oom(e))?;
                let init = class.find_method("<init>", "(Ljava/lang/String;)V").expect("exception(String) constructor");
                self.invoke(init, &[exception.slot(), message.slot()])?;
            }
            None => {
                let init = class.find_method("<init>", "()V").expect("exception() constructor");
                self.invoke(init, &[exception.slot()])?;
            }
        }
        Ok(exception)
    }

    fn fatal_oom(&self, error: VmError) -> Throw {
        panic!("cannot allocate exception: {error}");
    }

    // --- class initialisation (JVMS §5.5) ------------------------------------------

    pub fn ensure_initialized(&mut self, class: ClassRef) -> JResult<()> {
        if class.initialized.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        loop {
            let mut state = class.init_state.lock().unwrap();
            match *state {
                InitState::Initialized => {
                    class.initialized.store(true, std::sync::atomic::Ordering::Release);
                    return Ok(());
                }
                InitState::InProgress if class.init_thread.load(std::sync::atomic::Ordering::Relaxed) == self.shared.id => {
                    return Ok(()); // recursive request by the initialising thread
                }
                InitState::InProgress => {
                    // Another thread is initialising the class: wait for it (JVMS §5.5 step 2).
                    drop(state);
                    self.blocking(|| std::thread::sleep(std::time::Duration::from_micros(50)));
                }
                InitState::Erroneous => {
                    drop(state);
                    let message = format!("Could not initialize class {}", class.java_name());
                    return Err(self.throw_new("java/lang/NoClassDefFoundError", Some(&message)));
                }
                InitState::Uninitialized => {
                    // Claimed under the lock: only one thread initialises.
                    *state = InitState::InProgress;
                    class.init_thread.store(self.shared.id, std::sync::atomic::Ordering::Relaxed);
                    drop(state);
                    return self.initialize(class);
                }
            }
        }
    }

    fn initialize(&mut self, class: ClassRef) -> JResult<()> {
        self.initializing.push(class);
        let result = self.run_initializer(class);
        self.initializing.pop();
        match result {
            Ok(()) => {
                crate::native::class_initialized(self, class);
                *class.init_state.lock().unwrap() = InitState::Initialized;
                class.initialized.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            }
            Err(Throw(exception)) => {
                *class.init_state.lock().unwrap() = InitState::Erroneous;
                let error_class = self.load_class("java/lang/Error")?;
                if self.vm.class_of(exception).is_subclass_of(error_class) {
                    return Err(Throw(exception));
                }
                let wrapper_class = self.load_class("java/lang/ExceptionInInitializerError")?;
                self.ensure_initialized(wrapper_class)?;
                let wrapper = self.vm.new_object(wrapper_class).map_err(|e| self.fatal_oom(e))?;
                let init = wrapper_class.find_method("<init>", "(Ljava/lang/Throwable;)V").unwrap();
                self.invoke(init, &[wrapper.slot(), exception.slot()])?;
                Err(Throw(wrapper))
            }
        }
    }

    fn run_initializer(&mut self, class: ClassRef) -> JResult<()> {
        if !class.is_interface() {
            if let Some(super_class) = class.super_class {
                self.ensure_initialized(super_class)?;
            }
            // Superinterfaces, direct or indirect, that declare default
            // methods are initialised too (JVMS §5.5; their own
            // superinterfaces first, as HotSpot does).
            self.initialize_default_interfaces(class)?;
        }
        self.set_constant_values(class)?;
        if let Some(clinit) = class.find_method("<clinit>", "()V") {
            self.invoke(clinit, &[])?;
        }
        Ok(())
    }

    fn initialize_default_interfaces(&mut self, class: ClassRef) -> JResult<()> {
        for &iface in &class.interfaces {
            self.initialize_default_interfaces(iface)?;
            if iface.methods.iter().any(|m| !m.is_static() && !m.is_abstract()) {
                self.ensure_initialized(iface)?;
            }
        }
        Ok(())
    }

    fn set_constant_values(&mut self, class: ClassRef) -> JResult<()> {
        let statics = class.statics.get();
        for field in class.fields.iter().filter(|f| f.is_static()) {
            let Some(index) = field.constant_value else { continue };
            let heap = &self.vm.heap;
            match class.cp().get(index) {
                Ok(&Constant::Integer(v)) => match field.kind {
                    FieldKind::Boolean | FieldKind::Byte => heap.write(statics, field.offset, v as u8),
                    FieldKind::Char | FieldKind::Short => heap.write(statics, field.offset, v as u16),
                    _ => heap.write(statics, field.offset, v),
                },
                Ok(&Constant::Float(v)) => heap.write(statics, field.offset, v),
                Ok(&Constant::Long(v)) => heap.write(statics, field.offset, v),
                Ok(&Constant::Double(v)) => heap.write(statics, field.offset, v),
                Ok(&Constant::String(s)) => {
                    let units = classfile::mutf8::decode_utf16(class.cp().utf8_bytes(s).unwrap()).unwrap_or_default();
                    let string = self.vm.intern_utf16(&units).map_err(|e| self.throw_vm_error(e))?;
                    heap.write(statics, field.offset, string.0);
                }
                _ => {}
            }
        }
        Ok(())
    }

    // --- natives ----------------------------------------------------------------

    fn bind_native(&mut self, method: MethodRef) -> JResult<Option<NativeFn>> {
        if let Some(bound) = method.native.get() {
            return Ok(*bound);
        }
        let found = self.vm.native(&method.native_key());
        #[cfg(unix)]
        if method.is_native() && found.is_none() {
            if let Some(code) = self.vm.jni.find_native(method) {
                let bound: &'static crate::jni::JniMethod = Box::leak(Box::new(crate::jni::JniMethod::new(method, code)));
                let _ = method.jni.set(Some(bound));
                let _ = method.native.set(Some(jni_trampoline));
                return Ok(Some(jni_trampoline));
            }
        }
        if method.is_native() && found.is_none() {
            let message = format!("{} (native method not implemented)", method.native_key());
            return Err(self.throw_new("java/lang/UnsatisfiedLinkError", Some(&message)));
        }
        let _ = method.native.set(found);
        Ok(found)
    }

    fn call_native(&mut self, method: MethodRef, native: NativeFn, args: &[u64]) -> JResult<Option<u64>> {
        if method.is_static() {
            self.ensure_initialized(method.class())?;
        }
        #[cfg(unix)]
        if native as usize == jni_trampoline as *const () as usize {
            let jni = method.jni.get().copied().flatten().expect("JNI-bound method");
            return crate::jni::call(self, method, jni, args);
        }
        native(self, args)
    }

    // --- frames -------------------------------------------------------------------

    /// Pushes a frame for `method`, whose arguments are on top of the stack.
    fn push_frame(&mut self, method: MethodRef, entry: bool) -> JResult<()> {
        let code = method.code.as_ref().expect("method with code");
        let locals = self.stack.len() - method.arg_slots as usize;
        // Locals, then max_stack operand slots (plus slack for appendix
        // arguments); the unchecked push/pop below rely on this room.
        // Frame records grow down from the end of the same region.
        let needed = locals + code.max_locals as usize + code.max_stack as usize + STACK_SLACK;
        let reserve = if self.overflowing { 0 } else { crate::jit::compile::OVERFLOW_RESERVE };
        if needed * 8 + FrameStack::bytes(self.frames.len() + 1) + reserve > self.stack.capacity() * 8 {
            return Err(self.stack_overflow());
        }
        let monitor = if method.access_flags & classfile::access::SYNCHRONIZED != 0 {
            let obj = if method.is_static() { self.vm.mirror(method.class()) } else { Ref::from_slot(self.stack[locals]) };
            self.monitor_enter(obj)?;
            obj
        } else {
            Ref::NULL
        };
        self.stack.resize(locals + code.max_locals as usize, 0);
        // A hotness counter, not a precise count: avoid a locked add.
        let count = &method.invocations;
        count.store(count.load(std::sync::atomic::Ordering::Relaxed) + 1, std::sync::atomic::Ordering::Relaxed);
        if let Some(stats) = crate::jit::stats::get() {
            stats.interpreted_frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if method.jit.current().is_none() {
                let failed = matches!(method.jit.code.get(), Some(None));
                stats.uncompiled.lock().unwrap().entry(method as *const _ as usize).or_insert((0, method, failed)).0 += 1;
            }
        }
        self.frames.push(Frame { method, pc: 0, locals: locals as u32, entry, monitor });
        self.safepoint();
        Ok(())
    }

    #[inline]
    fn safepoint(&mut self) {
        self.ticks -= 1;
        if self.ticks == 0 || self.vm.threads.safepoints.stopping() {
            self.ticks = SAFEPOINT_INTERVAL;
            self.shared.truncate_handles(self.handle_base);
            if self.vm.heap.should_collect() {
                self.vm.collect();
            }
            self.yield_now();
        }
    }

    /// Pops the top frame, leaving its return value (if any) on the caller's
    /// operand stack, and advances the caller past its call instruction.
    fn pop_frame(&mut self, ret: Option<(u64, bool)>) {
        let frame = self.frames.pop();
        if !frame.monitor.is_null() {
            let _ = self.monitor_exit(frame.monitor);
        }
        self.stack.truncate(frame.locals as usize);
        if let Some((value, wide)) = ret {
            self.stack.push(value);
            if wide {
                self.stack.push(0);
            }
        }
        if !frame.entry {
            self.advance_caller();
        }
    }

    /// Moves the top frame past the call instruction it is suspended at.
    fn advance_caller(&mut self) {
        let caller = self.frames.top();
        let code = &caller.method.code.as_ref().unwrap().code;
        let length = match code[caller.pc as usize] {
            INVOKEINTERFACE | INVOKEDYNAMIC => 5,
            _ => 3,
        };
        self.frames.set_top_pc(caller.pc + length);
    }

    fn pop_value(&mut self, kind: FieldKind) -> u64 {
        if kind.is_wide() {
            self.stack.pop();
        }
        self.stack.pop().unwrap()
    }

    /// Runs until the frame stack is back to `base_depth` frames.
    fn run(&mut self, base_depth: usize) -> JResult<()> {
        while self.frames.len() > base_depth {
            let flow = match self.compiled_entry() {
                Some((code, block)) => self.run_compiled(code, block),
                None => self.execute::<false>(),
            };
            match flow {
                Ok(Flow::Continue) => {}
                Ok(Flow::Return) => unreachable!("return to compiled code outside run_to_return"),
                Err(Throw(exception)) => {
                    if !self.handle_exception(exception, base_depth) {
                        return Err(Throw(exception));
                    }
                }
            }
        }
        Ok(())
    }

    /// Unwinds to a matching handler. Returns false if the exception escapes
    /// past `base_depth`.
    fn handle_exception(&mut self, exception: Ref, base_depth: usize) -> bool {
        // Resolving catch types may run Java code (and collect garbage).
        crate::threads::push_handle(exception);
        while self.frames.len() > base_depth {
            let frame = self.frames.top();
            let method = frame.method;
            let code = method.code.as_ref().unwrap();
            for handler in &code.exception_table {
                if !(handler.start_pc as u32 <= frame.pc && frame.pc < handler.end_pc as u32) {
                    continue;
                }
                if handler.catch_type != 0 {
                    let Ok(catch_class) = self.resolve_class(method.class(), handler.catch_type) else { continue };
                    if !self.vm.class_of(exception).is_subclass_of(catch_class) {
                        continue;
                    }
                }
                self.frames.set_top_pc(handler.handler_pc as u32);
                self.stack.truncate(frame.locals as usize + code.max_locals as usize);
                self.stack.push(exception.slot());
                return true;
            }
            let frame = self.frames.pop();
            if !frame.monitor.is_null() {
                let _ = self.monitor_exit(frame.monitor);
            }
            self.stack.truncate(frame.locals as usize);
        }
        false
    }

    // --- the interpreter loop -------------------------------------------------------

    #[inline(always)]
    fn pop(&mut self) -> u64 {
        debug_assert!(!self.stack.is_empty(), "operand stack underflow");
        // SAFETY: verified bytecode never pops an empty operand stack.
        unsafe {
            let len = self.stack.len() - 1;
            self.stack.set_len(len);
            *self.stack.as_ptr().add(len)
        }
    }
    #[inline(always)]
    fn push(&mut self, v: u64) {
        debug_assert!(self.stack.len() < self.stack.capacity(), "operand stack overflow");
        // SAFETY: push_frame reserved max_stack (+ slack) slots for this frame.
        unsafe {
            let len = self.stack.len();
            *self.stack.as_mut_ptr().add(len) = v;
            self.stack.set_len(len + 1);
        }
    }
    #[inline(always)]
    fn local(&self, index: usize) -> u64 {
        debug_assert!(index < self.stack.len());
        // SAFETY: local indices are < max_locals, which push_frame allocated.
        unsafe { *self.stack.get_unchecked(index) }
    }
    #[inline(always)]
    fn set_local(&mut self, index: usize, value: u64) {
        debug_assert!(index < self.stack.len());
        // SAFETY: as for `local`.
        unsafe { *self.stack.get_unchecked_mut(index) = value }
    }
    #[inline]
    fn pop_i(&mut self) -> i32 {
        self.pop() as i32
    }
    #[inline]
    fn push_i(&mut self, v: i32) {
        self.push(v as u32 as u64)
    }
    #[inline]
    fn pop_l(&mut self) -> i64 {
        self.pop();
        self.pop() as i64
    }
    #[inline]
    fn push_l(&mut self, v: i64) {
        self.push(v as u64);
        self.push(0);
    }
    #[inline]
    fn pop_f(&mut self) -> f32 {
        f32::from_bits(self.pop() as u32)
    }
    #[inline]
    fn push_f(&mut self, v: f32) {
        self.push(v.to_bits() as u64)
    }
    #[inline]
    fn pop_d(&mut self) -> f64 {
        f64::from_bits(self.pop_l() as u64)
    }
    #[inline]
    fn push_d(&mut self, v: f64) {
        self.push_l(v.to_bits() as i64)
    }
    #[inline]
    fn pop_ref(&mut self) -> Ref {
        Ref::from_slot(self.pop())
    }
    #[inline]
    fn push_ref(&mut self, r: Ref) {
        self.push(r.slot())
    }

    /// RJAVA_GC_VERIFY failure: report where a dangling reference was used.
    #[cold]
    fn bad_reference(&self, r: Ref) -> ! {
        let trace: Vec<String> = self
            .stack_trace()
            .iter()
            .take(30)
            .map(|(m, pc)| format!("    {m:?} @{pc} (line {:?})", m.line_number(*pc)))
            .collect();
        panic!("dangling reference {r:?} used on thread #{}:\n{}", self.shared.id, trace.join("\n"));
    }

    fn null_pointer(&mut self) -> Throw {
        self.throw_new("java/lang/NullPointerException", None)
    }

    fn check_array(&mut self, array: Ref, index: i32) -> JResult<()> {
        if array.is_null() {
            return Err(self.null_pointer());
        }
        if crate::vm::verify_enabled() && !self.vm.heap.is_object(array) {
            self.bad_reference(array);
        }
        let length = self.vm.heap.array_length(array);
        if index < 0 || index >= length {
            let message = format!("Index {index} out of bounds for length {length}");
            return Err(self.throw_new("java/lang/ArrayIndexOutOfBoundsException", Some(&message)));
        }
        Ok(())
    }

    /// Executes instructions of the top frame until a call, return or throw
    /// (or a loop that compiled code should run). With SINGLE, executes just
    /// one instruction, which must not be a call, return or branch.
    fn execute<const SINGLE: bool>(&mut self) -> JResult<Flow> {
        let frame = self.frames.top();
        let method = frame.method;
        let class = method.class();
        let code: &'static [u8] = &method.code.as_ref().unwrap().code;
        let locals = frame.locals as usize;
        let mut pc = frame.pc as usize;
        let vm: &'vm Vm = self.vm;
        let heap = &vm.heap;

        macro_rules! save_pc {
            () => {
                self.frames.set_top_pc(pc as u32)
            };
        }
        // Raise an exception at the current instruction.
        macro_rules! throw {
            ($e:expr) => {{
                save_pc!();
                return Err($e);
            }};
        }
        macro_rules! try_vm {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => {
                        save_pc!();
                        return Err(self.throw_vm_error(e));
                    }
                }
            };
        }
        // RJAVA_GC_VERIFY: every dereferenced reference must be an allocated object.
        macro_rules! check_ref {
            ($r:expr) => {
                if crate::vm::verify_enabled() && !heap.is_object($r) {
                    save_pc!();
                    self.bad_reference($r);
                }
            };
        }
        // Resolution with a fast path for constants already in the cache.
        macro_rules! field_ref {
            ($index:expr) => {{
                let index = $index;
                match class.cp_cache[index as usize].get() {
                    Some(crate::class::Resolved::Field(field)) => *field,
                    _ => {
                        save_pc!();
                        self.resolve_field(class, index)?
                    }
                }
            }};
        }
        macro_rules! method_ref {
            ($index:expr) => {{
                let index = $index;
                match class.cp_cache[index as usize].get() {
                    Some(crate::class::Resolved::Method(method)) => *method,
                    _ => {
                        save_pc!();
                        self.resolve_method(class, index)?
                    }
                }
            }};
        }
        macro_rules! class_ref {
            ($index:expr) => {{
                let index = $index;
                match class.cp_cache[index as usize].get() {
                    Some(crate::class::Resolved::Class(target)) => *target,
                    _ => {
                        save_pc!();
                        self.resolve_class(class, index)?
                    }
                }
            }};
        }
        macro_rules! u1 {
            ($at:expr) => {
                code[$at] as usize
            };
        }
        macro_rules! u2 {
            ($at:expr) => {
                u16::from_be_bytes([code[$at], code[$at + 1]])
            };
        }
        macro_rules! i2 {
            ($at:expr) => {
                i16::from_be_bytes([code[$at], code[$at + 1]]) as isize
            };
        }
        macro_rules! i4 {
            ($at:expr) => {
                i32::from_be_bytes([code[$at], code[$at + 1], code[$at + 2], code[$at + 3]])
            };
        }
        macro_rules! branch {
            ($cond:expr) => {
                if $cond {
                    let offset = i2!(pc + 1);
                    pc = (pc as isize + offset) as usize;
                    if offset < 0 {
                        save_pc!();
                        self.safepoint();
                        if vm.jit.is_some() && method.jit.on_backedge(vm, method) {
                            return Ok(Flow::Continue);
                        }
                    }
                } else {
                    pc += 3;
                }
            };
        }
        macro_rules! array_load {
            ($t:ty, $push:ident, $conv:expr) => {{
                let index = self.pop_i();
                let array = self.pop_ref();
                save_pc!();
                self.check_array(array, index)?;
                let size = std::mem::size_of::<$t>() as u32;
                let v: $t = heap.read(array, ARRAY_BASE_OFFSET + index as u32 * size);
                self.$push($conv(v));
                pc += 1;
            }};
        }
        macro_rules! array_store {
            ($t:ty, $pop:ident, $conv:expr) => {{
                let v = self.$pop();
                let index = self.pop_i();
                let array = self.pop_ref();
                save_pc!();
                self.check_array(array, index)?;
                let size = std::mem::size_of::<$t>() as u32;
                heap.write::<$t>(array, ARRAY_BASE_OFFSET + index as u32 * size, $conv(v));
                pc += 1;
            }};
        }
        macro_rules! binop {
            ($pop:ident, $push:ident, |$a:ident, $b:ident| $e:expr) => {{
                let $b = self.$pop();
                let $a = self.$pop();
                self.$push($e);
                pc += 1;
            }};
        }

        let start_pc = pc;
        loop {
            if SINGLE && pc != start_pc {
                save_pc!();
                return Ok(Flow::Continue);
            }
            let op = code[pc];
            match op {
                NOP => pc += 1,
                ACONST_NULL => {
                    self.push_ref(Ref::NULL);
                    pc += 1
                }
                ICONST_M1..=ICONST_5 => {
                    self.push_i(op as i32 - ICONST_0 as i32);
                    pc += 1
                }
                LCONST_0 | LCONST_1 => {
                    self.push_l((op - LCONST_0) as i64);
                    pc += 1
                }
                FCONST_0..=FCONST_2 => {
                    self.push_f((op - FCONST_0) as f32);
                    pc += 1
                }
                DCONST_0 | DCONST_1 => {
                    self.push_d((op - DCONST_0) as f64);
                    pc += 1
                }
                BIPUSH => {
                    self.push_i(code[pc + 1] as i8 as i32);
                    pc += 2
                }
                SIPUSH => {
                    self.push_i(i2!(pc + 1) as i32);
                    pc += 3
                }
                LDC | LDC_W | LDC2_W => {
                    let (index, len) = if op == LDC { (u1!(pc + 1) as u16, 2) } else { (u2!(pc + 1), 3) };
                    save_pc!();
                    self.load_constant(class, index)?;
                    pc += len;
                }
                ILOAD | FLOAD | ALOAD => {
                    self.push(self.local(locals + u1!(pc + 1)));
                    pc += 2
                }
                LLOAD | DLOAD => {
                    self.push(self.local(locals + u1!(pc + 1)));
                    self.push(0);
                    pc += 2
                }
                ILOAD_0..=ILOAD_3 => {
                    self.push(self.local(locals + (op - ILOAD_0) as usize));
                    pc += 1
                }
                LLOAD_0..=LLOAD_3 => {
                    self.push(self.local(locals + (op - LLOAD_0) as usize));
                    self.push(0);
                    pc += 1
                }
                FLOAD_0..=FLOAD_3 => {
                    self.push(self.local(locals + (op - FLOAD_0) as usize));
                    pc += 1
                }
                DLOAD_0..=DLOAD_3 => {
                    self.push(self.local(locals + (op - DLOAD_0) as usize));
                    self.push(0);
                    pc += 1
                }
                ALOAD_0..=ALOAD_3 => {
                    self.push(self.local(locals + (op - ALOAD_0) as usize));
                    pc += 1
                }
                IALOAD => array_load!(i32, push_i, |v| v),
                LALOAD => array_load!(i64, push_l, |v| v),
                FALOAD => array_load!(f32, push_f, |v| v),
                DALOAD => array_load!(f64, push_d, |v| v),
                AALOAD => array_load!(u32, push_ref, Ref),
                BALOAD => array_load!(i8, push_i, |v: i8| v as i32),
                CALOAD => array_load!(u16, push_i, |v: u16| v as i32),
                SALOAD => array_load!(i16, push_i, |v: i16| v as i32),
                ISTORE | FSTORE | ASTORE => {
                    let v = self.pop();
                    self.set_local(locals + u1!(pc + 1), v);
                    pc += 2
                }
                LSTORE | DSTORE => {
                    self.pop();
                    let v = self.pop();
                    self.set_local(locals + u1!(pc + 1), v);
                    pc += 2
                }
                ISTORE_0..=ISTORE_3 => {
                    let v = self.pop();
                    self.set_local(locals + (op - ISTORE_0) as usize, v);
                    pc += 1
                }
                LSTORE_0..=LSTORE_3 => {
                    self.pop();
                    let v = self.pop();
                    self.set_local(locals + (op - LSTORE_0) as usize, v);
                    pc += 1
                }
                FSTORE_0..=FSTORE_3 => {
                    let v = self.pop();
                    self.set_local(locals + (op - FSTORE_0) as usize, v);
                    pc += 1
                }
                DSTORE_0..=DSTORE_3 => {
                    self.pop();
                    let v = self.pop();
                    self.set_local(locals + (op - DSTORE_0) as usize, v);
                    pc += 1
                }
                ASTORE_0..=ASTORE_3 => {
                    let v = self.pop();
                    self.set_local(locals + (op - ASTORE_0) as usize, v);
                    pc += 1
                }
                IASTORE => array_store!(i32, pop_i, |v| v),
                LASTORE => array_store!(i64, pop_l, |v| v),
                FASTORE => array_store!(f32, pop_f, |v| v),
                DASTORE => array_store!(f64, pop_d, |v| v),
                AASTORE => {
                    let value = self.pop_ref();
                    let index = self.pop_i();
                    let array = self.pop_ref();
                    save_pc!();
                    self.check_array(array, index)?;
                    if !value.is_null() {
                        let component = self.vm.class_of(array).component().unwrap();
                        let value_class = self.vm.class_of(value);
                        if !value_class.is_subclass_of(component) {
                            throw!(self.throw_new("java/lang/ArrayStoreException", Some(&value_class.java_name())));
                        }
                    }
                    heap.write(array, ARRAY_BASE_OFFSET + index as u32 * 4, value.0);
                    pc += 1
                }
                BASTORE => {
                    let v = self.pop_i();
                    let index = self.pop_i();
                    let array = self.pop_ref();
                    save_pc!();
                    self.check_array(array, index)?;
                    // Shared by byte[] and boolean[]; booleans are masked to 0/1.
                    let is_boolean = self.vm.class_of(array).element_kind() == Some(FieldKind::Boolean);
                    let v = if is_boolean { (v & 1) as u8 } else { v as u8 };
                    heap.write(array, ARRAY_BASE_OFFSET + index as u32, v);
                    pc += 1
                }
                CASTORE => array_store!(u16, pop_i, |v: i32| v as u16),
                SASTORE => array_store!(i16, pop_i, |v: i32| v as i16),
                POP => {
                    self.pop();
                    pc += 1
                }
                POP2 => {
                    self.pop();
                    self.pop();
                    pc += 1
                }
                DUP => {
                    let v = *self.stack.last().unwrap();
                    self.push(v);
                    pc += 1
                }
                DUP_X1 => {
                    let v1 = self.pop();
                    let v2 = self.pop();
                    self.push(v1);
                    self.push(v2);
                    self.push(v1);
                    pc += 1
                }
                DUP_X2 => {
                    let v1 = self.pop();
                    let v2 = self.pop();
                    let v3 = self.pop();
                    self.push(v1);
                    self.push(v3);
                    self.push(v2);
                    self.push(v1);
                    pc += 1
                }
                DUP2 => {
                    let n = self.stack.len();
                    let (v2, v1) = (self.stack[n - 2], self.stack[n - 1]);
                    self.push(v2);
                    self.push(v1);
                    pc += 1
                }
                DUP2_X1 => {
                    let v1 = self.pop();
                    let v2 = self.pop();
                    let v3 = self.pop();
                    self.push(v2);
                    self.push(v1);
                    self.push(v3);
                    self.push(v2);
                    self.push(v1);
                    pc += 1
                }
                DUP2_X2 => {
                    let v1 = self.pop();
                    let v2 = self.pop();
                    let v3 = self.pop();
                    let v4 = self.pop();
                    self.push(v2);
                    self.push(v1);
                    self.push(v4);
                    self.push(v3);
                    self.push(v2);
                    self.push(v1);
                    pc += 1
                }
                SWAP => {
                    let v1 = self.pop();
                    let v2 = self.pop();
                    self.push(v1);
                    self.push(v2);
                    pc += 1
                }
                IADD => binop!(pop_i, push_i, |a, b| a.wrapping_add(b)),
                LADD => binop!(pop_l, push_l, |a, b| a.wrapping_add(b)),
                FADD => binop!(pop_f, push_f, |a, b| a + b),
                DADD => binop!(pop_d, push_d, |a, b| a + b),
                ISUB => binop!(pop_i, push_i, |a, b| a.wrapping_sub(b)),
                LSUB => binop!(pop_l, push_l, |a, b| a.wrapping_sub(b)),
                FSUB => binop!(pop_f, push_f, |a, b| a - b),
                DSUB => binop!(pop_d, push_d, |a, b| a - b),
                IMUL => binop!(pop_i, push_i, |a, b| a.wrapping_mul(b)),
                LMUL => binop!(pop_l, push_l, |a, b| a.wrapping_mul(b)),
                FMUL => binop!(pop_f, push_f, |a, b| a * b),
                DMUL => binop!(pop_d, push_d, |a, b| a * b),
                IDIV | IREM => {
                    let b = self.pop_i();
                    let a = self.pop_i();
                    if b == 0 {
                        throw!(self.throw_new("java/lang/ArithmeticException", Some("/ by zero")));
                    }
                    self.push_i(if op == IDIV { a.wrapping_div(b) } else { a.wrapping_rem(b) });
                    pc += 1
                }
                LDIV | LREM => {
                    let b = self.pop_l();
                    let a = self.pop_l();
                    if b == 0 {
                        throw!(self.throw_new("java/lang/ArithmeticException", Some("/ by zero")));
                    }
                    self.push_l(if op == LDIV { a.wrapping_div(b) } else { a.wrapping_rem(b) });
                    pc += 1
                }
                FDIV => binop!(pop_f, push_f, |a, b| a / b),
                DDIV => binop!(pop_d, push_d, |a, b| a / b),
                FREM => binop!(pop_f, push_f, |a, b| a % b),
                DREM => binop!(pop_d, push_d, |a, b| a % b),
                INEG => {
                    let v = self.pop_i();
                    self.push_i(v.wrapping_neg());
                    pc += 1
                }
                LNEG => {
                    let v = self.pop_l();
                    self.push_l(v.wrapping_neg());
                    pc += 1
                }
                FNEG => {
                    let v = self.pop_f();
                    self.push_f(-v);
                    pc += 1
                }
                DNEG => {
                    let v = self.pop_d();
                    self.push_d(-v);
                    pc += 1
                }
                ISHL => binop!(pop_i, push_i, |a, b| a.wrapping_shl(b as u32 & 31)),
                ISHR => binop!(pop_i, push_i, |a, b| a.wrapping_shr(b as u32 & 31)),
                IUSHR => binop!(pop_i, push_i, |a, b| ((a as u32) >> (b as u32 & 31)) as i32),
                LSHL | LSHR | LUSHR => {
                    let shift = self.pop_i() as u32 & 63;
                    let a = self.pop_l();
                    self.push_l(match op {
                        LSHL => a.wrapping_shl(shift),
                        LSHR => a.wrapping_shr(shift),
                        _ => ((a as u64) >> shift) as i64,
                    });
                    pc += 1
                }
                IAND => binop!(pop_i, push_i, |a, b| a & b),
                LAND => binop!(pop_l, push_l, |a, b| a & b),
                IOR => binop!(pop_i, push_i, |a, b| a | b),
                LOR => binop!(pop_l, push_l, |a, b| a | b),
                IXOR => binop!(pop_i, push_i, |a, b| a ^ b),
                LXOR => binop!(pop_l, push_l, |a, b| a ^ b),
                IINC => {
                    let slot = locals + u1!(pc + 1);
                    let v = (self.local(slot) as i32).wrapping_add(code[pc + 2] as i8 as i32);
                    self.set_local(slot, v as u32 as u64);
                    pc += 3
                }
                I2L => {
                    let v = self.pop_i();
                    self.push_l(v as i64);
                    pc += 1
                }
                I2F => {
                    let v = self.pop_i();
                    self.push_f(v as f32);
                    pc += 1
                }
                I2D => {
                    let v = self.pop_i();
                    self.push_d(v as f64);
                    pc += 1
                }
                L2I => {
                    let v = self.pop_l();
                    self.push_i(v as i32);
                    pc += 1
                }
                L2F => {
                    let v = self.pop_l();
                    self.push_f(v as f32);
                    pc += 1
                }
                L2D => {
                    let v = self.pop_l();
                    self.push_d(v as f64);
                    pc += 1
                }
                // Rust's float→int `as` casts saturate and map NaN to 0, exactly as Java.
                F2I => {
                    let v = self.pop_f();
                    self.push_i(v as i32);
                    pc += 1
                }
                F2L => {
                    let v = self.pop_f();
                    self.push_l(v as i64);
                    pc += 1
                }
                F2D => {
                    let v = self.pop_f();
                    self.push_d(v as f64);
                    pc += 1
                }
                D2I => {
                    let v = self.pop_d();
                    self.push_i(v as i32);
                    pc += 1
                }
                D2L => {
                    let v = self.pop_d();
                    self.push_l(v as i64);
                    pc += 1
                }
                D2F => {
                    let v = self.pop_d();
                    self.push_f(v as f32);
                    pc += 1
                }
                I2B => {
                    let v = self.pop_i();
                    self.push_i(v as i8 as i32);
                    pc += 1
                }
                I2C => {
                    let v = self.pop_i();
                    self.push_i(v as u16 as i32);
                    pc += 1
                }
                I2S => {
                    let v = self.pop_i();
                    self.push_i(v as i16 as i32);
                    pc += 1
                }
                LCMP => {
                    let b = self.pop_l();
                    let a = self.pop_l();
                    self.push_i(a.cmp(&b) as i32);
                    pc += 1
                }
                FCMPL | FCMPG => {
                    let b = self.pop_f();
                    let a = self.pop_f();
                    let nan = if op == FCMPG { 1 } else { -1 };
                    self.push_i(a.partial_cmp(&b).map_or(nan, |o| o as i32));
                    pc += 1
                }
                DCMPL | DCMPG => {
                    let b = self.pop_d();
                    let a = self.pop_d();
                    let nan = if op == DCMPG { 1 } else { -1 };
                    self.push_i(a.partial_cmp(&b).map_or(nan, |o| o as i32));
                    pc += 1
                }
                IFEQ => {
                    let v = self.pop_i();
                    branch!(v == 0)
                }
                IFNE => {
                    let v = self.pop_i();
                    branch!(v != 0)
                }
                IFLT => {
                    let v = self.pop_i();
                    branch!(v < 0)
                }
                IFGE => {
                    let v = self.pop_i();
                    branch!(v >= 0)
                }
                IFGT => {
                    let v = self.pop_i();
                    branch!(v > 0)
                }
                IFLE => {
                    let v = self.pop_i();
                    branch!(v <= 0)
                }
                IF_ICMPEQ..=IF_ICMPLE => {
                    let b = self.pop_i();
                    let a = self.pop_i();
                    branch!(match op {
                        IF_ICMPEQ => a == b,
                        IF_ICMPNE => a != b,
                        IF_ICMPLT => a < b,
                        IF_ICMPGE => a >= b,
                        IF_ICMPGT => a > b,
                        _ => a <= b,
                    })
                }
                IF_ACMPEQ | IF_ACMPNE => {
                    let b = self.pop();
                    let a = self.pop();
                    branch!((a == b) == (op == IF_ACMPEQ))
                }
                GOTO => {
                    let offset = i2!(pc + 1);
                    pc = (pc as isize + offset) as usize;
                    if offset < 0 {
                        save_pc!();
                        self.safepoint();
                        if vm.jit.is_some() && method.jit.on_backedge(vm, method) {
                            return Ok(Flow::Continue);
                        }
                    }
                }
                GOTO_W => pc = (pc as isize + i4!(pc + 1) as isize) as usize,
                JSR | JSR_W | RET => {
                    throw!(self.throw_new("java/lang/VerifyError", Some("jsr/ret are not supported")));
                }
                TABLESWITCH => {
                    let base = (pc + 4) & !3;
                    let default = i4!(base);
                    let low = i4!(base + 4);
                    let high = i4!(base + 8);
                    let key = self.pop_i();
                    let offset = if key < low || key > high { default } else { i4!(base + 12 + 4 * (key - low) as usize) };
                    pc = (pc as isize + offset as isize) as usize;
                }
                LOOKUPSWITCH => {
                    let base = (pc + 4) & !3;
                    let default = i4!(base);
                    let pairs = i4!(base + 4) as usize;
                    let key = self.pop_i();
                    let mut offset = default;
                    // Keys are sorted; binary search.
                    let (mut lo, mut hi) = (0usize, pairs);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        let k = i4!(base + 8 + mid * 8);
                        if k == key {
                            offset = i4!(base + 12 + mid * 8);
                            break;
                        } else if k < key {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    pc = (pc as isize + offset as isize) as usize;
                }
                IRETURN | FRETURN | ARETURN | LRETURN | DRETURN | RETURN => {
                    let wide = matches!(op, LRETURN | DRETURN);
                    if wide {
                        self.pop();
                    }
                    let v = if op == RETURN { None } else { Some((self.pop(), wide)) };
                    if self.frames.len() == self.jit_stop {
                        // Compiled code called this frame and does the return.
                        unsafe { self.stack.set_len(locals + v.map_or(0, |_| 1 + wide as usize)) };
                        if let Some((v, _)) = v {
                            self.set_local(locals, v);
                        }
                        save_pc!();
                        return Ok(Flow::Return);
                    }
                    self.pop_frame(v);
                    return Ok(Flow::Continue);
                }
                GETSTATIC | PUTSTATIC => {
                    let field = field_ref!(u2!(pc + 1));
                    if !field.is_static {
                        throw!(self.throw_new("java/lang/IncompatibleClassChangeError", Some("expected static field")));
                    }
                    if !std::ptr::eq(field.holder, class) {
                        save_pc!();
                        self.ensure_initialized(field.holder)?;
                    }
                    let statics = field.holder.statics.get();
                    if op == GETSTATIC {
                        let v = read_field(heap, statics, field.offset, field.kind);
                        volatile_fence(field.volatile);
                        self.push(v);
                        if field.kind.is_wide() {
                            self.push(0);
                        }
                    } else {
                        let v = self.pop_value(field.kind);
                        write_field(heap, statics, field.offset, field.kind, v);
                        volatile_fence(field.volatile);
                    }
                    pc += 3
                }
                GETFIELD => {
                    let field = field_ref!(u2!(pc + 1));
                    let obj = self.pop_ref();
                    if obj.is_null() {
                        throw!(self.null_pointer());
                    }
                    check_ref!(obj);
                    let v = read_field(heap, obj, field.offset, field.kind);
                    volatile_fence(field.volatile);
                    self.push(v);
                    if field.kind.is_wide() {
                        self.push(0);
                    }
                    pc += 3
                }
                PUTFIELD => {
                    let field = field_ref!(u2!(pc + 1));
                    let v = self.pop_value(field.kind);
                    let obj = self.pop_ref();
                    if obj.is_null() {
                        throw!(self.null_pointer());
                    }
                    check_ref!(obj);
                    write_field(heap, obj, field.offset, field.kind, v);
                    volatile_fence(field.volatile);
                    pc += 3
                }
                INVOKEVIRTUAL | INVOKESPECIAL | INVOKESTATIC | INVOKEINTERFACE => {
                    save_pc!();
                    let resolved = method_ref!(u2!(pc + 1));
                    let target = if op == INVOKESTATIC {
                        if !resolved.is_static() {
                            throw!(self.throw_new("java/lang/IncompatibleClassChangeError", Some("expected static method")));
                        }
                        self.ensure_initialized(resolved.class())?;
                        resolved
                    } else {
                        let receiver = Ref::from_slot(self.stack[self.stack.len() - resolved.arg_slots as usize]);
                        if receiver.is_null() {
                            throw!(self.null_pointer());
                        }
                        check_ref!(receiver);
                        if op == INVOKESPECIAL {
                            try_vm!(self.select_special(class, resolved))
                        } else if resolved.intrinsic == Some(Intrinsic::Generic) {
                            let site = self.link_handle_site(class, u2!(pc + 1), resolved)?;
                            if let Some(appendix) = site.appendix {
                                self.push_ref(appendix.get());
                            }
                            site.invoker
                        } else if resolved.intrinsic.is_some() {
                            resolved
                        } else {
                            // Inline cache: same receiver class as last time → same target.
                            let receiver_class = heap.class_id(receiver);
                            let cache = &class.call_caches[u2!(pc + 1) as usize];
                            match cache.get() {
                                Some((cached, method)) if cached == receiver_class && !debug_switch("RJAVA_NO_IC") => {
                                    vm.method_by_index(method)
                                }
                                _ => {
                                    let selected = try_vm!(vm.select_method(vm.class_by_id(receiver_class), resolved));
                                    cache.set(receiver_class, selected.global_index);
                                    selected
                                }
                            }
                        }
                    };
                    self.call(target)?;
                    return Ok(Flow::Continue);
                }
                INVOKEDYNAMIC => {
                    save_pc!();
                    let site = self.link_indy(method, pc as u32, u2!(pc + 1))?;
                    if let Some(appendix) = site.appendix {
                        self.push_ref(appendix.get());
                    }
                    self.call(site.invoker)?;
                    return Ok(Flow::Continue);
                }
                NEW => {
                    let target = class_ref!(u2!(pc + 1));
                    if target.is_interface() || target.access_flags & classfile::access::ABSTRACT != 0 {
                        throw!(self.throw_new("java/lang/InstantiationError", Some(&target.java_name())));
                    }
                    save_pc!();
                    self.ensure_initialized(target)?;
                    let obj = try_vm!(self.vm.new_object(target));
                    self.push_ref(obj);
                    pc += 3
                }
                NEWARRAY => {
                    let kind = match code[pc + 1] {
                        4 => FieldKind::Boolean,
                        5 => FieldKind::Char,
                        6 => FieldKind::Float,
                        7 => FieldKind::Double,
                        8 => FieldKind::Byte,
                        9 => FieldKind::Short,
                        10 => FieldKind::Int,
                        _ => FieldKind::Long,
                    };
                    let length = self.pop_i();
                    let array = try_vm!(self.vm.new_primitive_array(kind, length));
                    self.push_ref(array);
                    pc += 2
                }
                ANEWARRAY => {
                    let component = class_ref!(u2!(pc + 1));
                    let length = self.pop_i();
                    let array = try_vm!(self.vm.new_array(self.vm.array_class(component), length));
                    self.push_ref(array);
                    pc += 3
                }
                MULTIANEWARRAY => {
                    let array_class = class_ref!(u2!(pc + 1));
                    let dims = code[pc + 3] as usize;
                    let start = self.stack.len() - dims;
                    let lengths: Vec<i32> = self.stack[start..].iter().map(|&v| v as i32).collect();
                    self.stack.truncate(start);
                    if let Some(&negative) = lengths.iter().find(|&&l| l < 0) {
                        throw!(self.throw_new("java/lang/NegativeArraySizeException", Some(&negative.to_string())));
                    }
                    let array = try_vm!(self.multi_array(array_class, &lengths));
                    self.push_ref(array);
                    pc += 4
                }
                ARRAYLENGTH => {
                    let array = self.pop_ref();
                    if array.is_null() {
                        throw!(self.null_pointer());
                    }
                    self.push_i(heap.array_length(array));
                    pc += 1
                }
                ATHROW => {
                    let exception = self.pop_ref();
                    if exception.is_null() {
                        throw!(self.null_pointer());
                    }
                    throw!(Throw(exception));
                }
                CHECKCAST => {
                    let target = class_ref!(u2!(pc + 1));
                    let obj = Ref::from_slot(*self.stack.last().unwrap());
                    if !obj.is_null() {
                        let obj_class = self.vm.class_of(obj);
                        if !obj_class.is_subclass_of(target) {
                            let message = format!("class {} cannot be cast to class {}", obj_class.java_name(), target.java_name());
                            throw!(self.throw_new("java/lang/ClassCastException", Some(&message)));
                        }
                    }
                    pc += 3
                }
                INSTANCEOF => {
                    let target = class_ref!(u2!(pc + 1));
                    let obj = self.pop_ref();
                    let result = !obj.is_null() && self.vm.class_of(obj).is_subclass_of(target);
                    self.push_i(result as i32);
                    pc += 3
                }
                MONITORENTER | MONITOREXIT => {
                    let obj = self.pop_ref();
                    if obj.is_null() {
                        throw!(self.null_pointer());
                    }
                    save_pc!();
                    if op == MONITORENTER {
                        self.monitor_enter(obj)?;
                    } else {
                        self.monitor_exit(obj)?;
                    }
                    pc += 1
                }
                WIDE => {
                    let wide_op = code[pc + 1];
                    let slot = locals + u2!(pc + 2) as usize;
                    match wide_op {
                        ILOAD | FLOAD | ALOAD => self.push(self.stack[slot]),
                        LLOAD | DLOAD => {
                            self.push(self.stack[slot]);
                            self.push(0);
                        }
                        ISTORE | FSTORE | ASTORE => {
                            let v = self.pop();
                            self.stack[slot] = v;
                        }
                        LSTORE | DSTORE => {
                            self.pop();
                            let v = self.pop();
                            self.stack[slot] = v;
                        }
                        IINC => {
                            let delta = i2!(pc + 4) as i32;
                            self.stack[slot] = (self.stack[slot] as i32).wrapping_add(delta) as u32 as u64;
                            pc += 2;
                        }
                        _ => throw!(self.throw_new("java/lang/VerifyError", Some("bad wide instruction"))),
                    }
                    pc += 4
                }
                IFNULL => {
                    let v = self.pop_ref();
                    branch!(v.is_null())
                }
                IFNONNULL => {
                    let v = self.pop_ref();
                    branch!(!v.is_null())
                }
                _ => throw!(self.throw_new("java/lang/VerifyError", Some(&format!("bad opcode {op:#x}")))),
            }
        }
    }

    /// Classifies (once) whether `method` can run without a frame.
    fn fast_path(&mut self, method: MethodRef) -> FastPath {
        if let Some(&fast) = method.fast_path.get() {
            return fast;
        }
        let fast = self.classify_fast_path(method);
        *method.fast_path.get_or_init(|| fast)
    }

    fn classify_fast_path(&mut self, method: MethodRef) -> FastPath {
        if debug_switch("RJAVA_NO_FASTPATH") {
            return FastPath::None;
        }
        let flags = method.access_flags;
        if method.intrinsic.is_some()
            || method.is_native()
            || method.is_abstract()
            || flags & classfile::access::SYNCHRONIZED != 0
            || self.vm.native(&method.native_key()).is_some()
        {
            return FastPath::None;
        }
        let Some(code) = method.code.as_ref().map(|c| &c.code[..]) else { return FastPath::None };
        let class = method.class();
        let returns_value = |op: u8| matches!(op, IRETURN | LRETURN | FRETURN | DRETURN | ARETURN);
        match *code {
            [RETURN] => FastPath::Empty,
            // A constructor that only calls a trivial super constructor.
            [ALOAD_0, INVOKESPECIAL, hi, lo, RETURN] if method.name == "<init>" => {
                match self.resolve_method(class, u16::from_be_bytes([hi, lo])) {
                    Ok(target) if target.name == "<init>" && target.arg_slots == 1 => match self.fast_path(target) {
                        FastPath::Empty => FastPath::Empty,
                        _ => FastPath::None,
                    },
                    Ok(_) => FastPath::None,
                    Err(_) => FastPath::None,
                }
            }
            [ALOAD_0, GETFIELD, hi, lo, ret] if !method.is_static() && returns_value(ret) => {
                match self.resolve_field(class, u16::from_be_bytes([hi, lo])) {
                    Ok(field) if !field.is_static => FastPath::Getter(field),
                    _ => FastPath::None,
                }
            }
            [ALOAD_0, ILOAD_1 | FLOAD_1 | ALOAD_1 | LLOAD_1 | DLOAD_1, PUTFIELD, hi, lo, RETURN] if !method.is_static() => {
                match self.resolve_field(class, u16::from_be_bytes([hi, lo])) {
                    Ok(field) if !field.is_static => FastPath::Setter(field),
                    _ => FastPath::None,
                }
            }
            [ICONST_M1..=ICONST_5, IRETURN] => FastPath::Constant((code[0] as i32 - ICONST_0 as i32) as u32 as u64),
            [ACONST_NULL, ARETURN] => FastPath::Constant(0),
            [BIPUSH, value, IRETURN] => FastPath::Constant(value as i8 as i32 as u32 as u64),
            _ => FastPath::None,
        }
    }

    /// The method a signature-polymorphic intrinsic dispatches to, given its
    /// arguments; `true` if the trailing MemberName argument must be dropped.
    fn intrinsic_target(&mut self, method: MethodRef, intrinsic: Intrinsic, args: &[u64]) -> JResult<(MethodRef, bool)> {
        match intrinsic {
            Intrinsic::InvokeBasic => {
                let mh = self.null_check(Ref::from_slot(args[0]))?;
                let form = self.field_ref(mh, "java/lang/invoke/MethodHandle", "form");
                let vmentry = self.field_ref(form, "java/lang/invoke/LambdaForm", "vmentry");
                Ok((crate::native::member_name_method(self, vmentry)?, false))
            }
            Intrinsic::LinkToStatic | Intrinsic::LinkToSpecial | Intrinsic::LinkToVirtual | Intrinsic::LinkToInterface => {
                let member_name = Ref::from_slot(*args.last().unwrap());
                let target = crate::native::member_name_method(self, member_name)?;
                if intrinsic == Intrinsic::LinkToStatic {
                    self.ensure_initialized(target.class())?;
                    return Ok((target, true));
                }
                let receiver = self.null_check(Ref::from_slot(args[0]))?;
                if intrinsic == Intrinsic::LinkToSpecial {
                    return Ok((target, true));
                }
                let selected = self.vm.select_method(self.vm.class_of(receiver), target).map_err(|e| self.throw_vm_error(e))?;
                Ok((selected, true))
            }
            Intrinsic::Generic => {
                let message = format!("{method:?} must be linked at its call site");
                Err(self.throw_new("java/lang/InternalError", Some(&message)))
            }
        }
    }

    /// invokespecial selection (JVMS §6.5 invokespecial).
    fn select_special(&self, current: ClassRef, resolved: MethodRef) -> Result<MethodRef, VmError> {
        Ok(select_special(current, resolved))
    }


    /// Calls `method` with its arguments on the operand stack: natives run
    /// immediately, Java methods get a frame.
    fn call(&mut self, method: MethodRef) -> JResult<()> {
        match self.fast_path(method) {
            FastPath::None => {}
            fast => {
                let start = self.stack.len() - method.arg_slots as usize;
                let heap = &self.vm.heap;
                match fast {
                    FastPath::Getter(field) => {
                        let value = read_field(heap, Ref::from_slot(self.stack[start]), field.offset, field.kind);
                        volatile_fence(field.volatile);
                        self.stack.truncate(start);
                        self.stack.push(value);
                        if field.kind.is_wide() {
                            self.stack.push(0);
                        }
                    }
                    FastPath::Setter(field) => {
                        write_field(heap, Ref::from_slot(self.stack[start]), field.offset, field.kind, self.stack[start + 1]);
                        volatile_fence(field.volatile);
                        self.stack.truncate(start);
                    }
                    FastPath::Constant(value) => {
                        self.stack.truncate(start);
                        self.stack.push(value);
                    }
                    FastPath::Empty | FastPath::None => self.stack.truncate(start),
                }
                self.advance_caller();
                return Ok(());
            }
        }
        if let Some(intrinsic) = method.intrinsic {
            let start = self.stack.len() - method.arg_slots as usize;
            let args = self.stack[start..].to_vec();
            let (target, drop_trailing) = self.intrinsic_target(method, intrinsic, &args)?;
            if drop_trailing {
                self.stack.pop();
            }
            return self.call(target);
        }
        if let Some(native) = self.bind_native(method)? {
            let count = method.arg_slots as usize;
            let start = self.stack.len() - count;
            // Copy the arguments off the stack without allocating.
            let mut buffer = [0u64; 24];
            let heap_args;
            let args: &[u64] = if count <= buffer.len() {
                buffer[..count].copy_from_slice(&self.stack[start..]);
                &buffer[..count]
            } else {
                heap_args = self.stack[start..].to_vec();
                &heap_args
            };
            let base = self.shared.handles_len();
            let result = self.call_native(method, native, args);
            self.shared.truncate_handles(base);
            let result = match result {
                Ok(result) => result,
                Err(Throw(exception)) => {
                    crate::threads::push_handle(exception);
                    return Err(Throw(exception));
                }
            };
            self.stack.truncate(start);
            if let (Some(kind), Some(value)) = (method.ret, result) {
                self.stack.push(value);
                if kind.is_wide() {
                    self.stack.push(0);
                }
            }
            self.advance_caller();
            return Ok(());
        }
        if method.is_abstract() {
            return Err(self.throw_vm_error(VmError::new("java/lang/AbstractMethodError", format!("{method:?}"))));
        }
        self.push_frame(method, false)
    }

    // --- compiled code -------------------------------------------------------------

    fn directly_callable(&self, method: MethodRef) -> bool {
        directly_callable(self.vm, method)
    }
    //
    // See jit::compile for the protocol. Compiled code pushes frame records
    // for its direct calls without telling the interpreter, so `frames.len()`
    // is only current inside imports (which `jit_sync` to the calling frame)
    // and after compiled code returned.

    /// This thread's block in linear memory (TLAB, native stack budget).
    pub(crate) fn thread_block(&self) -> &'static crate::memory::ThreadBlock {
        self.stack.thread_block()
    }

    /// Compiled code for the top frame and the block to enter at, compiling
    /// the method if it became hot.
    #[inline]
    fn compiled_entry(&mut self) -> Option<(&'static crate::jit::JitCode, u32)> {
        self.vm.jit.as_ref()?;
        let frame = self.frames.top();
        let method = frame.method;
        let code = match method.jit.code.get() {
            Some(_) => method.jit.current()?,
            None => {
                let thresholds = crate::jit::thresholds();
                let relaxed = std::sync::atomic::Ordering::Relaxed;
                if method.invocations.load(relaxed) < thresholds.invocations
                    && method.jit.backedges.load(relaxed) < thresholds.backedges
                {
                    return None;
                }
                self.vm.jit_queue.submit(self.vm.as_static(), method);
                method.jit.current()?
            }
        };
        // SAFETY: methods (and their JIT state) are immortal.
        let code: &'static crate::jit::JitCode = unsafe { &*(code as *const _) };
        // Too little native stack left: stay in the interpreter, whose calls
        // do not nest natively.
        if self.stack.thread_block().native_budget.load(std::sync::atomic::Ordering::Relaxed) < 2 * crate::jit::REENTRY_COST {
            return None;
        }
        let block = code.block_at(frame.pc)?;
        crate::jit::stats::bump(|s| &s.compiled_entries);
        Some((code, block))
    }

    /// Runs the top frame in compiled code from `block` until it returns or
    /// throws.
    fn run_compiled(&mut self, code: &'static crate::jit::JitCode, block: u32) -> JResult<Flow> {
        let depth = self.frames.len();
        let frame = self.frames.top();
        let body = frame.method.code.as_ref().unwrap();
        // The whole frame is live while compiled code runs (the collector
        // scans up to the stack length); push_frame reserved the room.
        unsafe { self.stack.set_len(frame.locals as usize + body.max_locals as usize + body.max_stack as usize) };
        let fp = (self.stack.region_offset() + frame.locals as usize * 8) as u32;
        let record = self.frames.offset(depth - 1);
        let jit = self.vm.jit.as_ref().unwrap();
        // Only this thread uses its budget.
        let budget = &self.stack.thread_block().native_budget;
        let left = budget.load(std::sync::atomic::Ordering::Relaxed);
        budget.store(left - crate::jit::REENTRY_COST, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: `self` is this thread's interpreter and stays borrowed.
        let status = unsafe { jit.enter(code.module, self as *mut Self as *mut (), fp, record, block) };
        budget.store(left, std::sync::atomic::Ordering::Relaxed);
        if status == crate::jit::compile::STATUS_EXCEPTION {
            // Frames are as the throwing import left them.
            let exception = std::mem::replace(&mut self.jit_exception, Ref::NULL);
            return Err(Throw(exception));
        }
        self.frames.set_len(depth);
        let slots = frame.method.ret.map_or(0, |kind| if kind.is_wide() { 2 } else { 1 });
        if depth == self.jit_stop {
            unsafe { self.stack.set_len(frame.locals as usize + slots) };
            return Ok(Flow::Return);
        }
        // The result is already in slot 0 of the frame.
        let frame = self.frames.pop();
        if !frame.monitor.is_null() {
            let _ = self.monitor_exit(frame.monitor);
        }
        unsafe { self.stack.set_len(frame.locals as usize + slots) };
        if !frame.entry {
            self.advance_caller();
        }
        Ok(Flow::Continue)
    }

    /// Runs the top frame until it executes a return instruction, which is
    /// left to the caller (compiled code): the result goes to slot 0 of the
    /// frame and the frame stays pushed. With `interpret_first`, the current
    /// instruction is interpreted even at a block start (compiled code handed
    /// it over).
    fn run_to_return(&mut self, interpret_first: bool) -> JResult<()> {
        let depth = self.frames.len();
        let saved = std::mem::replace(&mut self.jit_stop, depth);
        let mut interpret = interpret_first;
        let result = loop {
            let compiled = if std::mem::take(&mut interpret) { None } else { self.compiled_entry() };
            let flow = match compiled {
                Some((code, block)) => self.run_compiled(code, block),
                None => self.execute::<false>(),
            };
            match flow {
                Ok(Flow::Continue) => {}
                Ok(Flow::Return) => break Ok(()),
                Err(Throw(exception)) => {
                    if !self.handle_exception(exception, depth - 1) {
                        break Err(Throw(exception));
                    }
                }
            }
        };
        self.jit_stop = saved;
        result
    }

    /// Adopts the frame records up to the one at `record` (an import's
    /// caller) and makes its whole frame live. Returns its operand base.
    fn jit_sync(&mut self, record: u32) -> usize {
        self.frames.set_len(self.frames.index_at(record) + 1);
        let frame = self.frames.top();
        let body = frame.method.code.as_ref().unwrap();
        let operands = frame.locals as usize + body.max_locals as usize;
        unsafe { self.stack.set_len(operands + body.max_stack as usize) };
        operands
    }

    /// An import's status for a Java result.
    fn jit_status(&mut self, result: JResult<()>) -> i32 {
        match result {
            Ok(()) => crate::jit::compile::STATUS_RETURN,
            Err(Throw(exception)) => {
                crate::threads::push_handle(exception);
                self.jit_exception = exception;
                crate::jit::compile::STATUS_EXCEPTION
            }
        }
    }

    /// `vm.step`: executes the (non-call) instruction at `pc` in place.
    pub(crate) fn jit_step(&mut self, record: u32, pc: u32, depth: u32) -> i32 {
        let operands = self.jit_sync(record);
        self.frames.set_top_pc(pc);
        unsafe { self.stack.set_len(operands + depth as usize) };
        if let Some(stats) = crate::jit::stats::get() {
            stats.step[self.current_opcode() as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // An ldc that compiled code could now do itself (resolved since):
        // compile the method again (checked every so often).
        let method = self.frames.top().method;
        let code = &method.code.as_ref().unwrap().code;
        if matches!(code[pc as usize], LDC | LDC_W)
            && method.jit.generic_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 256 == 255
            && method.jit.version.load(std::sync::atomic::Ordering::Relaxed) < crate::jit::MAX_RECOMPILES
        {
            let index = if code[pc as usize] == LDC { code[pc as usize + 1] as usize } else { u16::from_be_bytes([code[pc as usize + 1], code[pc as usize + 2]]) as usize };
            if method.class().cp_cache[index].get().is_some() {
                let frames = self.frames.len();
                for i in (frames.saturating_sub(7)..frames).rev() {
                    self.vm.jit_queue.resubmit(self.vm.as_static(), self.frames.get(i).method);
                }
            }
        }
        let result = self.execute::<true>().map(|_| ());
        self.jit_status(result)
    }

    /// Why the call at the top frame's pc goes through the `invoke` import
    /// (for statistics), and the method concerned.
    fn generic_invoke_reason(&mut self) -> (&'static str, MethodRef) {
        let frame = self.frames.top();
        let code = &frame.method.code.as_ref().unwrap().code;
        let pc = frame.pc as usize;
        let op = code[pc];
        if op == INVOKEDYNAMIC {
            return ("indy", frame.method);
        }
        let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
        let Some(crate::class::Resolved::Method(resolved)) = frame.method.class().cp_cache[index as usize].get() else {
            return ("unresolved", frame.method);
        };
        let resolved = *resolved;
        let mut target = resolved;
        if resolved.intrinsic.is_some() {
            return ("intrinsic", resolved);
        }
        if op == INVOKESTATIC && !self.vm.is_initialized(resolved.class()) {
            return ("static-uninitialized", resolved);
        }
        if op != INVOKESTATIC {
            let receiver = Ref::from_slot(self.stack[self.stack.len() - resolved.arg_slots as usize]);
            if receiver.is_null() {
                return ("null-receiver", resolved);
            }
            if op != INVOKESPECIAL {
                if let Ok(selected) = self.vm.select_method(self.vm.class_of(receiver), resolved) {
                    target = selected;
                }
            }
        }
        let reason = if target.intrinsic.is_some() {
            "selected-intrinsic"
        } else if target.is_native() {
            "native"
        } else if !matches!(self.bind_native(target), Ok(None)) {
            "vm-override"
        } else if target.access_flags & classfile::access::SYNCHRONIZED != 0 {
            "synchronized"
        } else if target.is_abstract() {
            "abstract"
        } else {
            "callable-at-runtime"
        };
        (reason, target)
    }

    /// The opcode at the top frame's pc (for statistics).
    fn current_opcode(&self) -> u8 {
        let frame = self.frames.top();
        frame.method.code.as_ref().unwrap().code[frame.pc as usize]
    }

    /// `vm.invoke`: executes the call instruction at `pc`, including the
    /// callee; the result is pushed on the operand stack.
    pub(crate) fn jit_invoke(&mut self, record: u32, pc: u32, depth: u32) -> i32 {
        let operands = self.jit_sync(record);
        self.frames.set_top_pc(pc);
        unsafe { self.stack.set_len(operands + depth as usize) };
        let base = self.frames.len();
        if let Some(stats) = crate::jit::stats::get() {
            stats.invoke.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (reason, example) = self.generic_invoke_reason();
            let mut reasons = stats.invoke_reasons.lock().unwrap();
            let entry = reasons.entry(reason).or_insert_with(|| (0, format!("{example:?}")));
            entry.0 += 1;
            drop(reasons);
            let caller = self.frames.top().method;
            let key = (reason, example as *const crate::class::Method as usize, caller as *const crate::class::Method as usize);
            stats.invoke_targets.lock().unwrap().entry(key).or_insert((0, example, caller)).0 += 1;
        }
        // A call site that can now be a direct call (its target got loaded
        // or initialised since the caller was compiled): compile the caller
        // again (checked every so often).
        let caller = self.frames.top().method;
        if caller.jit.generic_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 256 == 255
            && caller.jit.version.load(std::sync::atomic::Ordering::Relaxed) < crate::jit::MAX_RECOMPILES
            && !matches!(jit_call_site(self.vm, caller, pc as usize), crate::jit::compile::CallSite::Generic)
        {
            // The site may be in a method inlined into one further down.
            let frames = self.frames.len();
            for index in (frames.saturating_sub(7)..frames).rev() {
                self.vm.jit_queue.resubmit(self.vm.as_static(), self.frames.get(index).method);
            }
        }
        let mut result = self.execute::<true>().map(|_| ());
        if result.is_ok() && self.frames.len() > base {
            result = self.run(base);
        }
        self.jit_status(result)
    }

    /// `vm.interpret`: runs the rest of the frame from `pc` in the interpreter.
    pub(crate) fn jit_interpret(&mut self, record: u32, pc: u32, depth: u32) -> i32 {
        let operands = self.jit_sync(record);
        self.frames.set_top_pc(pc);
        unsafe { self.stack.set_len(operands + depth as usize) };
        if let Some(stats) = crate::jit::stats::get() {
            stats.interpret[self.current_opcode() as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let result = self.run_to_return(true);
        self.jit_status(result)
    }

    /// `vm.select(record, class_id, method)`: the method index to call for
    /// method index `method` on a receiver of class `class_id` (NO_CLASS: no
    /// virtual dispatch), or -1 if it is not callable through the table (the
    /// `invoke` import then handles it).
    pub(crate) fn jit_select(&mut self, record: u32, class_id: u32, method: u32) -> i32 {
        crate::jit::stats::bump(|s| &s.select);
        let method = self.vm.method_by_index(method);
        let target = if class_id == crate::jit::compile::NO_CLASS {
            method
        } else {
            match self.vm.select_method(self.vm.class_by_id(class_id), method) {
                Ok(target) => target,
                Err(_) => return -1,
            }
        };
        if target.is_static() && !self.vm.is_initialized(target.class()) {
            self.jit_sync(record);
            if self.ensure_initialized(target.class()).is_err() {
                return -1;
            }
        }
        if self.directly_callable(target) { target.global_index as i32 } else { -1 }
    }

    /// The table stub: calls method `index` (not compiled, or not yet
    /// instantiated on this thread) for compiled code, as `call` would.
    pub(crate) fn jit_call(&mut self, record: u32, locals: u32, index: u32) -> i32 {
        let method = self.vm.method_by_index(index);
        crate::jit::stats::bump(|s| &s.call_stub);
        // The callers' records; the arguments end the caller's operand stack.
        self.frames.set_len(self.frames.index_at(record));
        unsafe { self.stack.set_len(locals as usize + method.arg_slots as usize) };
        let result = self.push_frame(method, false).and_then(|()| self.run_to_return(false));
        // The frame stays for the compiled caller to drop: release a
        // synchronized method's monitor here (pop_frame is not run).
        let result = result.and_then(|()| {
            let index = self.frames.len() - 1;
            let mut frame = self.frames.get(index);
            if frame.monitor.is_null() {
                return Ok(());
            }
            let monitor = std::mem::replace(&mut frame.monitor, Ref::NULL);
            self.frames.set(index, frame);
            self.monitor_exit(monitor)
        });
        self.jit_status(result)
    }

    /// `vm.monitor`: the monitor of a synchronized method entered from
    /// compiled code, kept in its frame record (as `push_frame` does).
    pub(crate) fn jit_monitor(&mut self, record: u32, enter: bool) -> i32 {
        self.jit_sync(record);
        let index = self.frames.len() - 1;
        let mut frame = self.frames.get(index);
        let result = if enter {
            let obj = if frame.method.is_static() { self.vm.mirror(frame.method.class()) } else { Ref::from_slot(self.stack[frame.locals as usize]) };
            self.monitor_enter(obj).map(|()| {
                frame.monitor = obj;
                self.frames.set(index, frame);
            })
        } else {
            let obj = std::mem::replace(&mut frame.monitor, Ref::NULL);
            self.frames.set(index, frame);
            // (None if the interpreter finished the frame and released it.)
            if obj.is_null() { Ok(()) } else { self.monitor_exit(obj) }
        };
        self.jit_status(result)
    }

    /// `vm.overflow`: the frame whose record would be at `record` does not fit.
    pub(crate) fn jit_overflow(&mut self, record: u32) -> i32 {
        self.jit_sync(record + crate::memory::frame_layout::SIZE as u32);
        let error = self.stack_overflow();
        self.jit_status(Err(error))
    }

    /// `vm.safepoint`.
    pub(crate) fn jit_safepoint(&mut self, record: u32) {
        self.jit_sync(record);
        self.ticks = 1;
        self.safepoint();
    }




    fn load_constant(&mut self, class: ClassRef, index: u16) -> JResult<()> {
        match class.cp().get(index).map_err(|e| self.throw_vm_error(VmError::new("java/lang/ClassFormatError", e.0)))? {
            &Constant::Integer(v) => self.push_i(v),
            &Constant::Float(v) => self.push_f(v),
            &Constant::Long(v) => self.push_l(v),
            &Constant::Double(v) => self.push_d(v),
            &Constant::String(utf8) => {
                let string = match class.cp_cache[index as usize].get() {
                    Some(crate::class::Resolved::String(s)) => *s,
                    _ => {
                        let bytes = class.cp().utf8_bytes(utf8).unwrap();
                        let units = classfile::mutf8::decode_utf16(bytes).unwrap_or_default();
                        let s = self.vm.intern_utf16(&units).map_err(|e| self.throw_vm_error(e))?;
                        let _ = class.cp_cache[index as usize].set(crate::class::Resolved::String(s));
                        s
                    }
                };
                self.push_ref(string);
            }
            &Constant::Class(_) => {
                let target = self.resolve_class(class, index)?;
                let mirror = self.vm.mirror(target);
                self.push_ref(mirror);
            }
            &Constant::MethodType(_) | &Constant::MethodHandle { .. } => {
                let value = self.linked_constant(class, index)?;
                self.push_ref(value);
            }
            &Constant::Dynamic { name_and_type, .. } => {
                let value = self.linked_constant(class, index)?;
                let (_, descriptor) = class.cp().name_and_type(name_and_type).unwrap();
                let kind = FieldKind::from_descriptor(descriptor);
                let slot = crate::native::unbox_value(self, kind, value)?;
                self.push(slot);
                if kind.is_wide() {
                    self.push(0);
                }
            }
            other => {
                let message = format!("ldc of {other:?} is not loadable");
                return Err(self.throw_new("java/lang/VerifyError", Some(&message)));
            }
        }
        Ok(())
    }

    fn multi_array(&self, class: ClassRef, lengths: &[i32]) -> Result<Ref, VmError> {
        let array = self.vm.new_array(class, lengths[0])?;
        if lengths.len() > 1 {
            let component = class.component().unwrap();
            for i in 0..lengths[0] {
                let sub = self.multi_array(component, &lengths[1..])?;
                self.vm.heap.write(array, ARRAY_BASE_OFFSET + i as u32 * 4, sub.0);
            }
        }
        Ok(array)
    }
}

/// invokespecial selection (JVMS §6.5 invokespecial).
fn select_special(current: ClassRef, resolved: MethodRef) -> MethodRef {
    let owner = resolved.class();
    let use_super = resolved.name != "<init>"
        && !owner.is_interface()
        && current.access_flags & classfile::access::SYNCHRONIZED != 0 // ACC_SUPER
        && !std::ptr::eq(owner, current)
        && current.is_subclass_of(owner);
    if !use_super {
        return resolved;
    }
    let start = current.super_class.expect("subclass has a superclass");
    for c in start.super_classes() {
        if let Some(m) = c.find_method(&resolved.name, &resolved.descriptor) {
            if !m.is_static() {
                return m;
            }
        }
    }
    resolved
}

/// Whether compiled code may call `method` through the table.
pub(crate) fn directly_callable(vm: &Vm, method: MethodRef) -> bool {
    method.code.is_some()
        && !method.is_abstract()
        && method.intrinsic.is_none()
        && (method.access_flags & classfile::access::SYNCHRONIZED == 0 || !crate::jit::compile::no_synchronized())
        && method.native.get_or_init(|| vm.native(&method.native_key())).is_none()
}

/// How compiled code should perform the call instruction at `pc`.
fn jit_call_site(vm: &Vm, method: MethodRef, pc: usize) -> crate::jit::compile::CallSite {
    use crate::jit::compile::CallSite;
    let code = &method.code.as_ref().unwrap().code;
    let op = code[pc];
    // A linked invokedynamic or signature-polymorphic call: the appendix
    // cell must be addressable by compiled code (under wasm).
    let linked = |site: Option<crate::vm::LinkedSite>| match site {
        Some(site) if directly_callable(vm, site.invoker) => match site.appendix {
            None => CallSite::Linked(site.invoker, None),
            Some(cell) if cfg!(target_family = "wasm") => {
                CallSite::Linked(site.invoker, Some(cell as *const crate::heap::RefCell32 as usize as u32))
            }
            Some(_) => CallSite::Generic,
        },
        _ => CallSite::Generic,
    };
    if op == INVOKEDYNAMIC {
        return linked(vm.indy_sites.lock().unwrap().get(&(method.global_index, pc as u32)).copied());
    }
    let class = method.class();
    let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
    let Some(crate::class::Resolved::Method(resolved)) = class.cp_cache[index as usize].get() else {
        return CallSite::Generic;
    };
    let resolved = *resolved;
    match resolved.intrinsic {
        None => {}
        Some(crate::class::Intrinsic::Generic) => {
            return if op == INVOKEVIRTUAL { linked(vm.handle_sites.lock().unwrap().get(&(class.id, index)).copied()) } else { CallSite::Generic };
        }
        Some(intrinsic) => {
            let offset = |class: &str, name: &str| -> Option<u32> {
                let class = vm.find_loaded(class)?;
                class.super_classes().find_map(|c| c.find_field_by_name(name).filter(|f| !f.is_static())).map(|f| f.offset)
            };
            let offsets = [
                offset("java/lang/invoke/MethodHandle", "form"),
                offset("java/lang/invoke/LambdaForm", "vmentry"),
                offset("java/lang/invoke/MemberName", "method"),
                offset("java/lang/invoke/ResolvedMethodName", "vmtarget"),
            ];
            return match offsets {
                [Some(a), Some(b), Some(c), Some(d)] => CallSite::Handle(intrinsic, [a, b, c, d]),
                _ => CallSite::Generic,
            };
        }
    }
    match op {
        INVOKESTATIC if resolved.is_static() && vm.is_initialized(resolved.class()) => {
            if directly_callable(vm, resolved) { CallSite::Direct(resolved) } else { CallSite::Generic }
        }
        INVOKESPECIAL if !resolved.is_static() => match select_special(class, resolved) {
            target if directly_callable(vm, target) => CallSite::Direct(target),
            _ => CallSite::Generic,
        },
        INVOKEVIRTUAL | INVOKEINTERFACE if !resolved.is_static() => {
            // What the interpreter's inline cache saw there, and the targets
            // compiled code found later (`jit::profile`), to inline them.
            let hint = class.call_caches[index as usize]
                .get()
                .map(|(class_id, method)| (class_id, vm.method_by_index(method)))
                .filter(|&(_, target)| directly_callable(vm, target));
            CallSite::Virtual(hint.into_iter().chain(crate::jit::profile::extra_targets(method, pc)).collect())
        }
        _ => CallSite::Generic,
    }
}

/// Compiles and loads a method (on a compiler thread); None if it cannot be compiled.
pub(crate) fn compile_method(vm: &Vm, method: MethodRef) -> Option<crate::jit::JitCode> {
    let jit = vm.jit.as_ref()?;
    let start = std::time::Instant::now();
    let result = crate::jit::compile::compile(method, vm, &mut |m, pc| jit_call_site(vm, m, pc)).and_then(
        |compiled| {
            // RJAVA_JIT_DUMP=<dir>: keep each module as <dir>/<method index>.wasm.
            if let Some(dir) = std::env::var_os("RJAVA_JIT_DUMP") {
                let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{}.wasm", method.global_index)), &compiled.wasm);
            }
            // A module the backend rejects is a compiler bug: always report it.
            let module = jit.load(&compiled.wasm, method.global_index, compiled.table_size).inspect_err(|e| {
                eprintln!("[jit] invalid module for {method:?}: {e}");
            })?;
            Ok(crate::jit::JitCode { module, entry_block: compiled.entry_block })
        },
    );
    if let Some(stats) = crate::jit::stats::get() {
        match &result {
            Ok(_) => {
                stats.compiled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                stats.compile_us.fetch_add(start.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => {
                let reason = e.split([' ', '(']).next().unwrap_or("?").to_owned();
                *stats.failed.lock().unwrap().entry(reason).or_default() += 1;
            }
        }
    }
    if crate::jit::log_enabled() {
        match &result {
            Ok(_) => eprintln!("[jit] {method:?} ({} µs)", start.elapsed().as_micros()),
            Err(e) => eprintln!("[jit] {method:?} not compiled: {e}"),
        }
    }
    result.ok()
}

/// Stands for "bound to a JNI function" in `Method::native` (see call_native).
#[cfg(unix)]
fn jni_trampoline(_: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    unreachable!("JNI methods are called through call_native")
}

/// The interpreter of the Java thread on this OS thread, if any.
#[cfg(unix)]
pub(crate) fn current_thread() -> Option<&'static mut Thread<'static>> {
    let interpreter = crate::threads::current_interpreter();
    // SAFETY: the attached interpreter of this OS thread.
    (!interpreter.is_null()).then(|| unsafe { &mut *(interpreter as *mut Thread<'static>) })
}

/// Debugging switches to disable individual optimisations (read once).
fn debug_switch(name: &'static str) -> bool {
    static SWITCHES: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    let (no_fast_path, no_ic) = *SWITCHES.get_or_init(|| {
        (std::env::var_os("RJAVA_NO_FASTPATH").is_some(), std::env::var_os("RJAVA_NO_IC").is_some())
    });
    match name {
        "RJAVA_NO_FASTPATH" => no_fast_path,
        _ => no_ic,
    }
}

/// Orders a volatile field access (sequential consistency; the heap accessors
/// are plain loads and stores).
#[inline(always)]
fn volatile_fence(volatile: bool) {
    if volatile {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn read_field(heap: &crate::heap::Heap, obj: Ref, offset: u32, kind: FieldKind) -> u64 {
    match kind {
        FieldKind::Byte => heap.read::<i8>(obj, offset) as i32 as u32 as u64,
        FieldKind::Boolean => heap.read::<u8>(obj, offset) as u64,
        FieldKind::Char => heap.read::<u16>(obj, offset) as u64,
        FieldKind::Short => heap.read::<i16>(obj, offset) as i32 as u32 as u64,
        FieldKind::Int | FieldKind::Float | FieldKind::Ref => heap.read::<u32>(obj, offset) as u64,
        FieldKind::Long | FieldKind::Double => heap.read::<u64>(obj, offset),
    }
}

pub fn write_field(heap: &crate::heap::Heap, obj: Ref, offset: u32, kind: FieldKind, value: u64) {
    match kind {
        FieldKind::Byte => heap.write(obj, offset, value as u8),
        FieldKind::Boolean => heap.write(obj, offset, value as u8 & 1),
        FieldKind::Char | FieldKind::Short => heap.write(obj, offset, value as u16),
        FieldKind::Int | FieldKind::Float | FieldKind::Ref => heap.write(obj, offset, value as u32),
        FieldKind::Long | FieldKind::Double => heap.write(obj, offset, value),
    }
}

impl Drop for Thread<'_> {
    fn drop(&mut self) {
        self.vm.memory.return_stack(&self.stack);
    }
}

impl ClassKind {
    pub fn is_instance(&self) -> bool {
        matches!(self, ClassKind::Instance)
    }
}
