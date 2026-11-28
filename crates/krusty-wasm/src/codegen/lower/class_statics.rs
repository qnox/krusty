//! The emitted form of a class's activation.
//!
//! Which statics wait for their class, and what initializing each class initializes, are common
//! lowering's facts, grouped by [`ClassStaticPlan`]. Here each class with statics of its own gets
//! one function that runs them in declaration order behind a once-only `i32` global, and each
//! activation point runs the class's recorded activation steps. A step for a class another file
//! declares calls the function that file defines for it, reserved by class in the module's
//! registry; another file reaching a class of this one does the same through the class's whole
//! activation function.

use super::*;
use crate::codegen::property_registry::ClassEntry;
use krusty::backend::class_statics::ClassStaticPlan;
use krusty::ir::IrClassActivationStep;

/// The emitted pieces of one class's static initialization.
struct ClassInitializer {
    /// Runs the class's statics at most once.
    function: u32,
    /// An `i32` global set once the initializers have started.
    flag: u32,
}

/// The plan of a file's class statics, the initializer of each class it gives statics, and the
/// entries this file defines for other files and calls in them.
pub(super) struct ClassInitializers {
    plan: ClassStaticPlan,
    classes: HashMap<ClassId, ClassInitializer>,
    /// The entries of this file's classes that this file defines: whole activations, companion
    /// initializations and the own-statics entries of other files.
    defined: Vec<(ClassId, ClassEntry, u32)>,
    /// The entry of another file's class each step of this file's activations calls.
    external: HashMap<(TypeName, ClassEntry), u32>,
}

impl ClassInitializers {
    /// Plan the file's class statics and reserve each class's initializer and entries.
    pub(super) fn declare(
        ir: &IrFile,
        module: &mut Module,
        properties: &mut PropertyRegistry,
    ) -> Result<Self, Unsupported> {
        let plan = ClassStaticPlan::new(ir)
            .map_err(|unordered| format!("a static property of `{}`", unordered.owner.render()))?;
        let mut classes = HashMap::default();
        for group in plan.classes() {
            let owner = ir.classes[group.class as usize].fq_name;
            // The initializer is the entry another file calls for this class's statics.
            let initializer = ClassInitializer {
                function: properties
                    .class_entry(owner, ClassEntry::OwnStatics, || module.declare()),
                flag: module.global(ValType::I32),
            };
            classes.insert(group.class, initializer);
        }
        let mut defined = Vec::new();
        let mut external = HashMap::default();
        for (position, declaration) in ir.classes.iter().enumerate() {
            let class = position as ClassId;
            if krusty::backend::is_declared_in_executable_code(ir, class) {
                continue;
            }
            for &step in declaration.activation.iter() {
                let (owner, entry) = match step {
                    IrClassActivationStep::OwnStatics(owner) => (owner, ClassEntry::OwnStatics),
                    IrClassActivationStep::Companion(owner) => (owner, ClassEntry::Companion),
                    IrClassActivationStep::Dependency(_) => continue,
                };
                if ir.class_id_by_name(owner).is_none() {
                    let function = properties.class_entry(owner, entry, || module.declare());
                    external.insert((owner, entry), function);
                }
            }
            if declaration.activation.is_empty() {
                continue;
            }
            let owner = declaration.fq_name;
            // Every class whose initialization does anything can be reached from another file:
            // through a block property or a subclass there.
            let activation =
                properties.class_entry(owner, ClassEntry::Activation, || module.declare());
            defined.push((class, ClassEntry::Activation, activation));
            if declaration
                .activation
                .contains(&IrClassActivationStep::Companion(owner))
            {
                let companion =
                    properties.class_entry(owner, ClassEntry::Companion, || module.declare());
                defined.push((class, ClassEntry::Companion, companion));
            }
        }
        // A file lowered before this one may have reserved an entry of a class of this one that
        // the loop above did not define.
        let reserved: Vec<(ClassId, ClassEntry, u32)> = properties
            .reserved_class_entries()
            .filter_map(|(owner, entry, function)| {
                Some((ir.class_id_by_name(owner)?, entry, function))
            })
            .filter(|&(class, entry, _)| {
                entry != ClassEntry::OwnStatics || !classes.contains_key(&class)
            })
            .filter(|reserved| !defined.contains(reserved))
            .collect();
        defined.extend(reserved);
        Ok(Self {
            plan,
            classes,
            defined,
            external,
        })
    }

    pub(super) fn plan(&self) -> &ClassStaticPlan {
        &self.plan
    }

    /// The function that initializes `class` the way constructing it does, when its
    /// initialization does anything.
    pub(super) fn activation_function(&self, class: ClassId) -> Option<u32> {
        self.defined
            .iter()
            .find(|&&(owner, entry, _)| owner == class && entry == ClassEntry::Activation)
            .map(|&(_, _, function)| function)
    }

    /// Define every class initializer and entry [`Self::declare`] reserved.
    pub(super) fn define(
        &self,
        context: &FileContext<'_>,
        module: &mut Module,
        runtime: &mut Runtime,
    ) -> Result<(), Unsupported> {
        let ir = context.ir;
        for group in self.plan.classes() {
            let initializer = &self.classes[&group.class];
            let declared = group
                .statics
                .iter()
                .filter_map(|&index| ir.statics[index as usize].init)
                .flat_map(|init| ir.declared_value_slot_types(init))
                .collect();
            let ty = module.func_type(Vec::new(), Vec::new());
            let mut body = Body::new(context, module, runtime, &[], declared, Ty::Unit);
            body.code.global_get(initializer.flag);
            body.code.if_(BlockType::Empty);
            body.code.return_();
            body.code.end();
            body.code.i32_const(1);
            body.code.global_set(initializer.flag);
            for &index in &group.statics {
                let property = &ir.statics[index as usize];
                if let Some(init) = property.init {
                    body.value(init, property.ty)?;
                    body.code.global_set(context.globals[index as usize]);
                }
            }
            let Body { code, locals, .. } = body;
            module.define(initializer.function, Function { ty, locals, code });
        }
        for &(class, entry, function) in &self.defined {
            let ty = module.func_type(Vec::new(), Vec::new());
            let mut body = Body::new(context, module, runtime, &[], Default::default(), Ty::Unit);
            match entry {
                ClassEntry::Activation => body.activate_class(class, None)?,
                ClassEntry::Companion => body.initialize_companion(class)?,
                // Another file expected statics this class does not run: nothing to do.
                ClassEntry::OwnStatics => {}
            }
            let Body { code, locals, .. } = body;
            module.define(function, Function { ty, locals, code });
        }
        Ok(())
    }
}

impl Body<'_> {
    /// Initialize `class` the way `new` does, before any constructor argument runs: each step of
    /// its recorded activation.
    ///
    /// Each class's companion goes through the singleton getter, which publishes the instance
    /// before the companion constructor, so a companion that constructs its holder does not
    /// re-enter. `initializing` is a class whose companion's initialization is already in
    /// progress, so activation does not start it again.
    pub(super) fn activate_class(
        &mut self,
        class: ClassId,
        initializing: Option<ClassId>,
    ) -> Result<(), Unsupported> {
        for &step in self.ir.classes[class as usize].activation.iter() {
            match step {
                IrClassActivationStep::OwnStatics(owner) => match self.ir.class_id_by_name(owner) {
                    Some(owner) => self.initialize_own_statics(owner)?,
                    None => self.call_external(owner, ClassEntry::OwnStatics)?,
                },
                IrClassActivationStep::Companion(owner) => match self.ir.class_id_by_name(owner) {
                    Some(owner) if Some(owner) == initializing => {}
                    Some(owner) => self.initialize_companion(owner)?,
                    None => self.call_external(owner, ClassEntry::Companion)?,
                },
                IrClassActivationStep::Dependency(superclass) => {
                    self.activate_dependency_superclass(superclass)?;
                }
            }
        }
        Ok(())
    }

    fn call_external(&mut self, owner: TypeName, entry: ClassEntry) -> Result<(), Unsupported> {
        let function = self
            .class_initializers
            .external
            .get(&(owner, entry))
            .copied()
            .ok_or_else(|| format!("an unreserved initialization entry of `{}`", owner.render()))?;
        self.code.call(function);
        Ok(())
    }

    /// Run `class`'s own statics here unless they already ran.
    pub(super) fn initialize_own_statics(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let initializer = self
            .class_initializers
            .classes
            .get(&class)
            .ok_or("class statics without an initializer")?;
        self.code.call(initializer.function);
        Ok(())
    }

    /// The constructor entry of `class`: its own statics, when the plan gives it any.
    pub(super) fn enter_constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        match self.class_initializers.plan.constructor_entry(class) {
            Some(class) => self.initialize_own_statics(class),
            None => Ok(()),
        }
    }

    /// Initialize the class that owns this file's storage of `target`, before it is read or
    /// written through the property rather than through `GetStatic`/`SetStatic`.
    pub(super) fn initialize_storage_owner(
        &mut self,
        target: krusty::fir::PropertyId,
    ) -> Result<(), Unsupported> {
        match self.ir.local_property_layouts.get(&target) {
            Some(krusty::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. }) => {
                self.initialize_static_owner(*storage)
            }
            // A `companion { }` block property of another file: its owner's activation function,
            // which that file defines, initializes the owner as an access there would.
            _ => {
                let owner =
                    self.ir
                        .referenced_module_properties
                        .get(&target)
                        .and_then(|reference| match reference.placement {
                            krusty::ir::IrStaticPlacement::CompanionBlock { declaring_class } => {
                                Some(declaring_class)
                            }
                            krusty::ir::IrStaticPlacement::Package => None,
                        });
                if let Some(function) = owner.and_then(|owner| {
                    self.properties
                        .reserved_class_entry(owner, ClassEntry::Activation)
                }) {
                    self.code.call(function);
                }
                Ok(())
            }
        }
    }

    /// Initialize the class that owns static `index`, before that static is read or written.
    pub(super) fn initialize_static_owner(&mut self, index: u32) -> Result<(), Unsupported> {
        match self.class_initializers.plan.access_activation(index) {
            Some(class) => self.activate_class(class, None),
            None => Ok(()),
        }
    }
}
