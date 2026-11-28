//! The emitted form of a class's own statics.
//!
//! Which statics wait for their class, and what each activation point initializes, is
//! [`ClassStaticPlan`]'s. Here each class of the plan gets one function that runs its statics in
//! declaration order behind a once-only `i32` global, and each activation point the plan names
//! calls it. Another file reaches a class of this one through the class's activation function,
//! which initializes it the way an access in this file does: superclasses, superinterfaces and
//! companions included.

use super::*;
use krusty::backend::class_statics::ClassStaticPlan;

/// The emitted pieces of one class's static initialization.
struct ClassInitializer {
    /// Runs the class's statics at most once.
    function: u32,
    /// An `i32` global set once the initializers have started.
    flag: u32,
}

/// The plan of a file's class statics, and the initializer of each class it gives statics.
pub(super) struct ClassInitializers {
    plan: ClassStaticPlan,
    classes: HashMap<ClassId, ClassInitializer>,
    /// The activation function of each class of this file another file can reach: every class
    /// with statics, and every class another file reserved one for.
    activations: Vec<(ClassId, u32)>,
}

impl ClassInitializers {
    /// Plan the file's class statics and reserve each class's initializer.
    pub(super) fn declare(
        ir: &IrFile,
        module: &mut Module,
        properties: &mut PropertyRegistry,
    ) -> Result<Self, Unsupported> {
        let plan = ClassStaticPlan::new(ir)
            .map_err(|unordered| format!("a static property of `{}`", unordered.owner.render()))?;
        let classes = plan
            .classes()
            .iter()
            .map(|group| {
                let initializer = ClassInitializer {
                    function: module.declare(),
                    flag: module.global(ValType::I32),
                };
                (group.class, initializer)
            })
            .collect();
        // A file that reads a class's statics before this file is lowered reserved its
        // activation function, which this file defines.
        let mut activations: Vec<(ClassId, u32)> = plan
            .classes()
            .iter()
            .map(|group| {
                let owner = ir.classes[group.class as usize].fq_name;
                (
                    group.class,
                    properties.owner_activation(owner, || module.declare()),
                )
            })
            .collect();
        activations.extend(
            properties
                .reserved_owner_activations()
                .filter_map(|(owner, function)| Some((ir.class_id_by_name(owner)?, function)))
                .filter(|(class, _)| !plan.has_statics(*class)),
        );
        Ok(Self {
            plan,
            classes,
            activations,
        })
    }

    pub(super) fn plan(&self) -> &ClassStaticPlan {
        &self.plan
    }

    /// Define every class initializer [`Self::declare`] reserved.
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
        for &(class, function) in &self.activations {
            let ty = module.func_type(Vec::new(), Vec::new());
            let mut body = Body::new(context, module, runtime, &[], Default::default(), Ty::Unit);
            body.activate_class(class, None)?;
            let Body { code, locals, .. } = body;
            module.define(function, Function { ty, locals, code });
        }
        Ok(())
    }
}

impl Body<'_> {
    /// Run `class`'s own statics here unless they already ran.
    pub(super) fn initialize_own_statics(&mut self, class: ClassId) {
        self.code
            .call(self.class_initializers.classes[&class].function);
    }

    /// The constructor entry of `class`: its own statics, when the plan gives it any.
    pub(super) fn enter_constructor(&mut self, class: ClassId) {
        if let Some(class) = self.class_initializers.plan.constructor_entry(class) {
            self.initialize_own_statics(class);
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
                if let Some(function) =
                    owner.and_then(|owner| self.properties.reserved_owner_activation(owner))
                {
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
