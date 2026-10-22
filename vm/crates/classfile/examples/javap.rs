//! Minimal javap: `cargo run -p classfile --example javap -- <lib/modules> <internal/Name> [-c]`
//! or `... -- <file.class> [-c]`.

use classfile::{ClassFile, access};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show_code = args.iter().any(|a| a == "-c");
    let bytes = if args[0].ends_with(".class") {
        std::fs::read(&args[0]).unwrap()
    } else {
        let image = jimage::Image::new(std::fs::read(&args[0]).unwrap()).unwrap();
        image.find_class(&args[1]).unwrap().expect("class not found").1
    };
    let class = ClassFile::parse(&bytes).unwrap();
    println!("class {} extends {:?} implements {:?} (flags {:#06x}, v{})", class.this_class, class.super_class, class.interfaces, class.access_flags, class.major_version);
    for f in &class.fields {
        let kind = if f.access_flags & access::STATIC != 0 { "static " } else { "" };
        println!("  field {kind}{} {} ({:#06x})", f.name, f.descriptor, f.access_flags);
    }
    for m in &class.methods {
        let native = if m.is_native() { "native " } else { "" };
        let stat = if m.is_static() { "static " } else { "" };
        println!("  method {stat}{native}{}{} ({:#06x})", m.name, m.descriptor, m.access_flags);
        if show_code {
            if let Some(code) = &m.code {
                println!("    stack={} locals={} len={}", code.max_stack, code.max_locals, code.code.len());
            }
        }
    }
    for a in &class.attributes {
        println!("  attribute {} ({} bytes)", a.name, a.data.len());
    }
}
