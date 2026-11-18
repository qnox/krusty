//! When a static a class owns is initialized, for targets with no class initialization of their
//! own.
//!
//! A `companion { … }` block property is stored with its class, and Kotlin runs its initializer
//! as part of initializing that class: the first construction of the class, the first call of one
//! of its block functions or accessors, or the first read or write of the storage itself, whichever
//! comes first. A superclass, and its companion, is initialized before its subclass. A file's
//! other statics run when the file is first touched; a class owes no such promise, so an
//! initializer that could observe the difference waits for its class.
//!
//! This plan decides that once for every target: which statics wait, which class each belongs to,
//! and what each activation point initializes. A target emits only the representation: one
//! function per class that runs the class's statics in declaration order behind a once-only flag,
//! set before the initializers run so that an initializer reaching back into its own class sees
//! the storage's default, as Kotlin does, and a call of that function at each activation point.

use std::collections::HashMap;

use crate::ir::{ClassId, FunId, IrCheckedOperation, IrExpr, IrFile, IrStatic};
use crate::types::{wk, TypeName};

/// The statics one class runs when it is initialized.
#[derive(Debug)]
pub struct ClassStatics {
    pub class: ClassId,
    /// Indexes into `IrFile::statics`, in declaration order.
    pub statics: Box<[u32]>,
}

/// A class owns a static whose initialization this plan cannot order with the rest of the class:
/// an interface, an enum, an object, a value class, or a class with a companion object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnorderedClassStatic {
    pub owner: TypeName,
}

/// Which statics of a file wait for their class, and what each activation point initializes.
#[derive(Debug, Default)]
pub struct ClassStaticPlan {
    /// By class id.
    classes: Vec<ClassStatics>,
    /// The class each waiting static belongs to.
    owners: HashMap<u32, ClassId>,
}

impl ClassStaticPlan {
    pub fn new(ir: &IrFile) -> Result<Self, UnorderedClassStatic> {
        let mut groups: HashMap<ClassId, Vec<u32>> = HashMap::new();
        let mut owners = HashMap::new();
        for (index, declaration) in ir.statics.iter().enumerate() {
            let Some(owner) = declaration.owner else {
                continue;
            };
            if observes_no_initialization_order(ir, declaration) {
                continue;
            }
            let unordered = UnorderedClassStatic { owner };
            let class = ir.class_id_by_name(owner).ok_or(unordered)?;
            let declaration = &ir.classes[class as usize];
            if declaration.is_interface
                || declaration.is_object
                || declaration.is_value
                || declaration.companion_class.is_some()
                || declaration.superclass == wk::kotlin_enum()
            {
                return Err(unordered);
            }
            groups.entry(class).or_default().push(index as u32);
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

    /// Every class with statics of its own, by class id.
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

    /// What activating one class of a superclass chain initializes, in order: its own statics,
    /// then its companion object. The chain itself is walked root first.
    pub fn activation_steps(&self, class: ClassId) -> impl Iterator<Item = ClassActivationStep> {
        let own = self
            .has_statics(class)
            .then_some(ClassActivationStep::OwnStatics(class));
        own.into_iter()
            .chain([ClassActivationStep::Companion(class)])
    }
}

/// One part of activating a class of a superclass chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassActivationStep {
    /// Run the class's own statics.
    OwnStatics(ClassId),
    /// Initialize the class's companion object, when it has one.
    Companion(ClassId),
}

/// Whether running `declaration`'s initializer at file start rather than with its class cannot
/// be observed.
///
/// Two initializers cannot tell. A `const val`'s is a compile-time constant. A delegated
/// property's `KProperty` metadata is a property REFERENCE, one emitted object per property with
/// no state of its own and nothing to allocate: the same value however early it is asked for. A
/// static with no initializer has nothing to run.
fn observes_no_initialization_order(ir: &IrFile, declaration: &IrStatic) -> bool {
    declaration.is_const
        || declaration.init.is_none_or(|init| {
            matches!(
                ir.expr(init),
                IrExpr::Checked(checked) if matches!(&**checked, IrCheckedOperation::PropertyReference { .. })
            )
        })
}

#[cfg(test)]
mod tests;
