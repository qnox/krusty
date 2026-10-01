//! Which constructor slots give a class kotlinc's hidden constructor.
//!
//! A class whose constructor takes a value class gets a private constructor over the carriers and a
//! public accessor taking a trailing `DefaultConstructorMarker`. kotlinc decides that from the
//! constructor's parameters when it lowers value classes: its declared parameters, an inner class's
//! outer instance, and a local class's captured values, which are already parameters by then. An
//! anonymous object's captures and a lambda class's are added after that lowering, as carriers, so
//! they never hide the constructor.

use std::collections::{HashMap, HashSet};

use crate::ir::{ExprId, IrClass, IrFile};
use crate::types::{Ty, TypeName};

/// Per primary-constructor slot of `class`, whether a value class there selects the hidden
/// constructor.
pub(super) fn selecting_slots(class: &IrClass) -> impl Iterator<Item = bool> + '_ {
    let captures_count = !class.is_anonymous_object && class.lambda.is_none();
    class
        .ctor_args
        .iter()
        .map(move |argument| argument.declared_ty.is_some() || captures_count)
}

/// Whether a value-class parameter in a selecting primary-constructor slot hides `class`'s
/// constructor.
pub(super) fn primary_has_value_class(
    class: &IrClass,
    is_value_class: impl Fn(&Ty) -> bool,
) -> bool {
    selecting_slots(class)
        .zip(&class.ctor_args)
        .any(|(selects, argument)| selects && is_value_class(&argument.ty))
}

/// The primary-constructor facts needed after expression erasure begins.
pub(super) struct PrimaryConstructorSelection {
    selecting_slots: HashMap<TypeName, Vec<bool>>,
    constructions: HashSet<ExprId>,
}

impl PrimaryConstructorSelection {
    pub(super) fn record(ir: &IrFile) -> Self {
        let selecting_slots = ir
            .classes
            .iter()
            .map(|class| (class.fq_name, selecting_slots(class).collect()))
            .collect();
        let constructions = ir
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(expression, _)| {
                let expression = expression as ExprId;
                ir.construction_targets
                    .get(&expression)
                    .is_none_or(|target| target.primary())
                    .then_some(expression)
            })
            .collect();
        Self {
            selecting_slots,
            constructions,
        }
    }

    /// Whether this construction selects a hidden primary constructor. A call without a recorded
    /// local declaration keeps the existing parameter-type rule for external constructors.
    pub(super) fn hides_value_class(
        &self,
        expression: ExprId,
        owner: TypeName,
        parameters: &[Ty],
        is_value_class: impl Fn(&Ty) -> bool,
    ) -> bool {
        let declared = self.selecting_slots.get(&owner).filter(|slots| {
            slots.len() == parameters.len() && self.constructions.contains(&expression)
        });
        match declared {
            Some(slots) => parameters
                .iter()
                .zip(slots)
                .any(|(parameter, &selects)| selects && is_value_class(parameter)),
            None => parameters.iter().any(is_value_class),
        }
    }
}
