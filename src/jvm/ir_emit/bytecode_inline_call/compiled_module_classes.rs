//! The classes a same-module inline callee declares, compiled with the callee so a call site reads
//! them as it reads a dependency's: kotlinc's `IrSourceCompilerForInline` generates the callee
//! through its file's class codegen, and the classes of the lambdas its body declares are in the
//! class factory by the time a call site's `AnonymousObjectTransformer` asks for them.
//!
//! A call site regenerates such a class when its body needs the call's reified type arguments, and
//! otherwise reuses it; either way the inliner reads the class's declaration. A class this file
//! writes is compiled once per pass, here, and the class loop writes those same bytes. A class
//! another file declares is compiled only to be read: that file writes it, and nothing it produced
//! here is kept.

use std::rc::Rc;

use super::*;
use crate::jvm::inline::{PropertyAccess, RegeneratedClass};

/// One class this file writes, already compiled for its callee's call sites.
struct WrittenClass {
    bytes: Vec<u8>,
    extra: Vec<(String, Vec<u8>)>,
}

/// The classes of one emission pass's compiled inline callees.
#[derive(Default)]
pub(in crate::jvm::ir_emit) struct CompiledModuleClasses {
    /// Each compiled class's file, by internal name, as the inliner reads it.
    files: std::cell::RefCell<HashMap<String, Rc<Vec<u8>>>>,
    /// The compiled classes this file writes, until the class loop writes them.
    written: std::cell::RefCell<HashMap<ClassId, WrittenClass>>,
}

impl CompiledModuleClasses {
    pub(super) fn clear(&self) {
        self.files.borrow_mut().clear();
        self.written.borrow_mut().clear();
    }

    /// Classes a compiled callee's own inlining regenerated: read here, written by its file.
    pub(super) fn read_only(&self, classes: Vec<(String, Vec<u8>)>) {
        let mut files = self.files.borrow_mut();
        for (name, bytes) in classes {
            files.insert(name, Rc::new(bytes));
        }
    }

    fn file(&self, internal: &str) -> Option<Rc<Vec<u8>>> {
        self.files.borrow().get(internal).cloned()
    }
}

/// Compile every class declared in `function`'s body, for the call sites that inline it.
pub(super) fn compile_declared_classes(
    ir: &IrFile,
    function: u32,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
) {
    let classes = &env.run.compiled_inline_callees.classes;
    for (class_id, class) in ir.classes.iter().enumerate() {
        let class_id = class_id as ClassId;
        // A continuation class is code of the suspend function whose machine it carries.
        let scope = class.enclosure.or_else(|| {
            ir.continuation_owners
                .get(&class_id)
                .map(|&owner| crate::ir::IrEnclosure::Function(owner))
        });
        let declared = scope.is_some_and(|scope| {
            crate::jvm::inner_classes::scope_within(ir, scope, &|owner| owner == function)
        });
        if !declared || classes.written.borrow().contains_key(&class_id) {
            continue;
        }
        let fq_name = class.fq_name();
        let foreign = crate::jvm::inner_classes::declared_by_another_file(ir, class);
        let machines = env.run.machine_classes.borrow().len();
        let plans = env.run.lambda_classes.borrow().len();
        let mut extra = Vec::new();
        let meta = class_meta(&fq_name);
        let bytes = emit_class(
            ir,
            class_id,
            class,
            facade,
            env,
            opts,
            meta.as_ref(),
            &mut extra,
        );
        // The objects the class's methods regenerated for their own inline calls are read by a
        // call site that regenerates the class in turn, as the callee's own are.
        if foreign {
            // The declaring file writes the class and everything its emission produced.
            let regenerated = env.run.machine_classes.borrow_mut().split_off(machines);
            classes.read_only(regenerated);
            env.run.lambda_classes.borrow_mut().truncate(plans);
            classes.files.borrow_mut().insert(fq_name, Rc::new(bytes));
        } else {
            let regenerated = env.run.machine_classes.borrow()[machines..].to_vec();
            classes.read_only(regenerated);
            classes
                .files
                .borrow_mut()
                .insert(fq_name, Rc::new(bytes.clone()));
            classes
                .written
                .borrow_mut()
                .insert(class_id, WrittenClass { bytes, extra });
        }
    }
}

/// Write `class_id`'s class file and the classes its emission adds beside it: the bytes compiled
/// with an inline callee, or a fresh emission. A class another file declares is not written.
pub(in crate::jvm::ir_emit) fn write_class(
    ir: &IrFile,
    class_id: ClassId,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    let class = &ir.classes[class_id as usize];
    let written = env
        .run
        .compiled_inline_callees
        .classes
        .written
        .borrow_mut()
        .remove(&class_id);
    let (bytes, extra) = match written {
        Some(WrittenClass { bytes, extra }) => (bytes, extra),
        None if crate::jvm::inner_classes::declared_by_another_file(ir, class) => return,
        None => {
            let meta = class_meta(&class.fq_name());
            let mut extra = Vec::new();
            let bytes = emit_class(
                ir,
                class_id,
                class,
                facade,
                env,
                opts,
                meta.as_ref(),
                &mut extra,
            );
            (bytes, extra)
        }
    };
    out.push((class.fq_name(), bytes));
    out.extend(extra);
}

/// The bodies a call site reads: the dependencies', and the classes this pass compiled for its
/// same-module inline callees.
pub(in crate::jvm::ir_emit) struct ModuleClassBodies<'a> {
    dependencies: &'a dyn MethodBodies,
    run: &'a EmitRun,
}

impl<'a> ModuleClassBodies<'a> {
    pub(in crate::jvm::ir_emit) fn new(
        dependencies: &'a dyn MethodBodies,
        run: &'a EmitRun,
    ) -> Self {
        Self { dependencies, run }
    }
}

impl MethodBodies for ModuleClassBodies<'_> {
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        self.dependencies.body(owner, name, descriptor)
    }

    fn class_file(&self, internal: &str) -> Option<Vec<u8>> {
        match self.run.compiled_inline_callees.classes.file(internal) {
            Some(bytes) => Some(bytes.as_ref().clone()),
            None => self.dependencies.class_file(internal),
        }
    }

    fn regenerated_class(&self, internal: &str) -> Option<RegeneratedClass> {
        let Some(bytes) = self.run.compiled_inline_callees.classes.file(internal) else {
            return self.dependencies.regenerated_class(internal);
        };
        // A compiled class is a lambda's or an object's, never a SAM wrapper: this file's
        // wrappers are written from plans, not from declared classes.
        let class = crate::jvm::classreader::parse_class(&bytes).ok()?;
        class
            .inner_class_self()
            .is_some_and(|entry| entry.name.is_none())
            .then_some(RegeneratedClass::AnonymousObject)
    }

    fn owner_is_interface_name(&self, owner: TypeName) -> bool {
        self.dependencies.owner_is_interface_name(owner)
    }

    fn member_is_private(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.dependencies.member_is_private(owner, name, descriptor)
    }

    fn member_is_publicly_reachable(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.dependencies
            .member_is_publicly_reachable(owner, name, descriptor)
    }

    fn class_is_publicly_reachable(&self, class: &str) -> bool {
        self.dependencies.class_is_publicly_reachable(class)
    }

    fn method_is_static(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.dependencies.method_is_static(owner, name, descriptor)
    }

    fn property_read_access(&self, owner: TypeName, property: &str) -> Option<PropertyAccess> {
        self.dependencies.property_read_access(owner, property)
    }

    fn property_write_access(&self, owner: TypeName, property: &str) -> Option<PropertyAccess> {
        self.dependencies.property_write_access(owner, property)
    }

    fn external_property_access(
        &self,
        accessor: crate::fir::ExternalCallableId,
    ) -> Option<PropertyAccess> {
        self.dependencies.external_property_access(accessor)
    }

    fn singleton_storage(&self, classifier: TypeName) -> Option<(TypeName, String)> {
        self.dependencies.singleton_storage(classifier)
    }
}
