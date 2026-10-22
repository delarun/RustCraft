//! Field and method descriptors (JVMS §4.3).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldType {
    Byte,
    Char,
    Double,
    Float,
    Int,
    Long,
    Short,
    Boolean,
    /// Internal binary name, e.g. `java/lang/String`.
    Object(String),
    Array(Box<FieldType>),
}

impl FieldType {
    pub fn parse(descriptor: &str) -> Result<FieldType, DescriptorError> {
        let (ty, rest) = parse_field(descriptor)?;
        if rest.is_empty() { Ok(ty) } else { Err(DescriptorError(descriptor.to_owned())) }
    }

    /// Number of local-variable / operand-stack slots (JVMS §2.6.1).
    pub fn slots(&self) -> usize {
        match self {
            FieldType::Long | FieldType::Double => 2,
            _ => 1,
        }
    }

    pub fn is_reference(&self) -> bool {
        matches!(self, FieldType::Object(_) | FieldType::Array(_))
    }

    pub fn descriptor(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldType::Byte => f.write_str("B"),
            FieldType::Char => f.write_str("C"),
            FieldType::Double => f.write_str("D"),
            FieldType::Float => f.write_str("F"),
            FieldType::Int => f.write_str("I"),
            FieldType::Long => f.write_str("J"),
            FieldType::Short => f.write_str("S"),
            FieldType::Boolean => f.write_str("Z"),
            FieldType::Object(name) => write!(f, "L{name};"),
            FieldType::Array(component) => write!(f, "[{component}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodDescriptor {
    pub params: Vec<FieldType>,
    /// `None` for `void`.
    pub ret: Option<FieldType>,
}

impl MethodDescriptor {
    pub fn parse(descriptor: &str) -> Result<MethodDescriptor, DescriptorError> {
        let err = || DescriptorError(descriptor.to_owned());
        let mut rest = descriptor.strip_prefix('(').ok_or_else(err)?;
        let mut params = Vec::new();
        while !rest.starts_with(')') {
            let (ty, next) = parse_field(rest).map_err(|_| err())?;
            params.push(ty);
            rest = next;
        }
        let rest = &rest[1..];
        let ret = if rest == "V" { None } else { Some(FieldType::parse(rest).map_err(|_| err())?) };
        Ok(MethodDescriptor { params, ret })
    }

    /// Slots taken by the parameters, excluding `this`.
    pub fn param_slots(&self) -> usize {
        self.params.iter().map(FieldType::slots).sum()
    }
}

fn parse_field(s: &str) -> Result<(FieldType, &str), DescriptorError> {
    let err = || DescriptorError(s.to_owned());
    let mut chars = s.chars();
    let ty = match chars.next().ok_or_else(err)? {
        'B' => FieldType::Byte,
        'C' => FieldType::Char,
        'D' => FieldType::Double,
        'F' => FieldType::Float,
        'I' => FieldType::Int,
        'J' => FieldType::Long,
        'S' => FieldType::Short,
        'Z' => FieldType::Boolean,
        'L' => {
            let end = s.find(';').ok_or_else(err)?;
            if end == 1 {
                return Err(err());
            }
            return Ok((FieldType::Object(s[1..end].to_owned()), &s[end + 1..]));
        }
        '[' => {
            let (component, rest) = parse_field(&s[1..])?;
            return Ok((FieldType::Array(Box::new(component)), rest));
        }
        _ => return Err(err()),
    };
    Ok((ty, chars.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorError(pub String);

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid descriptor: {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_method_descriptors() {
        let d = MethodDescriptor::parse("(IJ[Ljava/lang/String;[[D)Ljava/lang/Object;").unwrap();
        assert_eq!(
            d.params,
            [
                FieldType::Int,
                FieldType::Long,
                FieldType::Array(Box::new(FieldType::Object("java/lang/String".into()))),
                FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Double)))),
            ]
        );
        assert_eq!(d.param_slots(), 5);
        assert_eq!(d.ret, Some(FieldType::Object("java/lang/Object".into())));
        assert_eq!(MethodDescriptor::parse("()V").unwrap().ret, None);
    }

    #[test]
    fn round_trips_and_rejects() {
        for s in ["I", "[J", "Ljava/util/Map$Entry;", "[[Ljava/lang/Object;"] {
            assert_eq!(FieldType::parse(s).unwrap().to_string(), s);
        }
        for bad in ["", "X", "L;", "Ljava/lang/String", "II", "[", "(I", "(V)V", "()VV"] {
            assert!(FieldType::parse(bad).is_err() || MethodDescriptor::parse(bad).is_err(), "{bad}");
        }
        assert!(MethodDescriptor::parse("(I").is_err());
        assert!(MethodDescriptor::parse("()VV").is_err());
    }
}
