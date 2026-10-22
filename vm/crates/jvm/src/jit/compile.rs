//! Baseline compiler: one method's bytecode → WebAssembly functions that
//! run on the interpreter's own frame in linear memory.
//!
//! `run(fp, record, block) -> status` runs the frame whose record (see
//! `memory::frame_layout`) is at `record` and whose local 0 is at `fp`, from
//! basic block `block`; the operand stack follows the locals. `call(fp,
//! record, locals, method)` is the entry for direct calls from compiled code:
//! it writes the callee's frame record, then runs from block 0. Both return
//! STATUS_RETURN (the result, if any, is in slot 0 of the frame, where the
//! caller's operand stack expects it; the record is left for the caller to
//! drop) or STATUS_EXCEPTION (pending on the thread, frames as at the throw).
//!
//! Values live in wasm locals only within a basic block; at block boundaries
//! and around imports the operand stack is in memory, exactly as the
//! interpreter would have it. So the interpreter can take over at any block
//! start, and the collector (which scans thread stacks) sees every reference.
//!
//! Calls to methods with bytecode go through the `vm.table` funcref table,
//! indexed by `Method::global_index` (a stub that interprets until the method
//! is compiled), with a monomorphic inline cache for virtual calls. Other
//! calls, and complex instructions (allocation, type checks, statics, ...),
//! are executed by the interpreter in place through imports. Rare failing
//! checks (null, bounds, zero divisor) hand the rest of the frame to the
//! interpreter, which throws.

use std::collections::BTreeSet;

use classfile::Constant;
use classfile::opcodes::*;
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, EntityType, ExportKind, ExportSection, Function, FunctionSection, GlobalSection,
    GlobalType, ImportSection, InstructionSink, MemArg, MemoryType, Module, RefType, TableType, TypeSection, ValType,
};

use crate::class::{FieldKind, MethodRef, Resolved};
use crate::heap::{ARRAY_BASE_OFFSET, ARRAY_LENGTH_OFFSET};
use crate::memory::frame_layout;

/// `entry_block` value for pcs where compiled code cannot be entered.
pub const NO_BLOCK: u16 = u16::MAX;
/// The frame returned (also: an import completed normally).
pub const STATUS_RETURN: i32 = 0;
/// An exception is pending on the thread (set by an import).
pub const STATUS_EXCEPTION: i32 = 1;
/// Back edges and calls between safepoint checks in compiled code.
pub const SAFEPOINT_TICKS: i32 = 10_000;
/// Operand slots reserved beyond max_stack (as in the interpreter).
pub const STACK_SLACK: usize = 4;
/// Bytes of a thread stack kept free for constructing StackOverflowError.
pub const OVERFLOW_RESERVE: usize = 64 << 10;

const MAX_CODE: usize = 16_000;
/// Inlining limits: nesting, callee bytecode size and locals, bytes per
/// compiled method, and operand-local slots reserved for inlined frames.
const MAX_INLINE_LOCALS: usize = 32;
const INLINE_SLOTS: usize = 320;

/// Inlining limits, tunable for experiments: RJAVA_JIT_INLINE=size,depth,bytes.
struct InlineLimits {
    size: usize,
    depth: usize,
    bytes: usize,
}

fn inline_limits(tier: u8) -> &'static InlineLimits {
    static LIMITS: std::sync::OnceLock<[InlineLimits; 2]> = std::sync::OnceLock::new();
    let limits = LIMITS.get_or_init(|| {
        let parse = |var: &str, defaults: (usize, usize, usize)| {
            let values: Vec<usize> =
                std::env::var(var).map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
            let get = |i: usize, default: usize| values.get(i).copied().unwrap_or(default);
            InlineLimits { size: get(0, defaults.0), depth: get(1, defaults.1).min(MAX_INLINE_DEPTH), bytes: get(2, defaults.2) }
        };
        // Tier 2 (hot methods, RJAVA_JIT_INLINE2): a larger budget.
        [parse("RJAVA_JIT_INLINE", (80, 5, 4000)), parse("RJAVA_JIT_INLINE2", (160, 6, 10000))]
    });
    &limits[(tier >= 2) as usize]
}

/// Synchronized methods are left to the interpreter unless
/// RJAVA_JIT_SYNC_METHODS is set (compiled, each call pays two monitor
/// imports; measured no faster on the Minecraft server).
pub fn no_synchronized() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RJAVA_JIT_SYNC_METHODS").is_none())
}

/// Pools of wasm locals for inlined frames' Java locals (one per level).
const MAX_INLINE_DEPTH: usize = 6;

// Function indices: the imports, then `run` and `call`.
const FN_STEP: u32 = 0;
const FN_INVOKE: u32 = 1;
const FN_SELECT: u32 = 2;
const FN_INTERPRET: u32 = 3;
const FN_SAFEPOINT: u32 = 4;
const FN_OVERFLOW: u32 = 5;
/// The table stub (`host_call`): runs a method in the interpreter.
const FN_CALL_STUB: u32 = 6;
/// `subtype(class id, target class id) -> bool` (no side effects).
const FN_SUBTYPE: u32 = 7;
/// `pure(op, a, b) -> result or -1` (see `super::pure`).
const FN_PURE: u32 = 8;
/// `fmod(f64, f64) -> f64` (Java's % on floating point).
const FN_FMOD: u32 = 9;
/// `monitor(record, enter) -> status`: a synchronized method's monitor.
const FN_MONITOR: u32 = 10;
const FN_RUN: u32 = 11;
const FN_CALL: u32 = 12;
// Type indices.
const TYPE_3: u32 = 0; // (i32, i32, i32) -> i32
const TYPE_SAFEPOINT: u32 = 1; // (i32) -> ()
const TYPE_1: u32 = 2; // (i32) -> i32
const TYPE_CALL: u32 = 3; // (i32, i32, i32, i32) -> i32
const TYPE_2: u32 = 4; // (i32, i32) -> i32
const TYPE_FMOD: u32 = 5; // (f64, f64) -> f64
const GLOBAL_TICKS: u32 = 0;
// Parameters and fixed locals of `run`.
const FP: u32 = 0;
const RECORD: u32 = 1;
const BLOCK: u32 = 2;
/// The frame's locals index (from its record), for computing callees'.
const LOCALS_INDEX: u32 = 3;
const FIRST_OPERAND_LOCAL: u32 = 4;

/// How compiled code performs a call instruction.
pub enum CallSite {
    /// The target is known and has bytecode: through the table.
    Direct(MethodRef),
    /// invokevirtual/invokeinterface with a resolved method: inline cache,
    /// filled by the `select` import. With the receiver class and target
    /// the interpreter last saw there, if any (to inline it, guarded).
    Virtual(Option<(u32, MethodRef)>),
    /// MethodHandle.invokeBasic / linkTo*: the target method is read from
    /// the handle or MemberName (field offsets: MethodHandle.form,
    /// LambdaForm.vmentry, MemberName.method, ResolvedMethodName.vmtarget).
    Handle(crate::class::Intrinsic, [u32; 4]),
    /// invokedynamic, or an invocation of a signature-polymorphic method
    /// (VarHandle access, MethodHandle.invoke), already linked: a direct call
    /// of the invoker, with the appendix (read from its cell at the given
    /// address, which the collector keeps current) as the extra last argument.
    Linked(MethodRef, Option<u32>),
    /// Anything else: the `invoke` import.
    Generic,
}

/// Class id for `select` meaning "no virtual dispatch".
pub const NO_CLASS: u32 = u32::MAX;

pub struct Compiled {
    pub wasm: Vec<u8>,
    /// Block index for each pc that starts a block, else NO_BLOCK.
    pub entry_block: Box<[u16]>,
    /// Table entries the code uses: 1 + the highest method index it calls.
    pub table_size: u32,
}

/// Value types of operand-stack values held in wasm locals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum T {
    I32 = 0,
    I64 = 1,
    F32 = 2,
    F64 = 3,
}

impl T {
    fn val(self) -> ValType {
        match self {
            T::I32 => ValType::I32,
            T::I64 => ValType::I64,
            T::F32 => ValType::F32,
            T::F64 => ValType::F64,
        }
    }
}

/// Where an operand-stack slot's value is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Entry {
    /// In its memory slot.
    Mem,
    /// In the wasm local for (its depth, type).
    Local(T),
    /// Second slot of a long/double held in a local (no value of its own).
    Top,
    /// `this` (Java local 0 of an instance method that never overwrites it):
    /// non-null, in its Java local.
    This,
}

/// Compiles `method`; `call_site(pc)` classifies its call instructions.
pub fn compile(
    method: MethodRef,
    vm: &crate::vm::Vm,
    call_site: &mut dyn FnMut(MethodRef, usize) -> CallSite,
) -> Result<Compiled, String> {
    let memory = &vm.memory;
    // Natively the backend's memory is the VM's region; under wasm the
    // region lies inside the VM's own memory, which compiled code imports.
    let region = memory.region();
    let (memory_minimum, memory_maximum) = if cfg!(target_family = "wasm") {
        MEMORY_BASE.store(region.base as u64, std::sync::atomic::Ordering::Relaxed);
        ((region.base as u64 + region.size as u64).div_ceil(65536), 65536)
    } else {
        let pages = (region.size / 65536) as u64;
        (pages, pages)
    };
    if method.access_flags & classfile::access::SYNCHRONIZED != 0 && no_synchronized() {
        return Err("synchronized".into());
    }
    let code = method.code.as_ref().ok_or("no code")?;
    let bytes: &'static [u8] = &code.code;
    let tier = method.jit.tier.load(std::sync::atomic::Ordering::Relaxed);
    if bytes.len() > MAX_CODE {
        return Err(format!("code too large ({} bytes)", bytes.len()));
    }
    let analysis = analyze(method)?;
    let n = analysis.blocks.len();

    let mut entry_block = vec![NO_BLOCK; bytes.len()].into_boxed_slice();
    for (index, &start) in analysis.blocks.iter().enumerate() {
        if analysis.depth[start] >= 0 {
            entry_block[start] = index as u16;
        }
    }

    // Natural loops become wasm loops (else: a dispatcher loop); compiled
    // code is then entered only at blocks outside loops and at outermost
    // loop headers.
    let structure = if std::env::var_os("RJAVA_JIT_DISPATCHER").is_some() {
        None
    } else {
        loop_structure(&block_successors(bytes, &analysis, &entry_block)).map(std::rc::Rc::new)
    };
    let block_of = entry_block.clone();
    if let Some(structure) = &structure {
        for (b, &start) in analysis.blocks.iter().enumerate() {
            if b > 0 && structure.scope[b].is_some() {
                entry_block[start] = NO_BLOCK;
            }
        }
    }

    // Locals: operand slots (this method's, then inlined frames') × 4 types,
    // scratch, Java locals (raw slot values, written through to memory),
    // then a pool of Java locals per inlining level. Pass 1 reserves room
    // for inlining; pass 2 declares what pass 1 used (each wasm local costs
    // frame space, and engines' native stacks are small in the browser).
    let layout = |operand_slots: usize, inline_levels: usize| {
        let scratch = FIRST_OPERAND_LOCAL + operand_slots as u32 * 4;
        let mut locals: Vec<(u32, ValType)> = vec![(1, ValType::I32)]; // LOCALS_INDEX
        for _ in 0..operand_slots {
            locals.extend([(1, ValType::I32), (1, ValType::I64), (1, ValType::F32), (1, ValType::F64)]);
        }
        locals.push((SCRATCH_I32, ValType::I32));
        locals.push((SCRATCH_I64, ValType::I64));
        let java_locals = scratch + SCRATCH_I32 + SCRATCH_I64;
        locals.push((code.max_locals as u32, ValType::I64));
        let inline_java_locals = java_locals + code.max_locals as u32;
        locals.push(((inline_levels * MAX_INLINE_LOCALS) as u32, ValType::I64));
        (locals, scratch, java_locals, inline_java_locals)
    };
    let own_slots = code.max_stack as usize + 2;
    let mut max_stack = own_slots + INLINE_SLOTS;
    let mut inline_levels = MAX_INLINE_DEPTH;
    let (mut locals, mut scratch, mut java_locals, mut inline_java_locals) = layout(max_stack, inline_levels);
    macro_rules! new_gen {
        ($call_site:expr, $entry_dirty:expr) => {
    Gen {
        c: Ctx {
            method,
            code: bytes,
            max_locals: code.max_locals as usize,
            stack: Vec::new(),
            nest: 0,
            block_of: block_of.clone().into(),
            all_blocks: analysis.blocks.clone().into(),
            cur: 0,
            n,
            depth: analysis.depth.clone().into(),
            java_locals,
            this_constant: !method.is_static() && !stores_local_0(bytes),
            frame_offset: 0,
            op_base: 0,
            result: None,
            written: written_locals(bytes, code.max_locals as usize).into(),
            dirty: vec![false; code.max_locals as usize],
            written_since_sync: vec![false; code.max_locals as usize],
            synced: false,
        },
        f: Function::new(locals.clone()),
        scratch,
        call_site: $call_site,
        inlined: Vec::new(),
        operand_slots: max_stack,
        inline_java_locals,
        inlined_bytes: 0,
        memory,
        vm,
        inline_caches: 0,
        table_size: method.global_index + 1,
        needs_record: false,
        entry_dirty: $entry_dirty,
        summaries: vec![(false, Vec::new()); n],
        structure: structure.clone(),
        labels: Vec::new(),
        slots_used: own_slots,
        levels_used: 0,
        inline_levels,
        tier,
    }
        };
    }
    // Pass 1 finds which locals each block leaves dirty; their fixpoint
    // over the control flow gives the dirty locals at each block start for
    // pass 2, so that a call stores only locals changed since the last one.
    let mut first = new_gen!(&mut *call_site, None);
    first.body()?;
    let summaries = std::mem::take(&mut first.summaries);
    max_stack = first.slots_used;
    inline_levels = first.levels_used;
    (locals, scratch, java_locals, inline_java_locals) = layout(max_stack, inline_levels);
    drop(first);
    let entry_dirty = dirty_at_entries(bytes, &analysis, &block_of, &summaries, code.max_locals as usize);
    let mut generator = new_gen!(call_site, Some(entry_dirty));
    generator.body()?;
    let inline_caches = generator.inline_caches;
    let table_size = generator.table_size;
    // A synchronized method's `call` enters and exits the monitor (imports).
    let leaf = !generator.needs_record && method.access_flags & classfile::access::SYNCHRONIZED == 0;
    // Only the locals the body uses (RJAVA_JIT_ALL_LOCALS: as laid out).
    let run = if std::env::var_os("RJAVA_JIT_ALL_LOCALS").is_some() {
        generator.f.into_raw_body()
    } else {
        super::locals::compact(&generator.f.into_raw_body(), 3)?.0
    };
    // Native frame size of `run` (+ `call`) in a baseline engine tier: a
    // slot per local and some fixed overhead.
    let run_locals: u32 = 3 + declared_locals(&run);
    let frame_cost = 256 + 8 * run_locals as i32;
    let call = call_entry(method, leaf, memory.stop_flag_offset(), frame_cost);

    let mut module = Module::new();
    let mut types = TypeSection::new();
    let i32 = ValType::I32;
    types.ty().function([i32, i32, i32], [i32]);
    types.ty().function([i32], []);
    types.ty().function([i32], [i32]);
    types.ty().function([i32, i32, i32, i32], [i32]);
    types.ty().function([i32, i32], [i32]);
    types.ty().function([ValType::F64, ValType::F64], [ValType::F64]);
    module.section(&types);
    let mut imports = ImportSection::new();
    imports.import(
        "vm",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: memory_minimum,
            maximum: Some(memory_maximum),
            memory64: false,
            shared: true,
            page_size_log2: None,
        }),
    );
    imports.import(
        "vm",
        "table",
        EntityType::Table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: 0,
            maximum: None,
            shared: false,
        }),
    );
    imports.import("vm", "step", EntityType::Function(TYPE_3));
    imports.import("vm", "invoke", EntityType::Function(TYPE_3));
    imports.import("vm", "select", EntityType::Function(TYPE_3));
    imports.import("vm", "interpret", EntityType::Function(TYPE_3));
    imports.import("vm", "safepoint", EntityType::Function(TYPE_SAFEPOINT));
    imports.import("vm", "overflow", EntityType::Function(TYPE_1));
    imports.import("vm", "call", EntityType::Function(TYPE_CALL));
    imports.import("vm", "subtype", EntityType::Function(TYPE_2));
    imports.import("vm", "pure", EntityType::Function(TYPE_3));
    imports.import("vm", "fmod", EntityType::Function(TYPE_FMOD));
    imports.import("vm", "monitor", EntityType::Function(TYPE_2));
    module.section(&imports);
    let mut functions = FunctionSection::new();
    functions.function(TYPE_3); // run
    functions.function(TYPE_CALL); // call
    module.section(&functions);
    let mut globals = GlobalSection::new();
    let mutable = GlobalType { val_type: ValType::I32, mutable: true, shared: false };
    globals.global(mutable, &ConstExpr::i32_const(SAFEPOINT_TICKS));
    // Inline caches: (receiver class id, target method index).
    for _ in 0..inline_caches {
        globals.global(mutable, &ConstExpr::i32_const(-1));
        globals.global(mutable, &ConstExpr::i32_const(-1));
    }
    module.section(&globals);
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, FN_RUN);
    exports.export("call", ExportKind::Func, FN_CALL);
    module.section(&exports);
    let mut codes = CodeSection::new();
    codes.raw(&run);
    codes.function(&call);
    module.section(&codes);
    // Names for profilers and stack traces: the method, and its entry.
    let mut names = wasm_encoder::NameSection::new();
    let mut functions = wasm_encoder::NameMap::new();
    let name = format!("{}.{}{}", method.class().name, method.name, method.descriptor);
    functions.append(FN_RUN, &name);
    functions.append(FN_CALL, &format!("{name} [call]"));
    names.module(&name);
    names.functions(&functions);
    module.section(&names);
    Ok(Compiled { wasm: module.finish(), entry_block, table_size })
}

const SCRATCH_I32: u32 = 9;
const SCRATCH_I64: u32 = 4;

/// `call(fp, record, locals, method)`: checks the room for the frame, writes
/// its record and runs it from block 0.
fn call_entry(method: MethodRef, leaf: bool, stop_flag: u32, frame_cost: i32) -> Function {
    const LOCALS: u32 = 2;
    const TICK: u32 = 4;
    const BLOCK_ADDRESS: u32 = 5;
    const BUDGET: u32 = 6;
    const RESULT: u32 = 7;
    let code = method.code.as_ref().unwrap();
    let extent = (code.max_locals as usize + code.max_stack as usize + STACK_SLACK) * 8 + OVERFLOW_RESERVE;
    let record = |offset: u32, align: u32| mem(offset as usize, align);
    let mut f = Function::new([(4, ValType::I32)]);
    f.instructions()
        // Values grow up, records down: overflow when they would meet.
        .local_get(FP)
        .i32_const(extent as i32)
        .i32_add()
        .local_get(RECORD)
        .i32_gt_u()
        .if_(BlockType::Empty)
        .local_get(RECORD)
        .call(FN_OVERFLOW)
        .return_()
        .end();
    if cfg!(target_family = "wasm") && method.jit.next_version() < super::MAX_RECOMPILES {
        // Once the method is compiled again, leave to the interpreter, which
        // enters the newest version and installs it in this worker's table.
        let version = &method.jit.version as *const std::sync::atomic::AtomicU32 as usize as i32;
        f.instructions()
            .i32_const(version)
            .i32_load(MemArg { offset: 0, align: 2, memory_index: 0 })
            .i32_const(method.jit.next_version() as i32)
            .i32_ne()
            .if_(BlockType::Empty)
            .local_get(FP)
            .local_get(RECORD)
            .local_get(LOCALS)
            .i32_const(method.global_index as i32)
            .call(FN_CALL_STUB)
            .return_()
            .end();
    }
    let threshold = super::tier2_threshold();
    if cfg!(target_family = "wasm") && threshold > 0 && method.jit.tier.load(std::sync::atomic::Ordering::Relaxed) < 2 {
        // Count calls; at the threshold, ask for tier 2 (`pure` TIER_UP).
        let calls = &method.jit.calls as *const std::sync::atomic::AtomicU32 as usize as i32;
        let at = MemArg { offset: 0, align: 2, memory_index: 0 };
        f.instructions()
            .i32_const(calls)
            .i32_const(calls)
            .i32_load(at)
            .i32_const(1)
            .i32_add()
            .local_tee(TICK)
            .i32_store(at)
            .local_get(TICK)
            .i32_const(threshold as i32)
            .i32_eq()
            .if_(BlockType::Empty)
            .i32_const(super::pure::TIER_UP as i32)
            .i32_const(method.global_index as i32)
            .i32_const(0)
            .call(FN_PURE)
            .drop()
            .end();
    }
    if cfg!(target_family = "wasm") && super::stats::enabled() {
        let counter = &super::stats::COMPILED_CALLS as *const std::sync::atomic::AtomicU64 as usize as i32;
        f.instructions()
            .i32_const(counter)
            .i64_const(1)
            .i64_atomic_rmw_add(MemArg { offset: 0, align: 3, memory_index: 0 })
            .drop();
    }
    if leaf {
        // Nothing can look at the frame (no imports, no calls, no loops):
        // no record, no safepoint.
        f.instructions().local_get(FP).local_get(RECORD).i32_const(0).call(FN_RUN).end();
        return f;
    }
    // A limited native stack (the browser's): charge the frame to the
    // thread's budget, or run the method in the interpreter if it is spent.
    let budgeted = cfg!(target_family = "wasm");
    if budgeted {
        let budget = mem(crate::memory::NATIVE_BUDGET_OFFSET, 2);
        f.instructions()
            .local_get(RECORD)
            .i32_const(!(crate::memory::STACK_REGION_BYTES as i32 - 1))
            .i32_and()
            .local_tee(BLOCK_ADDRESS)
            .local_get(BLOCK_ADDRESS)
            .i32_load(budget)
            .local_tee(BUDGET)
            .i32_const(frame_cost)
            .i32_sub()
            .i32_store(budget)
            .local_get(BUDGET)
            .i32_const(frame_cost)
            .i32_lt_s()
            .if_(BlockType::Empty)
            .local_get(BLOCK_ADDRESS)
            .local_get(BUDGET)
            .i32_store(budget)
            .local_get(FP)
            .local_get(RECORD)
            .local_get(LOCALS)
            .i32_const(method.global_index as i32)
            .call(FN_CALL_STUB)
            .return_()
            .end();
    }
    f.instructions()
        .local_get(RECORD)
        .i64_const(method as *const crate::class::Method as usize as i64)
        .i64_store(record(frame_layout::METHOD, 3))
        // pc 0 and the locals index, in one store.
        .local_get(RECORD)
        .local_get(LOCALS)
        .i64_extend_i32_u()
        .i64_const(32)
        .i64_shl()
        .i64_store(record(frame_layout::PC, 3))
        .local_get(RECORD)
        .i64_const(0) // monitor, entry
        .i64_store(record(frame_layout::MONITOR, 3))
        .global_get(GLOBAL_TICKS)
        .i32_const(1)
        .i32_sub()
        .local_tee(TICK)
        .global_set(GLOBAL_TICKS)
        .local_get(TICK)
        .i32_eqz()
        // A collector wants the world stopped (atomic: never hoisted).
        .i32_const(stop_flag as i32)
        .i32_atomic_load(mem(0, 2))
        .i32_or()
        .if_(BlockType::Empty)
        .i32_const(SAFEPOINT_TICKS)
        .global_set(GLOBAL_TICKS)
        .local_get(RECORD)
        .call(FN_SAFEPOINT)
        .end();
    let synchronized = method.access_flags & classfile::access::SYNCHRONIZED != 0;
    if synchronized {
        // Entered from compiled code: take the monitor (recorded in the
        // frame record, so that unwinding an exception releases it).
        f.instructions().local_get(RECORD).i32_const(1).call(FN_MONITOR).if_(BlockType::Empty);
        if budgeted {
            f.instructions().local_get(BLOCK_ADDRESS).local_get(BUDGET).i32_store(mem(crate::memory::NATIVE_BUDGET_OFFSET, 2));
        }
        f.instructions().i32_const(STATUS_EXCEPTION).return_().end();
    }
    f.instructions()
        .local_get(FP)
        .local_get(RECORD)
        .i32_const(0)
        .call(FN_RUN);
    if synchronized {
        // Returned normally: release it.
        f.instructions()
            .local_tee(RESULT)
            .if_(BlockType::Result(ValType::I32))
            .local_get(RESULT)
            .else_()
            .local_get(RECORD)
            .i32_const(0)
            .call(FN_MONITOR)
            .end();
    }
    if budgeted {
        f.instructions()
            .local_set(RESULT)
            .local_get(BLOCK_ADDRESS)
            .local_get(BUDGET)
            .i32_store(mem(crate::memory::NATIVE_BUDGET_OFFSET, 2))
            .local_get(RESULT);
    }
    if crate::memory::SAMPLING.load(std::sync::atomic::Ordering::Relaxed) {
        // The record is no longer a frame once the method returned (on an
        // exception the VM adopts the records as they are).
        f.instructions()
            .local_tee(RESULT)
            .i32_eqz()
            .if_(BlockType::Empty)
            .local_get(RECORD)
            .i64_const(0)
            .i64_store(record(frame_layout::METHOD, 3))
            .end()
            .local_get(RESULT);
    }
    f.instructions().end();
    f
}

// --- analysis -----------------------------------------------------------------------

struct Analysis {
    /// Block start pcs, ascending.
    blocks: Vec<usize>,
    /// Operand stack depth (slots) before each instruction; -1 if unreachable.
    depth: Vec<i32>,
}

fn analyze(method: MethodRef) -> Result<Analysis, String> {
    let code = method.code.as_ref().unwrap();
    let bytes: &[u8] = &code.code;
    let mut starts = BTreeSet::new();
    starts.insert(0usize);
    let mut pc = 0;
    while pc < bytes.len() {
        let len = instruction_length(bytes, pc).ok_or("bad instruction")?;
        let op = bytes[pc];
        let next = pc + len;
        match op {
            JSR | JSR_W | RET => return Err("jsr/ret".into()),
            IFEQ..=IF_ACMPNE | IFNULL | IFNONNULL | GOTO | GOTO_W => {
                starts.insert(branch_target(bytes, pc));
                starts.insert(next);
            }
            TABLESWITCH | LOOKUPSWITCH => {
                for target in switch_targets(bytes, pc) {
                    starts.insert(target);
                }
                starts.insert(next);
            }
            // Calls and returns leave compiled code; the interpreter comes
            // back after the call.
            IRETURN..=RETURN | ATHROW | INVOKEVIRTUAL..=INVOKEDYNAMIC => {
                starts.insert(next);
            }
            _ => {}
        }
        pc = next;
    }
    for handler in &code.exception_table {
        starts.insert(handler.handler_pc as usize);
    }
    starts.retain(|&s| s < bytes.len());

    // Depths: worklist over instructions.
    let class = method.class();
    let mut depth = vec![-1i32; bytes.len()];
    let mut work: Vec<(usize, i32)> = vec![(0, 0)];
    for handler in &code.exception_table {
        work.push((handler.handler_pc as usize, 1));
    }
    while let Some((pc, d)) = work.pop() {
        if depth[pc] >= 0 {
            if depth[pc] != d {
                return Err(format!("inconsistent stack depth at {pc}"));
            }
            continue;
        }
        depth[pc] = d;
        let op = bytes[pc];
        let (pops, pushes) = stack_effect(class, bytes, pc)?;
        let after = d - pops as i32 + pushes as i32;
        if after < 0 || after > code.max_stack as i32 + 2 {
            return Err(format!("bad stack depth after {pc}"));
        }
        let next = pc + instruction_length(bytes, pc).unwrap();
        match op {
            GOTO | GOTO_W => work.push((branch_target(bytes, pc), after)),
            IFEQ..=IF_ACMPNE | IFNULL | IFNONNULL => {
                work.push((branch_target(bytes, pc), after));
                work.push((next, after));
            }
            TABLESWITCH | LOOKUPSWITCH => {
                for target in switch_targets(bytes, pc) {
                    work.push((target, after));
                }
            }
            IRETURN..=RETURN | ATHROW => {}
            _ => {
                if next < bytes.len() {
                    work.push((next, after));
                }
            }
        }
    }
    Ok(Analysis { blocks: starts.into_iter().collect(), depth })
}

fn branch_target(code: &[u8], pc: usize) -> usize {
    let offset = if code[pc] == GOTO_W {
        i32::from_be_bytes([code[pc + 1], code[pc + 2], code[pc + 3], code[pc + 4]]) as isize
    } else {
        i16::from_be_bytes([code[pc + 1], code[pc + 2]]) as isize
    };
    (pc as isize + offset) as usize
}

fn i4(code: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]])
}

/// Default target first, then the case targets.
fn switch_targets(code: &[u8], pc: usize) -> Vec<usize> {
    let base = (pc + 4) & !3;
    let target = |offset: i32| (pc as isize + offset as isize) as usize;
    let mut targets = vec![target(i4(code, base))];
    if code[pc] == TABLESWITCH {
        let (low, high) = (i4(code, base + 4), i4(code, base + 8));
        for i in 0..=(high - low) as usize {
            targets.push(target(i4(code, base + 12 + 4 * i)));
        }
    } else {
        let pairs = i4(code, base + 4) as usize;
        for i in 0..pairs {
            targets.push(target(i4(code, base + 12 + 8 * i)));
        }
    }
    targets
}

/// Slots taken by a method descriptor's parameters, and by its result.
fn descriptor_slots(descriptor: &str) -> (usize, usize) {
    let bytes = descriptor.as_bytes();
    let mut i = 1;
    let mut args = 0;
    while bytes[i] != b')' {
        match bytes[i] {
            b'J' | b'D' => args += 2,
            b'L' => {
                while bytes[i] != b';' {
                    i += 1;
                }
                args += 1;
            }
            b'[' => {
                while bytes[i] == b'[' {
                    i += 1;
                }
                if bytes[i] == b'L' {
                    while bytes[i] != b';' {
                        i += 1;
                    }
                }
                args += 1;
            }
            _ => args += 1,
        }
        i += 1;
    }
    let ret = match bytes[i + 1] {
        b'V' => 0,
        b'J' | b'D' => 2,
        _ => 1,
    };
    (args, ret)
}

fn field_slots(descriptor: &str) -> usize {
    if matches!(descriptor.as_bytes()[0], b'J' | b'D') { 2 } else { 1 }
}

/// (slots popped, slots pushed) by the instruction at `pc`.
fn stack_effect(class: crate::class::ClassRef, code: &[u8], pc: usize) -> Result<(usize, usize), String> {
    let u2 = |at: usize| u16::from_be_bytes([code[at], code[at + 1]]);
    let cp = class.cp();
    let member = |index: u16| cp.member_ref(index).map_err(|e| e.0.clone());
    Ok(match code[pc] {
        NOP => (0, 0),
        ACONST_NULL | ICONST_M1..=ICONST_5 | FCONST_0..=FCONST_2 | BIPUSH | SIPUSH | LDC | LDC_W => (0, 1),
        LCONST_0 | LCONST_1 | DCONST_0 | DCONST_1 | LDC2_W => (0, 2),
        ILOAD | FLOAD | ALOAD | ILOAD_0..=ILOAD_3 | FLOAD_0..=FLOAD_3 | ALOAD_0..=ALOAD_3 => (0, 1),
        LLOAD | DLOAD | LLOAD_0..=LLOAD_3 | DLOAD_0..=DLOAD_3 => (0, 2),
        IALOAD | FALOAD | AALOAD | BALOAD | CALOAD | SALOAD => (2, 1),
        LALOAD | DALOAD => (2, 2),
        ISTORE | FSTORE | ASTORE | ISTORE_0..=ISTORE_3 | FSTORE_0..=FSTORE_3 | ASTORE_0..=ASTORE_3 => (1, 0),
        LSTORE | DSTORE | LSTORE_0..=LSTORE_3 | DSTORE_0..=DSTORE_3 => (2, 0),
        IASTORE | FASTORE | AASTORE | BASTORE | CASTORE | SASTORE => (3, 0),
        LASTORE | DASTORE => (4, 0),
        POP => (1, 0),
        POP2 => (2, 0),
        DUP => (1, 2),
        DUP_X1 => (2, 3),
        DUP_X2 => (3, 4),
        DUP2 => (2, 4),
        DUP2_X1 => (3, 5),
        DUP2_X2 => (4, 6),
        SWAP => (2, 2),
        IADD | FADD | ISUB | FSUB | IMUL | FMUL | IDIV | FDIV | IREM | FREM => (2, 1),
        LADD | DADD | LSUB | DSUB | LMUL | DMUL | LDIV | DDIV | LREM | DREM => (4, 2),
        INEG | FNEG => (1, 1),
        LNEG | DNEG => (2, 2),
        ISHL | ISHR | IUSHR | IAND | IOR | IXOR => (2, 1),
        LSHL | LSHR | LUSHR => (3, 2),
        LAND | LOR | LXOR => (4, 2),
        IINC => (0, 0),
        I2L | I2D | F2L | F2D => (1, 2),
        I2F | F2I | I2B | I2C | I2S => (1, 1),
        L2I | L2F | D2I | D2F => (2, 1),
        L2D | D2L => (2, 2),
        LCMP | DCMPL | DCMPG => (4, 1),
        FCMPL | FCMPG => (2, 1),
        IFEQ..=IFLE | IFNULL | IFNONNULL | TABLESWITCH | LOOKUPSWITCH => (1, 0),
        IF_ICMPEQ..=IF_ACMPNE => (2, 0),
        GOTO | GOTO_W => (0, 0),
        IRETURN | FRETURN | ARETURN | ATHROW => (1, 0),
        LRETURN | DRETURN => (2, 0),
        RETURN => (0, 0),
        GETSTATIC => (0, field_slots(member(u2(pc + 1))?.descriptor)),
        PUTSTATIC => (field_slots(member(u2(pc + 1))?.descriptor), 0),
        GETFIELD => (1, field_slots(member(u2(pc + 1))?.descriptor)),
        PUTFIELD => (1 + field_slots(member(u2(pc + 1))?.descriptor), 0),
        INVOKEVIRTUAL | INVOKESPECIAL | INVOKEINTERFACE => {
            let (args, ret) = descriptor_slots(member(u2(pc + 1))?.descriptor);
            (args + 1, ret)
        }
        INVOKESTATIC => descriptor_slots(member(u2(pc + 1))?.descriptor),
        INVOKEDYNAMIC => {
            let Ok(Constant::InvokeDynamic { name_and_type, .. }) = cp.get(u2(pc + 1)) else {
                return Err("bad invokedynamic".into());
            };
            let (_, descriptor) = cp.name_and_type(*name_and_type).map_err(|e| e.0.clone())?;
            descriptor_slots(descriptor)
        }
        NEW => (0, 1),
        NEWARRAY | ANEWARRAY | ARRAYLENGTH | CHECKCAST | INSTANCEOF => (1, 1),
        MONITORENTER | MONITOREXIT => (1, 0),
        MULTIANEWARRAY => (code[pc + 3] as usize, 1),
        WIDE => match code[pc + 1] {
            ILOAD | FLOAD | ALOAD => (0, 1),
            LLOAD | DLOAD => (0, 2),
            ISTORE | FSTORE | ASTORE => (1, 0),
            LSTORE | DSTORE => (2, 0),
            IINC => (0, 0),
            op => return Err(format!("wide {op:#x}")),
        },
        op => return Err(format!("opcode {op:#x}")),
    })
}

// --- code generation -------------------------------------------------------------

/// The method being generated: the compiled one, or one inlined into it.
struct Ctx {
    method: MethodRef,
    code: &'static [u8],
    max_locals: usize,
    /// Operand stack entries (one per slot).
    stack: Vec<Entry>,
    /// Structured-control nesting inside the current block's code.
    nest: u32,
    block_of: std::rc::Rc<[u16]>,
    all_blocks: std::rc::Rc<[usize]>,
    /// Index of the block being generated.
    cur: usize,
    n: usize,
    depth: std::rc::Rc<[i32]>,
    /// First wasm local holding the Java locals (one i64 per slot).
    java_locals: u32,
    /// `this` is never overwritten: loads of local 0 are non-null.
    this_constant: bool,
    /// Byte offset of local 0 from FP (inlined frames sit above the caller's
    /// operand stack, where a call would put them).
    frame_offset: usize,
    /// Operand-local index (see `local_of`) of operand 0.
    op_base: usize,
    /// For an inlined method: the wasm local receiving its result.
    result: Option<(T, u32)>,
    /// Java locals the method ever writes (dirty at every block start).
    written: std::rc::Rc<[bool]>,
    /// Java locals whose wasm local may be newer than their frame slot: the
    /// slots are written only before imports and calls (whatever may read
    /// the frame: the interpreter, the collector, exception handling).
    dirty: Vec<bool>,
    /// Locals written in the current block since its last sync, and whether
    /// it had one (for the dirty-locals dataflow, see `compile`).
    written_since_sync: Vec<bool>,
    synced: bool,
}

/// An inlined frame enclosing the code being generated (for writing frame
/// records on demand).
struct Inlined {
    method: MethodRef,
    frame_offset: usize,
    /// The call instruction in the caller.
    call_pc: usize,
    /// The caller's dirty Java locals at the call: (wasm local, frame offset),
    /// written before imports in the inlined code.
    caller_dirty: Vec<(u32, usize)>,
}

struct Gen<'a> {
    c: Ctx,
    f: Function,
    /// The compilation tier (see `super::MethodJit::tier`).
    tier: u8,
    scratch: u32,
    call_site: &'a mut dyn FnMut(MethodRef, usize) -> CallSite,
    /// Inlined frames, outermost first (empty in the compiled method).
    inlined: Vec<Inlined>,
    /// Operand-local slots available (see `local_of`).
    operand_slots: usize,
    /// First wasm local of the Java-local pools for inlined frames.
    inline_java_locals: u32,
    /// Operand slots and inlining levels used so far, and the levels with
    /// locals declared (see `layout`).
    slots_used: usize,
    levels_used: usize,
    inline_levels: usize,
    /// Bytecode bytes inlined so far.
    inlined_bytes: usize,
    memory: &'a crate::memory::LinearMemory,
    vm: &'a crate::vm::Vm,
    /// Inline caches allocated (two globals each, after the tick counter).
    inline_caches: u32,
    table_size: u32,
    /// The code calls imports or other methods (which need its frame record).
    needs_record: bool,
    /// Dirty Java locals at each block's start (second pass), and per block
    /// what the first pass saw: (synced, written since the last sync).
    entry_dirty: Option<Vec<Vec<bool>>>,
    summaries: Vec<(bool, Vec<bool>)>,
    /// Structured layout of the compiled method's blocks, if it has one.
    structure: Option<std::rc::Rc<Structure>>,
    /// Open labels of the structured layout, outermost first.
    labels: Vec<Label>,
}

impl std::ops::Deref for Gen<'_> {
    type Target = Ctx;
    fn deref(&self) -> &Ctx {
        &self.c
    }
}

impl std::ops::DerefMut for Gen<'_> {
    fn deref_mut(&mut self) -> &mut Ctx {
        &mut self.c
    }
}

/// The wasm local holding operand-local `base + depth` as `t`.
/// The number of locals a raw function body declares.
fn declared_locals(body: &[u8]) -> u32 {
    fn leb(body: &[u8], at: &mut usize) -> u32 {
        let (mut value, mut shift) = (0u32, 0);
        loop {
            let byte = body[*at];
            *at += 1;
            value |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    }
    let mut at = 0;
    let groups = leb(body, &mut at);
    let mut total = 0;
    for _ in 0..groups {
        total += leb(body, &mut at);
        at += 1; // the type
    }
    total
}

fn local_of(base: usize, depth: usize, t: T) -> u32 {
    FIRST_OPERAND_LOCAL + (base + depth) as u32 * 4 + t as u32
}

/// Replaces a NaN f32 on the operand stack by the canonical NaN (via
/// `scratch`, an i32 local).
fn canonical_nan_f32(i: &mut InstructionSink<'_>, scratch: u32) {
    i.i32_reinterpret_f32()
        .local_set(scratch)
        .i32_const(0x7fc0_0000)
        .local_get(scratch)
        .local_get(scratch)
        .i32_const(0x7fff_ffff)
        .i32_and()
        .i32_const(0x7f80_0000)
        .i32_gt_u()
        .select()
        .f32_reinterpret_i32();
}

/// As `canonical_nan_f32`, for f64 (`scratch` is an i64 local).
fn canonical_nan_f64(i: &mut InstructionSink<'_>, scratch: u32) {
    i.i64_reinterpret_f64()
        .local_set(scratch)
        .i64_const(0x7ff8_0000_0000_0000)
        .local_get(scratch)
        .local_get(scratch)
        .i64_const(0x7fff_ffff_ffff_ffff)
        .i64_and()
        .i64_const(0x7ff0_0000_0000_0000)
        .i64_gt_u()
        .select()
        .f64_reinterpret_i64();
}

/// Address of the VM's region in the imported memory (see `compile`).
static MEMORY_BASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A memory access at region offset `offset` (plus the operand address).
fn mem(offset: usize, align: u32) -> MemArg {
    let base = MEMORY_BASE.load(std::sync::atomic::Ordering::Relaxed);
    MemArg { offset: base + offset as u64, align, memory_index: 0 }
}

impl Gen<'_> {
    fn ins(&mut self) -> InstructionSink<'_> {
        self.f.instructions()
    }

    fn scratch_i32(&self) -> u32 {
        self.scratch
    }

    fn scratch_i64(&self, i: u32) -> u32 {
        self.scratch + SCRATCH_I32 + i
    }

    fn slot_offset(&self, depth: usize) -> usize {
        self.frame_offset + (self.max_locals + depth) * 8
    }

    // --- operand stack ---

    fn load_slot(&mut self, offset: usize, t: T) {
        self.ins().local_get(FP);
        match t {
            T::I32 => self.ins().i32_load(mem(offset, 2)),
            T::I64 => self.ins().i64_load(mem(offset, 3)),
            T::F32 => self.ins().f32_load(mem(offset, 2)),
            T::F64 => self.ins().f64_load(mem(offset, 3)),
        };
    }

    /// Converts the raw 64-bit slot pattern on the wasm stack to `t`.
    fn from_raw(&mut self, t: T) {
        match t {
            T::I64 => {}
            T::I32 => {
                self.ins().i32_wrap_i64();
            }
            T::F32 => {
                self.ins().i32_wrap_i64().f32_reinterpret_i32();
            }
            T::F64 => {
                self.ins().f64_reinterpret_i64();
            }
        }
    }

    /// Converts a value of type `t` to its raw slot pattern (zero-extended).
    fn to_raw(&mut self, t: T) {
        match t {
            T::I64 => {}
            T::I32 => {
                self.ins().i64_extend_i32_u();
            }
            T::F32 => {
                self.ins().i32_reinterpret_f32().i64_extend_i32_u();
            }
            T::F64 => {
                self.ins().i64_reinterpret_f64();
            }
        }
    }

    /// Pushes the operand at `depth` onto the wasm stack as `t`.
    fn get(&mut self, depth: usize, t: T) {
        match self.stack[depth] {
            Entry::Mem => self.load_slot(self.slot_offset(depth), t),
            Entry::Local(u) => {
                self.f.instructions().local_get(local_of(self.c.op_base, depth, u));
                if u != t {
                    self.to_raw(u);
                    self.from_raw(t);
                }
            }
            Entry::Top => {
                self.ins().i64_const(0);
                self.from_raw(t);
            }
            Entry::This => {
                self.f.instructions().local_get(self.c.java_locals);
                self.from_raw(t);
            }
        }
    }

    /// Makes the operand at `depth` live in its local of type `t`.
    fn to_local(&mut self, depth: usize, t: T) -> u32 {
        let local = local_of(self.c.op_base, depth, t);
        if self.stack[depth] != Entry::Local(t) {
            self.get(depth, t);
            self.ins().local_set(local);
            self.stack[depth] = Entry::Local(t);
        }
        local
    }

    /// Pops the value on the wasm stack into a new operand.
    fn push(&mut self, t: T) {
        let depth = self.stack.len();
        self.f.instructions().local_set(local_of(self.c.op_base, depth, t));
        self.stack.push(Entry::Local(t));
        if matches!(t, T::I64 | T::F64) {
            self.stack.push(Entry::Top);
        }
    }

    fn top(&self, slots_from_top: usize) -> usize {
        self.stack.len() - slots_from_top
    }

    /// Emits stores putting `entries` in memory (does not change state).
    fn emit_flush(&mut self, entries: &[Entry]) {
        for (depth, entry) in entries.iter().enumerate() {
            if *entry == Entry::This {
                let offset = self.slot_offset(depth);
                self.f.instructions().local_get(FP).local_get(self.c.java_locals).i64_store(mem(offset, 3));
                continue;
            }
            if let Entry::Local(t) = *entry {
                let offset = self.slot_offset(depth);
                self.f.instructions().local_get(FP).local_get(local_of(self.c.op_base, depth, t));
                match t {
                    T::F64 => {
                        self.ins().f64_store(mem(offset, 3));
                    }
                    _ => {
                        self.to_raw(t);
                        self.ins().i64_store(mem(offset, 3));
                    }
                }
            }
        }
    }

    fn flush(&mut self) {
        let entries = std::mem::take(&mut self.stack);
        self.emit_flush(&entries);
        self.stack = vec![Entry::Mem; entries.len()];
    }

    /// Lets the interpreter run the rest of the frame from `pc`, with the
    /// operand stack `entries`, and returns its status.
    fn emit_exit(&mut self, entries: &[Entry], pc: usize) {
        self.needs_record = true;
        self.emit_flush(entries);
        self.sync_locals(false);
        self.materialize(pc);
        self.push_record();
        self.ins().i32_const(pc as i32).i32_const(entries.len() as i32).call(FN_INTERPRET);
        if self.inlined.is_empty() {
            self.ins().return_();
            return;
        }
        // An inlined method: the interpreter ran it to its return; the
        // result is in slot 0 of its frame. Continue in the caller.
        self.ins().if_(BlockType::Empty).i32_const(STATUS_EXCEPTION).return_().end();
        if let Some((t, local)) = self.c.result {
            let offset = self.c.frame_offset;
            self.load_slot(offset, t);
            self.ins().local_set(local);
        }
        let exit = self.exit_depth();
        self.ins().br(exit);
    }

    /// Branch depth from the current code of an inlined method to its exit.
    fn exit_depth(&self) -> u32 {
        (self.n - 1 - self.cur) as u32 + self.nest
    }

    /// `if (cond on wasm stack) { exit to the interpreter at pc }`, where
    /// `before` is the operand stack before the instruction.
    fn exit_if(&mut self, before: &[Entry], pc: usize) {
        self.ins().if_(BlockType::Empty);
        self.nest += 1;
        self.emit_exit(before, pc);
        self.nest -= 1;
        self.ins().end();
    }

    /// Pushes the address of the current frame's record (an inlined frame's
    /// is below its caller's, as for a call).
    fn push_record(&mut self) {
        self.ins().local_get(RECORD);
        let level = self.inlined.len();
        if level > 0 {
            self.ins().i32_const((level * frame_layout::SIZE) as i32).i32_sub();
        }
    }

    /// Before an import: records the current pc, and writes the records of
    /// the inlined frames (as if the calls had been made), the innermost at
    /// `pc`.
    fn materialize(&mut self, pc: usize) {
        let pc_of = |inlined: &[Inlined], level: usize| if level == inlined.len() { pc } else { inlined[level].call_pc };
        let root_pc = pc_of(&self.inlined, 0);
        self.ins().local_get(RECORD).i32_const(root_pc as i32).i32_store(mem(frame_layout::PC as usize, 2));
        // The enclosing frames may be resumed by the interpreter: their
        // locals must be in memory too.
        let caller_dirty: Vec<(u32, usize)> = self.inlined.iter().flat_map(|f| f.caller_dirty.iter().copied()).collect();
        for (local, offset) in caller_dirty {
            self.ins().local_get(FP).local_get(local).i64_store(mem(offset, 3));
        }
        let record_local = self.scratch + SCRATCH_I32 - 1;
        for level in 1..=self.inlined.len() {
            let frame = &self.inlined[level - 1];
            let (method, locals, frame_pc) = (frame.method, frame.frame_offset / 8, pc_of(&self.inlined, level));
            let record = |offset: u32, align: u32| mem(offset as usize, align);
            self.ins()
                .local_get(RECORD)
                .i32_const((level * frame_layout::SIZE) as i32)
                .i32_sub()
                .local_tee(record_local)
                .i64_const(method as *const crate::class::Method as usize as i64)
                .i64_store(record(frame_layout::METHOD, 3))
                .local_get(record_local)
                .i32_const(frame_pc as i32)
                .i32_store(record(frame_layout::PC, 2))
                .local_get(record_local)
                .local_get(LOCALS_INDEX)
                .i32_const(locals as i32)
                .i32_add()
                .i32_store(record(frame_layout::LOCALS, 2))
                .local_get(record_local)
                .i64_const(0)
                .i64_store(record(frame_layout::MONITOR, 3));
        }
    }

    /// After an import returned normally, under RJAVA_JSAMPLE: clears the
    /// records `materialize` wrote for inlined frames, which are not frames
    /// once the inlined code goes on (the sampler would see them as live).
    fn unmaterialize(&mut self) {
        if !crate::memory::SAMPLING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        for level in 1..=self.inlined.len() {
            self.ins()
                .local_get(RECORD)
                .i32_const((level * frame_layout::SIZE) as i32)
                .i32_sub()
                .i64_const(0)
                .i64_store(mem(frame_layout::METHOD as usize, 3));
        }
    }

    // --- control flow ---

    /// Unconditional jump to the block at `target` (stack flushed).
    fn jump(&mut self, target: usize) {
        let j = self.block_of[target] as usize;
        if self.structure.is_some() && self.inlined.is_empty() {
            let label = if j > self.cur { Label::Block(j) } else { Label::Loop(j) };
            if j <= self.cur {
                self.safepoint_poll();
            }
            // loop_structure guarantees the label is open.
            let depth = self.label_depth(label).expect("structured branch target");
            self.ins().br(depth);
            return;
        }
        if j > self.cur {
            let depth = (j - self.cur - 1) as u32 + self.nest;
            self.ins().br(depth);
        } else {
            // Backward: safepoint poll, then through the dispatcher.
            self.safepoint_poll();
            let loop_depth = (self.n - 1 - self.cur) as u32 + self.nest;
            self.ins().i32_const(j as i32).local_set(BLOCK).br(loop_depth);
        }
    }

    /// At a back edge: the safepoint import every SAFEPOINT_TICKS times, or
    /// when a collector wants the world stopped.
    fn safepoint_poll(&mut self) {
        self.needs_record = true;
        let tick = self.scratch_i32();
        let stop_flag = self.memory.stop_flag_offset();
        self.ins()
            .global_get(GLOBAL_TICKS)
            .i32_const(1)
            .i32_sub()
            .local_tee(tick)
            .global_set(GLOBAL_TICKS)
            .local_get(tick)
            .i32_eqz()
            // A collector wants the world stopped (atomic: never hoisted).
            .i32_const(stop_flag as i32)
            .i32_atomic_load(mem(0, 2))
            .i32_or()
            .if_(BlockType::Empty)
            .i32_const(SAFEPOINT_TICKS)
            .global_set(GLOBAL_TICKS);
        self.nest += 1;
        self.sync_locals(false);
        self.nest -= 1;
        self.ins().local_get(RECORD).call(FN_SAFEPOINT).end();
    }

    /// Jump to `target` if the i32 on the wasm stack is non-zero.
    fn jump_if(&mut self, target: usize) {
        let j = self.block_of[target] as usize;
        if self.structure.is_some() && self.inlined.is_empty() && j > self.cur {
            let depth = self.label_depth(Label::Block(j)).expect("structured branch target");
            self.ins().br_if(depth);
            return;
        }
        if j > self.cur {
            let depth = (j - self.cur - 1) as u32 + self.nest;
            self.ins().br_if(depth);
        } else {
            self.ins().if_(BlockType::Empty);
            self.nest += 1;
            self.jump(target);
            self.nest -= 1;
            self.ins().end();
        }
    }

    /// Jumps to `targets[i]` for the trampoline index `i` computed by `select`
    /// (which runs inside the innermost of `targets.len()` nested blocks and
    /// must branch out of it).
    fn multiway(&mut self, targets: &[usize], select: impl FnOnce(&mut Self)) {
        let m = targets.len() as u32;
        for _ in 0..m {
            self.ins().block(BlockType::Empty);
        }
        self.nest += m;
        select(self);
        for (i, &target) in targets.iter().enumerate() {
            self.ins().end();
            self.nest -= 1;
            let _ = i;
            self.jump(target);
        }
    }

    // --- the body ---

    fn body(&mut self) -> Result<(), String> {
        let n = self.n as u32;
        self.ins().local_get(RECORD).i32_load(mem(frame_layout::LOCALS as usize, 2)).local_set(LOCALS_INDEX);
        // The Java locals: just the arguments for a call (block 0), all of
        // them when resuming the frame elsewhere.
        let args = self.method.arg_slots as usize;
        self.ins().local_get(BLOCK).i32_eqz().if_(BlockType::Empty);
        for i in 0..args.min(self.max_locals) {
            self.f.instructions().local_get(FP).i64_load(mem(i * 8, 3)).local_set(self.c.java_locals + i as u32);
        }
        self.ins().else_();
        for i in 0..self.max_locals {
            self.f.instructions().local_get(FP).i64_load(mem(i * 8, 3)).local_set(self.c.java_locals + i as u32);
        }
        self.ins().end();
        if let Some(structure) = self.structure.clone() {
            return self.structured_body(&structure);
        }
        // loop $dispatch { block B_{n-1} ... block B_0 { br_table } code_0 } code_1 ... }
        self.ins().loop_(BlockType::Empty);
        for _ in 0..n {
            self.ins().block(BlockType::Empty);
        }
        self.ins().local_get(BLOCK).br_table(0..n, 0);
        for b in 0..self.n {
            self.ins().end();
            self.cur = b;
            self.nest = 0;
            self.block(b)?;
        }
        self.ins().end(); // loop
        self.ins().unreachable().end();
        Ok(())
    }

    /// Blocks in order, each loop a wasm `loop`, forward branches to blocks
    /// opened at the start of the scope (function or loop) containing their
    /// target:
    /// `block B.. { block $entry { br_table } code_0 } code_1 ... loop L_h { block B.. code_h ... } ...`
    fn structured_body(&mut self, structure: &Structure) -> Result<(), String> {
        let n = self.n;
        let in_scope = |scope: Option<usize>| -> Vec<usize> { (1..n).filter(|&j| structure.scope[j] == scope && Some(j) != scope).collect() };
        self.open_blocks(&in_scope(None));
        // Entry dispatch (only blocks of the function scope can be entered).
        self.ins().block(BlockType::Empty);
        let depth_of = |labels: &[Label], b: usize| -> u32 {
            match labels.iter().rposition(|&l| l == Label::Block(b)) {
                Some(i) => (labels.len() - i) as u32, // + the entry block
                None => 0,
            }
        };
        let targets: Vec<u32> = (0..n).map(|b| if b == 0 { 0 } else { depth_of(&self.labels, b) }).collect();
        self.ins().local_get(BLOCK).br_table(targets, 0).end();
        for b in 0..n {
            if b > 0 {
                if self.labels.last() != Some(&Label::Block(b)) {
                    return Err("structured layout: misplaced block".into());
                }
                self.labels.pop();
                self.ins().end();
            }
            if structure.loop_end[b].is_some() {
                self.ins().loop_(BlockType::Empty);
                self.labels.push(Label::Loop(b));
                self.open_blocks(&in_scope(Some(b)));
            }
            self.c.cur = b;
            self.c.nest = 0;
            self.block(b)?;
            while let Some(&Label::Loop(h)) = self.labels.last() {
                if structure.loop_end[h] != Some(b) {
                    break;
                }
                self.labels.pop();
                self.ins().end();
            }
        }
        if !self.labels.is_empty() {
            return Err("structured layout: labels left open".into());
        }
        self.ins().unreachable().end();
        Ok(())
    }

    /// Opens `block`s for `targets` (innermost: the first).
    fn open_blocks(&mut self, targets: &[usize]) {
        for &j in targets.iter().rev() {
            self.ins().block(BlockType::Empty);
            self.labels.push(Label::Block(j));
        }
    }

    /// Branch depth to a structured-layout label from the current code.
    fn label_depth(&self, label: Label) -> Option<u32> {
        let i = self.labels.iter().rposition(|&l| l == label)?;
        Some((self.labels.len() - 1 - i) as u32 + self.nest)
    }

    fn block(&mut self, b: usize) -> Result<(), String> {
        let root = self.inlined.is_empty();
        let result = self.block_code(b);
        if root {
            self.summaries[b] = (self.c.synced, self.c.written_since_sync.clone());
        }
        result
    }

    fn block_code(&mut self, b: usize) -> Result<(), String> {
        let start = self.all_blocks[b];
        let end = self.all_blocks.get(b + 1).copied().unwrap_or(self.code.len());
        if self.depth[start] < 0 {
            self.ins().unreachable();
            return Ok(());
        }
        self.stack = vec![Entry::Mem; self.depth[start] as usize];
        self.c.dirty = match &self.entry_dirty {
            Some(entries) if self.inlined.is_empty() => entries[b].clone(),
            // Unknown predecessors: anything the method writes may be dirty.
            _ => self.written.to_vec(),
        };
        self.c.written_since_sync.iter_mut().for_each(|g| *g = false);
        self.c.synced = false;
        let mut pc = start;
        while pc < end {
            debug_assert_eq!(self.stack.len() as i32, self.depth[pc], "{:?} @{pc}", self.method);
            let len = instruction_length(self.code, pc).unwrap();
            if !self.instruction(pc)? {
                return Ok(()); // control left the block
            }
            pc += len;
        }
        // Fall through into the next block.
        self.flush();
        Ok(())
    }

    fn u2(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.code[at], self.code[at + 1]])
    }

    /// Lets the interpreter execute the instruction in place.
    fn step(&mut self, pc: usize) -> Result<bool, String> {
        self.needs_record = true;
        let (pops, pushes) = stack_effect(self.method.class(), self.code, pc)?;
        self.flush();
        let depth = self.stack.len();
        self.sync_locals(true);
        self.materialize(pc);
        self.push_record();
        self.ins()
            .i32_const(pc as i32)
            .i32_const(depth as i32)
            .call(FN_STEP)
            .if_(BlockType::Empty)
            .i32_const(STATUS_EXCEPTION)
            .return_()
            .end();
        self.unmaterialize();
        self.stack = vec![Entry::Mem; depth - pops + pushes];
        Ok(true)
    }

    /// xRETURN: the result goes to slot 0 of the frame.
    fn return_(&mut self, op: u8) -> Result<bool, String> {
        if !self.inlined.is_empty() {
            // An inlined method: result to the caller's operand local.
            if let Some((t, local)) = self.c.result {
                let depth = self.top(if matches!(op, LRETURN | DRETURN) { 2 } else { 1 });
                self.get(depth, t);
                self.ins().local_set(local);
            }
            let exit = self.exit_depth();
            self.ins().br(exit);
            return Ok(false);
        }
        if op != RETURN {
            let depth = self.top(if matches!(op, LRETURN | DRETURN) { 2 } else { 1 });
            self.ins().local_get(FP);
            self.get(depth, T::I64);
            self.ins().i64_store(mem(0, 3));
        }
        self.ins().i32_const(STATUS_RETURN).return_();
        Ok(false)
    }

    /// Call instructions.
    fn invoke(&mut self, pc: usize) -> Result<bool, String> {
        let op = self.code[pc];
        if op == INVOKESTATIC && self.inline_static(pc) {
            return Ok(true);
        }
        if op == INVOKEVIRTUAL && self.inline_unsafe(pc) {
            return Ok(true);
        }
        let (pops, pushes) = stack_effect(self.method.class(), self.code, pc)?;
        self.needs_record = true;
        let receiver_is_this = op != INVOKESTATIC && op != INVOKEDYNAMIC && self.stack[self.stack.len() - pops] == Entry::This;
        self.flush();
        let depth = self.stack.len();
        let args = depth - pops;
        if let Some((op, arg_count)) = self.pure_native(pc) {
            // The `pure` import, else (-1) the `invoke` import; the result
            // goes through memory.
            let result = self.scratch_i32();
            let result_slot = self.slot_offset(args);
            self.emit_pure(op, arg_count, args, result);
            self.ins()
                .local_get(result)
                .i32_const(-1)
                .i32_ne()
                .if_(BlockType::Result(ValType::I32))
                .local_get(FP)
                .local_get(result)
                .i64_extend_i32_u()
                .i64_store(mem(result_slot, 3))
                .i32_const(STATUS_RETURN)
                .else_();
            // Not answered (e.g. hashCode overridden): the call as usual.
            self.sync_locals(false);
            let site = (self.call_site)(self.c.method, pc);
            self.emit_call(pc, &site, depth, args, receiver_is_this);
            self.ins().end().if_(BlockType::Empty).i32_const(STATUS_EXCEPTION).return_().end();
            self.unmaterialize();
            self.stack = vec![Entry::Mem; depth - pops + pushes];
            return Ok(true);
        }
        let site = (self.call_site)(self.c.method, pc);
        let inline = self.inline_candidate(&site, args);
        if inline.is_none() {
            // A real call: the frame must be current.
            self.sync_locals(true);
        }
        let Some((target, guard)) = inline else {
            self.emit_call(pc, &site, depth, args, receiver_is_this);
            self.ins().if_(BlockType::Empty).i32_const(STATUS_EXCEPTION).return_().end();
            self.unmaterialize();
            self.stack = vec![Entry::Mem; depth - pops + pushes];
            return Ok(true);
        };
        let null_check = op != INVOKESTATIC && !receiver_is_this;
        if guard.is_none() && !null_check {
            // Known target, receiver (if any) non-null: just the callee's code.
            return self.inline_call(pc, target, args).map(|()| true);
        }
        // Guarded: inlined if the receiver is non-null (and of the expected
        // class), else a real call; the result goes through memory.
        self.save_pc(pc);
        let receiver_slot = self.slot_offset(args);
        if null_check {
            self.ins().local_get(FP).i32_load(mem(receiver_slot, 2)).i32_eqz().if_(BlockType::Result(ValType::I32));
            self.sync_locals(false);
            self.emit_generic_invoke(pc, depth);
            self.ins().else_();
        }
        let counters = guard.and_then(|_| self.site_counters(pc, true, target));
        if let Some(class) = guard {
            self.count(counters, 0);
            self.ins()
                .local_get(FP)
                .i32_load(mem(receiver_slot, 2))
                .i32_load(mem(0, 2))
                .i32_const(class as i32)
                .i32_eq()
                .if_(BlockType::Result(ValType::I32));
        }
        self.inline_call(pc, target, args)?;
        self.flush();
        self.ins().i32_const(STATUS_RETURN);
        if guard.is_some() {
            self.ins().else_();
            self.count(counters, 1);
            self.sync_locals(false);
            self.emit_call(pc, &site, depth, args, receiver_is_this);
            self.ins().end();
        }
        if null_check {
            self.ins().end();
        }
        self.ins().if_(BlockType::Empty).i32_const(STATUS_EXCEPTION).return_().end();
        self.unmaterialize();
        self.stack = vec![Entry::Mem; depth - pops + pushes];
        Ok(true)
    }

    /// The `invoke` import for the call at `pc` (pushes its status).
    fn emit_generic_invoke(&mut self, pc: usize, depth: usize) {
        self.materialize(pc);
        self.push_record();
        self.ins().i32_const(pc as i32).i32_const(depth as i32).call(FN_INVOKE);
    }

    /// A call (not inlined) with the operand stack flushed; pushes the status.
    fn emit_call(&mut self, pc: usize, site: &CallSite, depth: usize, args: usize, receiver_is_this: bool) {
        // The callee's frame: locals from the arguments, record below ours.
        let callee_fp_offset = self.slot_offset(args);
        let callee_locals = (callee_fp_offset / 8) as i32;
        let indirect = |g: &mut Self, index: &dyn Fn(&mut InstructionSink<'_>)| {
            g.ins().local_get(FP).i32_const(callee_fp_offset as i32).i32_add();
            g.push_record();
            let mut ins = g.f.instructions();
            ins.i32_const(frame_layout::SIZE as i32).i32_sub().local_get(LOCALS_INDEX).i32_const(callee_locals).i32_add();
            index(&mut ins);
            index(&mut ins);
            ins.call_indirect(0, TYPE_CALL);
        };
        self.save_pc(pc);
        match *site {
            CallSite::Generic => self.emit_generic_invoke(pc, depth),
            CallSite::Handle(intrinsic, [form, vmentry, member_method, vmtarget]) => {
                use crate::class::Intrinsic;
                let scratch = self.scratch_i32();
                let (key, object, class, result) = (scratch + 2, scratch + 5, scratch + 6, scratch + 7);
                // Any null on the way: result -1, and the `invoke` import throws.
                let null = |g: &mut Self| {
                    g.ins().local_get(object).i32_eqz().br_if(0);
                };
                self.ins().i32_const(-1).local_set(result).block(BlockType::Empty);
                let receiver_slot = self.slot_offset(args);
                if intrinsic == Intrinsic::InvokeBasic {
                    // mh.form.vmentry
                    self.ins().local_get(FP).i32_load(mem(receiver_slot, 2)).local_set(object);
                    null(self);
                    self.ins().local_get(object).i32_load(mem(form as usize, 2)).local_set(object);
                    null(self);
                    self.ins().local_get(object).i32_load(mem(vmentry as usize, 2)).local_set(object);
                } else {
                    // The trailing MemberName argument.
                    let member_name_slot = self.slot_offset(depth - 1);
                    self.ins().local_get(FP).i32_load(mem(member_name_slot, 2)).local_set(object);
                }
                null(self);
                self.ins().local_get(object).i32_load(mem(member_method as usize, 2)).local_set(object);
                null(self);
                self.ins().local_get(object).i32_load(mem(vmtarget as usize, 2)).local_set(key);
                self.ins().i32_const(NO_CLASS as i32).local_set(class);
                if matches!(intrinsic, Intrinsic::LinkToVirtual | Intrinsic::LinkToInterface | Intrinsic::LinkToSpecial) {
                    self.ins().local_get(FP).i32_load(mem(receiver_slot, 2)).local_set(object);
                    null(self);
                    if intrinsic != Intrinsic::LinkToSpecial {
                        self.ins().local_get(object).i32_load(mem(0, 2)).local_set(class);
                    }
                }
                self.dispatch(class, key);
                self.ins().local_set(result).end();
                self.ins().local_get(result).i32_const(0).i32_lt_s().if_(BlockType::Result(ValType::I32));
                self.emit_generic_invoke(pc, depth);
                self.ins().else_();
                indirect(self, &|ins| {
                    ins.local_get(result);
                });
                self.ins().end();
            }
            CallSite::Linked(invoker, appendix) => {
                if let Some(cell) = appendix {
                    let slot = self.slot_offset(depth);
                    self.ins()
                        .local_get(FP)
                        .i32_const(cell as i32)
                        .i32_load(MemArg { offset: 0, align: 2, memory_index: 0 })
                        .i64_extend_i32_u()
                        .i64_store(mem(slot, 3));
                }
                self.table_size = self.table_size.max(invoker.global_index + 1);
                let index = invoker.global_index as i32;
                indirect(self, &|ins| {
                    ins.i32_const(index);
                });
            }
            CallSite::Direct(target) => {
                let index = target.global_index as i32;
                self.table_size = self.table_size.max(target.global_index + 1);
                let call = |g: &mut Self| {
                    indirect(g, &|ins| {
                        ins.i32_const(index);
                    })
                };
                if self.code[pc] == INVOKESPECIAL && !receiver_is_this {
                    // The receiver may be null (e.g. private methods in old class files).
                    let receiver_slot = self.slot_offset(args);
                    self.ins().local_get(FP).i32_load(mem(receiver_slot, 2)).i32_eqz().if_(BlockType::Result(ValType::I32));
                    self.emit_generic_invoke(pc, depth);
                    self.ins().else_();
                    call(self);
                    self.ins().end();
                } else {
                    call(self);
                }
            }
            CallSite::Virtual(_) => {
                let counters = match self.method.class().cp_cache[self.u2(pc + 1) as usize].get() {
                    Some(Resolved::Method(resolved)) => self.site_counters(pc, false, resolved),
                    _ => None,
                };
                self.count(counters, 0);
                let (cache_class, cache_target) = self.inline_cache();
                let receiver = self.scratch_i32();
                let receiver_slot = self.slot_offset(args);
                self.ins().local_get(FP).i32_load(mem(receiver_slot, 2)).local_tee(receiver);
                if receiver_is_this {
                    self.ins().drop().i32_const(0);
                } else {
                    self.ins().i32_eqz();
                }
                self.ins().if_(BlockType::Result(ValType::I32));
                self.emit_generic_invoke(pc, depth); // throws NullPointerException
                self.ins()
                    .else_()
                    .local_get(receiver)
                    .i32_load(mem(0, 2)) // class id
                    .local_tee(receiver)
                    .global_get(cache_class)
                    .i32_ne()
                    .if_(BlockType::Empty);
                self.count(counters, 1);
                self.dispatch_miss(pc, receiver, cache_target);
                self.ins()
                    .local_get(receiver)
                    .global_set(cache_class)
                    .end()
                    .global_get(cache_target)
                    .i32_const(0)
                    .i32_lt_s()
                    .if_(BlockType::Result(ValType::I32));
                self.emit_generic_invoke(pc, depth); // not callable through the table
                self.ins().else_();
                indirect(self, &|ins| {
                    ins.global_get(cache_target);
                });
                self.ins().end().end();
            }
        }
    }

    /// The method to inline for a call (and the receiver class it is only
    /// valid for), if any: small, straight-line or forward-branching, no
    /// handlers, within the nesting and size budgets.
    fn inline_candidate(&self, site: &CallSite, args: usize) -> Option<(MethodRef, Option<u32>)> {
        let limits = inline_limits(self.tier);
        if self.inlined.len() >= limits.depth.min(self.inline_levels) || self.inlined_bytes >= limits.bytes {
            return None;
        }
        let (target, guard) = match *site {
            CallSite::Direct(target) => (target, None),
            CallSite::Virtual(Some((class, target))) => (target, Some(class)),
            _ => return None,
        };
        let code = target.code.as_ref()?;
        let recursive = std::ptr::eq(target, self.c.method) || self.inlined.iter().any(|f| std::ptr::eq(f.method, target));
        let fits = self.c.op_base + args + code.max_locals as usize + code.max_stack as usize + 2 <= self.operand_slots;
        let simple = code.code.len() <= limits.size
            && code.exception_table.is_empty()
            && code.max_locals as usize <= MAX_INLINE_LOCALS
            && target.access_flags & classfile::access::SYNCHRONIZED == 0
            && forward_only(&code.code);
        (simple && fits && !recursive).then_some((target, guard))
    }

    /// Generates `target`'s code in place of a call whose arguments are in
    /// memory from operand `args`; its result becomes the operand there.
    fn inline_call(&mut self, pc: usize, target: MethodRef, args: usize) -> Result<(), String> {
        let code = target.code.as_ref().unwrap();
        let analysis = analyze(target)?;
        let mut block_of = vec![NO_BLOCK; code.code.len()];
        for (index, &start) in analysis.blocks.iter().enumerate() {
            block_of[start] = index as u16;
        }
        let level = self.inlined.len();
        let frame_offset = self.slot_offset(args);
        let result = target.ret.map(|kind| (kind_type(kind), local_of(self.c.op_base, args, kind_type(kind))));
        let java_locals = self.inline_java_locals + (level * MAX_INLINE_LOCALS) as u32;
        self.slots_used = self.slots_used.max(self.c.op_base + args + code.max_locals as usize + code.max_stack as usize + 2);
        self.levels_used = self.levels_used.max(level + 1);
        let ctx = Ctx {
            method: target,
            code: &code.code,
            max_locals: code.max_locals as usize,
            stack: Vec::new(),
            nest: 0,
            n: analysis.blocks.len(),
            block_of: block_of.into(),
            all_blocks: analysis.blocks.into(),
            cur: 0,
            depth: analysis.depth.into(),
            java_locals,
            this_constant: !target.is_static() && !stores_local_0(&code.code),
            frame_offset,
            op_base: self.c.op_base + args + code.max_locals as usize,
            result,
            written: written_locals(&code.code, code.max_locals as usize).into(),
            dirty: vec![false; code.max_locals as usize],
            written_since_sync: vec![false; code.max_locals as usize],
            synced: false,
        };
        let caller_dirty = self.dirty_slots();
        let caller = std::mem::replace(&mut self.c, ctx);
        self.inlined.push(Inlined { method: target, frame_offset, call_pc: pc, caller_dirty });
        for i in 0..target.arg_slots as usize {
            self.ins().local_get(FP).i64_load(mem(frame_offset + i * 8, 3)).local_set(java_locals + i as u32);
        }
        // block $exit { block B_{n-1} ... block B_1 { code_0 } code_1 ... } code_{n-1} }
        let n = self.n;
        self.ins().block(BlockType::Empty);
        for _ in 1..n {
            self.ins().block(BlockType::Empty);
        }
        let mut generated = Ok(());
        for b in 0..n {
            if b > 0 {
                self.ins().end();
            }
            self.c.cur = b;
            self.c.nest = 0;
            generated = generated.and_then(|()| self.block(b));
        }
        self.ins().end();
        self.inlined.pop();
        self.c = caller;
        generated?;
        self.inlined_bytes += code.code.len();
        self.stack.truncate(args);
        if let Some((t, _)) = result {
            self.stack.push(Entry::Local(t));
            if matches!(t, T::I64 | T::F64) {
                self.stack.push(Entry::Top);
            }
        }
        Ok(())
    }

    /// RJAVA_JIT_STATS under wasm: counters for the virtual call site at
    /// `pc` (see `stats::Site`), as their address.
    fn site_counters(&self, pc: usize, guarded: bool, target: MethodRef) -> Option<u64> {
        if !cfg!(target_family = "wasm") {
            return None;
        }
        let stats = super::stats::get()?;
        let counters: &'static [std::sync::atomic::AtomicU32; 2] = Box::leak(Box::default());
        let site = super::stats::Site { counters, guarded, caller: self.c.method, pc: pc as u32, target };
        stats.sites.lock().unwrap().push(site);
        Some(counters.as_ptr() as usize as u64)
    }

    /// Increments the counter at `address` (not atomically: statistics).
    fn count(&mut self, address: Option<u64>, index: u64) {
        if let Some(address) = address {
            let at = MemArg { offset: address + 4 * index, align: 2, memory_index: 0 };
            self.ins().i32_const(0).i32_const(0).i32_load(at).i32_const(1).i32_add().i32_store(at);
        }
    }

    /// Allocates an inline cache: two mutable i32 globals (initially -1).
    fn inline_cache(&mut self) -> (u32, u32) {
        let first = 1 + 2 * self.inline_caches;
        self.inline_caches += 1;
        (first, first + 1)
    }

    /// Calls `step` for the instruction at `pc` with the operand stack
    /// `before` flushed, returning on an exception (inside a slow path;
    /// does not change the generator's stack state).
    fn emit_step(&mut self, before: &[Entry], pc: usize) {
        self.needs_record = true;
        self.emit_flush(before);
        self.sync_locals(false);
        self.materialize(pc);
        self.push_record();
        self.ins()
            .i32_const(pc as i32)
            .i32_const(before.len() as i32)
            .call(FN_STEP)
            .if_(BlockType::Empty)
            .i32_const(STATUS_EXCEPTION)
            .return_()
            .end();
        self.unmaterialize();
    }

    /// The class id of the resolved class constant of the instruction at `pc`.
    fn resolved_class_operand(&self, pc: usize) -> Option<u32> {
        match self.method.class().cp_cache[self.u2(pc + 1) as usize].get() {
            Some(Resolved::Class(class)) => Some(class.id),
            _ => None,
        }
    }

    /// checkcast with a per-site cache of the last class that passed; other
    /// classes are checked by the `subtype` import if the target class is
    /// resolved, else by the interpreter.
    fn checkcast(&mut self, pc: usize) {
        let depth = self.top(1);
        let obj = self.to_local(depth, T::I32);
        let before = self.stack.clone();
        let (cached, _) = self.inline_cache();
        let class = self.scratch_i32();
        self.ins()
            .local_get(obj)
            .if_(BlockType::Empty)
            .local_get(obj)
            .i32_load(mem(0, 2))
            .local_tee(class)
            .global_get(cached)
            .i32_ne()
            .if_(BlockType::Empty);
        if let Some(target) = self.resolved_class_operand(pc) {
            self.ins()
                .local_get(class)
                .i32_const(target as i32)
                .call(FN_SUBTYPE)
                .i32_eqz()
                .if_(BlockType::Empty);
            self.emit_step(&before, pc); // throws ClassCastException
            self.ins().end();
        } else {
            self.emit_step(&before, pc); // throws ClassCastException if it fails
        }
        self.ins().local_get(class).global_set(cached).end().end();
    }

    /// instanceof with a per-site cache of (class, result).
    fn instanceof(&mut self, pc: usize) {
        let depth = self.top(1);
        let obj = self.to_local(depth, T::I32);
        let before = self.stack.clone();
        let (cached, result) = self.inline_cache();
        let class = self.scratch_i32();
        let slot = self.slot_offset(depth);
        self.ins()
            .local_get(obj)
            .i32_eqz()
            .if_(BlockType::Result(ValType::I32))
            .i32_const(0)
            .else_()
            .local_get(obj)
            .i32_load(mem(0, 2))
            .local_tee(class)
            .global_get(cached)
            .i32_eq()
            .if_(BlockType::Result(ValType::I32))
            .global_get(result)
            .else_();
        if let Some(target) = self.resolved_class_operand(pc) {
            self.ins().local_get(class).i32_const(target as i32).call(FN_SUBTYPE).global_set(result);
        } else {
            self.emit_step(&before, pc);
            self.ins().local_get(FP).i32_load(mem(slot, 2)).global_set(result);
        }
        self.ins()
            .local_get(class)
            .global_set(cached)
            .global_get(result)
            .end()
            .end();
        self.stack.truncate(depth);
        self.push(T::I32);
    }

    /// Math.max/min on two operands of type `t`.
    fn min_max(&mut self, t: T, max: bool) {
        let width = if t == T::I64 { 2 } else { 1 };
        let a_depth = self.top(2 * width);
        let a = self.to_local(a_depth, t);
        let b = self.to_local(self.top(width), t);
        let mut ins = self.f.instructions();
        ins.local_get(a).local_get(b).local_get(a).local_get(b);
        match (t, max) {
            (T::I64, true) => ins.i64_gt_s(),
            (T::I64, false) => ins.i64_lt_s(),
            (_, true) => ins.i32_gt_s(),
            (_, false) => ins.i32_lt_s(),
        };
        ins.select();
        self.stack.truncate(a_depth);
        self.push(t);
    }

    /// Unsafe.compareAndSet{Int,Long,Reference} as an atomic compare-exchange
    /// (wasm only, like `inline_unsafe`).
    fn inline_cas(&mut self, m: MethodRef, kind: &str) -> bool {
        use T::*;
        let (t, desc) = match kind {
            "Int" => (I32, "I"),
            "Long" => (I64, "J"),
            "Reference" => (I32, "Ljava/lang/Object;"),
            _ => return false,
        };
        if m.descriptor != format!("(Ljava/lang/Object;J{desc}{desc})Z") {
            return false;
        }
        let width = if t == I64 { 2 } else { 1 };
        let receiver = self.top(4 + 2 * width);
        let (base, offset, expected, new) = (receiver + 1, receiver + 2, receiver + 4, receiver + 4 + width);
        let region = MEMORY_BASE.load(std::sync::atomic::Ordering::Relaxed) as i32;
        let address = self.scratch_i32();
        self.get(offset, I64);
        self.ins().i32_wrap_i64();
        self.get(base, I32);
        self.ins()
            .local_tee(address)
            .i32_const(region)
            .i32_add()
            .i32_const(0)
            .local_get(address)
            .select()
            .i32_add();
        self.get(expected, t);
        self.get(new, t);
        if t == I64 {
            self.ins().i64_atomic_rmw_cmpxchg(MemArg { offset: 0, align: 3, memory_index: 0 });
            self.get(expected, I64);
            self.ins().i64_eq();
        } else {
            self.ins().i32_atomic_rmw_cmpxchg(MemArg { offset: 0, align: 2, memory_index: 0 });
            self.get(expected, I32);
            self.ins().i32_eq();
        }
        self.stack.truncate(receiver);
        self.push(I32);
        true
    }

    /// The answer to `pure` operation `op` on the arguments in memory from
    /// operand `args`, into local `result` (-1: not answered). Common cases
    /// are answered inline; the rest by the import.
    fn emit_pure(&mut self, op: u32, arg_count: usize, args: usize, result: u32) {
        use super::pure::*;
        let scratch = self.scratch_i32();
        let (a, b, class) = (scratch + 1, scratch + 2, scratch + 3);
        for (i, local) in [a, b].into_iter().enumerate() {
            if i < arg_count {
                let slot = self.slot_offset(args + i);
                self.ins().local_get(FP).i32_load(mem(slot, 2)).local_set(local);
            } else {
                self.ins().i32_const(0).local_set(local);
            }
        }
        let call = |g: &mut Self| {
            g.ins().i32_const(op as i32).local_get(a).local_get(b).call(FN_PURE);
        };
        // The identity hash in the header (0: not assigned yet).
        let header_hash = |g: &mut Self, obj: u32| {
            g.ins().local_get(obj).i32_load(mem(4, 2));
        };
        match op {
            IDENTITY_HASH => {
                // null: 0; an assigned hash: it; else the import assigns one.
                self.ins().local_get(a).i32_eqz().if_(BlockType::Result(ValType::I32)).i32_const(0).else_();
                header_hash(self, a);
                self.ins().local_tee(result).if_(BlockType::Result(ValType::I32)).local_get(result).else_();
                call(self);
                self.ins().end().end().local_set(result);
            }
            OBJECT_HASH => {
                // A per-site cache of a class that does not override hashCode.
                let (cached, _) = self.inline_cache();
                self.ins()
                    .local_get(a)
                    .if_(BlockType::Result(ValType::I32))
                    .local_get(a)
                    .i32_load(mem(0, 2))
                    .local_tee(class)
                    .global_get(cached)
                    .i32_eq()
                    .if_(BlockType::Result(ValType::I32));
                header_hash(self, a);
                self.ins().else_().i32_const(0).end().local_tee(result).if_(BlockType::Result(ValType::I32)).local_get(result).else_();
                call(self);
                self.ins()
                    .local_tee(result)
                    .i32_const(-1)
                    .i32_ne()
                    .if_(BlockType::Empty)
                    .local_get(class)
                    .global_set(cached)
                    .end()
                    .local_get(result)
                    .end()
                    .else_();
                call(self);
                self.ins().end().local_set(result);
            }
            IS_INSTANCE => {
                // a (the Class) non-null, b non-null: the same class, or the
                // pair that last went through the import.
                let (cached_mirror_class, cached_object_class) = self.inline_cache();
                let (cached_result, _) = self.inline_cache();
                let class_id = self.vm.mirror_class_id_offset() as usize;
                let mirror_class = scratch + 4;
                self.ins()
                    .local_get(a)
                    .local_get(b)
                    .i32_const(0)
                    .i32_ne()
                    .i32_const(0)
                    .select()
                    .if_(BlockType::Result(ValType::I32))
                    .local_get(a)
                    .i32_load(mem(class_id, 2))
                    .local_tee(mirror_class)
                    .local_get(b)
                    .i32_load(mem(0, 2))
                    .local_tee(class)
                    .i32_eq()
                    .if_(BlockType::Result(ValType::I32))
                    .i32_const(1)
                    .else_()
                    .local_get(mirror_class)
                    .global_get(cached_mirror_class)
                    .i32_eq()
                    .local_get(class)
                    .global_get(cached_object_class)
                    .i32_eq()
                    .i32_and()
                    .if_(BlockType::Result(ValType::I32))
                    .global_get(cached_result)
                    .else_();
                call(self);
                self.ins()
                    .local_tee(result)
                    .global_set(cached_result)
                    .local_get(mirror_class)
                    .global_set(cached_mirror_class)
                    .local_get(class)
                    .global_set(cached_object_class)
                    .local_get(result)
                    .end()
                    .end()
                    .else_();
                call(self);
                self.ins().end().local_set(result);
            }
            _ => {
                call(self);
                self.ins().local_set(result);
            }
        }
    }

    /// Natives answered by the `pure` import: (operation, argument slots).
    fn pure_native(&self, pc: usize) -> Option<(u32, usize)> {
        use super::pure::*;
        let index = self.u2(pc + 1);
        let Some(Resolved::Method(m)) = self.method.class().cp_cache[index as usize].get() else { return None };
        let op = self.code[pc];
        Some(match (op, m.class().name.as_str(), m.name.as_str(), m.descriptor.as_str()) {
            (INVOKESTATIC, "java/lang/Thread", "currentThread", "()Ljava/lang/Thread;") => (CURRENT_THREAD, 0),
            (INVOKESTATIC, "java/lang/System", "identityHashCode", "(Ljava/lang/Object;)I") => (IDENTITY_HASH, 1),
            (INVOKEVIRTUAL, "java/lang/Object", "getClass", "()Ljava/lang/Class;") => (GET_CLASS, 1),
            (INVOKEVIRTUAL, "java/lang/Object", "hashCode", "()I") => (OBJECT_HASH, 1),
            (INVOKEVIRTUAL, "java/lang/Class", "isInstance", "(Ljava/lang/Object;)Z") => (IS_INSTANCE, 2),
            (INVOKEVIRTUAL, "jdk/internal/misc/Unsafe", "allocateInstance", "(Ljava/lang/Class;)Ljava/lang/Object;") => (ALLOCATE_INSTANCE, 2),
            _ => return None,
        })
    }

    /// Unsafe.get*/put* (plain and volatile) compiled inline as loads and
    /// stores: at `base + offset` in the heap, or at the native address
    /// `offset` if the base is null. Only under wasm, where native memory is
    /// the linear memory compiled code addresses.
    fn inline_unsafe(&mut self, pc: usize) -> bool {
        if !cfg!(target_family = "wasm") {
            return false;
        }
        let index = self.u2(pc + 1);
        let Some(Resolved::Method(m)) = self.method.class().cp_cache[index as usize].get() else { return false };
        let m = *m;
        if m.class().name != "jdk/internal/misc/Unsafe" || !m.is_native() || m.is_static() {
            return false;
        }
        let name = m.name.as_str();
        if let Some(kind) = name.strip_prefix("compareAndSet") {
            return self.inline_cas(m, kind);
        }
        let (get, rest) = if let Some(rest) = name.strip_prefix("get") {
            (true, rest)
        } else if let Some(rest) = name.strip_prefix("put") {
            (false, rest)
        } else {
            return false;
        };
        let (kind, volatile) = match rest.strip_suffix("Volatile") {
            Some(kind) => (kind, true),
            None => (rest, false),
        };
        use T::*;
        let (t, desc) = match kind {
            "Int" => (I32, "I"),
            "Long" => (I64, "J"),
            "Boolean" => (I32, "Z"),
            "Byte" => (I32, "B"),
            "Short" => (I32, "S"),
            "Char" => (I32, "C"),
            "Float" => (F32, "F"),
            "Double" => (F64, "D"),
            "Reference" => (I32, "Ljava/lang/Object;"),
            _ => return false,
        };
        let expected = if get { format!("(Ljava/lang/Object;J){desc}") } else { format!("(Ljava/lang/Object;J{desc})V") };
        if m.descriptor != expected {
            return false;
        }
        let width = if matches!(t, I64 | F64) { 2 } else { 1 };
        let receiver = self.top(if get { 4 } else { 4 + width });
        let (base, offset) = (receiver + 1, receiver + 2);
        let region = MEMORY_BASE.load(std::sync::atomic::Ordering::Relaxed) as i32;
        let address = self.scratch_i32();
        // address = offset + (base != 0 ? base + region : 0)
        self.get(offset, I64);
        self.ins().i32_wrap_i64();
        self.get(base, I32);
        self.ins()
            .local_tee(address)
            .i32_const(region)
            .i32_add()
            .i32_const(0)
            .local_get(address)
            .select()
            .i32_add()
            .local_set(address);
        let at = |align: u32| MemArg { offset: 0, align, memory_index: 0 };
        if get {
            if volatile {
                self.ins().atomic_fence();
            }
            self.ins().local_get(address);
            match kind {
                "Int" | "Reference" => self.ins().i32_load(at(0)),
                "Long" => self.ins().i64_load(at(0)),
                "Boolean" => self.ins().i32_load8_u(at(0)).i32_const(0).i32_ne(),
                "Byte" => self.ins().i32_load8_s(at(0)),
                "Short" => self.ins().i32_load16_s(at(0)),
                "Char" => self.ins().i32_load16_u(at(0)),
                "Float" => self.ins().f32_load(at(0)),
                _ => self.ins().f64_load(at(0)),
            };
            if volatile {
                self.ins().atomic_fence();
            }
            self.stack.truncate(receiver);
            self.push(t);
        } else {
            let value = offset + 2;
            if volatile {
                self.ins().atomic_fence();
            }
            self.ins().local_get(address);
            self.get(value, t);
            match kind {
                "Int" | "Reference" => self.ins().i32_store(at(0)),
                "Long" => self.ins().i64_store(at(0)),
                "Boolean" => self.ins().i32_const(0).i32_ne().i32_store8(at(0)),
                "Byte" => self.ins().i32_store8(at(0)),
                "Short" | "Char" => self.ins().i32_store16(at(0)),
                "Float" => self.ins().f32_store(at(0)),
                _ => self.ins().f64_store(at(0)),
            };
            if volatile {
                self.ins().atomic_fence();
            }
            self.stack.truncate(receiver);
        }
        true
    }

    /// Static library methods compiled inline (the interpreter has VM
    /// implementations of most of them). Their wasm counterparts have the
    /// same semantics, NaN and signed zeros included.
    fn inline_static(&mut self, pc: usize) -> bool {
        let index = self.u2(pc + 1);
        let Some(Resolved::Method(m)) = self.method.class().cp_cache[index as usize].get() else { return false };
        let m = *m;
        if !m.is_static() || !m.class().initialized.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        use T::*;
        let class = m.class().name.as_str();
        let math = class == "java/lang/Math" || class == "java/lang/StrictMath";
        match (class, m.name.as_str(), m.descriptor.as_str()) {
            (_, "sqrt", "(D)D") if math => self.unop(F64, F64, |i| {
                i.f64_sqrt();
            }),
            (_, "floor", "(D)D") if math => self.unop(F64, F64, |i| {
                i.f64_floor();
            }),
            (_, "ceil", "(D)D") if math => self.unop(F64, F64, |i| {
                i.f64_ceil();
            }),
            (_, "abs", "(D)D") if math => self.unop(F64, F64, |i| {
                i.f64_abs();
            }),
            (_, "abs", "(F)F") if math => self.unop(F32, F32, |i| {
                i.f32_abs();
            }),
            // wasm leaves a NaN result's bits to the engine (V8 on x86 gives
            // the sign-set default NaN): Java's is the canonical NaN.
            (_, "max", "(DD)D") if math => {
                let s = self.scratch_i64(0);
                self.binop(F64, F64, |i| {
                    canonical_nan_f64(i.f64_max(), s);
                })
            }
            (_, "min", "(DD)D") if math => {
                let s = self.scratch_i64(0);
                self.binop(F64, F64, |i| {
                    canonical_nan_f64(i.f64_min(), s);
                })
            }
            (_, "max", "(FF)F") if math => {
                let s = self.scratch_i32();
                self.binop(F32, F32, |i| {
                    canonical_nan_f32(i.f32_max(), s);
                })
            }
            (_, "min", "(FF)F") if math => {
                let s = self.scratch_i32();
                self.binop(F32, F32, |i| {
                    canonical_nan_f32(i.f32_min(), s);
                })
            }
            (_, "max", "(II)I") if math => self.min_max(I32, true),
            (_, "min", "(II)I") if math => self.min_max(I32, false),
            (_, "max", "(JJ)J") if math => self.min_max(I64, true),
            (_, "min", "(JJ)J") if math => self.min_max(I64, false),
            (_, "abs", "(I)I") if math => {
                let a = self.to_local(self.top(1), I32);
                let sign = self.scratch_i32();
                self.unop(I32, I32, |i| {
                    i.i32_const(31).i32_shr_s().local_tee(sign).local_get(a).i32_xor().local_get(sign).i32_sub();
                });
            }
            (_, "abs", "(J)J") if math => {
                let a = self.to_local(self.top(2), I64);
                let sign = self.scratch_i64(0);
                self.unop(I64, I64, |i| {
                    i.i64_const(63).i64_shr_s().local_tee(sign).local_get(a).i64_xor().local_get(sign).i64_sub();
                });
            }
            ("java/lang/Integer", "rotateLeft", "(II)I") => self.binop(I32, I32, |i| {
                i.i32_rotl();
            }),
            ("java/lang/Integer", "rotateRight", "(II)I") => self.binop(I32, I32, |i| {
                i.i32_rotr();
            }),
            ("java/lang/Long", "rotateLeft", "(JI)J") => self.shift(I64, |i| {
                i.i64_rotl();
            }),
            ("java/lang/Long", "rotateRight", "(JI)J") => self.shift(I64, |i| {
                i.i64_rotr();
            }),
            ("java/lang/Integer", "bitCount", "(I)I") => self.unop(I32, I32, |i| {
                i.i32_popcnt();
            }),
            ("java/lang/Long", "bitCount", "(J)I") => self.unop(I64, I32, |i| {
                i.i64_popcnt().i32_wrap_i64();
            }),
            ("java/lang/Integer", "numberOfLeadingZeros", "(I)I") => self.unop(I32, I32, |i| {
                i.i32_clz();
            }),
            ("java/lang/Integer", "numberOfTrailingZeros", "(I)I") => self.unop(I32, I32, |i| {
                i.i32_ctz();
            }),
            ("java/lang/Long", "numberOfLeadingZeros", "(J)I") => self.unop(I64, I32, |i| {
                i.i64_clz().i32_wrap_i64();
            }),
            ("java/lang/Long", "numberOfTrailingZeros", "(J)I") => self.unop(I64, I32, |i| {
                i.i64_ctz().i32_wrap_i64();
            }),
            ("java/lang/Byte", "toUnsignedInt", "(B)I") => self.unop(I32, I32, |i| {
                i.i32_const(0xFF).i32_and();
            }),
            ("java/lang/Float", "floatToRawIntBits", "(F)I") => self.unop(F32, I32, |i| {
                i.i32_reinterpret_f32();
            }),
            ("java/lang/Float", "intBitsToFloat", "(I)F") => self.unop(I32, F32, |i| {
                i.f32_reinterpret_i32();
            }),
            ("java/lang/Double", "doubleToRawLongBits", "(D)J") => self.unop(F64, I64, |i| {
                i.i64_reinterpret_f64();
            }),
            ("java/lang/Double", "longBitsToDouble", "(J)D") => self.unop(I64, F64, |i| {
                i.f64_reinterpret_i64();
            }),
            ("java/util/Objects", "requireNonNull", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                // The argument is the result; null: the interpreter throws.
                let value = self.to_local(self.top(1), I32);
                let before = self.stack.clone();
                self.ins().local_get(value).i32_eqz();
                self.exit_if(&before, pc);
            }
            _ => return false,
        }
        true
    }

    /// Inline cache miss of the virtual call at `pc` for the receiver class
    /// id in local `class`: the target, through the dispatch cache, goes to
    /// the `target` global.
    fn dispatch_miss(&mut self, pc: usize, class: u32, target: u32) {
        let index = self.u2(pc + 1);
        let Some(Resolved::Method(resolved)) = self.method.class().cp_cache[index as usize].get() else {
            unreachable!("virtual call sites are resolved")
        };
        let key = self.scratch_i32() + 2;
        self.ins().i32_const(resolved.global_index as i32).local_set(key);
        self.dispatch(class, key);
        self.ins().global_set(target);
    }

    /// Pushes the method index to call for method index `key` (local) on a
    /// receiver of class id `class` (local; NO_CLASS for no dispatch), or -1
    /// when the target is not callable through the table: looked up in the
    /// shared dispatch cache, else selected by the VM (`select`), which fills
    /// the entry.
    fn dispatch(&mut self, class: u32, key: u32) {
        // Entry: [class | key << 32][target | check << 32], check = the full
        // hash: written second-word-first and validated by the check, so a
        // reader racing a writer of another key cannot get a wrong target.
        let base = self.memory.dispatch_cache_offset();
        let (hash, entry, target) = (self.scratch_i32() + 1, self.scratch_i32() + 3, self.scratch_i32() + 4);
        let (pair, second) = (self.scratch_i64(0), self.scratch_i64(1));
        let shift = 32 - crate::memory::DISPATCH_ENTRIES.trailing_zeros();
        self.ins()
            .local_get(class)
            .i32_const(0x9E37_79B1u32 as i32)
            .i32_mul()
            .local_get(key)
            .i32_const(0x85EB_CA6Bu32 as i32)
            .i32_mul()
            .i32_xor()
            .local_tee(hash)
            // entry = base + (hash >> shift) * 16
            .i32_const(shift as i32)
            .i32_shr_u()
            .i32_const(crate::memory::DISPATCH_ENTRY_SIZE.trailing_zeros() as i32)
            .i32_shl()
            .i32_const(base as i32)
            .i32_add()
            .local_tee(entry)
            .i64_atomic_load(mem(0, 3))
            .local_get(class)
            .i64_extend_i32_u()
            .local_get(key)
            .i64_extend_i32_u()
            .i64_const(32)
            .i64_shl()
            .i64_or()
            .local_tee(pair)
            .i64_eq()
            .local_get(entry)
            .i64_atomic_load(mem(8, 3))
            .local_tee(second)
            .i64_const(32)
            .i64_shr_u()
            .i32_wrap_i64()
            .local_get(hash)
            .i32_eq()
            .i32_and()
            // Another thread may have filled the entry: the target must also
            // be in this thread's table (-1, "not through the table", is fine).
            .local_get(second)
            .i32_wrap_i64()
            .local_tee(target)
            .i32_const(1)
            .i32_add()
            .table_size(0)
            .i32_le_u()
            .i32_and()
            .i32_eqz()
            .if_(BlockType::Empty);
        self.push_record();
        self.ins()
            .local_get(class)
            .local_get(key)
            .call(FN_SELECT)
            .local_set(target)
            .local_get(entry)
            .local_get(target)
            .i64_extend_i32_u()
            .local_get(hash)
            .i64_extend_i32_u()
            .i64_const(32)
            .i64_shl()
            .i64_or()
            .i64_atomic_store(mem(8, 3))
            .local_get(entry)
            .local_get(pair)
            .i64_atomic_store(mem(0, 3))
            .end()
            .local_get(target);
    }

    /// The class id and instance size for `new` of constant pool class
    /// `index`, if compiled code may allocate it: resolved, initialised, a
    /// concrete instance class, small enough for the TLAB.
    fn new_instance_class(&self, index: u16) -> Option<(u32, u32)> {
        let Some(Resolved::Class(class)) = self.method.class().cp_cache[index as usize].get() else { return None };
        let concrete = class.kind.is_instance() && !class.is_interface() && class.access_flags & classfile::access::ABSTRACT == 0;
        let size = class.instance_size.max(8).next_multiple_of(8);
        (concrete && class.initialized.load(std::sync::atomic::Ordering::Acquire) && (size as usize) <= MAX_INLINE_ALLOCATION)
            .then_some((class.id, size))
    }

    /// The array class id and element size shift for newarray/anewarray at
    /// `pc`, if its class already exists.
    fn new_array_class(&self, pc: usize) -> Option<(u32, u32)> {
        let component = if self.code[pc] == NEWARRAY {
            let kind = match self.code[pc + 1] {
                4 => FieldKind::Boolean,
                5 => FieldKind::Char,
                6 => FieldKind::Float,
                7 => FieldKind::Double,
                8 => FieldKind::Byte,
                9 => FieldKind::Short,
                10 => FieldKind::Int,
                _ => FieldKind::Long,
            };
            self.vm.primitive_class(Some(kind))
        } else {
            match self.method.class().cp_cache[self.u2(pc + 1) as usize].get() {
                Some(Resolved::Class(component)) => *component,
                _ => return None,
            }
        };
        let array = component.array_class.get()?;
        Some((array.id, array.element_kind()?.size().trailing_zeros()))
    }

    /// Allocates from the thread's TLAB (in its thread block, found from the
    /// frame record) or, when it is exhausted (or the array is large or has
    /// a negative length), lets the interpreter do it. Pushes the reference.
    fn allocate(&mut self, pc: usize, class_id: u32, what: Allocation) {
        let scratch = self.scratch_i32();
        let (block, obj, top, word, length) = (scratch + 2, scratch + 3, scratch + 4, scratch + 5, scratch + 6);
        let length_depth = self.stack.len().wrapping_sub(1);
        if let Allocation::Array(_) = what {
            self.to_local(length_depth, T::I32);
        }
        let before = self.stack.clone();
        let mut ins = self.f.instructions();
        ins.local_get(RECORD)
            .i32_const(!(crate::memory::STACK_REGION_BYTES as i32 - 1))
            .i32_and()
            .local_tee(block)
            .i32_load(mem(0, 2)) // tlab_top
            .local_tee(obj);
        match what {
            Allocation::Object(size) => {
                ins.i32_const(size as i32).i32_add().local_set(top);
                ins.i32_const(0); // never too large
            }
            Allocation::Array(shift) => {
                let length_local = local_of(self.c.op_base, length_depth, T::I32);
                ins.local_get(length_local)
                    .local_tee(length)
                    .i32_const(shift as i32)
                    .i32_shl()
                    .i32_const(ARRAY_BASE_OFFSET as i32 + 7)
                    .i32_add()
                    .i32_const(-8)
                    .i32_and()
                    .i32_add()
                    .local_set(top);
                // Negative (huge unsigned) or too large for the TLAB.
                ins.local_get(length).i32_const(((MAX_INLINE_ALLOCATION - ARRAY_BASE_OFFSET as usize) >> shift) as i32).i32_gt_u();
            }
        }
        ins.local_get(top).local_get(block).i32_load(mem(4, 2)).i32_gt_u().i32_or().if_(BlockType::Empty);
        self.emit_step(&before, pc);
        let result_slot = self.slot_offset(before.len() - if matches!(what, Allocation::Array(_)) { 1 } else { 0 });
        let starts = self.memory.starts_bitmap_offset();
        let mut ins = self.f.instructions();
        ins.local_get(FP).i32_load(mem(result_slot, 2)).local_set(obj).else_();
        ins.local_get(block).local_get(top).i32_store(mem(0, 2));
        ins.local_get(obj).i32_const(class_id as i32).i32_store(mem(0, 2));
        if let Allocation::Array(_) = what {
            ins.local_get(obj).local_get(length).i32_store(mem(ARRAY_LENGTH_OFFSET as usize, 2));
        }
        // The object-start bit: granule obj/8, word (obj/8)/64; atomic, as
        // a word can cover two threads' buffers.
        let _ = word;
        ins.local_get(obj)
            .i32_const(9)
            .i32_shr_u()
            .i32_const(3)
            .i32_shl()
            .i32_const(starts as i32)
            .i32_add()
            .i64_const(1)
            .local_get(obj)
            .i32_const(3)
            .i32_shr_u()
            .i64_extend_i32_u()
            .i64_shl()
            .i64_atomic_rmw_or(mem(0, 3))
            .drop()
            .end()
            .local_get(obj);
        if let Allocation::Array(_) = what {
            self.stack.truncate(length_depth);
        }
        self.push(T::I32);
    }

    /// Records the current pc in the frame record (for stack traces, and for
    /// the interpreter resuming after the call).
    fn save_pc(&mut self, pc: usize) {
        self.materialize(pc);
    }

    fn load_local(&mut self, index: usize, t: T) {
        if index == 0 && self.this_constant {
            self.stack.push(Entry::This);
            return;
        }
        self.f.instructions().local_get(self.c.java_locals + index as u32);
        self.from_raw(t);
        self.push(t);
    }

    fn store_local(&mut self, index: usize, wide: bool) {
        let depth = self.top(if wide { 2 } else { 1 });
        match self.stack[depth] {
            Entry::Local(t) => {
                self.f.instructions().local_get(local_of(self.c.op_base, depth, t));
                self.to_raw(t);
            }
            _ => self.get(depth, T::I64),
        }
        self.f.instructions().local_set(self.c.java_locals + index as u32);
        self.mark_written(index);
        self.stack.truncate(depth);
    }

    /// After writing Java local `index`'s wasm local. RJAVA_JIT_WRITE_THROUGH
    /// (debugging): store it to the frame right away instead.
    fn mark_written(&mut self, index: usize) {
        static WRITE_THROUGH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *WRITE_THROUGH.get_or_init(|| std::env::var_os("RJAVA_JIT_WRITE_THROUGH").is_some()) {
            let (local, offset) = (self.c.java_locals + index as u32, self.c.frame_offset + index * 8);
            self.ins().local_get(FP).local_get(local).i64_store(mem(offset, 3));
            return;
        }
        self.c.dirty[index] = true;
        self.c.written_since_sync[index] = true;
    }

    /// The current method's dirty Java locals: (wasm local, frame offset).
    fn dirty_slots(&self) -> Vec<(u32, usize)> {
        (0..self.max_locals)
            .filter(|&i| self.dirty[i])
            .map(|i| (self.java_locals + i as u32, self.frame_offset + i * 8))
            .collect()
    }

    /// Writes the dirty Java locals to the frame (before an import or a
    /// call); with `clean`, they are no longer dirty (on paths that always
    /// pass here, not in slow paths).
    fn sync_locals(&mut self, clean: bool) {
        for (local, offset) in self.dirty_slots() {
            self.ins().local_get(FP).local_get(local).i64_store(mem(offset, 3));
        }
        if clean {
            self.c.dirty.iter_mut().for_each(|d| *d = false);
            self.c.written_since_sync.iter_mut().for_each(|g| *g = false);
            self.c.synced = true;
        }
    }

    /// Binary operation on two operands of type `t`, result `r`.
    fn binop(&mut self, t: T, r: T, op: impl FnOnce(&mut InstructionSink<'_>)) {
        let width = if matches!(t, T::I64 | T::F64) { 2 } else { 1 };
        let a = self.top(2 * width);
        let b = self.top(width);
        self.get(a, t);
        self.get(b, t);
        op(&mut self.ins());
        self.stack.truncate(a);
        self.push(r);
    }

    /// Shift: value of type `t` and an int count.
    fn shift(&mut self, t: T, op: impl FnOnce(&mut InstructionSink<'_>)) {
        let width = if t == T::I64 { 2 } else { 1 };
        let a = self.top(width + 1);
        let b = self.top(1);
        self.get(a, t);
        self.get(b, T::I32);
        if t == T::I64 {
            self.ins().i64_extend_i32_u();
        }
        op(&mut self.ins());
        self.stack.truncate(a);
        self.push(t);
    }

    fn unop(&mut self, t: T, r: T, op: impl FnOnce(&mut InstructionSink<'_>)) {
        let width = if matches!(t, T::I64 | T::F64) { 2 } else { 1 };
        let a = self.top(width);
        self.get(a, t);
        op(&mut self.ins());
        self.stack.truncate(a);
        self.push(r);
    }

    /// Integer division or remainder with Java semantics; a zero divisor
    /// exits so the interpreter throws ArithmeticException.
    fn divide(&mut self, pc: usize, t: T, rem: bool) {
        let width = if t == T::I64 { 2 } else { 1 };
        let a_depth = self.top(2 * width);
        let a = self.to_local(a_depth, t);
        let b = self.to_local(self.top(width), t);
        let before = self.stack.clone();
        self.ins().local_get(b);
        if t == T::I64 {
            self.ins().i64_eqz();
        } else {
            self.ins().i32_eqz();
        }
        self.exit_if(&before, pc);
        if rem {
            // rem_s does not trap on MIN % -1 (the result is 0, as in Java).
            self.ins().local_get(a).local_get(b);
            if t == T::I64 {
                self.ins().i64_rem_s();
            } else {
                self.ins().i32_rem_s();
            }
        } else {
            // div_s traps on MIN / -1; Java wraps to MIN (= 0 - MIN).
            let vt = t.val();
            self.ins().local_get(b);
            if t == T::I64 {
                self.ins().i64_const(-1).i64_eq();
            } else {
                self.ins().i32_const(-1).i32_eq();
            }
            self.ins().if_(BlockType::Result(vt));
            if t == T::I64 {
                self.ins().i64_const(0).local_get(a).i64_sub().else_().local_get(a).local_get(b).i64_div_s();
            } else {
                self.ins().i32_const(0).local_get(a).i32_sub().else_().local_get(a).local_get(b).i32_div_s();
            }
            self.ins().end();
        }
        self.stack.truncate(a_depth);
        self.push(t);
    }

    /// Compares two operands of type `t` as lcmp/fcmpl/fcmpg/... (-1, 0, 1).
    fn compare(&mut self, t: T, nan_is_greater: bool) {
        let width = if matches!(t, T::I64 | T::F64) { 2 } else { 1 };
        let a_depth = self.top(2 * width);
        let a = self.to_local(a_depth, t);
        let b = self.to_local(self.top(width), t);
        let mut ins = self.f.instructions();
        match (t, nan_is_greater) {
            (T::I64, _) => ins.local_get(a).local_get(b).i64_gt_s().local_get(a).local_get(b).i64_lt_s().i32_sub(),
            // fcmpl: (a > b) - !(a >= b)  → NaN gives -1
            (T::F32, false) => ins.local_get(a).local_get(b).f32_gt().local_get(a).local_get(b).f32_ge().i32_eqz().i32_sub(),
            (T::F64, false) => ins.local_get(a).local_get(b).f64_gt().local_get(a).local_get(b).f64_ge().i32_eqz().i32_sub(),
            // fcmpg: !(a <= b) - (a < b)  → NaN gives 1
            (T::F32, true) => ins.local_get(a).local_get(b).f32_le().i32_eqz().local_get(a).local_get(b).f32_lt().i32_sub(),
            (T::F64, true) => ins.local_get(a).local_get(b).f64_le().i32_eqz().local_get(a).local_get(b).f64_lt().i32_sub(),
            _ => unreachable!(),
        };
        self.stack.truncate(a_depth);
        self.push(T::I32);
    }

    /// Array element access: checks null and bounds (exiting so the
    /// interpreter throws), leaving the element address on the wasm stack
    /// (to be used with offset ARRAY_BASE_OFFSET). `value_slots` operand
    /// slots sit above the index.
    fn array_address(&mut self, pc: usize, value_slots: usize, shift: i32) {
        let index_depth = self.top(value_slots + 1);
        let array = self.to_local(index_depth - 1, T::I32);
        let index = self.to_local(index_depth, T::I32);
        let before = self.stack.clone();
        self.ins()
            .local_get(array)
            .i32_eqz()
            .local_get(index)
            .local_get(array)
            .i32_load(mem(ARRAY_LENGTH_OFFSET as usize, 2))
            .i32_ge_u()
            .i32_or();
        self.exit_if(&before, pc);
        self.ins().local_get(array).local_get(index);
        if shift > 0 {
            self.ins().i32_const(shift).i32_shl();
        }
        self.ins().i32_add();
    }

    fn array_load(&mut self, pc: usize, kind: FieldKind) {
        let shift = kind.size().trailing_zeros() as i32;
        self.array_address(pc, 0, shift);
        let t = self.load_kind(kind, ARRAY_BASE_OFFSET as usize);
        let array_depth = self.top(2);
        self.stack.truncate(array_depth);
        self.push(t);
    }

    /// aastore: the store check with a per-site cache of the (array class,
    /// value class) pair that last passed, else the `pure` import; a failing
    /// check is left to the interpreter (ArrayStoreException).
    fn aastore(&mut self, pc: usize) {
        let value = self.to_local(self.top(1), T::I32);
        let array = self.to_local(self.top(3), T::I32);
        let before = self.stack.clone();
        let (cached_array, cached_value) = self.inline_cache();
        let scratch = self.scratch_i32();
        let (array_class, value_class) = (scratch + 1, scratch + 2);
        self.ins()
            .local_get(value)
            .i32_const(0)
            .i32_ne()
            .local_get(array)
            .i32_const(0)
            .i32_ne()
            .i32_and()
            .if_(BlockType::Empty)
            .local_get(array)
            .i32_load(mem(0, 2))
            .local_tee(array_class)
            .global_get(cached_array)
            .i32_ne()
            .local_get(value)
            .i32_load(mem(0, 2))
            .local_tee(value_class)
            .global_get(cached_value)
            .i32_ne()
            .i32_or()
            .if_(BlockType::Empty)
            .i32_const(super::pure::ARRAY_STORE_CHECK as i32)
            .local_get(array)
            .local_get(value)
            .call(FN_PURE)
            .i32_eqz()
            .if_(BlockType::Empty);
        self.emit_step(&before, pc); // throws ArrayStoreException
        self.ins()
            .end()
            .local_get(array_class)
            .global_set(cached_array)
            .local_get(value_class)
            .global_set(cached_value)
            .end()
            .end();
        self.array_store(pc, FieldKind::Ref);
    }

    fn array_store(&mut self, pc: usize, kind: FieldKind) {
        let t = kind_type(kind);
        let slots = if kind.is_wide() { 2 } else { 1 };
        let shift = kind.size().trailing_zeros() as i32;
        self.array_address(pc, slots, shift);
        let value = self.top(slots);
        self.get(value, t);
        self.store_kind(kind, ARRAY_BASE_OFFSET as usize);
        let array_depth = self.top(slots + 2);
        self.stack.truncate(array_depth);
    }

    /// Loads a `kind` value from the address on the wasm stack + `offset`.
    fn load_kind(&mut self, kind: FieldKind, offset: usize) -> T {
        let mut ins = self.f.instructions();
        match kind {
            FieldKind::Byte => {
                ins.i32_load8_s(mem(offset, 0));
            }
            FieldKind::Boolean => {
                ins.i32_load8_u(mem(offset, 0));
            }
            FieldKind::Char => {
                ins.i32_load16_u(mem(offset, 1));
            }
            FieldKind::Short => {
                ins.i32_load16_s(mem(offset, 1));
            }
            FieldKind::Int | FieldKind::Ref => {
                ins.i32_load(mem(offset, 2));
            }
            FieldKind::Float => {
                ins.f32_load(mem(offset, 2));
            }
            FieldKind::Long => {
                ins.i64_load(mem(offset, 3));
            }
            FieldKind::Double => {
                ins.f64_load(mem(offset, 3));
            }
        }
        kind_type(kind)
    }

    /// Field access: atomic (sequentially consistent) for volatile fields,
    /// which also keeps Cranelift from caching them in loops.
    fn load_field(&mut self, field: crate::class::FieldRef) -> T {
        if !field.volatile {
            return self.load_kind(field.kind, field.offset as usize);
        }
        let offset = field.offset as usize;
        let mut ins = self.f.instructions();
        match field.kind {
            FieldKind::Byte => ins.i32_atomic_load8_u(mem(offset, 0)).i32_extend8_s(),
            FieldKind::Boolean => ins.i32_atomic_load8_u(mem(offset, 0)),
            FieldKind::Char => ins.i32_atomic_load16_u(mem(offset, 1)),
            FieldKind::Short => ins.i32_atomic_load16_u(mem(offset, 1)).i32_extend16_s(),
            FieldKind::Int | FieldKind::Ref => ins.i32_atomic_load(mem(offset, 2)),
            FieldKind::Float => ins.i32_atomic_load(mem(offset, 2)).f32_reinterpret_i32(),
            FieldKind::Long => ins.i64_atomic_load(mem(offset, 3)),
            FieldKind::Double => ins.i64_atomic_load(mem(offset, 3)).f64_reinterpret_i64(),
        };
        kind_type(field.kind)
    }

    fn store_field(&mut self, field: crate::class::FieldRef) {
        if !field.volatile {
            return self.store_kind(field.kind, field.offset as usize);
        }
        let offset = field.offset as usize;
        let mut ins = self.f.instructions();
        match field.kind {
            FieldKind::Byte => ins.i32_atomic_store8(mem(offset, 0)),
            FieldKind::Boolean => ins.i32_const(1).i32_and().i32_atomic_store8(mem(offset, 0)),
            FieldKind::Char | FieldKind::Short => ins.i32_atomic_store16(mem(offset, 1)),
            FieldKind::Int | FieldKind::Ref => ins.i32_atomic_store(mem(offset, 2)),
            FieldKind::Float => ins.i32_reinterpret_f32().i32_atomic_store(mem(offset, 2)),
            FieldKind::Long => ins.i64_atomic_store(mem(offset, 3)),
            FieldKind::Double => ins.i64_reinterpret_f64().i64_atomic_store(mem(offset, 3)),
        };
    }

    fn store_kind(&mut self, kind: FieldKind, offset: usize) {
        let mut ins = self.f.instructions();
        match kind {
            FieldKind::Byte => ins.i32_store8(mem(offset, 0)),
            FieldKind::Boolean => ins.i32_const(1).i32_and().i32_store8(mem(offset, 0)),
            FieldKind::Char | FieldKind::Short => ins.i32_store16(mem(offset, 1)),
            FieldKind::Int | FieldKind::Ref => ins.i32_store(mem(offset, 2)),
            FieldKind::Float => ins.f32_store(mem(offset, 2)),
            FieldKind::Long => ins.i64_store(mem(offset, 3)),
            FieldKind::Double => ins.f64_store(mem(offset, 3)),
        };
    }

    /// A resolved static field whose class needs no initialisation check: it
    /// is initialised, or it is this method's class (as in the interpreter).
    fn static_field(&self, index: u16) -> Option<crate::class::FieldRef> {
        let class = self.method.class();
        match class.cp_cache[index as usize].get() {
            Some(Resolved::Field(field))
                if field.is_static
                    && (std::ptr::eq(field.holder, class)
                        || field.holder.initialized.load(std::sync::atomic::Ordering::Acquire)) =>
            {
                Some(*field)
            }
            _ => None,
        }
    }

    fn resolved_field(&self, index: u16) -> Option<crate::class::FieldRef> {
        match self.method.class().cp_cache[index as usize].get() {
            Some(Resolved::Field(field)) if !field.is_static => Some(*field),
            _ => None,
        }
    }

    /// Stack shuffles (dup_x1, swap, ...): `pops` slots are replaced by
    /// `result`, given as indices into the popped slots (0 = deepest).
    fn shuffle(&mut self, pops: usize, result: &[usize]) {
        let base = self.top(pops);
        for i in 0..pops {
            self.get(base + i, T::I64);
            let scratch = self.scratch_i64(i as u32);
            self.ins().local_set(scratch);
        }
        self.stack.truncate(base);
        for &i in result {
            let scratch = self.scratch_i64(i as u32);
            let depth = self.stack.len();
            self.f.instructions().local_get(scratch).local_set(local_of(self.c.op_base, depth, T::I64));
            self.stack.push(Entry::Local(T::I64));
        }
    }

    /// Generates one instruction; false if control does not fall through.
    fn instruction(&mut self, pc: usize) -> Result<bool, String> {
        let code = self.code;
        let op = code[pc];
        match op {
            NOP => {}
            ACONST_NULL => {
                self.ins().i32_const(0);
                self.push(T::I32);
            }
            ICONST_M1..=ICONST_5 => {
                self.ins().i32_const(op as i32 - ICONST_0 as i32);
                self.push(T::I32);
            }
            LCONST_0 | LCONST_1 => {
                self.ins().i64_const((op - LCONST_0) as i64);
                self.push(T::I64);
            }
            FCONST_0..=FCONST_2 => {
                self.ins().f32_const(((op - FCONST_0) as f32).into());
                self.push(T::F32);
            }
            DCONST_0 | DCONST_1 => {
                self.ins().f64_const(((op - DCONST_0) as f64).into());
                self.push(T::F64);
            }
            BIPUSH => {
                self.ins().i32_const(code[pc + 1] as i8 as i32);
                self.push(T::I32);
            }
            SIPUSH => {
                let v = self.u2(pc + 1) as i16 as i32;
                self.ins().i32_const(v);
                self.push(T::I32);
            }
            LDC | LDC_W | LDC2_W => {
                let index = if op == LDC { code[pc + 1] as u16 } else { self.u2(pc + 1) };
                let class = self.method.class();
                match class.cp().get(index) {
                    Ok(&Constant::Integer(v)) => {
                        self.ins().i32_const(v);
                        self.push(T::I32);
                    }
                    Ok(&Constant::Float(v)) => {
                        self.ins().f32_const(v.into());
                        self.push(T::F32);
                    }
                    Ok(&Constant::Long(v)) => {
                        self.ins().i64_const(v);
                        self.push(T::I64);
                    }
                    Ok(&Constant::Double(v)) => {
                        self.ins().f64_const(v.into());
                        self.push(T::F64);
                    }
                    // Interned strings never move (they are pinned roots).
                    Ok(Constant::String(_)) => match class.cp_cache[index as usize].get() {
                        Some(Resolved::String(s)) => {
                            self.ins().i32_const(s.0 as i32);
                            self.push(T::I32);
                        }
                        _ => return self.step(pc),
                    },
                    // A class literal: its mirror, read from the class's
                    // cell (which the collector updates), once there is one.
                    Ok(Constant::Class(_)) if cfg!(target_family = "wasm") => match class.cp_cache[index as usize].get() {
                        Some(Resolved::Class(c)) => {
                            let cell = &c.mirror as *const crate::heap::RefCell32 as usize as i32;
                            let before = self.stack.clone();
                            let slot = self.slot_offset(before.len());
                            let mirror = self.scratch_i32();
                            self.ins()
                                .i32_const(cell)
                                .i32_load(MemArg { offset: 0, align: 2, memory_index: 0 })
                                .local_tee(mirror)
                                .if_(BlockType::Result(ValType::I32))
                                .local_get(mirror)
                                .else_();
                            self.emit_step(&before, pc);
                            self.ins().local_get(FP).i32_load(mem(slot, 2)).end();
                            self.push(T::I32);
                        }
                        _ => return self.step(pc),
                    },
                    _ => return self.step(pc),
                }
            }
            ILOAD | FLOAD | ALOAD => self.load_local(code[pc + 1] as usize, if op == FLOAD { T::F32 } else { T::I32 }),
            LLOAD => self.load_local(code[pc + 1] as usize, T::I64),
            DLOAD => self.load_local(code[pc + 1] as usize, T::F64),
            ILOAD_0..=ILOAD_3 => self.load_local((op - ILOAD_0) as usize, T::I32),
            LLOAD_0..=LLOAD_3 => self.load_local((op - LLOAD_0) as usize, T::I64),
            FLOAD_0..=FLOAD_3 => self.load_local((op - FLOAD_0) as usize, T::F32),
            DLOAD_0..=DLOAD_3 => self.load_local((op - DLOAD_0) as usize, T::F64),
            ALOAD_0..=ALOAD_3 => self.load_local((op - ALOAD_0) as usize, T::I32),
            ISTORE | FSTORE | ASTORE => self.store_local(code[pc + 1] as usize, false),
            LSTORE | DSTORE => self.store_local(code[pc + 1] as usize, true),
            ISTORE_0..=ISTORE_3 => self.store_local((op - ISTORE_0) as usize, false),
            LSTORE_0..=LSTORE_3 => self.store_local((op - LSTORE_0) as usize, true),
            FSTORE_0..=FSTORE_3 => self.store_local((op - FSTORE_0) as usize, false),
            DSTORE_0..=DSTORE_3 => self.store_local((op - DSTORE_0) as usize, true),
            ASTORE_0..=ASTORE_3 => self.store_local((op - ASTORE_0) as usize, false),
            IINC => self.iinc(code[pc + 1] as usize, code[pc + 2] as i8 as i32),
            WIDE => {
                let index = self.u2(pc + 2) as usize;
                match code[pc + 1] {
                    ILOAD | ALOAD => self.load_local(index, T::I32),
                    FLOAD => self.load_local(index, T::F32),
                    LLOAD => self.load_local(index, T::I64),
                    DLOAD => self.load_local(index, T::F64),
                    ISTORE | FSTORE | ASTORE => self.store_local(index, false),
                    LSTORE | DSTORE => self.store_local(index, true),
                    IINC => self.iinc(index, self.u2(pc + 4) as i16 as i32),
                    _ => return Err("bad wide".into()),
                }
            }
            IALOAD => self.array_load(pc, FieldKind::Int),
            LALOAD => self.array_load(pc, FieldKind::Long),
            FALOAD => self.array_load(pc, FieldKind::Float),
            DALOAD => self.array_load(pc, FieldKind::Double),
            AALOAD => self.array_load(pc, FieldKind::Ref),
            BALOAD => self.array_load(pc, FieldKind::Byte),
            CALOAD => self.array_load(pc, FieldKind::Char),
            SALOAD => self.array_load(pc, FieldKind::Short),
            IASTORE => self.array_store(pc, FieldKind::Int),
            LASTORE => self.array_store(pc, FieldKind::Long),
            FASTORE => self.array_store(pc, FieldKind::Float),
            DASTORE => self.array_store(pc, FieldKind::Double),
            // Shared by byte[] and boolean[]; javac only stores 0/1 into boolean[].
            BASTORE => self.array_store(pc, FieldKind::Byte),
            CASTORE => self.array_store(pc, FieldKind::Char),
            SASTORE => self.array_store(pc, FieldKind::Short),
            AASTORE => self.aastore(pc),
            POP => {
                self.stack.pop();
            }
            POP2 => {
                let len = self.stack.len() - 2;
                self.stack.truncate(len);
            }
            DUP => {
                let depth = self.top(1);
                match self.stack[depth] {
                    Entry::This => self.stack.push(Entry::This),
                    // A one-slot value, even when held as a raw i64: no Top.
                    Entry::Local(t) => {
                        self.f.instructions().local_get(local_of(self.c.op_base, depth, t)).local_set(local_of(self.c.op_base, depth + 1, t));
                        self.stack.push(Entry::Local(t));
                    }
                    _ => {
                        self.get(depth, T::I64);
                        self.push_raw();
                    }
                }
            }
            DUP_X1 => self.shuffle(2, &[1, 0, 1]),
            DUP_X2 => self.shuffle(3, &[2, 0, 1, 2]),
            DUP2 => self.shuffle(2, &[0, 1, 0, 1]),
            DUP2_X1 => self.shuffle(3, &[1, 2, 0, 1, 2]),
            DUP2_X2 => self.shuffle(4, &[2, 3, 0, 1, 2, 3]),
            SWAP => self.shuffle(2, &[1, 0]),
            IADD => self.binop(T::I32, T::I32, |i| {
                i.i32_add();
            }),
            LADD => self.binop(T::I64, T::I64, |i| {
                i.i64_add();
            }),
            FADD => self.binop(T::F32, T::F32, |i| {
                i.f32_add();
            }),
            DADD => self.binop(T::F64, T::F64, |i| {
                i.f64_add();
            }),
            ISUB => self.binop(T::I32, T::I32, |i| {
                i.i32_sub();
            }),
            LSUB => self.binop(T::I64, T::I64, |i| {
                i.i64_sub();
            }),
            FSUB => self.binop(T::F32, T::F32, |i| {
                i.f32_sub();
            }),
            DSUB => self.binop(T::F64, T::F64, |i| {
                i.f64_sub();
            }),
            IMUL => self.binop(T::I32, T::I32, |i| {
                i.i32_mul();
            }),
            LMUL => self.binop(T::I64, T::I64, |i| {
                i.i64_mul();
            }),
            FMUL => self.binop(T::F32, T::F32, |i| {
                i.f32_mul();
            }),
            DMUL => self.binop(T::F64, T::F64, |i| {
                i.f64_mul();
            }),
            FDIV => self.binop(T::F32, T::F32, |i| {
                i.f32_div();
            }),
            DDIV => self.binop(T::F64, T::F64, |i| {
                i.f64_div();
            }),
            IDIV => self.divide(pc, T::I32, false),
            LDIV => self.divide(pc, T::I64, false),
            IREM => self.divide(pc, T::I32, true),
            LREM => self.divide(pc, T::I64, true),
            FREM => {
                // Exact in double: fmod of the widened values is the float result.
                let b = self.to_local(self.top(1), T::F32);
                let a = self.top(2);
                self.get(a, T::F32);
                self.ins().f64_promote_f32().local_get(b).f64_promote_f32().call(FN_FMOD).f32_demote_f64();
                self.stack.truncate(a);
                self.push(T::F32);
            }
            DREM => self.binop(T::F64, T::F64, |i| {
                i.call(FN_FMOD);
            }),
            INEG => self.unop(T::I32, T::I32, |i| {
                // 0 - x: wasm has no i32.neg.
                i.i32_const(-1).i32_mul();
            }),
            LNEG => self.unop(T::I64, T::I64, |i| {
                i.i64_const(-1).i64_mul();
            }),
            FNEG => self.unop(T::F32, T::F32, |i| {
                i.f32_neg();
            }),
            DNEG => self.unop(T::F64, T::F64, |i| {
                i.f64_neg();
            }),
            ISHL => self.shift(T::I32, |i| {
                i.i32_shl();
            }),
            LSHL => self.shift(T::I64, |i| {
                i.i64_shl();
            }),
            ISHR => self.shift(T::I32, |i| {
                i.i32_shr_s();
            }),
            LSHR => self.shift(T::I64, |i| {
                i.i64_shr_s();
            }),
            IUSHR => self.shift(T::I32, |i| {
                i.i32_shr_u();
            }),
            LUSHR => self.shift(T::I64, |i| {
                i.i64_shr_u();
            }),
            IAND => self.binop(T::I32, T::I32, |i| {
                i.i32_and();
            }),
            LAND => self.binop(T::I64, T::I64, |i| {
                i.i64_and();
            }),
            IOR => self.binop(T::I32, T::I32, |i| {
                i.i32_or();
            }),
            LOR => self.binop(T::I64, T::I64, |i| {
                i.i64_or();
            }),
            IXOR => self.binop(T::I32, T::I32, |i| {
                i.i32_xor();
            }),
            LXOR => self.binop(T::I64, T::I64, |i| {
                i.i64_xor();
            }),
            I2L => self.unop(T::I32, T::I64, |i| {
                i.i64_extend_i32_s();
            }),
            I2F => self.unop(T::I32, T::F32, |i| {
                i.f32_convert_i32_s();
            }),
            I2D => self.unop(T::I32, T::F64, |i| {
                i.f64_convert_i32_s();
            }),
            L2I => self.unop(T::I64, T::I32, |i| {
                i.i32_wrap_i64();
            }),
            L2F => self.unop(T::I64, T::F32, |i| {
                i.f32_convert_i64_s();
            }),
            L2D => self.unop(T::I64, T::F64, |i| {
                i.f64_convert_i64_s();
            }),
            F2I => self.unop(T::F32, T::I32, |i| {
                i.i32_trunc_sat_f32_s();
            }),
            F2L => self.unop(T::F32, T::I64, |i| {
                i.i64_trunc_sat_f32_s();
            }),
            F2D => self.unop(T::F32, T::F64, |i| {
                i.f64_promote_f32();
            }),
            D2I => self.unop(T::F64, T::I32, |i| {
                i.i32_trunc_sat_f64_s();
            }),
            D2L => self.unop(T::F64, T::I64, |i| {
                i.i64_trunc_sat_f64_s();
            }),
            D2F => self.unop(T::F64, T::F32, |i| {
                i.f32_demote_f64();
            }),
            I2B => self.unop(T::I32, T::I32, |i| {
                i.i32_extend8_s();
            }),
            I2C => self.unop(T::I32, T::I32, |i| {
                i.i32_const(0xFFFF).i32_and();
            }),
            I2S => self.unop(T::I32, T::I32, |i| {
                i.i32_extend16_s();
            }),
            LCMP => self.compare(T::I64, false),
            FCMPL | FCMPG => self.compare(T::F32, op == FCMPG),
            DCMPL | DCMPG => self.compare(T::F64, op == DCMPG),
            IFEQ..=IFLE | IFNULL | IFNONNULL => {
                let depth = self.top(1);
                self.get(depth, T::I32);
                let mut ins = self.f.instructions();
                match op {
                    IFEQ | IFNULL => ins.i32_eqz(),
                    IFNE | IFNONNULL => &mut ins, // non-zero is true
                    IFLT => ins.i32_const(0).i32_lt_s(),
                    IFGE => ins.i32_const(0).i32_ge_s(),
                    IFGT => ins.i32_const(0).i32_gt_s(),
                    _ => ins.i32_const(0).i32_le_s(),
                };
                self.stack.truncate(depth);
                self.flush();
                self.jump_if(branch_target(code, pc));
            }
            IF_ICMPEQ..=IF_ACMPNE => {
                let a = self.top(2);
                self.get(a, T::I32);
                self.get(a + 1, T::I32);
                let mut ins = self.f.instructions();
                match op {
                    IF_ICMPEQ | IF_ACMPEQ => ins.i32_eq(),
                    IF_ICMPNE | IF_ACMPNE => ins.i32_ne(),
                    IF_ICMPLT => ins.i32_lt_s(),
                    IF_ICMPGE => ins.i32_ge_s(),
                    IF_ICMPGT => ins.i32_gt_s(),
                    _ => ins.i32_le_s(),
                };
                self.stack.truncate(a);
                self.flush();
                self.jump_if(branch_target(code, pc));
            }
            GOTO | GOTO_W => {
                self.flush();
                self.jump(branch_target(code, pc));
                return Ok(false);
            }
            TABLESWITCH | LOOKUPSWITCH => {
                let depth = self.top(1);
                let key = self.to_local(depth, T::I32);
                self.stack.truncate(depth);
                self.flush();
                let targets = switch_targets(code, pc);
                let base = (pc + 4) & !3;
                if op == TABLESWITCH {
                    let low = i4(code, base + 4);
                    let cases = targets.len() as u32 - 1;
                    self.multiway(&targets, |g| {
                        // Trampoline 0 is the default.
                        g.ins().local_get(key).i32_const(low).i32_sub().i32_const(1).i32_add();
                        let key1 = g.scratch_i32();
                        g.ins().local_tee(key1).i32_const(cases as i32).i32_gt_u().if_(BlockType::Empty).i32_const(0).local_set(key1).end();
                        g.ins().local_get(key1).br_table(0..cases + 1, 0);
                    });
                } else {
                    self.multiway(&targets, |g| {
                        for i in 1..targets.len() {
                            let k = i4(code, base + 8 + (i - 1) * 8);
                            g.ins().local_get(key).i32_const(k).i32_eq().br_if(i as u32);
                        }
                        g.ins().br(0);
                    });
                }
                return Ok(false);
            }
            IRETURN..=RETURN => return self.return_(op),
            INVOKEVIRTUAL..=INVOKEDYNAMIC => return self.invoke(pc),
            GETFIELD => {
                let Some(field) = self.resolved_field(self.u2(pc + 1)) else { return self.step(pc) };
                let depth = self.top(1);
                let is_this = self.stack[depth] == Entry::This;
                let obj = self.to_local(depth, T::I32);
                let before = self.stack.clone();
                if !is_this {
                    self.ins().local_get(obj).i32_eqz();
                    self.exit_if(&before, pc);
                }
                self.ins().local_get(obj);
                let t = self.load_field(field);
                self.stack.truncate(depth);
                self.push(t);
            }
            PUTFIELD => {
                let Some(field) = self.resolved_field(self.u2(pc + 1)) else { return self.step(pc) };
                let slots = if field.kind.is_wide() { 2 } else { 1 };
                let depth = self.top(slots + 1);
                let is_this = self.stack[depth] == Entry::This;
                let obj = self.to_local(depth, T::I32);
                let before = self.stack.clone();
                if !is_this {
                    self.ins().local_get(obj).i32_eqz();
                    self.exit_if(&before, pc);
                }
                self.ins().local_get(obj);
                self.get(depth + 1, kind_type(field.kind));
                self.store_field(field);
                self.stack.truncate(depth);
            }
            ARRAYLENGTH => {
                let depth = self.top(1);
                let array = self.to_local(depth, T::I32);
                let before = self.stack.clone();
                self.ins().local_get(array).i32_eqz();
                self.exit_if(&before, pc);
                self.ins().local_get(array).i32_load(mem(ARRAY_LENGTH_OFFSET as usize, 2));
                self.stack.truncate(depth);
                self.push(T::I32);
            }
            ATHROW => {
                self.step(pc)?;
                // athrow always throws.
                self.ins().i32_const(STATUS_EXCEPTION).return_();
                return Ok(false);
            }
            GETSTATIC | PUTSTATIC => {
                let Some(field) = self.static_field(self.u2(pc + 1)) else { return self.step(pc) };
                // The statics block can move (compaction): load it from the
                // class metadata each time.
                let cell = self.memory.class_meta_offset(field.holder.id);
                if op == GETSTATIC {
                    self.ins().i32_const(cell as i32).i32_load(mem(0, 2));
                    let t = self.load_field(field);
                    self.push(t);
                } else {
                    let slots = if field.kind.is_wide() { 2 } else { 1 };
                    let value = self.top(slots);
                    self.ins().i32_const(cell as i32).i32_load(mem(0, 2));
                    self.get(value, kind_type(field.kind));
                    self.store_field(field);
                    self.stack.truncate(value);
                }
            }
            CHECKCAST => self.checkcast(pc),
            INSTANCEOF => self.instanceof(pc),
            NEW => match self.new_instance_class(self.u2(pc + 1)) {
                Some((id, size)) => self.allocate(pc, id, Allocation::Object(size)),
                None => return self.step(pc),
            },
            NEWARRAY | ANEWARRAY => match self.new_array_class(pc) {
                Some((id, shift)) => self.allocate(pc, id, Allocation::Array(shift)),
                None => return self.step(pc),
            },
            MULTIANEWARRAY | MONITORENTER | MONITOREXIT => return self.step(pc),
            _ => return Err(format!("unsupported opcode {op:#x} at {pc}")),
        }
        Ok(true)
    }

    /// Pushes the raw slot value on the wasm stack as a new operand.
    fn push_raw(&mut self) {
        let depth = self.stack.len();
        self.f.instructions().local_set(local_of(self.c.op_base, depth, T::I64));
        self.stack.push(Entry::Local(T::I64));
    }

    fn iinc(&mut self, index: usize, delta: i32) {
        let local = self.java_locals + index as u32;
        self.ins().local_get(local).i32_wrap_i64().i32_const(delta).i32_add().i64_extend_i32_u().local_set(local);
        self.mark_written(index);
    }
}

/// Each reachable block's successor blocks.
fn block_successors(code: &[u8], analysis: &Analysis, block_of: &[u16]) -> Vec<Vec<usize>> {
    let n = analysis.blocks.len();
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 0..n {
        let start = analysis.blocks[b];
        if analysis.depth[start] < 0 {
            continue;
        }
        let end = analysis.blocks.get(b + 1).copied().unwrap_or(code.len());
        let mut last = start;
        let mut pc = start;
        while pc < end {
            last = pc;
            pc += instruction_length(code, pc).unwrap();
        }
        let mut targets = Vec::new();
        match code[last] {
            GOTO | GOTO_W => targets.push(branch_target(code, last)),
            IFEQ..=IF_ACMPNE | IFNULL | IFNONNULL => {
                targets.push(branch_target(code, last));
                targets.push(end);
            }
            TABLESWITCH | LOOKUPSWITCH => targets.extend(switch_targets(code, last)),
            IRETURN..=RETURN | ATHROW => {}
            _ => targets.push(end),
        }
        successors[b] = targets
            .into_iter()
            .filter(|&t| t < code.len() && block_of[t] != NO_BLOCK)
            .map(|t| block_of[t] as usize)
            .collect();
    }
    successors
}

/// Loops for a structured layout: each loop is the blocks from its header
/// to its last back edge's source. None if they do not nest, or a branch
/// enters a loop elsewhere than its header (then: the dispatcher layout).
struct Structure {
    /// For a loop header: its last block.
    loop_end: Vec<Option<usize>>,
    /// The header of the innermost loop containing the block (headers belong
    /// to the enclosing scope).
    scope: Vec<Option<usize>>,
}

fn loop_structure(successors: &[Vec<usize>]) -> Option<Structure> {
    let n = successors.len();
    let mut loop_end: Vec<Option<usize>> = vec![None; n];
    for (b, targets) in successors.iter().enumerate() {
        for &t in targets {
            if t <= b {
                loop_end[t] = Some(loop_end[t].map_or(b, |e| e.max(b)));
            }
        }
    }
    // Proper nesting, and scopes.
    let mut scope: Vec<Option<usize>> = vec![None; n];
    let mut open: Vec<(usize, usize)> = Vec::new(); // (header, end)
    for b in 0..n {
        while open.last().is_some_and(|&(_, e)| e < b) {
            open.pop();
        }
        scope[b] = open.last().map(|&(h, _)| h);
        if let Some(e) = loop_end[b] {
            if open.last().is_some_and(|&(_, outer)| e > outer) {
                return None; // overlapping loops
            }
            open.push((b, e));
        }
    }
    // Forward branches may only enter a loop through its header.
    for (b, targets) in successors.iter().enumerate() {
        for &t in targets {
            let mut l = scope[t];
            while let Some(h) = l {
                if b < h {
                    return None;
                }
                l = scope[h];
            }
        }
    }
    Some(Structure { loop_end, scope })
}

/// A label of the structured layout (see `Gen::structured_body`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Label {
    /// Ends right before the block's code (forward branches to it).
    Block(usize),
    /// A loop around a header's code (back edges).
    Loop(usize),
}

/// The dirty Java locals at each block's start: the union over the block's
/// predecessors of what they leave dirty (a block that syncs leaves what it
/// wrote after its last sync), to a fixpoint. Entering from the host (pc 0,
/// handlers, resumption) the frame is current.
fn dirty_at_entries(
    code: &[u8],
    analysis: &Analysis,
    block_of: &[u16],
    summaries: &[(bool, Vec<bool>)],
    max_locals: usize,
) -> Vec<Vec<bool>> {
    let n = analysis.blocks.len();
    let successors = block_successors(code, analysis, block_of);
    let mut entry = vec![vec![false; max_locals]; n];
    loop {
        let mut changed = false;
        for b in 0..n {
            let (synced, written_since_sync) = &summaries[b];
            if written_since_sync.is_empty() {
                continue; // not generated (unreachable)
            }
            let exit: Vec<bool> = (0..max_locals).map(|i| written_since_sync[i] || (!synced && entry[b][i])).collect();
            for &s in &successors[b] {
                for i in 0..max_locals {
                    if exit[i] && !entry[s][i] {
                        entry[s][i] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return entry;
        }
    }
}

/// The locals `code` writes.
fn written_locals(code: &[u8], max_locals: usize) -> Vec<bool> {
    let mut written = vec![false; max_locals];
    let mut pc = 0;
    while pc < code.len() {
        let (index, wide) = match code[pc] {
            ISTORE | FSTORE | ASTORE | IINC => (Some(code[pc + 1] as usize), false),
            LSTORE | DSTORE => (Some(code[pc + 1] as usize), true),
            op @ ISTORE_0..=ASTORE_3 => {
                let n = ((op - ISTORE_0) % 4) as usize;
                (Some(n), matches!(op, LSTORE_0..=LSTORE_3 | DSTORE_0..=DSTORE_3))
            }
            WIDE => {
                let index = u16::from_be_bytes([code[pc + 2], code[pc + 3]]) as usize;
                (Some(index), matches!(code[pc + 1], LSTORE | DSTORE))
            }
            _ => (None, false),
        };
        if let Some(index) = index {
            for i in [index, index + wide as usize] {
                if i < max_locals {
                    written[i] = true;
                }
            }
        }
        match instruction_length(code, pc) {
            Some(len) => pc += len,
            None => break,
        }
    }
    written
}

/// Whether all branches of `code` go forward (no loops).
fn forward_only(code: &[u8]) -> bool {
    let mut pc = 0;
    while pc < code.len() {
        match code[pc] {
            IFEQ..=IF_ACMPNE | IFNULL | IFNONNULL | GOTO | GOTO_W if branch_target(code, pc) <= pc => return false,
            TABLESWITCH | LOOKUPSWITCH if switch_targets(code, pc).iter().any(|&t| t <= pc) => return false,
            JSR | JSR_W | RET => return false,
            _ => {}
        }
        match instruction_length(code, pc) {
            Some(len) => pc += len,
            None => return false,
        }
    }
    true
}

/// Whether the code writes local 0.
fn stores_local_0(code: &[u8]) -> bool {
    let mut pc = 0;
    while pc < code.len() {
        let stores = match code[pc] {
            ISTORE | LSTORE | FSTORE | DSTORE | ASTORE | IINC => code[pc + 1] == 0,
            ISTORE_0 | LSTORE_0 | FSTORE_0 | DSTORE_0 | ASTORE_0 => true,
            WIDE => matches!(code[pc + 1], ISTORE | LSTORE | FSTORE | DSTORE | ASTORE | IINC) && code[pc + 2] == 0 && code[pc + 3] == 0,
            _ => false,
        };
        if stores {
            return true;
        }
        pc += instruction_length(code, pc).unwrap_or(1);
    }
    false
}

/// What `Gen::allocate` allocates: an object of a size, or an array with
/// elements of `1 << shift` bytes (length on the operand stack).
#[derive(Clone, Copy)]
enum Allocation {
    Object(u32),
    Array(u32),
}

/// Largest allocation compiled code makes from the TLAB (as the VM's).
const MAX_INLINE_ALLOCATION: usize = crate::threads::TLAB_SIZE / 4;

fn kind_type(kind: FieldKind) -> T {
    match kind {
        FieldKind::Long => T::I64,
        FieldKind::Float => T::F32,
        FieldKind::Double => T::F64,
        _ => T::I32,
    }
}

fn instruction_length(code: &[u8], pc: usize) -> Option<usize> {
    classfile::opcodes::instruction_length(code, pc)
}
