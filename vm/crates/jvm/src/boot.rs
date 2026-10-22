//! VM startup, following HotSpot's `Threads::initialize_java_lang_classes`:
//! core classes, the initial thread group and thread, then System.initPhase1.

use crate::heap::Ref;
use crate::interp::{JResult, Thread};
use crate::native::THREAD_STATUS_RUNNABLE;

/// Host environment reported through system properties.
#[derive(Clone, Debug)]
pub struct SystemConfig {
    pub java_home: String,
    pub class_path: String,
    /// Main class and arguments (`sun.java.command`).
    pub command: String,
    /// `-D` properties.
    pub properties: Vec<(String, String)>,
    pub os_name: String,
    pub os_arch: String,
    pub os_version: String,
    pub user_dir: String,
    pub user_home: String,
    pub user_name: String,
    pub tmp_dir: String,
}

impl SystemConfig {
    pub fn from_host() -> Self {
        let os_arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            arch => arch,
        };
        SystemConfig {
            java_home: "/jdk".into(),
            class_path: String::new(),
            command: String::new(),
            properties: Vec::new(),
            os_name: "Linux".into(),
            os_arch: os_arch.into(),
            os_version: "6.0".into(),
            user_dir: std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_else(|_| "/".into()),
            user_home: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
            user_name: std::env::var("USER").unwrap_or_else(|_| "player".into()),
            tmp_dir: "/tmp".into(),
        }
    }
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self::from_host()
    }
}

impl Thread<'_> {
    fn initialize_class(&mut self, name: &str) -> JResult<()> {
        let class = self.load_class(name)?;
        self.ensure_initialized(class)
    }

    fn construct(&mut self, class: &str, descriptor: &str, args: &[u64]) -> JResult<Ref> {
        let class = self.load_class(class)?;
        self.ensure_initialized(class)?;
        let obj = self.vm.new_object(class).map_err(|e| self.throw_vm_error(e))?;
        let init = class.find_method("<init>", descriptor).expect("constructor");
        let mut all = vec![obj.slot()];
        all.extend_from_slice(args);
        self.invoke(init, &all)?;
        Ok(obj)
    }

    /// Brings the class library up to the point where `main` can run.
    pub fn boot(&mut self) -> JResult<()> {
        for class in ["java/lang/String", "java/lang/System", "java/lang/Class", "java/lang/ThreadGroup"] {
            self.initialize_class(class)?;
        }
        let system_group = self.construct("java/lang/ThreadGroup", "()V", &[])?;
        let main_name = self.new_string("main")?;
        let main_group = self.construct(
            "java/lang/ThreadGroup",
            "(Ljava/lang/ThreadGroup;Ljava/lang/String;)V",
            &[system_group.slot(), main_name.slot()],
        )?;

        // The Thread constructor calls Thread.currentThread(), so the object
        // must be current before it is constructed.
        self.initialize_class("java/lang/Thread")?;
        let thread_class = self.load_class("java/lang/Thread")?;
        let thread = self.vm.new_object(thread_class).map_err(|e| self.throw_vm_error(e))?;
        self.java_thread = thread;
        self.shared.java_thread.set(thread);
        self.set_field(thread, "java/lang/Thread", "eetop", self.shared.id);
        let init = thread_class.find_method("<init>", "(Ljava/lang/ThreadGroup;Ljava/lang/String;)V").unwrap();
        self.invoke(init, &[thread.slot(), main_group.slot(), main_name.slot()])?;
        let holder = self.field_ref(thread, "java/lang/Thread", "holder");
        self.set_field(holder, "java/lang/Thread$FieldHolder", "threadStatus", THREAD_STATUS_RUNNABLE);

        for class in ["java/lang/Module", "java/lang/reflect/Method", "java/lang/ref/Finalizer"] {
            self.initialize_class(class)?;
        }
        self.invoke_static("java/lang/System", "initPhase1", "()V", &[])?;
        for class in [
            "java/lang/OutOfMemoryError",
            "java/lang/NullPointerException",
            "java/lang/ClassCastException",
            "java/lang/ArrayStoreException",
            "java/lang/ArithmeticException",
            "java/lang/StackOverflowError",
            "java/lang/IllegalMonitorStateException",
            "java/lang/IllegalArgumentException",
        ] {
            self.initialize_class(class)?;
        }
        // Module system boot (ModuleBootstrap.boot); returns a JNI status code.
        let status = self.invoke_static("java/lang/System", "initPhase2", "(ZZ)I", &[1, 1])?;
        if status != Some(0) {
            let message = format!("System.initPhase2 failed with status {status:?}");
            return Err(self.throw_new("java/lang/InternalError", Some(&message)));
        }
        // System class loader, security setup, context class loader.
        self.invoke_static("java/lang/System", "initPhase3", "()V", &[])?;
        Ok(())
    }

    /// The application class loader (ClassLoader.getSystemClassLoader()).
    pub fn system_class_loader(&mut self) -> JResult<Ref> {
        let loader = self.invoke_static("java/lang/ClassLoader", "getSystemClassLoader", "()Ljava/lang/ClassLoader;", &[])?;
        Ok(Ref::from_slot(loader.unwrap()))
    }
}
