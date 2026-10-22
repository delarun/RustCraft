//! Runs against the JDK 25 image downloaded into .scratch (see README); skipped if absent.

use jimage::Image;
use std::path::PathBuf;

fn image() -> Option<Image> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.scratch/jre25/lib/modules");
    let Ok(data) = std::fs::read(&path) else {
        eprintln!("skipping: {} not found", path.display());
        return None;
    };
    Some(Image::new(data).expect("valid image"))
}

#[test]
fn reads_core_classes() {
    let Some(image) = image() else { return };
    let object = image.read("/java.base/java/lang/Object.class").unwrap().expect("Object.class");
    assert_eq!(&object[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
    assert_eq!(u16::from_be_bytes([object[6], object[7]]), 69, "Java 25 class file");
    assert!(image.find("/java.base/java/lang/NoSuchClass.class").is_none());

    let (module, _) = image.find_class("java/util/logging/Logger").unwrap().expect("Logger");
    assert_eq!(module, "java.logging");
    assert_eq!(image.package_modules("java/lang").unwrap(), ["java.base"]);
}

#[test]
fn every_resource_is_readable() {
    let Some(image) = image() else { return };
    let mut classes = 0;
    for name in image.names() {
        let name = name.unwrap();
        let location = image.find(&name).unwrap_or_else(|| panic!("lookup failed for {name}"));
        let bytes = image.read_location(&location).unwrap();
        if name.ends_with(".class") && name.starts_with("/java.base/") {
            assert_eq!(&bytes[..4], &[0xCA, 0xFE, 0xBA, 0xBE], "{name}");
            classes += 1;
        }
    }
    assert!(classes > 5000, "java.base has {classes} classes");
}
