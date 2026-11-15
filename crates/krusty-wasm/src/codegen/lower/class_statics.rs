//! Statics a class owns: initialized when the class is first touched, not at file start.
//!
//! A `companion { … }` block property is stored with its class, and Kotlin runs its initializer
//! as part of initializing that class: the first construction of the class, the first call of one
//! of its block functions or accessors, or the first read or write of the storage itself, whichever
//! comes first. A superclass is initialized before its subclass. Each class gets one function that
//! runs its initializers behind a once-only flag, called at each of those entry points.
//!
//! The flag is set before the initializers run: an initializer that reaches back into its own
//! class sees the storage's default, which is what Kotlin does.

use super::*;

/// The emitted pieces of one class's static initialization.
struct ClassInitializer {
    /// Runs the class's initializers at most once.
    function: u32,
    /// An `i32` global set once the initializers have started.
    flag: u32,
    /// The class's statics, by index into `IrFile::statics`, in declaration order.
    statics: Vec<u32>,
}

/// The initializer of each class of a file that owns statics.
#[derive(Default)]
pub(super) struct ClassInitializers {
    classes: HashMap<ClassId, ClassInitializer>,
}

impl ClassInitializers {
    /// Assign every owned static of `ir` to its class's initializer. A class whose initialization
    /// has other parts this generator does not order — an interface, an enum, an object, a value
    /// class, or a class with a companion object — declines.
    pub(super) fn declare(ir: &IrFile, module: &mut Module) -> Result<Self, Unsupported> {
        let mut initializers = Self::default();
        for (index, property) in ir.statics.iter().enumerate() {
            // A `const` is a compile-time constant: reading it touches nothing, and it has no
            // order to keep, so it is stored with the file.
            let Some(owner) = property.owner.filter(|_| !property.is_const) else {
                continue;
            };
            let declined = || format!("a static property of `{}`", owner.render());
            let Some(class) = ir.class_id_by_name(owner) else {
                return Err(declined());
            };
            let declaration = &ir.classes[class as usize];
            if declaration.is_interface
                || declaration.is_object
                || declaration.is_value
                || declaration.companion_class.is_some()
                || declaration.superclass == krusty::types::wk::kotlin_enum()
            {
                return Err(declined());
            }
            initializers
                .classes
                .entry(class)
                .or_insert_with(|| ClassInitializer {
                    function: module.declare(),
                    flag: module.global(ValType::I32),
                    statics: Vec::new(),
                })
                .statics
                .push(index as u32);
        }
        Ok(initializers)
    }

    /// Whether this static waits for its class rather than running with the file.
    pub(super) fn is_deferred(&self, index: u32) -> bool {
        self.classes
            .values()
            .any(|initializer| initializer.statics.contains(&index))
    }

    /// The initializer to run before touching `class`: its own, or its nearest initialized
    /// superclass's, since constructing it runs every superclass constructor.
    fn for_class(&self, ir: &IrFile, class: ClassId) -> Option<u32> {
        let mut current = Some(class);
        while let Some(class) = current {
            if let Some(initializer) = self.classes.get(&class) {
                return Some(initializer.function);
            }
            current = ir.class_id_by_name(ir.classes[class as usize].superclass);
        }
        None
    }

    /// Define every class initializer [`Self::declare`] reserved.
    pub(super) fn define(
        &self,
        context: &FileContext<'_>,
        module: &mut Module,
        runtime: &mut Runtime,
    ) -> Result<(), Unsupported> {
        let ir = context.ir;
        let mut classes: Vec<ClassId> = self.classes.keys().copied().collect();
        classes.sort_unstable();
        for class in classes {
            let initializer = &self.classes[&class];
            let declared = initializer
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
            let superclass = ir
                .class_id_by_name(ir.classes[class as usize].superclass)
                .and_then(|parent| self.for_class(ir, parent));
            if let Some(superclass) = superclass {
                body.code.call(superclass);
            }
            for &index in &initializer.statics {
                let property = &ir.statics[index as usize];
                if let Some(init) = property.init {
                    body.value(init, property.ty)?;
                    body.code.global_set(context.globals[index as usize]);
                }
            }
            let Body { code, locals, .. } = body;
            module.define(initializer.function, Function { ty, locals, code });
        }
        Ok(())
    }
}

impl Body<'_> {
    /// Initialize `class` here unless it already is: the entry of its constructors and of its
    /// block functions and accessors.
    pub(super) fn initialize_class(&mut self, class: ClassId) {
        if let Some(function) = self.class_initializers.for_class(self.ir, class) {
            self.code.call(function);
        }
    }

    /// Initialize the class that owns static `index`, before that static is read or written.
    pub(super) fn initialize_static_owner(&mut self, index: u32) {
        let property = &self.ir.statics[index as usize];
        let owner = property
            .owner
            .filter(|_| !property.is_const)
            .and_then(|owner| self.ir.class_id_by_name(owner));
        if let Some(class) = owner {
            self.initialize_class(class);
        }
    }
}
