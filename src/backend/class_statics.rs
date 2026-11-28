//! The statics a class runs when it is initialized, for targets with no class initialization of
//! their own.
//!
//! Common lowering records which statics wait for their class ([`IrStatic::deferred`]) and what
//! initializing each class initializes ([`crate::ir::IrClass::activation`]). This plan groups a
//! file's deferred statics by the class that runs them and names the activation points: the
//! first construction of a class, the first call of one of its block functions or accessors, and
//! the first read or write of the storage itself. A target emits only the representation: one
//! function per class that runs the class's statics in declaration order behind a once-only flag,
//! set before the initializers run so that an initializer reaching back into its own class sees
//! the storage's default, as Kotlin does, and the steps of the class's activation at each
//! activation point.
//!
//! [`IrStatic::deferred`]: crate::ir::IrStatic::deferred

use std::collections::HashMap;

use crate::ir::{ClassId, FunId, IrClassActivationStep, IrFile};
use crate::types::TypeName;

/// The statics one class runs when it is initialized.
#[derive(Debug)]
pub struct ClassStatics {
    pub class: ClassId,
    /// Indexes into `IrFile::statics`, in declaration order.
    pub statics: Box<[u32]>,
}

/// A class owns a static whose initialization this plan cannot order with the rest of the class:
/// an enum, an object, or a value class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnorderedClassStatic {
    pub owner: TypeName,
}

/// Which statics of a file wait for their class, and what each activation point initializes.
#[derive(Debug, Default)]
pub struct ClassStaticPlan {
    /// By class id: every class of the file whose activation runs its own statics.
    classes: Vec<ClassStatics>,
    /// The class each waiting static belongs to.
    owners: HashMap<u32, ClassId>,
}

impl ClassStaticPlan {
    pub fn new(ir: &IrFile) -> Result<Self, UnorderedClassStatic> {
        let mut groups: HashMap<ClassId, Vec<u32>> = HashMap::new();
        for (position, declaration) in ir.classes.iter().enumerate() {
            let class = ClassId::try_from(position).expect("class ids fit in u32");
            if declaration
                .activation
                .contains(&IrClassActivationStep::OwnStatics(declaration.fq_name))
            {
                groups.insert(class, Vec::new());
            }
        }
        let mut owners = HashMap::new();
        for (index, declaration) in ir.statics.iter().enumerate() {
            if !declaration.deferred {
                continue;
            }
            let owner = declaration
                .owner
                .expect("a static that waits for its class has an owner");
            let unordered = UnorderedClassStatic { owner };
            let class = ir.class_id_by_name(owner).ok_or(unordered)?;
            let class_declaration = &ir.classes[class as usize];
            // A class's or an interface's companion object is initialized by the activation step
            // that follows its statics; an object, an enum and a value class have initialization
            // parts of their own this plan does not order.
            if class_declaration.is_object
                || class_declaration.is_value
                || class_declaration.is_enum
            {
                return Err(unordered);
            }
            groups.get_mut(&class).ok_or(unordered)?.push(index as u32);
            owners.insert(index as u32, class);
        }
        let mut classes: Vec<ClassStatics> = groups
            .into_iter()
            .map(|(class, statics)| ClassStatics {
                class,
                statics: statics.into_boxed_slice(),
            })
            .collect();
        classes.sort_unstable_by_key(|group| group.class);
        Ok(Self { classes, owners })
    }

    /// Every class of the file whose activation runs statics of its own, by class id.
    pub fn classes(&self) -> &[ClassStatics] {
        &self.classes
    }

    /// Whether static `index` waits for its class rather than running with the file.
    pub fn is_deferred(&self, index: u32) -> bool {
        self.owners.contains_key(&index)
    }

    /// Whether `class` has statics of its own to run.
    pub fn has_statics(&self, class: ClassId) -> bool {
        self.classes
            .binary_search_by_key(&class, |group| group.class)
            .is_ok()
    }

    /// The class to activate before static `index` is read or written.
    pub fn access_activation(&self, index: u32) -> Option<ClassId> {
        self.owners.get(&index).copied()
    }

    /// The class to activate on entry to `function`: a block function or accessor is a static
    /// member of its class, so calling one initializes the class.
    pub fn function_activation(&self, ir: &IrFile, function: FunId) -> Option<ClassId> {
        ir.companion_blocks.declaring_class(function)
    }

    /// The class whose own statics run on entry to its constructor, which a subclass reaches
    /// through `super(…)` even when it was laid out in another file. A construction itself
    /// activates the whole class first, superclasses and companions included.
    pub fn constructor_entry(&self, class: ClassId) -> Option<ClassId> {
        self.has_statics(class).then_some(class)
    }
}

#[cfg(test)]
mod tests;
