//! StackWalker support (HotSpot's stackwalk.cpp): `callStackWalk` fills the
//! first batch of frame infos and calls back `doStackWalk`; further batches
//! come from `fetchStackFrames` using the walk's anchor.

use super::*;
use crate::class::MethodRef;
use crate::heap::ARRAY_BASE_OFFSET;

const ASW: &str = "java/lang/StackStreamFactory$AbstractStackWalker";
const CFI: &str = "java/lang/ClassFrameInfo";
const SFI: &str = "java/lang/StackFrameInfo";

// StackStreamFactory modes.
const CLASS_INFO_ONLY: i32 = 0x2;
const SHOW_HIDDEN_FRAMES: i32 = 0x20;

// MemberName flags recorded in ClassFrameInfo.flags.
const MN_IS_METHOD: i32 = 0x0001_0000;
const MN_IS_CONSTRUCTOR: i32 = 0x0002_0000;
const MN_CALLER_SENSITIVE: i32 = 0x0010_0000;
const MN_HIDDEN_MEMBER: i32 = 0x0040_0000;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/StackStreamFactory.checkStackWalkModes()Z", |_, _| ret_z(true));
    n.add(
        "java/lang/StackStreamFactory$AbstractStackWalker.callStackWalk(IILjdk/internal/vm/ContinuationScope;Ljdk/internal/vm/Continuation;II[Ljava/lang/Object;)Ljava/lang/Object;",
        call_stack_walk,
    );
    n.add("java/lang/StackStreamFactory$AbstractStackWalker.fetchStackFrames(IJIII[Ljava/lang/Object;)I", fetch_stack_frames);
    n.add("java/lang/StackStreamFactory$AbstractStackWalker.setContinuation(J[Ljava/lang/Object;Ljdk/internal/vm/Continuation;)V", no_op);
    n.add("java/lang/StackFrameInfo.expandStackFrameInfo()V", |t, a| {
        let info = arg_ref(a, 0);
        let method = frame_method(t, info)?;
        let name = t.new_string(&method.name)?;
        let name = t.vm.intern(name);
        t.set_field(info, SFI, "name", name.slot());
        let descriptor = t.new_string(&method.descriptor)?;
        t.set_field(info, SFI, "type", descriptor.slot());
        ret_void()
    });
    n.add("java/lang/StackTraceElement.initStackTraceElement(Ljava/lang/StackTraceElement;Ljava/lang/StackFrameInfo;)V", |t, a| {
        let (element, info) = (t.null_check(arg_ref(a, 0))?, t.null_check(arg_ref(a, 1))?);
        let method = frame_method(t, info)?;
        let bci = t.field(info, SFI, "bci") as u32;
        super::lang::fill_stack_trace_element(t, element, method, bci)?;
        ret_void()
    });
}

/// Whether a frame is hidden from stack traces and walks (HotSpot's
/// Method::is_hidden): methods of hidden classes and @Hidden methods.
pub(crate) fn is_hidden_frame(method: MethodRef) -> bool {
    method.class().hidden || super::invoke::has_annotation(method, "Ljdk/internal/vm/annotation/Hidden;")
}

fn frame_method(t: &mut Thread<'_>, info: Ref) -> JResult<MethodRef> {
    let target = t.field_ref(info, CFI, "classOrMemberName");
    let target = t.null_check(target)?;
    Ok(t.vm.method_by_index(t.field(target, "java/lang/invoke/ResolvedMethodName", "vmtarget") as u32))
}

/// The frames a walk sees, innermost first.
fn walk_frames(t: &mut Thread<'_>, mode: i32, skip: i32) -> Vec<(MethodRef, u32)> {
    let walker_classes = ["java/lang/StackWalker", ASW];
    let frames = t.stack_trace();
    let mut index = 0;
    // Skip the stack walking implementation itself.
    while index < frames.len() {
        let class = frames[index].0.class();
        let is_walker = walker_classes.contains(&class.name.as_str())
            || class.super_class.is_some_and(|s| s.name == ASW);
        if !is_walker {
            break;
        }
        index += 1;
    }
    frames[index..]
        .iter()
        .copied()
        .filter(|(method, _)| mode & SHOW_HIDDEN_FRAMES != 0 || !is_hidden_frame(*method))
        .skip(skip.max(0) as usize)
        .collect()
}

/// Fills buffer[start..end) (the batch: `end` is the batch size, frames
/// start at index 1) from `frames`; returns the number filled.
fn fill(t: &mut Thread<'_>, mode: i32, buffer: Ref, start: i32, end: i32, frames: &[(MethodRef, u32)]) -> JResult<i32> {
    let end = end.min(t.vm.heap.array_length(buffer));
    let count = frames.len().min((end - start).max(0) as usize);
    for (i, &(method, pc)) in frames[..count].iter().enumerate() {
        let info = Ref(t.vm.heap.read(buffer, ARRAY_BASE_OFFSET + (start as u32 + i as u32) * 4));
        crate::threads::push_handle(info);
        let info = t.null_check(info)?;
        let mut flags = t.field(info, CFI, "flags") as i32 | (method.access_flags as i32 & 0x1DFF);
        flags |= if method.name == "<init>" { MN_IS_CONSTRUCTOR } else { MN_IS_METHOD };
        if super::invoke::has_annotation(method, "Ljdk/internal/reflect/CallerSensitive;") {
            flags |= MN_CALLER_SENSITIVE;
        }
        if is_hidden_frame(method) {
            flags |= MN_HIDDEN_MEMBER;
        }
        t.set_field(info, CFI, "flags", flags as u32 as u64);
        if mode & CLASS_INFO_ONLY != 0 {
            t.set_field(info, CFI, "classOrMemberName", t.vm.mirror(method.class()).slot());
        } else {
            let resolved = super::invoke::resolved_method_name(t, method)?;
            t.set_field(info, CFI, "classOrMemberName", resolved.slot());
            t.set_field(info, SFI, "bci", pc as u64);
        }
    }
    Ok(count as i32)
}

fn call_stack_walk(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let walker = arg_ref(a, 0);
    let (mode, skip) = (arg_i(a, 1), arg_i(a, 2));
    let (batch, start) = (arg_i(a, 5), arg_i(a, 6));
    let buffer = t.null_check(arg_ref(a, 7))?;
    let frames = walk_frames(t, mode, skip);
    let filled = fill(t, mode, buffer, start, batch, &frames)?;
    let anchor = t.begin_stack_walk(frames, filled as usize);
    let result = t.invoke_virtual(
        walker,
        "doStackWalk",
        "(JIIII)Ljava/lang/Object;",
        &[anchor as u64, 0, skip as u32 as u64, filled as u32 as u64, start as u32 as u64, (start + filled) as u32 as u64],
    );
    t.end_stack_walk(anchor);
    ret_ref(Ref::from_slot(result?.unwrap()))
}

fn fetch_stack_frames(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mode = arg_i(a, 1);
    let anchor = arg_j(a, 2);
    let (batch, start) = (arg_i(a, 5), arg_i(a, 6));
    let buffer = t.null_check(arg_ref(a, 7))?;
    let Some(frames) = t.stack_walk_remaining(anchor) else {
        return Err(t.throw_new("java/lang/InternalError", Some("doStackWalk: corrupted buffers on stack")));
    };
    let filled = fill(t, mode, buffer, start, batch, &frames)?;
    t.advance_stack_walk(anchor, filled as usize);
    ret_i(filled)
}
