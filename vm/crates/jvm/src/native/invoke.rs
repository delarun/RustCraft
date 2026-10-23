//! java.lang.invoke support, modelled on HotSpot's methodHandles.cpp:
//! MethodHandleNatives, linking of invokedynamic / invokehandle call sites and
//! of MethodHandle, MethodType and dynamic constants.
//!
//! A MemberName's `method` field holds a ResolvedMethodName whose injected
//! `vmtarget` is the target's global method index; `vmindex` holds a field
//! offset or vtable index.

use super::*;
use crate::class::{FieldKind, MethodRef};
use crate::heap::ARRAY_BASE_OFFSET;
use crate::vm::LinkedSite;
use classfile::{Constant, FieldType, MethodDescriptor, ReferenceKind, access};

const MHN: &str = "java/lang/invoke/MethodHandleNatives";
const MN: &str = "java/lang/invoke/MemberName";
const RMN: &str = "java/lang/invoke/ResolvedMethodName";

// MethodHandleNatives.Constants (checked against the class in registerNatives).
const IS_METHOD: i32 = 0x0001_0000;
const IS_CONSTRUCTOR: i32 = 0x0002_0000;
const IS_FIELD: i32 = 0x0004_0000;
const IS_TYPE: i32 = 0x0008_0000;
const CALLER_SENSITIVE: i32 = 0x0010_0000;
const TRUSTED_FINAL: i32 = 0x0020_0000;
const REFERENCE_KIND_SHIFT: i32 = 24;
const REFERENCE_KIND_MASK: i32 = 0x0F;

const REF_GET_FIELD: i32 = 1;
const REF_GET_STATIC: i32 = 2;
const REF_PUT_FIELD: i32 = 3;
const REF_PUT_STATIC: i32 = 4;
const REF_INVOKE_VIRTUAL: i32 = 5;
const REF_INVOKE_STATIC: i32 = 6;
const REF_INVOKE_SPECIAL: i32 = 7;
const REF_NEW_INVOKE_SPECIAL: i32 = 8;
const REF_INVOKE_INTERFACE: i32 = 9;

/// HotSpot's JVM_RECOGNIZED_METHOD_MODIFIERS / JVM_RECOGNIZED_FIELD_MODIFIERS.
const METHOD_MODIFIERS: i32 = 0x1DFF;
const FIELD_MODIFIERS: i32 = 0x50DF;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/invoke/MethodHandleNatives.registerNatives()V", mhn_register_natives);
    n.add("java/lang/invoke/MethodHandleNatives.getNamedCon(I[Ljava/lang/Object;)I", return_zero_int);
    n.add("java/lang/invoke/MethodHandleNatives.init(Ljava/lang/invoke/MemberName;Ljava/lang/Object;)V", mhn_init);
    n.add("java/lang/invoke/MethodHandleNatives.expand(Ljava/lang/invoke/MemberName;)V", mhn_expand);
    n.add(
        "java/lang/invoke/MethodHandleNatives.resolve(Ljava/lang/invoke/MemberName;Ljava/lang/Class;IZ)Ljava/lang/invoke/MemberName;",
        mhn_resolve,
    );
    n.add("java/lang/invoke/MethodHandleNatives.objectFieldOffset(Ljava/lang/invoke/MemberName;)J", mhn_field_offset);
    n.add("java/lang/invoke/MethodHandleNatives.staticFieldOffset(Ljava/lang/invoke/MemberName;)J", mhn_field_offset);
    n.add("java/lang/invoke/MethodHandleNatives.staticFieldBase(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;", |t, a| {
        let mn = t.null_check(arg_ref(a, 0))?;
        let class = t.vm.class_from_mirror(t.field_ref(mn, MN, "clazz"));
        ret_ref(class.statics.get())
    });
    n.add("java/lang/invoke/MethodHandleNatives.getMemberVMInfo(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;", mhn_member_vm_info);
    n.add(
        "java/lang/invoke/MethodHandleNatives.setCallSiteTargetNormal(Ljava/lang/invoke/CallSite;Ljava/lang/invoke/MethodHandle;)V",
        set_call_site_target,
    );
    n.add(
        "java/lang/invoke/MethodHandleNatives.setCallSiteTargetVolatile(Ljava/lang/invoke/CallSite;Ljava/lang/invoke/MethodHandle;)V",
        set_call_site_target,
    );
}

fn mhn_register_natives(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let constants = t.load_class("java/lang/invoke/MethodHandleNatives$Constants")?;
    let expected = [
        ("MN_IS_METHOD", IS_METHOD),
        ("MN_IS_CONSTRUCTOR", IS_CONSTRUCTOR),
        ("MN_IS_FIELD", IS_FIELD),
        ("MN_IS_TYPE", IS_TYPE),
        ("MN_CALLER_SENSITIVE", CALLER_SENSITIVE),
        ("MN_TRUSTED_FINAL", TRUSTED_FINAL),
        ("MN_REFERENCE_KIND_SHIFT", REFERENCE_KIND_SHIFT),
        ("MN_REFERENCE_KIND_MASK", REFERENCE_KIND_MASK),
    ];
    for (name, value) in expected {
        let field = constants.find_field_by_name(name).expect("MethodHandleNatives constant");
        let actual = match constants.cp().get(field.constant_value.unwrap()) {
            Ok(Constant::Integer(v)) => *v,
            _ => panic!("{name} is not an int constant"),
        };
        assert_eq!(actual, value, "MethodHandleNatives.Constants.{name} changed");
    }
    ret_void()
}

// --- descriptors and types ------------------------------------------------------

/// Field descriptor of a class (`I`, `Ljava/lang/String;`, `[J`, `V`).
pub(crate) fn class_descriptor(class: ClassRef) -> String {
    match &class.kind {
        crate::class::ClassKind::Primitive(Some(kind)) => kind.descriptor_char().to_string(),
        crate::class::ClassKind::Primitive(None) => "V".into(),
        crate::class::ClassKind::Array { .. } => class.name.clone(),
        crate::class::ClassKind::Instance => format!("L{};", class.name),
    }
}

fn mirror_descriptor(t: &Thread<'_>, mirror: Ref) -> String {
    class_descriptor(t.vm.class_from_mirror(mirror))
}

fn class_array_descriptors(t: &Thread<'_>, array: Ref) -> String {
    let len = t.vm.heap.array_length(array) as u32;
    (0..len).map(|i| mirror_descriptor(t, Ref(t.vm.heap.read(array, ARRAY_BASE_OFFSET + i * 4)))).collect()
}

/// Descriptor of a MemberName `type`: a MethodType, a descriptor String, a
/// Class (fields) or `Object[] { Class rtype, Class[] ptypes }`.
fn type_descriptor(t: &mut Thread<'_>, ty: Ref) -> JResult<String> {
    let ty = t.null_check(ty)?;
    let class = t.vm.class_of(ty);
    Ok(match class.name.as_str() {
        "java/lang/String" => t.vm.string_value(ty),
        "java/lang/Class" => mirror_descriptor(t, ty),
        "java/lang/invoke/MethodType" => method_type_descriptor(t, ty),
        "[Ljava/lang/Object;" => {
            let rtype = Ref(t.vm.heap.read(ty, ARRAY_BASE_OFFSET));
            let ptypes = Ref(t.vm.heap.read(ty, ARRAY_BASE_OFFSET + 4));
            format!("({}){}", class_array_descriptors(t, ptypes), mirror_descriptor(t, rtype))
        }
        other => return Err(t.throw_new("java/lang/InternalError", Some(&format!("unrecognized MemberName type {other}")))),
    })
}

pub(crate) fn method_type_descriptor(t: &Thread<'_>, method_type: Ref) -> String {
    let rtype = t.field_ref(method_type, "java/lang/invoke/MethodType", "rtype");
    let ptypes = t.field_ref(method_type, "java/lang/invoke/MethodType", "ptypes");
    format!("({}){}", class_array_descriptors(t, ptypes), mirror_descriptor(t, rtype))
}

/// The class named by a field descriptor, loaded relative to `loader`.
pub(crate) fn class_for_type(t: &mut Thread<'_>, loader: Ref, ty: &FieldType) -> JResult<ClassRef> {
    let vm = t.vm;
    Ok(match ty {
        FieldType::Object(name) => t.load_class_in(loader, name)?,
        FieldType::Array(_) => t.load_class_in(loader, &ty.descriptor())?,
        primitive => vm.primitive_class(Some(FieldKind::from_descriptor(&primitive.descriptor()))),
    })
}

fn mirror_for_type(t: &mut Thread<'_>, loader: Ref, ty: Option<&FieldType>) -> JResult<Ref> {
    let class = match ty {
        Some(ty) => class_for_type(t, loader, ty)?,
        None => t.vm.primitive_class(None),
    };
    Ok(t.vm.mirror(class))
}

/// A MethodType for a method descriptor (via MethodHandleNatives.findMethodHandleType).
pub(crate) fn method_type(t: &mut Thread<'_>, loader: Ref, descriptor: &str) -> JResult<Ref> {
    let parsed = MethodDescriptor::parse(descriptor)
        .map_err(|e| t.throw_new("java/lang/ClassFormatError", Some(&e.to_string())))?;
    let rtype = mirror_for_type(t, loader, parsed.ret.as_ref())?;
    let mut ptypes = Vec::with_capacity(parsed.params.len());
    for param in &parsed.params {
        ptypes.push(mirror_for_type(t, loader, Some(param))?);
    }
    let ptypes = t.new_object_array("java/lang/Class", &ptypes)?;
    let result = t.invoke_static(
        MHN,
        "findMethodHandleType",
        "(Ljava/lang/Class;[Ljava/lang/Class;)Ljava/lang/invoke/MethodType;",
        &[rtype.slot(), ptypes.slot()],
    )?;
    Ok(Ref::from_slot(result.unwrap()))
}

// --- MemberName <-> methods --------------------------------------------------------

/// The method a resolved MemberName points at.
pub(crate) fn member_name_method(t: &mut Thread<'_>, mn: Ref) -> JResult<MethodRef> {
    let mn = t.null_check(mn)?;
    let resolved = t.field_ref(mn, MN, "method");
    if resolved.is_null() {
        return Err(t.throw_new("java/lang/InternalError", Some("MemberName is not resolved to a method")));
    }
    Ok(t.vm.method_by_index(t.field(resolved, RMN, "vmtarget") as u32))
}

pub(crate) fn resolved_method_name(t: &mut Thread<'_>, method: MethodRef) -> JResult<Ref> {
    new_resolved_method_name(t, method)
}

fn new_resolved_method_name(t: &mut Thread<'_>, method: MethodRef) -> JResult<Ref> {
    let class = t.load_class(RMN)?;
    let rmn = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    t.set_field(rmn, RMN, "vmholder", t.vm.mirror(method.class()).slot());
    t.set_field(rmn, RMN, "vmtarget", method.global_index as u64);
    Ok(rmn)
}

/// Fills a MemberName for a method (HotSpot's init_method_MemberName).
fn fill_method(t: &mut Thread<'_>, mn: Ref, method: MethodRef, ref_kind: i32, holder: ClassRef) -> JResult<()> {
    let kind_flag = if method.name == "<init>" { IS_CONSTRUCTOR } else { IS_METHOD };
    let mut flags = (method.access_flags as i32 & METHOD_MODIFIERS) | kind_flag | (ref_kind << REFERENCE_KIND_SHIFT);
    if is_caller_sensitive(method) {
        flags |= CALLER_SENSITIVE;
    }
    let vmindex = match ref_kind {
        REF_INVOKE_VIRTUAL | REF_INVOKE_INTERFACE => method.vtable_index.map_or(-1, |i| i as i64),
        _ => -1,
    };
    let rmn = new_resolved_method_name(t, method)?;
    t.set_field(mn, MN, "flags", flags as u32 as u64);
    t.set_field(mn, MN, "method", rmn.slot());
    t.set_field(mn, MN, "vmindex", vmindex as u64);
    t.set_field(mn, MN, "clazz", t.vm.mirror(holder).slot());
    Ok(())
}

/// The reference kind for calling `method` (HotSpot's CallInfo classification).
fn call_kind(method: MethodRef, holder: ClassRef) -> i32 {
    if method.is_static() {
        REF_INVOKE_STATIC
    } else if method.name == "<init>" {
        REF_INVOKE_SPECIAL
    } else if holder.is_interface() {
        if method.is_private() { REF_INVOKE_SPECIAL } else { REF_INVOKE_INTERFACE }
    } else if can_be_statically_bound(method) {
        REF_INVOKE_SPECIAL
    } else {
        REF_INVOKE_VIRTUAL
    }
}

fn can_be_statically_bound(method: MethodRef) -> bool {
    method.is_private() || method.access_flags & access::FINAL != 0 || method.class().access_flags & access::FINAL != 0
}

fn fill_field(t: &mut Thread<'_>, mn: Ref, holder: ClassRef, field: &crate::class::Field, setter: bool) -> JResult<()> {
    let mut ref_kind = if field.is_static() { REF_GET_STATIC } else { REF_GET_FIELD };
    if setter {
        ref_kind += REF_PUT_FIELD - REF_GET_FIELD;
    }
    let mut flags = (field.access_flags as i32 & FIELD_MODIFIERS) | IS_FIELD | (ref_kind << REFERENCE_KIND_SHIFT);
    if is_trusted_final(holder, field) {
        flags |= TRUSTED_FINAL;
    }
    t.set_field(mn, MN, "flags", flags as u32 as u64);
    t.set_field(mn, MN, "vmindex", field.offset as u64);
    t.set_field(mn, MN, "clazz", t.vm.mirror(holder).slot());
    Ok(())
}

pub(crate) fn is_trusted_final(holder: ClassRef, field: &crate::class::Field) -> bool {
    field.access_flags & access::FINAL != 0
        && (field.is_static() || holder.hidden || holder.file.as_ref().is_some_and(|f| f.record_components.is_some()))
}

fn is_caller_sensitive(method: MethodRef) -> bool {
    has_annotation(method, "Ljdk/internal/reflect/CallerSensitive;")
}

/// Whether a method carries a (runtime-visible) annotation of this type.
pub(crate) fn has_annotation(method: MethodRef, descriptor: &str) -> bool {
    let Some(file) = method.class().file.as_ref() else { return false };
    let Some(info) = file.methods.iter().find(|m| m.name == method.name && m.descriptor == method.descriptor) else {
        return false;
    };
    let Some(annotations) = info.attribute("RuntimeVisibleAnnotations") else { return false };
    annotation_types(&file.constant_pool, annotations).iter().any(|t| *t == descriptor)
}

/// Type descriptors of the annotations in a Runtime*Annotations attribute.
fn annotation_types<'a>(cp: &'a classfile::ConstantPool, data: &[u8]) -> Vec<&'a str> {
    fn u2(data: &[u8], pos: &mut usize) -> Option<u16> {
        let v = u16::from_be_bytes([*data.get(*pos)?, *data.get(*pos + 1)?]);
        *pos += 2;
        Some(v)
    }
    fn skip_element_value(data: &[u8], pos: &mut usize) -> Option<()> {
        let tag = *data.get(*pos)?;
        *pos += 1;
        match tag {
            b'e' => *pos += 4,
            b'@' => skip_annotation(data, pos)?,
            b'[' => {
                for _ in 0..u2(data, pos)? {
                    skip_element_value(data, pos)?;
                }
            }
            _ => *pos += 2,
        }
        Some(())
    }
    fn skip_annotation(data: &[u8], pos: &mut usize) -> Option<()> {
        *pos += 2;
        for _ in 0..u2(data, pos)? {
            *pos += 2;
            skip_element_value(data, pos)?;
        }
        Some(())
    }
    let mut types = Vec::new();
    let mut pos = 0;
    let Some(count) = u2(data, &mut pos) else { return types };
    for _ in 0..count {
        let start = pos;
        let Some(type_index) = u2(data, &mut pos) else { break };
        pos = start;
        if skip_annotation(data, &mut pos).is_none() {
            break;
        }
        if let Ok(name) = cp.utf8(type_index) {
            types.push(name);
        }
    }
    types
}

// --- MethodHandleNatives natives -------------------------------------------------

fn mhn_init(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mn = t.null_check(arg_ref(a, 0))?;
    let target = t.null_check(arg_ref(a, 1))?;
    let target_class = t.vm.class_of(target).name.clone();
    match target_class.as_str() {
        "java/lang/reflect/Method" | "java/lang/reflect/Constructor" => {
            let holder = t.vm.class_from_mirror(t.field_ref(target, &target_class, "clazz"));
            let slot = t.field(target, &target_class, "slot") as usize;
            let method = holder.find_method_by_slot(slot);
            fill_method(t, mn, method, call_kind(method, holder), holder)?;
        }
        "java/lang/reflect/Field" => {
            let holder = t.vm.class_from_mirror(t.field_ref(target, &target_class, "clazz"));
            let slot = t.field(target, &target_class, "slot") as usize;
            let field = &holder.fields[slot];
            fill_field(t, mn, holder, field, false)?;
            let name = t.new_string(&field.name)?;
            let name = t.vm.intern(name);
            t.set_field(mn, MN, "name", name.slot());
            let ty = FieldType::parse(&field.descriptor).unwrap();
            let ty = mirror_for_type(t, holder.loader, Some(&ty))?;
            t.set_field(mn, MN, "type", ty.slot());
        }
        other => return Err(t.throw_new("java/lang/InternalError", Some(&format!("MemberName.init: unsupported {other}")))),
    }
    ret_void()
}

fn mhn_expand(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mn = t.null_check(arg_ref(a, 0))?;
    let flags = t.field(mn, MN, "flags") as i32;
    if flags & (IS_METHOD | IS_CONSTRUCTOR) != 0 {
        let method = member_name_method(t, mn)?;
        if t.field_ref(mn, MN, "name").is_null() {
            let name = t.new_string(&method.name)?;
            let name = t.vm.intern(name);
            t.set_field(mn, MN, "name", name.slot());
        }
        if t.field_ref(mn, MN, "type").is_null() {
            let descriptor = t.new_string(&method.descriptor)?;
            t.set_field(mn, MN, "type", descriptor.slot());
        }
    }
    ret_void()
}

fn mhn_resolve(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mn = t.null_check(arg_ref(a, 0))?;
    let speculative = arg_z(a, 3);
    match resolve_member_name(t, mn) {
        Ok(()) => ret_ref(mn),
        Err(error) if speculative => {
            let _ = error;
            ret_ref(Ref::NULL)
        }
        Err(error) => Err(error),
    }
}

fn resolve_member_name(t: &mut Thread<'_>, mn: Ref) -> JResult<()> {
    if !t.field_ref(mn, MN, "method").is_null() {
        return Ok(()); // already resolved
    }
    let flags = t.field(mn, MN, "flags") as i32;
    let ref_kind = (flags >> REFERENCE_KIND_SHIFT) & REFERENCE_KIND_MASK;
    let clazz = t.null_check(t.field_ref(mn, MN, "clazz"))?;
    let defc = t.vm.class_from_mirror(clazz);
    let name_ref = t.null_check(t.field_ref(mn, MN, "name"))?;
    let name = t.vm.string_value(name_ref);
    let ty = t.field_ref(mn, MN, "type");
    let descriptor = type_descriptor(t, ty)?;

    if flags & IS_FIELD != 0 {
        let found = defc.super_classes().find_map(|c| c.find_field(&name, &descriptor).map(|f| (c, f))).or_else(|| {
            defc.all_interfaces().iter().copied().find_map(|i| i.find_field(&name, &descriptor).map(|f| (i, f)))
        });
        let Some((holder, field)) = found else {
            let message = format!("{}.{name}", defc.java_name());
            return Err(t.throw_new("java/lang/NoSuchFieldError", Some(&message)));
        };
        let setter = matches!(ref_kind, REF_PUT_FIELD | REF_PUT_STATIC);
        return fill_field(t, mn, holder, field, setter);
    }
    if flags & (IS_METHOD | IS_CONSTRUCTOR) == 0 {
        return Err(t.throw_new("java/lang/InternalError", Some("unrecognized MemberName kind")));
    }

    let vm = t.vm;
    let lookup = |interface: bool| vm.lookup_method(defc, &name, &descriptor, interface);
    let resolved = match ref_kind {
        REF_INVOKE_STATIC | REF_INVOKE_SPECIAL | REF_INVOKE_VIRTUAL | REF_INVOKE_INTERFACE => lookup(defc.is_interface()),
        REF_NEW_INVOKE_SPECIAL => defc.find_method("<init>", &descriptor).ok_or_else(|| {
            crate::vm::VmError::new("java/lang/NoSuchMethodError", format!("{}.<init>{descriptor}", defc.java_name()))
        }),
        _ => Err(crate::vm::VmError::new("java/lang/InternalError", format!("bad reference kind {ref_kind}"))),
    };
    let mut method = resolved.map_err(|e| t.throw_vm_error(e))?;
    if method.is_signature_polymorphic() {
        method = t.vm.signature_polymorphic(method, &descriptor);
    }
    if (ref_kind == REF_INVOKE_STATIC) != method.is_static() {
        let message = format!("{}.{name}{descriptor}: static mismatch", defc.java_name());
        return Err(t.throw_new("java/lang/IncompatibleClassChangeError", Some(&message)));
    }
    let resolved_kind = match ref_kind {
        REF_INVOKE_VIRTUAL | REF_INVOKE_INTERFACE if method.intrinsic.is_none() => {
            if method.is_private() || (!method.class().is_interface() && can_be_statically_bound(method)) {
                REF_INVOKE_SPECIAL
            } else if method.class().is_interface() && ref_kind == REF_INVOKE_INTERFACE {
                REF_INVOKE_INTERFACE
            } else {
                REF_INVOKE_VIRTUAL
            }
        }
        REF_NEW_INVOKE_SPECIAL => REF_INVOKE_SPECIAL,
        kind => kind,
    };
    // Virtual calls through an interface method keep the requested class as holder.
    let holder = if resolved_kind == REF_INVOKE_VIRTUAL && method.class().is_interface() { defc } else { method.class() };
    fill_method(t, mn, method, resolved_kind, holder)
}

fn mhn_field_offset(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mn = t.null_check(arg_ref(a, 0))?;
    ret_j(t.field(mn, MN, "vmindex") as i64)
}

fn mhn_member_vm_info(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mn = t.null_check(arg_ref(a, 0))?;
    let vmindex = t.field(mn, MN, "vmindex");
    let boxed = box_value(t, FieldKind::Long, vmindex)?;
    let flags = t.field(mn, MN, "flags") as i32;
    let target = if flags & IS_FIELD != 0 { t.field_ref(mn, MN, "clazz") } else { mn };
    ret_ref(t.new_object_array("java/lang/Object", &[boxed, target])?)
}

fn set_call_site_target(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let site = t.null_check(arg_ref(a, 0))?;
    t.set_field(site, "java/lang/invoke/CallSite", "target", a[1]);
    ret_void()
}

// --- boxing -----------------------------------------------------------------------

const WRAPPERS: [(FieldKind, &str, &str); 8] = [
    (FieldKind::Boolean, "java/lang/Boolean", "Z"),
    (FieldKind::Byte, "java/lang/Byte", "B"),
    (FieldKind::Char, "java/lang/Character", "C"),
    (FieldKind::Short, "java/lang/Short", "S"),
    (FieldKind::Int, "java/lang/Integer", "I"),
    (FieldKind::Long, "java/lang/Long", "J"),
    (FieldKind::Float, "java/lang/Float", "F"),
    (FieldKind::Double, "java/lang/Double", "D"),
];

/// Boxes a primitive slot value (`Integer.valueOf` etc.); references pass through.
pub(crate) fn box_value(t: &mut Thread<'_>, kind: FieldKind, value: u64) -> JResult<Ref> {
    let Some((_, class, desc)) = WRAPPERS.iter().find(|(k, ..)| *k == kind) else {
        return Ok(Ref::from_slot(value));
    };
    let args: &[u64] = if kind.is_wide() { &[value, 0] } else { &[value] };
    let boxed = t.invoke_static(class, "valueOf", &format!("({desc})L{class};"), args)?;
    Ok(Ref::from_slot(boxed.unwrap()))
}

/// Unboxes a wrapper object to a slot value of `kind`, widening as Java's
/// method invocation conversion allows; throws IllegalArgumentException on mismatch.
pub(crate) fn unbox_value(t: &mut Thread<'_>, kind: FieldKind, obj: Ref) -> JResult<u64> {
    if kind == FieldKind::Ref {
        return Ok(obj.slot());
    }
    if obj.is_null() {
        return Err(t.throw_new("java/lang/IllegalArgumentException", Some("null cannot be unboxed")));
    }
    let class_name = t.vm.class_of(obj).name.clone();
    let Some(&(source, class, _)) = WRAPPERS.iter().find(|(_, c, _)| *c == class_name) else {
        return Err(t.throw_new("java/lang/IllegalArgumentException", Some("argument type mismatch")));
    };
    let raw = t.field(obj, class, "value");
    match widen_slot(source, kind, raw) {
        Some(value) => Ok(value),
        None => Err(t.throw_new("java/lang/IllegalArgumentException", Some("argument type mismatch"))),
    }
}

/// A primitive slot value of kind `source` as kind `target`, by an identity
/// or widening primitive conversion (JLS 5.1.2); None if there is none.
pub(crate) fn widen_slot(source: FieldKind, target: FieldKind, raw: u64) -> Option<u64> {
    use FieldKind::*;
    Some(match (source, target) {
        (s, k) if s == k => raw,
        (Byte | Short | Char | Int, Int) => raw,
        (Byte, Short) => raw,
        (Byte | Short | Char | Int, Long) => raw as i32 as i64 as u64,
        (Byte | Short | Char | Int, Float) => (raw as i32 as f32).to_bits() as u64,
        (Byte | Short | Char | Int, Double) => (raw as i32 as f64).to_bits(),
        (Long, Float) => (raw as i64 as f32).to_bits() as u64,
        (Long, Double) => (raw as i64 as f64).to_bits(),
        (Float, Double) => (f32::from_bits(raw as u32) as f64).to_bits(),
        _ => return None,
    })
}

// --- constants and call sites --------------------------------------------------------

impl Thread<'_> {
    /// Resolves a loadable constant to an object (boxing primitives), as passed
    /// to bootstrap methods.
    pub(crate) fn constant_object(&mut self, class: ClassRef, index: u16) -> JResult<Ref> {
        let constant = class.cp().get(index).map_err(|e| self.throw_new("java/lang/ClassFormatError", Some(&e.0)))?.clone();
        match constant {
            Constant::Integer(v) => box_value(self, FieldKind::Int, v as u32 as u64),
            Constant::Float(v) => box_value(self, FieldKind::Float, v.to_bits() as u64),
            Constant::Long(v) => box_value(self, FieldKind::Long, v as u64),
            Constant::Double(v) => box_value(self, FieldKind::Double, v.to_bits()),
            Constant::String(utf8) => {
                let units = classfile::mutf8::decode_utf16(class.cp().utf8_bytes(utf8).unwrap()).unwrap_or_default();
                self.vm.intern_utf16(&units).map_err(|e| self.throw_vm_error(e))
            }
            Constant::Class(_) => {
                let target = self.resolve_class(class, index)?;
                Ok(self.vm.mirror(target))
            }
            Constant::MethodType(_) | Constant::MethodHandle { .. } | Constant::Dynamic { .. } => {
                self.linked_constant(class, index)
            }
            other => {
                let message = format!("unexpected loadable constant {other:?}");
                Err(self.throw_new("java/lang/InternalError", Some(&message)))
            }
        }
    }

    /// MethodType, MethodHandle and dynamic constants, linked once per class
    /// and cached (dynamic constants of primitive type are boxed).
    pub(crate) fn linked_constant(&mut self, class: ClassRef, index: u16) -> JResult<Ref> {
        let cached = self.vm.constants.lock().unwrap().get(&(class.id, index)).copied();
        if let Some(value) = cached {
            crate::threads::push_handle(value);
            return Ok(value);
        }
        let constant = class.cp().get(index).unwrap().clone();
        let value = match constant {
            Constant::MethodType(descriptor) => {
                let descriptor = class.cp().utf8(descriptor).unwrap().to_owned();
                method_type(self, class.loader, &descriptor)?
            }
            Constant::MethodHandle { kind, reference } => self.method_handle_constant(class, kind, reference)?,
            Constant::Dynamic { bootstrap_method, name_and_type } => {
                let (name, descriptor) = class.cp().name_and_type(name_and_type).unwrap();
                let (name, descriptor) = (name.to_owned(), descriptor.to_owned());
                let bsm = self.bootstrap_method(class, bootstrap_method)?;
                let static_args = self.bootstrap_arguments(class, bootstrap_method)?;
                let name = self.new_string(&name)?;
                let name = self.vm.intern(name);
                let ty = FieldType::parse(&descriptor).unwrap();
                let ty = mirror_for_type(self, class.loader, Some(&ty))?;
                let caller = self.vm.mirror(class);
                let result = self.invoke_static(
                    MHN,
                    "linkDynamicConstant",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    &[caller.slot(), bsm.slot(), name.slot(), ty.slot(), static_args.slot()],
                )?;
                Ref::from_slot(result.unwrap())
            }
            _ => unreachable!("not a linked constant"),
        };
        Ok(*self.vm.constants.lock().unwrap().entry((class.id, index)).or_insert(value))
    }

    fn method_handle_constant(&mut self, class: ClassRef, kind: ReferenceKind, reference: u16) -> JResult<Ref> {
        let cp = class.cp();
        let member = cp.member_ref(reference).unwrap();
        let (name, descriptor) = (member.name.to_owned(), member.descriptor.to_owned());
        let class_index = match cp.get(reference).unwrap() {
            Constant::FieldRef(m) | Constant::MethodRef(m) | Constant::InterfaceMethodRef(m) => m.class,
            _ => unreachable!("validated by the parser"),
        };
        let defc = self.resolve_class(class, class_index)?;
        let ty = match kind {
            ReferenceKind::GetField | ReferenceKind::GetStatic | ReferenceKind::PutField | ReferenceKind::PutStatic => {
                let ty = FieldType::parse(&descriptor).unwrap();
                mirror_for_type(self, class.loader, Some(&ty))?
            }
            _ => method_type(self, class.loader, &descriptor)?,
        };
        let name = self.new_string(&name)?;
        let name = self.vm.intern(name);
        let caller = self.vm.mirror(class);
        let defc = self.vm.mirror(defc);
        let result = self.invoke_static(
            MHN,
            "linkMethodHandleConstant",
            "(Ljava/lang/Class;ILjava/lang/Class;Ljava/lang/String;Ljava/lang/Object;)Ljava/lang/invoke/MethodHandle;",
            &[caller.slot(), kind as u64, defc.slot(), name.slot(), ty.slot()],
        )?;
        Ok(Ref::from_slot(result.unwrap()))
    }

    fn bootstrap_method(&mut self, class: ClassRef, index: u16) -> JResult<Ref> {
        let spec = &class.file.as_ref().unwrap().bootstrap_methods[index as usize];
        self.linked_constant(class, spec.method_ref)
    }

    /// Static bootstrap arguments: null when there are none, else an Object[].
    fn bootstrap_arguments(&mut self, class: ClassRef, index: u16) -> JResult<Ref> {
        let spec = &class.file.as_ref().unwrap().bootstrap_methods[index as usize];
        if spec.arguments.is_empty() {
            return Ok(Ref::NULL);
        }
        let mut args = Vec::with_capacity(spec.arguments.len());
        for &arg in &spec.arguments {
            args.push(self.constant_object(class, arg)?);
        }
        self.new_object_array("java/lang/Object", &args)
    }

    /// Links an invokedynamic instruction (once per call site).
    pub(crate) fn link_indy(&mut self, method: MethodRef, pc: u32, index: u16) -> JResult<LinkedSite> {
        if let Some(&site) = self.vm.indy_sites.lock().unwrap().get(&(method.global_index, pc)) {
            return Ok(site);
        }
        let class = method.class();
        let (bootstrap_index, name_and_type) = match class.cp().get(index) {
            Ok(&Constant::InvokeDynamic { bootstrap_method, name_and_type }) => (bootstrap_method, name_and_type),
            _ => return Err(self.throw_new("java/lang/ClassFormatError", Some("bad invokedynamic constant"))),
        };
        let (name, descriptor) = class.cp().name_and_type(name_and_type).unwrap();
        let (name, descriptor) = (name.to_owned(), descriptor.to_owned());
        let bsm = self.bootstrap_method(class, bootstrap_index)?;
        let static_args = self.bootstrap_arguments(class, bootstrap_index)?;
        let name_string = self.new_string(&name)?;
        let name_string = self.vm.intern(name_string);
        let ty = method_type(self, class.loader, &descriptor)?;
        let appendix = self.new_object_array("java/lang/Object", &[Ref::NULL])?;
        let caller = self.vm.mirror(class);
        let invoker = self.invoke_static(
            MHN,
            "linkCallSite",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/invoke/MemberName;",
            &[caller.slot(), bsm.slot(), name_string.slot(), ty.slot(), static_args.slot(), appendix.slot()],
        )?;
        let site = self.linked_site(Ref::from_slot(invoker.unwrap()), appendix, &descriptor, 0)?;
        Ok(*self.vm.indy_sites.lock().unwrap().entry((method.global_index, pc)).or_insert(site))
    }

    /// Links a MethodHandle.invoke/invokeExact or VarHandle call site.
    pub(crate) fn link_handle_site(&mut self, class: ClassRef, index: u16, resolved: MethodRef) -> JResult<LinkedSite> {
        if let Some(&site) = self.vm.handle_sites.lock().unwrap().get(&(class.id, index)) {
            return Ok(site);
        }
        let ty = method_type(self, class.loader, &resolved.descriptor)?;
        let name = self.new_string(&resolved.name)?;
        let name = self.vm.intern(name);
        let appendix = self.new_object_array("java/lang/Object", &[Ref::NULL])?;
        let caller = self.vm.mirror(class);
        let defc = self.vm.mirror(resolved.class());
        let invoker = self.invoke_static(
            MHN,
            "linkMethod",
            "(Ljava/lang/Class;ILjava/lang/Class;Ljava/lang/String;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/invoke/MemberName;",
            &[caller.slot(), REF_INVOKE_VIRTUAL as u64, defc.slot(), name.slot(), ty.slot(), appendix.slot()],
        )?;
        let site = self.linked_site(Ref::from_slot(invoker.unwrap()), appendix, &resolved.descriptor, 1)?;
        Ok(*self.vm.handle_sites.lock().unwrap().entry((class.id, index)).or_insert(site))
    }

    fn linked_site(&mut self, invoker: Ref, appendix: Ref, descriptor: &str, receiver_slots: usize) -> JResult<LinkedSite> {
        let invoker = member_name_method(self, invoker)?;
        let call_slots = MethodDescriptor::parse(descriptor).unwrap().param_slots() + receiver_slots;
        let appendix_value = Ref(self.vm.heap.read(appendix, ARRAY_BASE_OFFSET));
        let has_appendix = invoker.arg_slots as usize == call_slots + 1;
        let appendix = has_appendix.then(|| &*Box::leak(Box::new(crate::heap::RefCell32::new(appendix_value))));
        Ok(LinkedSite { invoker, appendix })
    }
}
