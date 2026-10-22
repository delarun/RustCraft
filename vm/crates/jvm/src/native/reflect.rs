//! jdk.internal.reflect natives.

use super::*;

pub(super) fn register(n: &mut Natives) {
    n.add("jdk/internal/reflect/Reflection.getCallerClass()Ljava/lang/Class;", reflection_get_caller_class);
    n.add("jdk/internal/reflect/Reflection.getClassAccessFlags(Ljava/lang/Class;)I", |t, a| {
        let class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 0))?);
        ret_i(class.access_flags as i32)
    });
    n.add("jdk/internal/reflect/Reflection.areNestMates(Ljava/lang/Class;Ljava/lang/Class;)Z", reflection_are_nest_mates);
}

/// The class of the caller of the method that called `getCallerClass()`,
/// skipping reflection machinery frames.
fn reflection_get_caller_class(t: &mut Thread<'_>, _: &[u64]) -> JResult<Option<u64>> {
    let caller = t
        .stack_trace()
        .into_iter()
        .skip(1)
        .map(|(method, _)| method.class())
        .find(|class| !is_reflection_frame(&class.name));
    ret_ref(caller.map_or(Ref::NULL, |c| t.vm.mirror(c)))
}

fn is_reflection_frame(class: &str) -> bool {
    class == "java/lang/reflect/Method" || class.starts_with("jdk/internal/reflect/")
}

fn nest_host(t: &mut Thread<'_>, class: ClassRef) -> JResult<ClassRef> {
    t.nest_host(class)
}

fn reflection_are_nest_mates(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let first = t.vm.class_from_mirror(t.null_check(arg_ref(a, 0))?);
    let second = t.vm.class_from_mirror(t.null_check(arg_ref(a, 1))?);
    let (first, second) = (nest_host(t, first)?, nest_host(t, second)?);
    ret_z(std::ptr::eq(first, second))
}

// --- Class.getDeclared*0 ---------------------------------------------------------

pub(super) fn register_members(n: &mut Natives) {
    // No MethodParameters attribute data: Executable synthesizes arg0, arg1...
    n.add("java/lang/reflect/Executable.getParameters0()[Ljava/lang/reflect/Parameter;", |_, _| ret_ref(Ref::NULL));
    n.add("java/lang/Class.getDeclaredFields0(Z)[Ljava/lang/reflect/Field;", class_declared_fields);
    n.add("java/lang/Class.getDeclaredMethods0(Z)[Ljava/lang/reflect/Method;", |t, a| class_declared_methods(t, a, false));
    n.add("java/lang/Class.getDeclaredConstructors0(Z)[Ljava/lang/reflect/Constructor;", |t, a| {
        class_declared_methods(t, a, true)
    });
    n.add("java/lang/Class.getDeclaredClasses0()[Ljava/lang/Class;", class_declared_classes);
    n.add("java/lang/Class.getPermittedSubclasses0()[Ljava/lang/Class;", class_permitted_subclasses);
    n.add("java/lang/Class.getRecordComponents0()[Ljava/lang/reflect/RecordComponent;", class_record_components);
    n.add(
        "jdk/internal/reflect/DirectMethodHandleAccessor$NativeAccessor.invoke0(Ljava/lang/reflect/Method;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;",
        native_accessor_invoke,
    );
    n.add(
        "jdk/internal/reflect/DirectConstructorHandleAccessor$NativeAccessor.newInstance0(Ljava/lang/reflect/Constructor;[Ljava/lang/Object;)Ljava/lang/Object;",
        native_accessor_new_instance,
    );
    n.add("java/lang/reflect/Array.newArray(Ljava/lang/Class;I)Ljava/lang/Object;", array_new_array);
    n.add("java/lang/reflect/Array.getLength(Ljava/lang/Object;)I", |t, a| {
        let array = t.null_check(arg_ref(a, 0))?;
        if !t.vm.class_of(array).is_array() {
            return Err(t.throw_new("java/lang/IllegalArgumentException", Some("Argument is not an array")));
        }
        ret_i(t.vm.heap.array_length(array))
    });
    n.add("java/lang/reflect/Array.multiNewArray(Ljava/lang/Class;[I)Ljava/lang/Object;", array_multi_new_array);
}

fn this_class(t: &Thread<'_>, a: &[u64]) -> ClassRef {
    t.vm.class_from_mirror(arg_ref(a, 0))
}

fn mirrors(t: &mut Thread<'_>, loader: Ref, types: &[classfile::FieldType]) -> JResult<Ref> {
    let mut items = Vec::with_capacity(types.len());
    for ty in types {
        let class = super::invoke::class_for_type(t, loader, ty)?;
        items.push(t.vm.mirror(class));
    }
    t.new_object_array("java/lang/Class", &items)
}

fn optional_string(t: &mut Thread<'_>, s: Option<&str>) -> JResult<Ref> {
    match s {
        Some(s) => t.new_string(s),
        None => Ok(Ref::NULL),
    }
}

fn optional_bytes(t: &mut Thread<'_>, data: Option<&[u8]>) -> JResult<Ref> {
    match data {
        Some(data) => super::lang::new_byte_array(t, data),
        None => Ok(Ref::NULL),
    }
}

fn construct(t: &mut Thread<'_>, class: &str, descriptor: &str, args: &[u64]) -> JResult<Ref> {
    let class = t.load_class(class)?;
    t.ensure_initialized(class)?;
    let obj = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
    let init = class.find_method("<init>", descriptor).expect("reflection constructor");
    let mut all = vec![obj.slot()];
    all.extend_from_slice(args);
    t.invoke(init, &all)?;
    Ok(obj)
}

fn class_declared_fields(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let public_only = arg_z(a, 1);
    let mut fields = Vec::new();
    let file_fields = class.file.as_ref().map(|f| &f.fields[..]).unwrap_or(&[]);
    for (slot, field) in class.fields.iter().enumerate() {
        if field.injected || (public_only && field.access_flags & classfile::access::PUBLIC == 0) {
            continue;
        }
        let info = &file_fields[slot];
        let declaring = t.vm.mirror(class);
        let name = t.new_string(&field.name)?;
        let name = t.vm.intern(name);
        let ty = super::invoke::class_for_type(t, class.loader, &classfile::FieldType::parse(&field.descriptor).unwrap())?;
        let ty = t.vm.mirror(ty);
        let signature = optional_string(t, info.signature.as_deref())?;
        let annotations = optional_bytes(t, info.attribute("RuntimeVisibleAnnotations"))?;
        let trusted_final = super::invoke::is_trusted_final(class, field);
        let modifiers = field.access_flags as i32 & 0x50DF;
        fields.push(construct(
            t,
            "java/lang/reflect/Field",
            "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/Class;IZILjava/lang/String;[B)V",
            &[declaring.slot(), name.slot(), ty.slot(), modifiers as u32 as u64, trusted_final as u64, slot as u64, signature.slot(), annotations.slot()],
        )?);
    }
    ret_ref(t.new_object_array("java/lang/reflect/Field", &fields)?)
}

fn class_declared_methods(t: &mut Thread<'_>, a: &[u64], constructors: bool) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let public_only = arg_z(a, 1);
    let mut members = Vec::new();
    if class.file.is_none() {
        let element = if constructors { "java/lang/reflect/Constructor" } else { "java/lang/reflect/Method" };
        return ret_ref(t.new_object_array(element, &[])?);
    }
    for (slot, method) in class.methods.iter().enumerate() {
        if (method.name == "<init>") != constructors || method.name == "<clinit>" {
            continue;
        }
        if public_only && method.access_flags & classfile::access::PUBLIC == 0 {
            continue;
        }
        let member = member_object(t, class, slot)?;
        members.push(member);
    }
    let element = if constructors { "java/lang/reflect/Constructor" } else { "java/lang/reflect/Method" };
    ret_ref(t.new_object_array(element, &members)?)
}

/// A java.lang.reflect.Method or Constructor for the method in `slot`.
fn member_object(t: &mut Thread<'_>, class: ClassRef, slot: usize) -> JResult<Ref> {
    let method = &class.methods[slot];
    let file = class.file.as_ref().expect("class with methods has a class file");
    let constructors = method.name == "<init>";
    let info = &file.methods[slot];
    let descriptor = classfile::MethodDescriptor::parse(&method.descriptor).unwrap();
    let declaring = t.vm.mirror(class);
    let params = mirrors(t, class.loader, &descriptor.params)?;
    let exceptions: Vec<classfile::FieldType> =
        info.exceptions.iter().map(|e| classfile::FieldType::Object(e.clone())).collect();
    let exceptions = mirrors(t, class.loader, &exceptions)?;
    let modifiers = method.access_flags as i32 & 0x1DFF;
    let signature = optional_string(t, info.signature.as_deref())?;
    let annotations = optional_bytes(t, info.attribute("RuntimeVisibleAnnotations"))?;
    let parameter_annotations = optional_bytes(t, info.attribute("RuntimeVisibleParameterAnnotations"))?;
    let member = if constructors {
        construct(
        t,
        "java/lang/reflect/Constructor",
        "(Ljava/lang/Class;[Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B)V",
        &[
            declaring.slot(),
            params.slot(),
            exceptions.slot(),
            modifiers as u32 as u64,
            slot as u64,
            signature.slot(),
            annotations.slot(),
            parameter_annotations.slot(),
        ],
        )?
    } else {
        let name = t.new_string(&method.name)?;
        let name = t.vm.intern(name);
        let ret = match &descriptor.ret {
        Some(ty) => super::invoke::class_for_type(t, class.loader, ty)?,
        None => t.vm.primitive_class(None),
        };
        let ret = t.vm.mirror(ret);
        let default = optional_bytes(t, info.attribute("AnnotationDefault"))?;
        construct(
        t,
        "java/lang/reflect/Method",
        "(Ljava/lang/Class;Ljava/lang/String;[Ljava/lang/Class;Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B[B)V",
        &[
            declaring.slot(),
            name.slot(),
            params.slot(),
            ret.slot(),
            exceptions.slot(),
            modifiers as u32 as u64,
            slot as u64,
            signature.slot(),
            annotations.slot(),
            parameter_annotations.slot(),
            default.slot(),
        ],
        )?
    };
    Ok(member)
}

fn class_record_components(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let Some(components) = class.file.as_ref().and_then(|f| f.record_components.as_ref()) else {
        return ret_ref(Ref::NULL);
    };
    let rc = "java/lang/reflect/RecordComponent";
    let rc_class = t.load_class(rc)?;
    t.ensure_initialized(rc_class)?;
    let mut items = Vec::with_capacity(components.len());
    for component in components {
        let obj = t.vm.new_object(rc_class).map_err(|e| t.throw_vm_error(e))?;
        t.set_field(obj, rc, "clazz", t.vm.mirror(class).slot());
        let name = t.new_string(&component.name)?;
        let name = t.vm.intern(name);
        t.set_field(obj, rc, "name", name.slot());
        let ty = super::invoke::class_for_type(t, class.loader, &classfile::FieldType::parse(&component.descriptor).unwrap())?;
        t.set_field(obj, rc, "type", t.vm.mirror(ty).slot());
        let accessor_descriptor = format!("(){}", component.descriptor);
        if let Some(slot) = class.methods.iter().position(|m| m.name == component.name && m.descriptor == accessor_descriptor) {
            let accessor = member_object(t, class, slot)?;
            t.set_field(obj, rc, "accessor", accessor.slot());
        }
        let signature = optional_string(t, component.signature.as_deref())?;
        t.set_field(obj, rc, "signature", signature.slot());
        let find = |name: &str| component.attributes.iter().find(|a| &*a.name == name).map(|a| &*a.data);
        let annotations = optional_bytes(t, find("RuntimeVisibleAnnotations"))?;
        t.set_field(obj, rc, "annotations", annotations.slot());
        let type_annotations = optional_bytes(t, find("RuntimeVisibleTypeAnnotations"))?;
        t.set_field(obj, rc, "typeAnnotations", type_annotations.slot());
        items.push(obj);
    }
    ret_ref(t.new_object_array(rc, &items)?)
}

fn class_declared_classes(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let names: Vec<String> = class
        .file
        .as_ref()
        .map(|f| {
            f.inner_classes
                .iter()
                .filter(|ic| ic.outer_class.as_deref() == Some(class.name.as_str()) && ic.inner_name.is_some())
                .map(|ic| ic.inner_class.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut mirrors = Vec::new();
    for name in names {
        let inner = t.load_class_in(class.loader, &name)?;
        mirrors.push(t.vm.mirror(inner));
    }
    ret_ref(t.new_object_array("java/lang/Class", &mirrors)?)
}

fn class_permitted_subclasses(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let class = this_class(t, a);
    let Some(file) = class.file.as_ref().filter(|f| !f.permitted_subclasses.is_empty()) else {
        return ret_ref(Ref::NULL);
    };
    let mut mirrors = Vec::new();
    for name in &file.permitted_subclasses {
        let sub = t.load_class_in(class.loader, name)?;
        mirrors.push(t.vm.mirror(sub));
    }
    ret_ref(t.new_object_array("java/lang/Class", &mirrors)?)
}

// --- reflective invocation ----------------------------------------------------------

/// Unboxes `args` (an Object[]) into argument slots for `method`.
fn reflective_args(t: &mut Thread<'_>, method: crate::class::MethodRef, receiver: Option<Ref>, args: Ref) -> JResult<Vec<u64>> {
    let count = if args.is_null() { 0 } else { t.vm.heap.array_length(args) as usize };
    if count != method.params.len() {
        return Err(t.throw_new("java/lang/IllegalArgumentException", Some("wrong number of arguments")));
    }
    let mut slots = Vec::with_capacity(method.arg_slots as usize);
    if let Some(receiver) = receiver {
        slots.push(receiver.slot());
    }
    for (i, &kind) in method.params.iter().enumerate() {
        let arg = Ref(t.vm.heap.read(args, crate::heap::ARRAY_BASE_OFFSET + i as u32 * 4));
        slots.push(super::invoke::unbox_value(t, kind, arg)?);
        if kind.is_wide() {
            slots.push(0);
        }
    }
    Ok(slots)
}

fn wrap_invocation_target(t: &mut Thread<'_>, error: crate::interp::Throw) -> crate::interp::Throw {
    let crate::interp::Throw(exception) = error;
    match construct(t, "java/lang/reflect/InvocationTargetException", "(Ljava/lang/Throwable;)V", &[exception.slot()]) {
        Ok(wrapper) => crate::interp::Throw(wrapper),
        Err(e) => e,
    }
}

fn native_accessor_invoke(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let method_obj = t.null_check(arg_ref(a, 0))?;
    let holder = t.vm.class_from_mirror(t.field_ref(method_obj, "java/lang/reflect/Method", "clazz"));
    let slot = t.field(method_obj, "java/lang/reflect/Method", "slot") as usize;
    let mut method = holder.find_method_by_slot(slot);
    let receiver = if method.is_static() {
        t.ensure_initialized(holder)?;
        None
    } else {
        let receiver = arg_ref(a, 1);
        if receiver.is_null() {
            return Err(t.throw_new("java/lang/NullPointerException", None));
        }
        let receiver_class = t.vm.class_of(receiver);
        if !receiver_class.is_subclass_of(holder) {
            return Err(t.throw_new("java/lang/IllegalArgumentException", Some("object is not an instance of declaring class")));
        }
        if !method.is_private() {
            method = t.vm.select_method(receiver_class, method).map_err(|e| t.throw_vm_error(e))?;
        }
        Some(receiver)
    };
    let args = reflective_args(t, method, receiver, arg_ref(a, 2))?;
    let result = t.invoke(method, &args).map_err(|e| wrap_invocation_target(t, e))?;
    match (method.ret, result) {
        (Some(kind), Some(value)) => ret_ref(super::invoke::box_value(t, kind, value)?),
        _ => ret_ref(Ref::NULL),
    }
}

fn native_accessor_new_instance(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let ctor = t.null_check(arg_ref(a, 0))?;
    let holder = t.vm.class_from_mirror(t.field_ref(ctor, "java/lang/reflect/Constructor", "clazz"));
    let slot = t.field(ctor, "java/lang/reflect/Constructor", "slot") as usize;
    let method = holder.find_method_by_slot(slot);
    if holder.access_flags & classfile::access::ABSTRACT != 0 {
        return Err(t.throw_new("java/lang/InstantiationException", Some(&holder.java_name())));
    }
    t.ensure_initialized(holder)?;
    let obj = t.vm.new_object(holder).map_err(|e| t.throw_vm_error(e))?;
    let args = reflective_args(t, method, Some(obj), arg_ref(a, 1))?;
    t.invoke(method, &args).map_err(|e| wrap_invocation_target(t, e))?;
    ret_ref(obj)
}

fn array_new_array(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let component = t.vm.class_from_mirror(t.null_check(arg_ref(a, 0))?);
    if matches!(component.kind, crate::class::ClassKind::Primitive(None)) {
        return Err(t.throw_new("java/lang/IllegalArgumentException", Some("void")));
    }
    let array_class = t.vm.array_class(component);
    let array = t.vm.new_array(array_class, arg_i(a, 1)).map_err(|e| t.throw_vm_error(e))?;
    ret_ref(array)
}

fn array_multi_new_array(t: &mut Thread<'_>, a: &[u64]) -> JResult<Option<u64>> {
    let mut class = t.vm.class_from_mirror(t.null_check(arg_ref(a, 0))?);
    let dims = t.null_check(arg_ref(a, 1))?;
    let count = t.vm.heap.array_length(dims);
    let lengths: Vec<i32> = (0..count as u32).map(|i| t.vm.heap.read(dims, crate::heap::ARRAY_BASE_OFFSET + i * 4)).collect();
    for _ in 0..count {
        class = t.vm.array_class(class);
    }
    fn build(t: &mut Thread<'_>, class: ClassRef, lengths: &[i32]) -> JResult<Ref> {
        let array = t.vm.new_array(class, lengths[0]).map_err(|e| t.throw_vm_error(e))?;
        if lengths.len() > 1 {
            for i in 0..lengths[0] {
                let sub = build(t, class.component().unwrap(), &lengths[1..])?;
                t.vm.heap.write(array, crate::heap::ARRAY_BASE_OFFSET + i as u32 * 4, sub.0);
            }
        }
        Ok(array)
    }
    ret_ref(build(t, class, &lengths)?)
}

// --- jdk.internal.reflect.ConstantPool (annotation parsing) ----------------------------

const CP: &str = "jdk/internal/reflect/ConstantPool";

pub(super) fn register_constant_pool(n: &mut Natives) {
    n.add("java/lang/Class.getConstantPool()Ljdk/internal/reflect/ConstantPool;", |t, a| {
        let mirror = arg_ref(a, 0);
        let class = t.load_class(CP)?;
        t.ensure_initialized(class)?;
        let pool = t.vm.new_object(class).map_err(|e| t.throw_vm_error(e))?;
        t.set_field(pool, CP, "constantPoolOop", mirror.slot());
        ret_ref(pool)
    });
    n.add("jdk/internal/reflect/ConstantPool.getSize0(Ljava/lang/Object;)I", |t, a| {
        ret_i(pool_class(t, a).file.as_ref().map_or(0, |f| f.constant_pool.len() as i32))
    });
    n.add("jdk/internal/reflect/ConstantPool.getUTF8At0(Ljava/lang/Object;I)Ljava/lang/String;", |t, a| {
        let class = pool_class(t, a);
        let bytes = class.cp().utf8_bytes(arg_i(a, 2) as u16).map_err(|e| t.throw_new("java/lang/IllegalArgumentException", Some(&e.0)))?;
        let units = classfile::mutf8::decode_utf16(bytes).unwrap_or_default();
        ret_ref(t.vm.new_string_utf16(&units).map_err(|e| t.throw_vm_error(e))?)
    });
    n.add("jdk/internal/reflect/ConstantPool.getStringAt0(Ljava/lang/Object;I)Ljava/lang/String;", |t, a| {
        let class = pool_class(t, a);
        ret_ref(t.constant_object(class, arg_i(a, 2) as u16)?)
    });
    n.add("jdk/internal/reflect/ConstantPool.getClassAt0(Ljava/lang/Object;I)Ljava/lang/Class;", |t, a| {
        let class = pool_class(t, a);
        let target = t.resolve_class(class, arg_i(a, 2) as u16)?;
        ret_ref(t.vm.mirror(target))
    });
    n.add("jdk/internal/reflect/ConstantPool.getIntAt0(Ljava/lang/Object;I)I", |t, a| match pool_constant(t, a)? {
        classfile::Constant::Integer(v) => ret_i(v),
        _ => Err(t.throw_new("java/lang/IllegalArgumentException", Some("Wrong type at constant pool index"))),
    });
    n.add("jdk/internal/reflect/ConstantPool.getLongAt0(Ljava/lang/Object;I)J", |t, a| match pool_constant(t, a)? {
        classfile::Constant::Long(v) => ret_j(v),
        _ => Err(t.throw_new("java/lang/IllegalArgumentException", Some("Wrong type at constant pool index"))),
    });
    n.add("jdk/internal/reflect/ConstantPool.getFloatAt0(Ljava/lang/Object;I)F", |t, a| match pool_constant(t, a)? {
        classfile::Constant::Float(v) => ret_i(v.to_bits() as i32),
        _ => Err(t.throw_new("java/lang/IllegalArgumentException", Some("Wrong type at constant pool index"))),
    });
    n.add("jdk/internal/reflect/ConstantPool.getDoubleAt0(Ljava/lang/Object;I)D", |t, a| match pool_constant(t, a)? {
        classfile::Constant::Double(v) => ret_j(v.to_bits() as i64),
        _ => Err(t.throw_new("java/lang/IllegalArgumentException", Some("Wrong type at constant pool index"))),
    });
    n.add("jdk/internal/reflect/ConstantPool.getTagAt0(Ljava/lang/Object;I)B", |t, a| {
        use classfile::Constant::*;
        let tag = match pool_constant(t, a)? {
            Unusable => 0,
            Utf8(_) => 1,
            Integer(_) => 3,
            Float(_) => 4,
            Long(_) => 5,
            Double(_) => 6,
            Class(_) => 7,
            String(_) => 8,
            FieldRef(_) => 9,
            MethodRef(_) => 10,
            InterfaceMethodRef(_) => 11,
            NameAndType { .. } => 12,
            MethodHandle { .. } => 15,
            MethodType(_) => 16,
            Dynamic { .. } => 17,
            InvokeDynamic { .. } => 18,
            Module(_) => 19,
            Package(_) => 20,
        };
        ret_i(tag)
    });
}

fn pool_class(t: &Thread<'_>, a: &[u64]) -> ClassRef {
    t.vm.class_from_mirror(arg_ref(a, 1))
}

fn pool_constant(t: &mut Thread<'_>, a: &[u64]) -> JResult<classfile::Constant> {
    let class = pool_class(t, a);
    let index = arg_i(a, 2);
    let entries = class.cp().entries();
    match entries.get(index as usize) {
        Some(constant) => Ok(constant.clone()),
        None => Err(t.throw_new("java/lang/IllegalArgumentException", Some("Constant pool index out of bounds"))),
    }
}
