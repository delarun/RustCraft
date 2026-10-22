//! LWJGL's libffi natives, and its upcalls (C calling Java callbacks).
//!
//! Only what LWJGL itself needs from libffi: type descriptors, `ffi_cif`
//! preparation and closure memory. Callbacks do not use libffi closures:
//! a function pointer has a fixed wasm signature, so `Upcalls.upcallCreate`
//! (overridden here) hands out a free trampoline of the callback's
//! signature from a static pool; the trampoline calls the Java object's
//! `callback(ret, args)` with libffi-style arguments.

use std::collections::HashMap;

use super::*;
use crate::native::*;

// libffi type codes.
const FFI_TYPE_VOID: u16 = 0;
const FFI_TYPE_FLOAT: u16 = 2;
const FFI_TYPE_DOUBLE: u16 = 3;
const FFI_TYPE_UINT64: u16 = 11;
const FFI_TYPE_SINT64: u16 = 12;

/// `ffi_type` with 32-bit pointers: size, alignment, type, elements.
#[repr(C)]
struct FfiType {
    size: u32,
    alignment: u16,
    kind: u16,
    elements: u32,
}

macro_rules! ffi_types {
    ($($name:ident = $size:expr, $kind:expr;)*) => {
        $(static $name: FfiType = FfiType { size: $size, alignment: if $size == 0 { 1 } else { $size }, kind: $kind, elements: 0 };)*
    };
}
ffi_types! {
    VOID = 0, 0;
    UINT8 = 1, 5;
    SINT8 = 1, 6;
    UINT16 = 2, 7;
    SINT16 = 2, 8;
    UINT32 = 4, 9;
    SINT32 = 4, 10;
    UINT64 = 8, 11;
    SINT64 = 8, 12;
    FLOAT = 4, 2;
    DOUBLE = 8, 3;
    POINTER = 4, 14;
}

fn address_of(t: &'static FfiType) -> i64 {
    t as *const FfiType as u32 as i64
}

/// `ffi_cif` with 32-bit pointers.
#[repr(C)]
struct Cif {
    abi: i32,
    nargs: u32,
    arg_types: u32,
    rtype: u32,
    bytes: u32,
    flags: u32,
}

const FFI_SYSV: i32 = 1;

macro_rules! constants {
    ($n:ident, $($name:literal = $value:expr),* $(,)?) => {
        $($n.add(concat!("org/lwjgl/system/libffi/LibFFI.", $name, "()I"), |_, _| ret_i($value));)*
    };
}

macro_rules! types {
    ($n:ident, $($name:literal = $ty:ident),* $(,)?) => {
        $($n.add(concat!("org/lwjgl/system/libffi/LibFFI.nffi_type_", $name, "()J"), |_, _| ret_j(address_of(&$ty)));)*
    };
}

pub(super) fn register(n: &mut Natives) {
    constants!(n,
        "FFI_DEFAULT_ABI" = FFI_SYSV, "FFI_SYSV" = FFI_SYSV, "FFI_UNIX64" = FFI_SYSV,
        "FFI_FIRST_ABI" = 0, "FFI_LAST_ABI" = 2, "ffi_get_default_abi" = FFI_SYSV,
        "FFI_WIN64" = -1, "FFI_GNUW64" = -1, "FFI_EFI64" = -1, "FFI_STDCALL" = -1, "FFI_THISCALL" = -1,
        "FFI_FASTCALL" = -1, "FFI_MS_CDECL" = -1, "FFI_PASCAL" = -1, "FFI_REGISTER" = -1, "FFI_VFP" = -1,
    );
    n.add("org/lwjgl/system/libffi/LibFFI.FFI_TYPE_DOUBLE()S", |_, _| ret_i(FFI_TYPE_DOUBLE as i32));
    n.add("org/lwjgl/system/libffi/LibFFI.FFI_TYPE_LONGDOUBLE()S", |_, _| ret_i(4));
    types!(n,
        "void" = VOID, "uint8" = UINT8, "sint8" = SINT8, "uint16" = UINT16, "sint16" = SINT16,
        "uint32" = UINT32, "sint32" = SINT32, "uint64" = UINT64, "sint64" = SINT64,
        "uchar" = UINT8, "schar" = SINT8, "ushort" = UINT16, "sshort" = SINT16, "uint" = UINT32, "sint" = SINT32,
        "ulong" = UINT32, "slong" = SINT32, "float" = FLOAT, "double" = DOUBLE, "longdouble" = DOUBLE, "pointer" = POINTER,
    );
    n.add("org/lwjgl/system/libffi/LibFFI.nffi_prep_cif(JIIJJ)I", |_, a| {
        let cif = arg_j(a, 0) as usize as *mut Cif;
        // SAFETY: LWJGL's ffi_cif buffer (FFICIF.SIZEOF bytes).
        unsafe {
            *cif = Cif {
                abi: arg_i(a, 2),
                nargs: arg_i(a, 3) as u32,
                arg_types: arg_j(a, 6) as u32,
                rtype: arg_j(a, 4) as u32,
                bytes: 0,
                flags: 0,
            }
        };
        ret_i(0) // FFI_OK
    });
    n.add("org/lwjgl/system/libffi/FFICIF.offsets(J)I", |_, a| {
        write_ints(arg_j(a, 0), &[0, 4, 8, 12, 16, 20, 4]);
        ret_i(std::mem::size_of::<Cif>() as i32)
    });
    // ffi_closure: cif, fun, user_data.
    n.add("org/lwjgl/system/libffi/FFIClosure.offsets(J)I", |_, a| {
        write_ints(arg_j(a, 0), &[0, 4, 8, 4]);
        ret_i(12)
    });
    n.add("org/lwjgl/system/libffi/LibFFI.ffi_get_closure_size()J", |_, _| ret_j(12));
    n.add("org/lwjgl/system/libffi/LibFFI.nffi_closure_alloc(JJ)J", |_, a| {
        // SAFETY: allocation; the code address is the closure itself (only
        // used as a key: callbacks are made by `upcall_create`).
        unsafe {
            let closure = calloc(1, arg_j(a, 0) as usize);
            *(arg_j(a, 2) as usize as *mut u32) = closure as u32;
            ret_j(closure as u32 as i64)
        }
    });
    n.add("org/lwjgl/system/libffi/LibFFI.nffi_closure_free(J)V", |_, a| {
        unsafe { free(arg_j(a, 0) as usize as *mut std::ffi::c_void) };
        ret_void()
    });
    n.add("org/lwjgl/system/libffi/LibFFI.nffi_prep_closure_loc(JJJJJ)I", |_, _| ret_i(0));
    n.add("org/lwjgl/system/Upcalls.getCallbackHandler(Ljava/lang/reflect/Method;)J", |_, _| ret_j(1));
    n.add("org/lwjgl/system/Upcalls.shutdown()V", no_op);

    // Overrides of LWJGL's Java implementation (see the module comment).
    n.add("org/lwjgl/system/Upcalls.upcallCreate(Lorg/lwjgl/system/Callback$Descriptor;Ljava/lang/Object;)J", |t, a| {
        let descriptor = t.null_check(arg_ref(a, 0))?;
        let cif = t.field_ref(descriptor, "org/lwjgl/system/Callback$Descriptor", "cif");
        let cif = t.null_check(cif)?;
        let cif = t.field(cif, "org/lwjgl/system/Pointer$Default", "address") as usize as *const Cif;
        // SAFETY: the callback's cif, prepared by nffi_prep_cif.
        let signature = unsafe { signature_of(&*cif) };
        let Some(address) = allocate(signature.clone(), new_global(arg_ref(a, 1)), cif as u32) else {
            let message = format!("no callback trampolines left for signature {signature:?}");
            return Err(t.throw_new("java/lang/UnsupportedOperationException", Some(&message)));
        };
        ret_j(address as i64)
    });
    n.add("org/lwjgl/system/Upcalls.upcallGet(J)Lorg/lwjgl/system/CallbackI;", |_, a| {
        let closures = CLOSURES.lock().unwrap();
        ret_ref(closures.as_ref().and_then(|c| c.get(&(arg_j(a, 0) as usize))).map_or(Ref::NULL, |c| global(c.object)))
    });
    n.add("org/lwjgl/system/Upcalls.upcallFree(J)V", |_, a| {
        let address = arg_j(a, 0) as usize;
        let removed = CLOSURES.lock().unwrap().as_mut().and_then(|c| c.remove(&address));
        if let Some(closure) = removed {
            delete_global(closure.object);
            FREE.lock().unwrap().push(address);
        }
        ret_void()
    });
}

fn write_ints(address: i64, values: &[i32]) {
    for (i, &v) in values.iter().enumerate() {
        // SAFETY: LWJGL's offsets buffer.
        unsafe { *(address as usize as *mut i32).add(i) = v };
    }
}

/// A wasm value type of a callback's arguments or result.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum W {
    Void,
    I32,
    I64,
    F32,
    F64,
}

fn wasm_type(ty: u32) -> W {
    // SAFETY: an ffi_type from nffi_type_*.
    match unsafe { (*(ty as usize as *const FfiType)).kind } {
        FFI_TYPE_VOID => W::Void,
        FFI_TYPE_FLOAT => W::F32,
        FFI_TYPE_DOUBLE => W::F64,
        FFI_TYPE_UINT64 | FFI_TYPE_SINT64 => W::I64,
        _ => W::I32,
    }
}

type Signature = (Vec<W>, W);

/// # Safety
/// `cif` must be prepared.
unsafe fn signature_of(cif: &Cif) -> Signature {
    let types = cif.arg_types as usize as *const u32;
    let args = (0..cif.nargs as usize).map(|i| wasm_type(unsafe { *types.add(i) })).collect();
    (args, wasm_type(cif.rtype))
}

struct Closure {
    object: u64,
    cif: u32,
}

/// Live callbacks by trampoline address, and free trampolines.
static CLOSURES: Mutex<Option<HashMap<usize, Closure>>> = Mutex::new(None);
static FREE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn allocate(signature: Signature, object: u64, cif: u32) -> Option<usize> {
    let pool = pool(&signature)?;
    let mut closures = CLOSURES.lock().unwrap();
    let closures = closures.get_or_insert_with(HashMap::new);
    let address = pool.iter().copied().find(|a| !closures.contains_key(a))?;
    FREE.lock().unwrap().retain(|&a| a != address);
    closures.insert(address, Closure { object, cif });
    Some(address)
}

/// Calls the Java callback behind trampoline `address` with `args` (raw
/// values) and returns its raw result.
fn upcall(address: usize, args: &[u64]) -> u64 {
    let (object, cif) = {
        let closures = CLOSURES.lock().unwrap();
        let closure = closures.as_ref().and_then(|c| c.get(&address)).expect("a live callback");
        (global(closure.object), closure.cif)
    };
    // libffi's convention: an array of pointers to the argument values, and
    // a pointer to the result.
    let values: Vec<u64> = args.to_vec();
    let pointers: Vec<u32> = values.iter().map(|v| v as *const u64 as u32).collect();
    let mut result = 0u64;
    let _ = cif;
    with_current(|t| {
        let call = t.invoke_virtual(
            object,
            "callback",
            "(JJ)V",
            &[&mut result as *mut u64 as u32 as u64, 0, pointers.as_ptr() as u32 as u64, 0],
        );
        if let Err(throw) = call {
            let _ = t.invoke_virtual(throw.0, "printStackTrace", "()V", &[throw.0.slot()]);
        }
    });
    result
}

macro_rules! trampolines {
    ($pool:ident, ($($arg:ident: $ty:ty),*) -> $ret:ty, $wrap:expr) => {
        #[allow(clippy::unused_unit)]
        extern "C" fn $pool<const K: usize>($($arg: $ty),*) -> $ret {
            let raw = upcall($pool::<K> as *const () as usize, &[$(to_raw($arg)),*]);
            $wrap(raw)
        }
    };
}

trait Raw {
    fn raw(self) -> u64;
}
impl Raw for i32 {
    fn raw(self) -> u64 {
        self as u32 as u64
    }
}
impl Raw for i64 {
    fn raw(self) -> u64 {
        self as u64
    }
}
fn to_raw(v: impl Raw) -> u64 {
    v.raw()
}

trampolines!(v0, () -> (), |_| ());
trampolines!(v1, (a: i32) -> (), |_| ());
trampolines!(v2, (a: i32, b: i32) -> (), |_| ());
trampolines!(v3, (a: i32, b: i32, c: i32) -> (), |_| ());
trampolines!(v4, (a: i32, b: i32, c: i32, d: i32) -> (), |_| ());
trampolines!(v5, (a: i32, b: i32, c: i32, d: i32, e: i32) -> (), |_| ());
trampolines!(v6, (a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> (), |_| ());
trampolines!(v7, (a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> (), |_| ());
trampolines!(i0, () -> i32, |r: u64| r as i32);
trampolines!(i1, (a: i32) -> i32, |r: u64| r as i32);
trampolines!(i2, (a: i32, b: i32) -> i32, |r: u64| r as i32);
trampolines!(i3, (a: i32, b: i32, c: i32) -> i32, |r: u64| r as i32);
trampolines!(i4, (a: i32, b: i32, c: i32, d: i32) -> i32, |r: u64| r as i32);
trampolines!(i5, (a: i32, b: i32, c: i32, d: i32, e: i32) -> i32, |r: u64| r as i32);
trampolines!(i6, (a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32, |r: u64| r as i32);

macro_rules! pool {
    ($f:ident) => {
        vec![
            $f::<0> as *const () as usize, $f::<1> as *const () as usize, $f::<2> as *const () as usize, $f::<3> as *const () as usize,
            $f::<4> as *const () as usize, $f::<5> as *const () as usize, $f::<6> as *const () as usize, $f::<7> as *const () as usize,
        ]
    };
}

/// The trampolines for a signature (all-i32 arguments, void or i32 result).
fn pool(signature: &Signature) -> Option<Vec<usize>> {
    let (args, ret) = signature;
    if args.iter().any(|&w| w != W::I32) {
        return None;
    }
    Some(match (args.len(), ret) {
        (0, W::Void) => pool!(v0),
        (1, W::Void) => pool!(v1),
        (2, W::Void) => pool!(v2),
        (3, W::Void) => pool!(v3),
        (4, W::Void) => pool!(v4),
        (5, W::Void) => pool!(v5),
        (6, W::Void) => pool!(v6),
        (7, W::Void) => pool!(v7),
        (0, W::I32) => pool!(i0),
        (1, W::I32) => pool!(i1),
        (2, W::I32) => pool!(i2),
        (3, W::I32) => pool!(i3),
        (4, W::I32) => pool!(i4),
        (5, W::I32) => pool!(i5),
        (6, W::I32) => pool!(i6),
        _ => return None,
    })
}
