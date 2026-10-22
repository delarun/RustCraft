use crate::{ClassFormatError, Result, mutf8, reader::Reader};

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    /// Index 0, and the slot following a Long or Double.
    Unusable,
    Utf8(Utf8),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    FieldRef(MemberRef),
    MethodRef(MemberRef),
    InterfaceMethodRef(MemberRef),
    NameAndType { name: u16, descriptor: u16 },
    MethodHandle { kind: ReferenceKind, reference: u16 },
    MethodType(u16),
    Dynamic { bootstrap_method: u16, name_and_type: u16 },
    InvokeDynamic { bootstrap_method: u16, name_and_type: u16 },
    Module(u16),
    Package(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf8 {
    /// The original modified-UTF-8 bytes (exact contents for string literals).
    pub bytes: Box<[u8]>,
    /// Decoded form, for names and descriptors.
    pub string: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRef {
    pub class: u16,
    pub name_and_type: u16,
}

/// JVMS §5.4.3.5
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReferenceKind {
    GetField = 1,
    GetStatic = 2,
    PutField = 3,
    PutStatic = 4,
    InvokeVirtual = 5,
    InvokeStatic = 6,
    InvokeSpecial = 7,
    NewInvokeSpecial = 8,
    InvokeInterface = 9,
}

impl ReferenceKind {
    fn from_u8(kind: u8) -> Option<ReferenceKind> {
        use ReferenceKind::*;
        Some(match kind {
            1 => GetField,
            2 => GetStatic,
            3 => PutField,
            4 => PutStatic,
            5 => InvokeVirtual,
            6 => InvokeStatic,
            7 => InvokeSpecial,
            8 => NewInvokeSpecial,
            9 => InvokeInterface,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConstantPool {
    entries: Vec<Constant>,
}

/// A resolved symbolic member reference: owner class, name and descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRefInfo<'a> {
    pub class: &'a str,
    pub name: &'a str,
    pub descriptor: &'a str,
}

impl ConstantPool {
    pub(crate) fn parse(r: &mut Reader) -> Result<ConstantPool> {
        let count = r.u2()? as usize;
        if count == 0 {
            return Err(ClassFormatError::new("constant_pool_count is 0"));
        }
        let mut entries = Vec::with_capacity(count);
        entries.push(Constant::Unusable);
        while entries.len() < count {
            let tag = r.u1()?;
            let constant = match tag {
                1 => {
                    let len = r.u2()? as usize;
                    let bytes = r.bytes(len)?;
                    let string = mutf8::decode(bytes)
                        .map_err(|e| ClassFormatError::new(format!("constant #{}: {e}", entries.len())))?;
                    Constant::Utf8(Utf8 { bytes: bytes.into(), string: string.into() })
                }
                3 => Constant::Integer(r.u4()? as i32),
                4 => Constant::Float(f32::from_bits(r.u4()?)),
                5 => Constant::Long(r.u8()? as i64),
                6 => Constant::Double(f64::from_bits(r.u8()?)),
                7 => Constant::Class(r.u2()?),
                8 => Constant::String(r.u2()?),
                9 => Constant::FieldRef(MemberRef { class: r.u2()?, name_and_type: r.u2()? }),
                10 => Constant::MethodRef(MemberRef { class: r.u2()?, name_and_type: r.u2()? }),
                11 => Constant::InterfaceMethodRef(MemberRef { class: r.u2()?, name_and_type: r.u2()? }),
                12 => Constant::NameAndType { name: r.u2()?, descriptor: r.u2()? },
                15 => {
                    let kind = r.u1()?;
                    let kind = ReferenceKind::from_u8(kind)
                        .ok_or_else(|| ClassFormatError::new(format!("bad method handle kind {kind}")))?;
                    Constant::MethodHandle { kind, reference: r.u2()? }
                }
                16 => Constant::MethodType(r.u2()?),
                17 => Constant::Dynamic { bootstrap_method: r.u2()?, name_and_type: r.u2()? },
                18 => Constant::InvokeDynamic { bootstrap_method: r.u2()?, name_and_type: r.u2()? },
                19 => Constant::Module(r.u2()?),
                20 => Constant::Package(r.u2()?),
                _ => return Err(ClassFormatError::new(format!("unknown constant tag {tag} at #{}", entries.len()))),
            };
            let wide = matches!(constant, Constant::Long(_) | Constant::Double(_));
            entries.push(constant);
            if wide {
                if entries.len() >= count {
                    return Err(ClassFormatError::new("Long/Double constant in last slot"));
                }
                entries.push(Constant::Unusable);
            }
        }
        let pool = ConstantPool { entries };
        pool.validate()?;
        Ok(pool)
    }

    /// Checks that every cross-reference points at an entry of the right kind.
    fn validate(&self) -> Result<()> {
        for (index, entry) in self.entries.iter().enumerate() {
            let ok = match *entry {
                Constant::Class(name) | Constant::Module(name) | Constant::Package(name) => self.utf8(name).is_ok(),
                Constant::String(s) | Constant::MethodType(s) => self.utf8(s).is_ok(),
                Constant::FieldRef(m) | Constant::MethodRef(m) | Constant::InterfaceMethodRef(m) => {
                    self.class_name(m.class).is_ok() && self.name_and_type(m.name_and_type).is_ok()
                }
                Constant::NameAndType { name, descriptor } => self.utf8(name).is_ok() && self.utf8(descriptor).is_ok(),
                Constant::MethodHandle { kind, reference } => match kind {
                    ReferenceKind::GetField
                    | ReferenceKind::GetStatic
                    | ReferenceKind::PutField
                    | ReferenceKind::PutStatic => matches!(self.get(reference), Ok(Constant::FieldRef(_))),
                    _ => matches!(self.get(reference), Ok(Constant::MethodRef(_) | Constant::InterfaceMethodRef(_))),
                },
                Constant::Dynamic { name_and_type, .. } | Constant::InvokeDynamic { name_and_type, .. } => {
                    self.name_and_type(name_and_type).is_ok()
                }
                _ => true,
            };
            if !ok {
                return Err(ClassFormatError::new(format!("constant #{index} has an invalid reference")));
            }
        }
        Ok(())
    }

    /// Appends a constant (not Long/Double, which take two slots); returns
    /// its index.
    pub fn push(&mut self, constant: Constant) -> u16 {
        assert!(!matches!(constant, Constant::Long(_) | Constant::Double(_)), "wide constants take two slots");
        assert!(self.entries.len() < u16::MAX as usize, "constant pool full");
        self.entries.push(constant);
        (self.entries.len() - 1) as u16
    }

    pub fn push_utf8(&mut self, s: &str) -> u16 {
        let bytes = crate::mutf8::encode(s);
        self.push(Constant::Utf8(Utf8 { bytes: bytes.into_boxed_slice(), string: s.into() }))
    }

    pub fn push_class(&mut self, name: &str) -> u16 {
        let name = self.push_utf8(name);
        self.push(Constant::Class(name))
    }

    fn push_member(&mut self, class: &str, name: &str, descriptor: &str) -> MemberRef {
        let class = self.push_class(class);
        let (name, descriptor) = (self.push_utf8(name), self.push_utf8(descriptor));
        let name_and_type = self.push(Constant::NameAndType { name, descriptor });
        MemberRef { class, name_and_type }
    }

    pub fn push_field_ref(&mut self, class: &str, name: &str, descriptor: &str) -> u16 {
        let member = self.push_member(class, name, descriptor);
        self.push(Constant::FieldRef(member))
    }

    pub fn push_method_ref(&mut self, class: &str, name: &str, descriptor: &str) -> u16 {
        let member = self.push_member(class, name, descriptor);
        self.push(Constant::MethodRef(member))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.len() <= 1
    }

    pub fn entries(&self) -> &[Constant] {
        &self.entries
    }

    pub fn get(&self, index: u16) -> Result<&Constant> {
        match self.entries.get(index as usize) {
            Some(Constant::Unusable) | None => Err(ClassFormatError::new(format!("bad constant pool index {index}"))),
            Some(c) => Ok(c),
        }
    }

    pub fn utf8(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            Constant::Utf8(u) => Ok(&u.string),
            other => Err(wrong_kind(index, "Utf8", other)),
        }
    }

    pub fn utf8_bytes(&self, index: u16) -> Result<&[u8]> {
        match self.get(index)? {
            Constant::Utf8(u) => Ok(&u.bytes),
            other => Err(wrong_kind(index, "Utf8", other)),
        }
    }

    pub fn class_name(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            Constant::Class(name) => self.utf8(*name),
            other => Err(wrong_kind(index, "Class", other)),
        }
    }

    pub fn name_and_type(&self, index: u16) -> Result<(&str, &str)> {
        match self.get(index)? {
            Constant::NameAndType { name, descriptor } => Ok((self.utf8(*name)?, self.utf8(*descriptor)?)),
            other => Err(wrong_kind(index, "NameAndType", other)),
        }
    }

    /// Field, method or interface method reference.
    pub fn member_ref(&self, index: u16) -> Result<MemberRefInfo<'_>> {
        match self.get(index)? {
            Constant::FieldRef(m) | Constant::MethodRef(m) | Constant::InterfaceMethodRef(m) => {
                let (name, descriptor) = self.name_and_type(m.name_and_type)?;
                Ok(MemberRefInfo { class: self.class_name(m.class)?, name, descriptor })
            }
            other => Err(wrong_kind(index, "member reference", other)),
        }
    }
}

fn wrong_kind(index: u16, expected: &str, found: &Constant) -> ClassFormatError {
    ClassFormatError::new(format!("constant #{index}: expected {expected}, found {found:?}"))
}
