//! The emitted form of a class's own statics.
//!
//! Which statics wait for their class, and what each activation point initializes, is
//! [`krusty::backend::class_statics::ClassStaticPlan`]'s. Here each class of the plan gets
//! `kt_classinit_<class>`, which runs its statics in declaration order behind a once-only flag,
//! and each activation point the plan names calls it.

use super::super::super::symbols::{module_class_activation_symbol, ClassActivationEntry};
use super::*;
use krusty::backend::class_statics::ClassStaticPlan;
use krusty::ir::IrClassActivationStep;

/// A file's class static plan, and the emitted initializer of each class it gives statics.
pub(super) struct FileClassStatics {
    pub(super) plan: ClassStaticPlan,
    initializers: HashMap<ClassId, ClassInitializer>,
}

impl FileClassStatics {
    /// Plan the file's class statics; their initializers are declared once its statics are.
    pub(super) fn plan(ir: &IrFile) -> Result<Self, Unsupported> {
        let plan = ClassStaticPlan::new(ir).map_err(|unordered| {
            declined!(
                "a non-`const` property stored on `{}`",
                unordered.owner.render()
            )
        })?;
        Ok(Self {
            plan,
            initializers: HashMap::default(),
        })
    }
}

/// The emitted pieces of one class's static initialization.
struct ClassInitializer {
    /// `kt_classinit_<class>`: runs the class's statics at most once.
    function: FuncId,
    /// Set once the initializers have started.
    flag: DataId,
}

impl<'a> FileLowering<'a> {
    /// Declare the initializer function and flag of every class the plan gives statics.
    pub(super) fn declare_class_initializers(&mut self) -> Result<(), Unsupported> {
        let ir = self.ir;
        let classes: Vec<ClassId> = self
            .class_statics
            .plan
            .classes()
            .iter()
            .map(|group| group.class)
            .collect();
        for class in classes {
            let declaration = &ir.classes[class as usize];
            let base = super::super::super::symbols::c_identifier(&declaration.fq_name());
            let function =
                self.declare_local_function(&format!("kt_classinit_{base}"), &[], Ty::Unit)?;
            let flag = self.declare_local_data(&format!("kt_classinit_done_{base}"), true)?;
            let mut description = DataDescription::new();
            description.define(Box::new([0u8]));
            description.set_align(1);
            self.module
                .define_data(flag, &description)
                .map_err(|error| {
                    format!("defining the initialization flag of `{base}` ({error})")
                })?;
            self.class_statics
                .initializers
                .insert(class, ClassInitializer { function, flag });
        }
        Ok(())
    }

    /// Define every class initializer [`Self::declare_class_initializers`] declared.
    pub(super) fn define_class_initializers(&mut self) -> Result<(), Unsupported> {
        let ir = self.ir;
        for position in 0..self.class_statics.plan.classes().len() {
            let group = &self.class_statics.plan.classes()[position];
            let class = group.class;
            let declarations: Vec<_> = group
                .statics
                .iter()
                .map(|&index| {
                    let declaration = &ir.statics[index as usize];
                    (
                        self.statics[index as usize],
                        declaration.ty,
                        declaration.init,
                    )
                })
                .collect();
            let initializer = &self.class_statics.initializers[&class];
            let (function, flag) = (initializer.function, initializer.flag);
            let name = format!("{}.<clinit>", ir.classes[class as usize].fq_name());
            let signature = self.signature_of(&[], Ty::Unit)?;
            self.emit_function(function, signature, Ty::Unit, &name, &mut |body, _| {
                let flag_address = body.data_address(flag);
                let state = body
                    .builder
                    .ins()
                    .load(types::I8, trusted(), flag_address, 0);
                let run = body.builder.create_block();
                let done = body.builder.create_block();
                body.builder.ins().brif(state, done, &[], run, &[]);

                body.continue_in(run);
                let flag_address = body.data_address(flag);
                let one = body.builder.ins().iconst(types::I8, 1);
                body.builder.ins().store(trusted(), one, flag_address, 0);
                // Every reference slot is rooted before the first initializer runs, which may
                // allocate and collect. The declaring file's initializer leaves these slots to
                // this function, because another file can reach it without running that one.
                for &(slot, ty, _) in &declarations {
                    if body.carrier(ty) != Carrier::Ref {
                        continue;
                    }
                    let address = body.data_address(slot);
                    body.runtime_call("kt_gc_add_global_root", &[any()], Ty::Unit, &[address])?;
                }
                for &(slot, ty, init) in &declarations {
                    let Some(init) = init else {
                        continue;
                    };
                    if body.terminated {
                        break;
                    }
                    let value = body.coerce(init, ty)?;
                    if body.terminated {
                        break;
                    }
                    let Some(value) = value else {
                        return Err("a class property initialized from a `Unit` value".into());
                    };
                    let address = body.data_address(slot);
                    body.builder.ins().store(trusted(), value, address, 0);
                }
                if !body.terminated {
                    body.builder.ins().jump(done, &[]);
                }
                body.continue_in(done);
                Ok(())
            })?;
        }
        Ok(())
    }
}

impl FileLowering<'_> {
    /// The singleton getter of `class`'s companion object, which this file declares with it.
    fn companion_getter(&self, class: ClassId) -> Result<FuncId, Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        declaration
            .companion_class
            .and_then(|companion| self.ir.class_id_by_name(companion))
            .and_then(|companion| self.classes[companion as usize].singleton)
            .map(|(_, getter)| getter)
            .ok_or_else(|| {
                declined!(
                    "the companion object of `{}` outside its file",
                    declaration.fq_name()
                )
            })
    }

    /// Define the entries another file of the module calls for a step of initializing a class of
    /// this one: its own statics, and its companion. Each runs this file's initializer first, as
    /// every entry into the file does.
    pub(super) fn define_class_activation_entries(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        if self.unit_kind != krusty::backend::CheckedIrUnitKind::Source {
            return Ok(());
        }
        let ir = self.ir;
        for class in 0..ir.classes.len() {
            let class = class as ClassId;
            if krusty::backend::is_declared_in_executable_code(ir, class) {
                continue;
            }
            let declaration = &ir.classes[class as usize];
            for &step in declaration.activation.iter() {
                let (entry, target) = match step {
                    IrClassActivationStep::OwnStatics(owner) if owner == declaration.fq_name => {
                        let initializer =
                            self.class_statics.initializers.get(&class).ok_or_else(|| {
                                declined!(
                                    "the statics of `{}` without an initializer",
                                    declaration.fq_name()
                                )
                            })?;
                        (ClassActivationEntry::Statics, initializer.function)
                    }
                    IrClassActivationStep::Companion(owner) if owner == declaration.fq_name => (
                        ClassActivationEntry::Companion,
                        self.companion_getter(class)?,
                    ),
                    _ => continue,
                };
                let symbol =
                    module_class_activation_symbol(self.module_name, declaration.fq_name, entry);
                let signature = self.signature_of(&[], Ty::Unit)?;
                let function = self
                    .module
                    .declare_function(&symbol, Linkage::Hidden, &signature)
                    .map_err(|error| declined!("declaring `{symbol}` ({error})"))?;
                self.emit_function(function, signature, Ty::Unit, &symbol, &mut |body, _| {
                    let init = body.func_ref(file_init);
                    body.emit_call(init, &[])?;
                    if body.terminated {
                        return Ok(());
                    }
                    let target = body.func_ref(target);
                    body.emit_call(target, &[])?;
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Initialize `class` the way `new` does, before any constructor argument runs: each step of
    /// its recorded activation.
    ///
    /// A companion is initialized through its singleton getter, which publishes the instance
    /// before the companion constructor, so a companion that constructs its holder does not
    /// re-enter.
    pub(super) fn activate_class(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let steps = self.file.ir.classes[class as usize].activation.clone();
        for &step in steps.iter() {
            match step {
                IrClassActivationStep::OwnStatics(owner) => self.run_class_statics(owner)?,
                IrClassActivationStep::Companion(owner) => self.initialize_companion(owner)?,
                IrClassActivationStep::Dependency(superclass) => {
                    self.activate_dependency_superclass(superclass)?;
                }
            }
            if self.terminated {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Run `owner`'s own statics: this file's initializer of it, or the entry the file that
    /// declares it defines.
    fn run_class_statics(&mut self, owner: TypeName) -> Result<(), Unsupported> {
        match self.file.ir.class_id_by_name(owner) {
            Some(class) => self.initialize_own_statics(class),
            None => self.call_activation_entry(owner, ClassActivationEntry::Statics),
        }
    }

    /// Initialize `owner`'s companion object: through its singleton getter when this file
    /// declares it, else through the entry the declaring file defines.
    fn initialize_companion(&mut self, owner: TypeName) -> Result<(), Unsupported> {
        let Some(class) = self.file.ir.class_id_by_name(owner) else {
            return self.call_activation_entry(owner, ClassActivationEntry::Companion);
        };
        let getter = self.file.companion_getter(class)?;
        let func_ref = self.func_ref(getter);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    fn call_activation_entry(
        &mut self,
        owner: TypeName,
        entry: ClassActivationEntry,
    ) -> Result<(), Unsupported> {
        let symbol = module_class_activation_symbol(self.file.module_name, owner, entry);
        let function = self.file.import(&symbol, &[], Ty::Unit)?;
        let func_ref = self.func_ref(function);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    /// Run `class`'s own statics here unless they already ran.
    pub(super) fn initialize_own_statics(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let function = self
            .file
            .class_statics
            .initializers
            .get(&class)
            .ok_or_else(|| declined!("class statics without an initializer"))?
            .function;
        let func_ref = self.func_ref(function);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    /// The constructor entry of `class`: its own statics, when the plan gives it any.
    pub(super) fn enter_constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        match self.file.class_statics.plan.constructor_entry(class) {
            Some(class) => self.initialize_own_statics(class),
            None => Ok(()),
        }
    }

    /// Initialize the class that owns static `index`, before that static is read or written.
    pub(super) fn initialize_static_owner(&mut self, index: u32) -> Result<(), Unsupported> {
        match self.file.class_statics.plan.access_activation(index) {
            Some(class) => self.activate_class(class),
            None => Ok(()),
        }
    }

    /// A block function or accessor is a static member of its class: entering one initializes
    /// the class.
    pub(super) fn enter_block_function(&mut self, function: FunId) -> Result<(), Unsupported> {
        match self
            .file
            .class_statics
            .plan
            .function_activation(self.file.ir, function)
        {
            Some(class) => self.activate_class(class),
            None => Ok(()),
        }
    }
}
