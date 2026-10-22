//! Record equals/hashCode as plain bytecode.
//!
//! javac compiles a record's `equals` and `hashCode` to an invokedynamic of
//! `ObjectMethods.bootstrap`, which builds a tree of method handles that our
//! VM runs layer by layer on every call (Minecraft compares and hashes
//! records such as TagKey and ChunkPos constantly). Before such a class is
//! defined, those two methods are replaced with the bytecode the handle tree
//! amounts to, with the same results: the hash is `31 * h + hash(field)`
//! over the components in order (each hashed as by its wrapper's static
//! hashCode, references by Objects.hashCode), equality tests the components
//! last to first (as the guardWithTest nest does), floats and doubles by
//! Float/Double.compare. toString keeps the bootstrap.

use classfile::opcodes::*;
use classfile::{ClassFile, Code, Constant, ReferenceKind};

pub(crate) fn rewrite_object_methods(file: &mut ClassFile) {
    if file.super_class.as_deref() != Some("java/lang/Record") {
        return;
    }
    for index in 0..file.methods.len() {
        let method = &file.methods[index];
        let equals = match (method.name.as_str(), method.descriptor.as_str()) {
            ("equals", "(Ljava/lang/Object;)Z") => true,
            ("hashCode", "()I") => false,
            _ => continue,
        };
        let Some(code) = &method.code else { continue };
        let Some(components) = bootstrapped_components(file, &code.code, equals) else { continue };
        let Some(fields) = components.iter().map(|name| field_descriptor(file, name)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let components: Vec<(String, String)> = components.into_iter().zip(fields).collect();
        let code = if equals { equals_code(file, &components) } else { hash_code(file, &components) };
        file.methods[index].code = Some(code);
    }
}

/// The component names if `code` is `aload_0 [aload_1] invokedynamic
/// ObjectMethods.bootstrap; return`.
fn bootstrapped_components(file: &ClassFile, code: &[u8], equals: bool) -> Option<Vec<String>> {
    let (prefix, ret): (&[u8], u8) = if equals { (&[ALOAD_0, ALOAD_1], IRETURN) } else { (&[ALOAD_0], IRETURN) };
    let n = prefix.len();
    if code.len() != n + 6 || &code[..n] != prefix || code[n] != INVOKEDYNAMIC || code[n + 5] != ret {
        return None;
    }
    let cp = &file.constant_pool;
    let index = u16::from_be_bytes([code[n + 1], code[n + 2]]);
    let Ok(&Constant::InvokeDynamic { bootstrap_method, .. }) = cp.get(index) else { return None };
    let bootstrap = file.bootstrap_methods.get(bootstrap_method as usize)?;
    let Ok(&Constant::MethodHandle { kind: ReferenceKind::InvokeStatic, reference }) = cp.get(bootstrap.method_ref) else {
        return None;
    };
    let target = cp.member_ref(reference).ok()?;
    if target.class != "java/lang/runtime/ObjectMethods" || target.name != "bootstrap" {
        return None;
    }
    // Arguments: the record class, "a;b;c", the getters.
    let Ok(&Constant::String(names)) = cp.get(*bootstrap.arguments.get(1)?) else { return None };
    let names = cp.utf8(names).ok()?;
    Some(if names.is_empty() { Vec::new() } else { names.split(';').map(str::to_owned).collect() })
}

fn field_descriptor(file: &ClassFile, name: &str) -> Option<String> {
    file.fields
        .iter()
        .find(|f| f.name == name && f.access_flags & classfile::access::STATIC == 0)
        .map(|f| f.descriptor.clone())
}

/// Bytecode with forward branches patched once their targets are known.
struct Assembler {
    code: Vec<u8>,
}

impl Assembler {
    fn op(&mut self, op: u8) -> &mut Self {
        self.code.push(op);
        self
    }

    fn u2(&mut self, value: u16) -> &mut Self {
        self.code.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// A branch to be patched: returns its position.
    fn branch(&mut self, op: u8) -> usize {
        let at = self.code.len();
        self.code.extend_from_slice(&[op, 0, 0]);
        at
    }

    /// Points the branch at `at` here.
    fn land(&mut self, at: usize) {
        let offset = (self.code.len() - at) as i16;
        self.code[at + 1..at + 3].copy_from_slice(&offset.to_be_bytes());
    }
}

fn code(code: Vec<u8>, max_stack: u16, max_locals: u16) -> Code {
    Code { max_stack, max_locals, code, exception_table: Vec::new(), line_numbers: Vec::new(), stack_map_table: None }
}

fn hash_code(file: &mut ClassFile, components: &[(String, String)]) -> Code {
    let this = file.this_class.clone();
    let cp = &mut file.constant_pool;
    let mut a = Assembler { code: Vec::new() };
    a.op(ICONST_0);
    for (name, descriptor) in components {
        let field = cp.push_field_ref(&this, name, descriptor);
        a.op(BIPUSH).op(31).op(IMUL).op(ALOAD_0).op(GETFIELD).u2(field);
        match descriptor.as_str() {
            "I" | "S" | "C" | "B" => {}
            "Z" => {
                let zero = a.branch(IFEQ);
                a.op(SIPUSH).u2(1231);
                let done = a.branch(GOTO);
                a.land(zero);
                a.op(SIPUSH).u2(1237);
                a.land(done);
            }
            "J" => {
                a.op(DUP2).op(BIPUSH).op(32).op(LUSHR).op(LXOR).op(L2I);
            }
            "F" => {
                let m = cp.push_method_ref("java/lang/Float", "hashCode", "(F)I");
                a.op(INVOKESTATIC).u2(m);
            }
            "D" => {
                let m = cp.push_method_ref("java/lang/Double", "hashCode", "(D)I");
                a.op(INVOKESTATIC).u2(m);
            }
            _ => {
                let m = cp.push_method_ref("java/util/Objects", "hashCode", "(Ljava/lang/Object;)I");
                a.op(INVOKESTATIC).u2(m);
            }
        }
        a.op(IADD);
    }
    a.op(IRETURN);
    code(a.code, 6, 1)
}

fn equals_code(file: &mut ClassFile, components: &[(String, String)]) -> Code {
    let this = file.this_class.clone();
    let cp = &mut file.constant_pool;
    let class = cp.push_class(&this);
    let mut a = Assembler { code: Vec::new() };
    // Same object: true; not an instance of this record: false.
    a.op(ALOAD_0).op(ALOAD_1);
    let different = a.branch(IF_ACMPNE);
    a.op(ICONST_1).op(IRETURN);
    a.land(different);
    a.op(ALOAD_1).op(INSTANCEOF).u2(class);
    let instance = a.branch(IFNE);
    a.op(ICONST_0).op(IRETURN);
    a.land(instance);
    a.op(ALOAD_1).op(CHECKCAST).u2(class).op(ASTORE_2);
    let mut unequal = Vec::new();
    for (name, descriptor) in components.iter().rev() {
        let field = cp.push_field_ref(&this, name, descriptor);
        a.op(ALOAD_0).op(GETFIELD).u2(field).op(ALOAD_2).op(GETFIELD).u2(field);
        match descriptor.as_str() {
            "I" | "S" | "C" | "B" | "Z" => unequal.push(a.branch(IF_ICMPNE)),
            "J" => {
                a.op(LCMP);
                unequal.push(a.branch(IFNE));
            }
            "F" => {
                let m = cp.push_method_ref("java/lang/Float", "compare", "(FF)I");
                a.op(INVOKESTATIC).u2(m);
                unequal.push(a.branch(IFNE));
            }
            "D" => {
                let m = cp.push_method_ref("java/lang/Double", "compare", "(DD)I");
                a.op(INVOKESTATIC).u2(m);
                unequal.push(a.branch(IFNE));
            }
            _ => {
                let m = cp.push_method_ref("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z");
                a.op(INVOKESTATIC).u2(m);
                unequal.push(a.branch(IFEQ));
            }
        }
    }
    a.op(ICONST_1).op(IRETURN);
    for at in unequal {
        a.land(at);
    }
    a.op(ICONST_0).op(IRETURN);
    code(a.code, 4, 3)
}
