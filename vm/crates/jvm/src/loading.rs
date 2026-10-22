//! Class loading through Java class loaders (JVMS §5.3.2): a class referenced
//! from a class defined by a user loader is loaded by calling that loader's
//! `loadClass`, and the result is recorded for it as initiating loader.

use crate::class::{ClassRef, FieldRef, MethodRef, Resolved};
use crate::heap::Ref;
use crate::interp::{JResult, Thread, Throw};
use classfile::Constant;

impl Thread<'_> {
    /// Loads `name` (internal form, arrays included) as `loader` would.
    pub fn load_class_in(&mut self, loader: Ref, name: &str) -> JResult<ClassRef> {
        if loader.is_null() {
            return self.load_class(name);
        }
        if let Some(class) = self.vm.find_loaded_by(loader, name) {
            return Ok(class);
        }
        if let Some(component) = name.strip_prefix('[') {
            let component = match component.strip_prefix('L').and_then(|c| c.strip_suffix(';')) {
                Some(element) => self.load_class_in(loader, element)?,
                None if component.starts_with('[') => self.load_class_in(loader, component)?,
                None => return self.load_class(name), // primitive arrays
            };
            return Ok(self.vm.array_class(component));
        }
        let java_name = self.new_string(&name.replace('/', "."))?;
        let result = self.invoke_virtual(loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[java_name.slot()]);
        let mirror = match result {
            Ok(mirror) => Ref::from_slot(mirror.unwrap()),
            Err(Throw(exception)) => {
                let not_found = self.load_class("java/lang/ClassNotFoundException")?;
                if self.vm.class_of(exception).is_subclass_of(not_found) {
                    return Err(self.throw_new("java/lang/NoClassDefFoundError", Some(&name.replace('/', "."))));
                }
                return Err(Throw(exception));
            }
        };
        if mirror.is_null() {
            return Err(self.throw_new("java/lang/NoClassDefFoundError", Some(&name.replace('/', "."))));
        }
        let class = self.vm.class_from_mirror(mirror);
        if class.name != name {
            let message = format!("{name} (wrong name: {})", class.name);
            return Err(self.throw_new("java/lang/NoClassDefFoundError", Some(&message)));
        }
        self.vm.record_initiating_loader(loader, name, class);
        Ok(class)
    }

    /// The nest host (JVMS §5.4.4): set dynamically for hidden nestmates, else
    /// from the NestHost attribute (loaded by the class's loader), else the
    /// class itself.
    pub fn nest_host(&mut self, class: ClassRef) -> JResult<ClassRef> {
        if let Some(&host) = class.dynamic_nest_host.get() {
            return Ok(host);
        }
        match class.file.as_ref().and_then(|f| f.nest_host.as_deref()) {
            Some(host) => self.load_class_in(class.loader, host),
            None => Ok(class),
        }
    }

    /// Resolves a Class constant relative to the referencing class's loader.
    pub fn resolve_class(&mut self, from: ClassRef, index: u16) -> JResult<ClassRef> {
        if let Some(Resolved::Class(class)) = from.cp_cache[index as usize].get() {
            return Ok(class);
        }
        let name = from.cp().class_name(index).map_err(|e| self.throw_new("java/lang/ClassFormatError", Some(&e.0)))?;
        let class = self.load_class_in(from.loader, name)?;
        let _ = from.cp_cache[index as usize].set(Resolved::Class(class));
        Ok(class)
    }

    /// Resolves the class part of a member reference, then the member.
    fn resolve_member_owner(&mut self, from: ClassRef, index: u16) -> JResult<()> {
        let class_index = match from.cp().get(index) {
            Ok(Constant::FieldRef(m) | Constant::MethodRef(m) | Constant::InterfaceMethodRef(m)) => m.class,
            _ => return Err(self.throw_new("java/lang/ClassFormatError", Some("bad member reference"))),
        };
        self.resolve_class(from, class_index).map(|_| ())
    }

    pub fn resolve_field(&mut self, from: ClassRef, index: u16) -> JResult<FieldRef> {
        if let Some(Resolved::Field(field)) = from.cp_cache[index as usize].get() {
            return Ok(*field);
        }
        self.resolve_member_owner(from, index)?;
        self.vm.resolve_field(from, index).map_err(|e| self.throw_vm_error(e))
    }

    pub fn resolve_method(&mut self, from: ClassRef, index: u16) -> JResult<MethodRef> {
        if let Some(Resolved::Method(method)) = from.cp_cache[index as usize].get() {
            return Ok(method);
        }
        self.resolve_member_owner(from, index)?;
        self.vm.resolve_method(from, index).map_err(|e| self.throw_vm_error(e))
    }
}
