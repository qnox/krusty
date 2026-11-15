//! Statics a class owns: initialized when the class is first touched, not at program start.
//!
//! A `companion { … }` block property is stored with its class, and Kotlin runs its initializer
//! as part of initializing that class: the first construction of the class, the first call of one
//! of its block functions or accessors, or the first read or write of the storage itself, whichever
//! comes first. A superclass is initialized before its subclass. The top-level slots of
//! [`super::statics`] all run at program start because the file's entry touches the file first;
//! a class owes no such promise, so an initializer that could observe the difference runs here,
//! behind a once-only flag, at each of those entry points.
//!
//! The flag is set before the initializers run, as the file initializer's is: an initializer that
//! reaches back into its own class sees the slot's default, which is what Kotlin does.

use super::*;

/// The emitted pieces of one class's static initialization.
pub(super) struct ClassInitializer {
    /// `kt_classinit_<class>`: runs the initializers below at most once.
    function: FuncId,
    /// Set once the initializers have started.
    flag: DataId,
    /// The class's deferred statics, by index into `IrFile::statics`, in declaration order.
    statics: Vec<u32>,
}

impl<'a> FileLowering<'a> {
    /// Defer an owned static to its class's initializer, declaring that initializer the first
    /// time the class is named. A class whose initialization has other parts this generator does
    /// not order — an interface, an enum, an object, a value class, or a class with a companion
    /// object — still declines.
    pub(super) fn defer_to_class(
        &mut self,
        index: u32,
        owner: krusty::types::TypeName,
    ) -> Result<(), Unsupported> {
        let declined = || declined!("a non-`const` property stored on `{}`", owner.render());
        let Some(class) = self.ir.class_id_by_name(owner) else {
            return Err(declined());
        };
        let declaration = &self.ir.classes[class as usize];
        if declaration.is_interface
            || declaration.is_object
            || declaration.is_value
            || declaration.companion_class.is_some()
            || super::super::super::classes::is_enum(declaration)
        {
            return Err(declined());
        }
        if let Some(initializer) = self.class_initializers.get_mut(&class) {
            initializer.statics.push(index);
            return Ok(());
        }
        let base = super::super::super::symbols::c_identifier(&declaration.fq_name());
        let function =
            self.declare_local_function(&format!("kt_classinit_{base}"), &[], Ty::Unit)?;
        let flag = self.declare_local_data(&format!("kt_classinit_done_{base}"), true)?;
        let mut description = DataDescription::new();
        description.define(Box::new([0u8]));
        description.set_align(1);
        self.module
            .define_data(flag, &description)
            .map_err(|error| format!("defining the initialization flag of `{base}` ({error})"))?;
        self.class_initializers.insert(
            class,
            ClassInitializer {
                function,
                flag,
                statics: vec![index],
            },
        );
        Ok(())
    }

    /// Whether this static waits for its class rather than running at program start.
    pub(super) fn is_deferred_static(&self, index: u32) -> bool {
        self.class_initializers
            .values()
            .any(|initializer| initializer.statics.contains(&index))
    }

    /// The initializer to run before touching `class`: its own, or its nearest initialized
    /// superclass's, since constructing it runs every superclass constructor.
    fn initializer_for(&self, class: ClassId) -> Option<FuncId> {
        let mut current = Some(class);
        while let Some(class) = current {
            if let Some(initializer) = self.class_initializers.get(&class) {
                return Some(initializer.function);
            }
            current = self.model.layout(class).superclass;
        }
        None
    }

    /// Define every class initializer declared by [`Self::defer_to_class`].
    pub(super) fn define_class_initializers(&mut self) -> Result<(), Unsupported> {
        let mut classes: Vec<ClassId> = self.class_initializers.keys().copied().collect();
        classes.sort_unstable();
        for class in classes {
            let initializer = &self.class_initializers[&class];
            let (function, flag) = (initializer.function, initializer.flag);
            let declarations: Vec<(DataId, Ty, Option<u32>)> = initializer
                .statics
                .iter()
                .map(|&index| {
                    let declaration = &self.ir.statics[index as usize];
                    (
                        self.statics[index as usize],
                        declaration.ty,
                        declaration.init,
                    )
                })
                .collect();
            let superclass = self
                .model
                .layout(class)
                .superclass
                .and_then(|parent| self.initializer_for(parent));
            let name = format!("{}.<clinit>", self.ir.classes[class as usize].fq_name());
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
                if let Some(superclass) = superclass {
                    let func_ref = body.func_ref(superclass);
                    body.emit_call(func_ref, &[])?;
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

impl BodyLowering<'_, '_, '_> {
    /// Initialize `class` here unless it already is: the entry of its constructors and of its
    /// block functions and accessors.
    pub(super) fn initialize_class(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let Some(initializer) = self.file.initializer_for(class) else {
            return Ok(());
        };
        let func_ref = self.func_ref(initializer);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    /// Initialize the class that owns static `index`, before that static is read or written.
    pub(super) fn initialize_static_owner(&mut self, index: u32) -> Result<(), Unsupported> {
        // Reading a `const` touches nothing: its value is a compile-time constant.
        let declaration = &self.file.ir.statics[index as usize];
        let Some(owner) = declaration.owner.filter(|_| !declaration.is_const) else {
            return Ok(());
        };
        match self.file.ir.class_id_by_name(owner) {
            Some(class) if self.file.class_initializers.contains_key(&class) => {
                self.initialize_class(class)
            }
            _ => Ok(()),
        }
    }
}
