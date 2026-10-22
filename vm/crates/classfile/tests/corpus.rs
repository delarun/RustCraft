//! Parses every class of the JDK 25 image and the Minecraft client jar found in
//! .scratch (skipped if absent), and walks all bytecode instruction boundaries.

use classfile::{ClassFile, FieldType, MethodDescriptor, opcodes};
use std::{io::Read, path::PathBuf};

fn scratch(file: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.scratch").join(file);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} not found", path.display());
        None
    }
}

fn check(name: &str, bytes: &[u8]) {
    let class = ClassFile::parse(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    for method in &class.methods {
        MethodDescriptor::parse(&method.descriptor).unwrap();
        let Some(code) = &method.code else { continue };
        let mut pc = 0;
        while pc < code.code.len() {
            pc += opcodes::instruction_length(&code.code, pc)
                .unwrap_or_else(|| panic!("{name}.{}: bad instruction at {pc}", method.name));
        }
        assert_eq!(pc, code.code.len(), "{name}.{}", method.name);
    }
    for field in &class.fields {
        FieldType::parse(&field.descriptor).unwrap();
    }
}

#[test]
fn parses_jdk_image() {
    let Some(path) = scratch("jre25/lib/modules") else { return };
    let image = jimage::Image::new(std::fs::read(path).unwrap()).unwrap();
    let mut count = 0;
    for name in image.names() {
        let name = name.unwrap();
        if !name.ends_with(".class") || name.ends_with("module-info.class") {
            continue;
        }
        check(&name, &image.read(&name).unwrap().unwrap());
        count += 1;
    }
    eprintln!("parsed {count} JDK classes");
    assert!(count > 20_000);
}

#[test]
fn parses_minecraft_client() {
    let Some(path) = scratch("client-26.3.jar") else { return };
    let mut jar = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut count = 0;
    for i in 0..jar.len() {
        let mut entry = jar.by_index(i).unwrap();
        let name = entry.name().unwrap().into_owned();
        if !name.ends_with(".class") {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        check(&name, &bytes);
        count += 1;
    }
    eprintln!("parsed {count} Minecraft classes");
    assert!(count > 5_000);
}
