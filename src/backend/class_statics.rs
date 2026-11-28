//! When a static a class owns is initialized, for targets with no class initialization of their
//! own.
//!
//! A `companion { … }` block property is stored with its class, and Kotlin runs its initializer
//! as part of initializing that class: the first construction of the class, the first call of one
//! of its block functions or accessors, or the first read or write of the storage itself, whichever
//! comes first. A superclass, and its companion, is initialized before its subclass, and so is
//! each superinterface with statics and a non-abstract member (JLS 12.4.2). A file's
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
    /// Indexes into `IrFile::statics`, in the order they run.
    pub statics: Box<[u32]>,
    /// When the class's static state initializes in program order around its `companion object`
    /// (kotlinc 2.5): how many leading `statics` were written before the companion and so run
    /// before it, the rest after its whole initialization. `None` runs every static first, then
    /// the companion.
    pub before_companion: Option<usize>,
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
    /// By class id.
    classes: Vec<ClassStatics>,
    /// The class each waiting static belongs to.
    owners: HashMap<u32, ClassId>,
    /// The superinterfaces with statics that initializing a class initializes first, in
    /// initialization order. Only classes that have any are listed.
    superinterfaces: HashMap<ClassId, Box<[ClassId]>>,
}

impl ClassStaticPlan {
    /// Plan `ir`'s class statics. `program_order` is whether a class's static state initializes in
    /// program order around its companion object, as kotlinc 2.5 does, rather than with the
    /// companion's body last.
    pub fn new(ir: &IrFile, program_order: bool) -> Result<Self, UnorderedClassStatic> {
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
            // An interface has no superclass chain and no constructor, so what initializes it is
            // exactly the access to its storage or the call of one of its block functions: the
            // same activation points a class has. A class's or an interface's companion object is
            // initialized by the activation step that follows its statics.
            if declaration.is_object
                || declaration.is_value
                || declaration.superclass == wk::kotlin_enum()
            {
                return Err(unordered);
            }
            groups.entry(class).or_default().push(index as u32);
            owners.insert(index as u32, class);
        }
        let mut classes: Vec<ClassStatics> = groups
            .into_iter()
            .map(|(class, mut statics)| {
                let before_companion = companion_source_order(ir, class)
                    .filter(|_| program_order)
                    .map(|companion| {
                        // Stable: each side keeps declaration order.
                        statics.sort_by_key(|&index| {
                            ir.statics[index as usize].source_order >= companion
                        });
                        statics.partition_point(|&index| {
                            ir.statics[index as usize].source_order < companion
                        })
                    });
                ClassStatics {
                    class,
                    statics: statics.into_boxed_slice(),
                    before_companion,
                }
            })
            .collect();
        classes.sort_unstable_by_key(|group| group.class);
        let mut plan = Self {
            classes,
            owners,
            superinterfaces: HashMap::new(),
        };
        plan.superinterfaces = (0..ir.classes.len())
            .filter_map(|class| {
                let class = ClassId::try_from(class).expect("class ids fit in u32");
                if ir.classes[class as usize].is_interface {
                    return None;
                }
                let mut order = Vec::new();
                for &interface in &plan.direct_superinterfaces(ir, class) {
                    plan.enumerate_superinterface(ir, interface, &mut order);
                }
                (!order.is_empty()).then(|| (class, order.into_boxed_slice()))
            })
            .collect();
        Ok(plan)
    }

    /// The superinterfaces `class` names that this file declares, in declaration order.
    fn direct_superinterfaces(&self, ir: &IrFile, class: ClassId) -> Vec<ClassId> {
        ir.classes[class as usize]
            .interfaces
            .iter()
            .filter_map(|name| ir.class_id_by_name(name))
            .filter(|&interface| ir.classes[interface as usize].is_interface)
            .collect()
    }

    /// JLS 12.4.2's recursive enumeration: `interface`'s own superinterfaces first, then
    /// `interface` itself when it has statics of its own and declares a non-abstract member.
    fn enumerate_superinterface(&self, ir: &IrFile, interface: ClassId, order: &mut Vec<ClassId>) {
        if order.contains(&interface) {
            return;
        }
        for parent in self.direct_superinterfaces(ir, interface) {
            self.enumerate_superinterface(ir, parent, order);
        }
        let declares_non_abstract_member =
            ir.classes[interface as usize]
                .methods
                .iter()
                .any(|&method| {
                    let function = &ir.functions[method as usize];
                    function.body.is_some() && !function.is_static
                });
        if self.has_statics(interface)
            && declares_non_abstract_member
            && !order.contains(&interface)
        {
            order.push(interface);
        }
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

    /// What activating one class of a superclass chain initializes, in order: the superinterfaces
    /// it initializes first (each one's statics, then its companion), its own statics, then its
    /// companion object. The chain itself is walked root first.
    pub fn activation_steps(&self, class: ClassId) -> std::vec::IntoIter<ClassActivationStep> {
        let mut steps = self.superinterface_steps(class);
        let mut all: Vec<_> = steps.by_ref().collect();
        all.extend(self.owner_steps(class));
        all.into_iter()
    }

    /// The part of [`Self::activation_steps`] that initializes the superinterfaces of `class`.
    pub fn superinterface_steps(&self, class: ClassId) -> std::vec::IntoIter<ClassActivationStep> {
        self.superinterfaces
            .get(&class)
            .into_iter()
            .flat_map(|interfaces| interfaces.iter().copied())
            .flat_map(|owner| self.owner_steps(owner))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// `owner`'s own statics, when it has any, then its companion object.
    fn owner_steps(&self, owner: ClassId) -> impl Iterator<Item = ClassActivationStep> {
        self.has_statics(owner)
            .then_some(ClassActivationStep::OwnStatics(owner))
            .into_iter()
            .chain([ClassActivationStep::Companion(owner)])
    }
}

/// Where the `companion object` of `class` was written, for the class's static state to initialize
/// in program order around it: for a class with a companion object declared in this
/// file.
fn companion_source_order(ir: &IrFile, class: ClassId) -> Option<u32> {
    ir.classes[class as usize]
        .companion_class
        .and_then(|companion| ir.class_id_by_name(companion))
        .and_then(|companion| ir.class_source_order(companion))
}

/// One part of activating a class of a superclass chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassActivationStep {
    /// Run the class's own statics, which initialize its companion object among them when the
    /// class's state initializes in program order.
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
