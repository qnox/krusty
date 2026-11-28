//! What initializing each class of a file initializes, and which statics wait for their class.
//!
//! Kotlin initializes a class's `companion { … }` block statics with the class. A superclass, and
//! its companion, is initialized before its subclass, and so is each superinterface that has
//! statics of its own and declares a non-abstract member, in JLS 12.4.2's order: an interface's
//! own superinterfaces first. The sequence is read from the module's declaration headers, so a
//! superclass or superinterface declared in another file of the module takes part exactly as a
//! same-file one does. A superclass of another module ends the chain with one step: its module
//! publishes the entry that initializes it and its own superclasses.

use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::ir::{IrCheckedOperation, IrClassActivationStep, IrExpr, IrFile, IrStatic};
use crate::types::{wk, TypeName};

/// Record [`crate::ir::IrClass::activation`] on every class of `ir`, and
/// [`IrStatic::deferred`] on every static.
pub(super) fn record(index: &ResolvedModuleIndex, ir: &mut IrFile) {
    for position in 0..ir.statics.len() {
        let deferred = waits_for_its_class(ir, &ir.statics[position]);
        ir.statics[position].deferred = deferred;
    }
    for class in &mut ir.classes {
        class.activation = activation(index, class.fq_name).into_boxed_slice();
    }
}

/// Whether `declaration`'s initializer runs with its class rather than with the file.
///
/// Two initializers cannot tell the difference, so they stay with the file. A `const val`'s is a
/// compile-time constant. A delegated property's `KProperty` metadata is a property reference: one
/// emitted object per property with no state of its own, the same value however early it is asked
/// for. A static with no initializer has nothing to run.
fn waits_for_its_class(ir: &IrFile, declaration: &IrStatic) -> bool {
    declaration.owner.is_some()
        && !declaration.is_const
        && declaration.init.is_some_and(|init| {
            !matches!(
                ir.expr(init),
                IrExpr::Checked(checked)
                    if matches!(&**checked, IrCheckedOperation::PropertyReference { .. })
            )
        })
}

/// The steps that initializing `class` takes: its superclass chain root first, and for each
/// class of it the superinterfaces it initializes, its own statics, then its companion.
fn activation(index: &ResolvedModuleIndex, class: TypeName) -> Vec<IrClassActivationStep> {
    let mut chain = Vec::new();
    let mut steps = Vec::new();
    let mut current = index.classifier_declaration(class);
    while let Some(declaration) = current {
        if chain.contains(&declaration) {
            break;
        }
        chain.push(declaration);
        current = match superclass(index, declaration) {
            Some(Superclass::Module(superclass)) => Some(superclass),
            Some(Superclass::Dependency(superclass)) => {
                steps.push(IrClassActivationStep::Dependency(superclass));
                None
            }
            None => None,
        };
    }
    for &declaration in chain.iter().rev() {
        if !is_interface(index, declaration) {
            let mut interfaces = Vec::new();
            for interface in direct_superinterfaces(index, declaration) {
                enumerate_superinterface(index, interface, &mut interfaces);
            }
            for interface in interfaces {
                own_steps(index, interface, &mut steps);
            }
        }
        own_steps(index, declaration, &mut steps);
    }
    steps
}

/// A classifier's own statics, then its companion object. An enum's companion is initialized by
/// the enum's own initialization, not by this sequence.
fn own_steps(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    steps: &mut Vec<IrClassActivationStep>,
) {
    let Some(classifier) = index.classifier_identity(declaration) else {
        return;
    };
    if has_class_statics(index, classifier) {
        steps.push(IrClassActivationStep::OwnStatics(classifier));
    }
    let is_enum = flags(index, declaration).has(DeclarationFlags::ENUM);
    if !is_enum && index.companion_declaration(declaration).is_some() {
        steps.push(IrClassActivationStep::Companion(classifier));
    }
}

/// JLS 12.4.2's recursive enumeration: `interface`'s own superinterfaces first, then `interface`
/// itself when it has statics of its own and declares a non-abstract member.
fn enumerate_superinterface(
    index: &ResolvedModuleIndex,
    interface: DeclarationId,
    order: &mut Vec<DeclarationId>,
) {
    if order.contains(&interface) {
        return;
    }
    for parent in direct_superinterfaces(index, interface) {
        enumerate_superinterface(index, parent, order);
    }
    let initializes = index
        .classifier_identity(interface)
        .is_some_and(|classifier| has_class_statics(index, classifier))
        && declares_non_abstract_member(index, interface);
    if initializes && !order.contains(&interface) {
        order.push(interface);
    }
}

/// Whether `classifier` declares a block property whose initializer runs with the class: one with
/// an initializer or a delegate that is not a compile-time constant.
fn has_class_statics(index: &ResolvedModuleIndex, classifier: TypeName) -> bool {
    index
        .companion_block_properties(classifier)
        .iter()
        .any(|&property| {
            let flags = flags(index, property);
            (flags.has(DeclarationFlags::HAS_INITIALIZER) || flags.has(DeclarationFlags::DELEGATED))
                && !flags.has(DeclarationFlags::CONST)
        })
}

/// Whether `interface` declares a member function or property that is not abstract.
fn declares_non_abstract_member(index: &ResolvedModuleIndex, interface: DeclarationId) -> bool {
    index
        .owned_declarations(interface)
        .iter()
        .filter_map(|&member| index.declaration_header(member))
        .any(|header| {
            matches!(
                header.kind,
                DeclarationKind::Function | DeclarationKind::Property
            ) && !header.flags.has(DeclarationFlags::ABSTRACT)
        })
}

/// The superclass a class extends, other than `Any`.
enum Superclass {
    /// Declared in this module.
    Module(DeclarationId),
    /// Declared in another module.
    Dependency(TypeName),
}

fn superclass(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Option<Superclass> {
    let superclass = index.classifier_header(declaration)?.superclass?;
    let classifier = superclass.get().non_null().obj_internal()?;
    if classifier == wk::any() {
        return None;
    }
    Some(match index.classifier_declaration(classifier) {
        Some(declaration) => Superclass::Module(declaration),
        None => Superclass::Dependency(classifier),
    })
}

/// The module interfaces `declaration` names as direct supertypes, in declaration order.
fn direct_superinterfaces(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Vec<DeclarationId> {
    let Some(header) = index.classifier_header(declaration) else {
        return Vec::new();
    };
    header
        .interfaces
        .iter()
        .filter_map(|interface| interface.get().non_null().obj_internal())
        .filter_map(|classifier| index.classifier_declaration(classifier))
        .filter(|&interface| is_interface(index, interface))
        .collect()
}

fn is_interface(index: &ResolvedModuleIndex, declaration: DeclarationId) -> bool {
    flags(index, declaration).has(DeclarationFlags::INTERFACE)
}

fn flags(index: &ResolvedModuleIndex, declaration: DeclarationId) -> DeclarationFlags {
    index
        .declaration_header(declaration)
        .map(|header| header.flags)
        .unwrap_or_default()
}
