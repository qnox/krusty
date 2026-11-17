//! The member function slots of a source classifier (see [`crate::fir::ResolvedFunctionSlotFamily`]).
//!
//! Every declaration the applied hierarchy declares under a name joins the slot of a declaration it
//! is override-equivalent to there, and the classifier's own declaration joins the slot of each
//! inherited declaration whose inputs it overrides. Only names that keep more than one slot are
//! published: a single slot is one member on every target.

use super::{
    applied_parameters_with_receiver, declarations_by_target, declared_functions, function_target,
    module_parameter_identities, overridden_parameter_identities,
    realized_overridden_declaration_signature, resolved_ty, resolved_types, OverridingFunction,
};
use crate::fir::{
    ResolvedAppliedClassifier, ResolvedFunctionOverrideTarget, ResolvedFunctionSlotFamily,
    ResolvedModuleIndex, ResolvedSlotFunction,
};
use crate::libraries::FunctionInfo;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// A slot member and, for an inherited declaration, the applied view override equivalence is
/// decided on.
struct Member<'a> {
    function: ResolvedSlotFunction,
    view: MemberView<'a>,
}

enum MemberView<'a> {
    /// The classifier's own declaration, with its inputs as override matching compares them.
    Declared(&'a OverridingFunction, Vec<Ty>),
    /// An inherited declaration as the classifier's applied supertype sees it.
    Inherited(Box<FunctionInfo>),
}

/// The classifier's own function declarations under one name, as their override edges were built.
pub(super) struct DeclaredFunction<'a> {
    pub(super) implementation: &'a OverridingFunction,
}

pub(super) fn function_slot_families(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    owner: TypeName,
    declared: &[(String, Vec<DeclaredFunction<'_>>)],
    hierarchy: &[ResolvedAppliedClassifier],
) -> Vec<ResolvedFunctionSlotFamily> {
    let mut names = declared
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let Some(classifier) = source.classifier(supertype.classifier) else {
            continue;
        };
        for name in &classifier.declared_callable_order {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            let own = declared
                .iter()
                .find(|(declared_name, _)| *declared_name == name)
                .map(|(_, functions)| functions.as_slice())
                .unwrap_or_default();
            name_slots(index, source, owner, &name, own, hierarchy)
        })
        .collect()
}

fn name_slots(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    own: &[DeclaredFunction<'_>],
    hierarchy: &[ResolvedAppliedClassifier],
) -> Option<ResolvedFunctionSlotFamily> {
    let mut members = own
        .iter()
        .map(|declared| Member {
            function: declared_slot_function(index, owner, declared.implementation),
            view: MemberView::Declared(declared.implementation, declared.implementation.inputs()),
        })
        .collect::<Vec<_>>();
    let mut supertypes = hierarchy
        .iter()
        .filter(|entry| entry.depth != 0)
        .collect::<Vec<_>>();
    supertypes.sort_by_key(|entry| entry.depth);
    for supertype in supertypes {
        let raw = declarations_by_target(
            declared_functions(source, Ty::obj_name(supertype.classifier), name),
            |function| function_target(index, function),
        );
        for applied in declared_functions(source, supertype.applied.get(), name) {
            let Some(target) = function_target(index, &applied) else {
                continue;
            };
            if members
                .iter()
                .any(|member| member.function.target == target)
            {
                continue;
            }
            let Some(declared) = raw.get(&target) else {
                continue;
            };
            // A declaration its provider cannot realize has no signature a target could compare.
            let Some(function) = inherited_slot_function(
                index,
                source,
                supertype.classifier,
                target,
                declared,
                &applied,
            ) else {
                continue;
            };
            members.push(Member {
                function,
                view: MemberView::Inherited(Box::new(applied)),
            });
        }
    }
    if members.len() < 2 {
        return None;
    }
    let mut slot_of = (0..members.len()).collect::<Vec<_>>();
    fn root(slot_of: &mut [usize], mut member: usize) -> usize {
        while slot_of[member] != member {
            slot_of[member] = slot_of[slot_of[member]];
            member = slot_of[member];
        }
        member
    }
    for left in 0..members.len() {
        for right in left + 1..members.len() {
            if same_slot(source, &members[left], &members[right]) {
                let (left, right) = (root(&mut slot_of, left), root(&mut slot_of, right));
                // The earlier member roots the slot, so a declared member leads its slot.
                slot_of[left.max(right)] = left.min(right);
            }
        }
    }
    let mut slots = Vec::<(usize, Vec<ResolvedSlotFunction>)>::new();
    for (position, member) in members.into_iter().enumerate() {
        let slot = root(&mut slot_of, position);
        match slots.iter_mut().find(|(root, _)| *root == slot) {
            Some((_, functions)) => functions.push(member.function),
            None => slots.push((slot, vec![member.function])),
        }
    }
    (slots.len() > 1).then(|| ResolvedFunctionSlotFamily {
        name: name.into(),
        slots: slots
            .into_iter()
            .map(|(_, functions)| functions.into_boxed_slice())
            .collect(),
    })
}

/// Whether two members are one slot: the classifier's declaration and an inherited declaration
/// whose inputs it overrides, or two inherited declarations that override one another or merge
/// into one fake override in the classifier's applied view. A declaration overrides by its inputs
/// alone; a result or modifier it gets wrong is the frontend's diagnostic, not another slot.
fn same_slot(source: &dyn SymbolSource, left: &Member<'_>, right: &Member<'_>) -> bool {
    match (&left.view, &right.view) {
        (MemberView::Inherited(left), MemberView::Inherited(right)) => {
            crate::symbol_resolver::function_override_input_slots_match(source, left, right)
                || crate::symbol_resolver::function_override_input_slots_match(source, right, left)
        }
        (MemberView::Declared(declared, inputs), MemberView::Inherited(inherited))
        | (MemberView::Inherited(inherited), MemberView::Declared(declared, inputs)) => {
            declared.overrides_inputs_of(source, inputs, inherited)
        }
        (MemberView::Declared(..), MemberView::Declared(..)) => false,
    }
}

fn declared_slot_function(
    index: &ResolvedModuleIndex,
    owner: TypeName,
    implementation: &OverridingFunction,
) -> ResolvedSlotFunction {
    let parameters = resolved_types(
        implementation.parameters.iter().copied(),
        "declared slot function parameters",
    );
    let result = resolved_ty(implementation.result, "declared slot function result");
    let identities = match implementation.target {
        ResolvedFunctionOverrideTarget::Module(callable) => {
            module_parameter_identities(index, callable, parameters.len())
        }
        ResolvedFunctionOverrideTarget::External(_) => implementation.parameter_identities.clone(),
    };
    ResolvedSlotFunction {
        target: implementation.target,
        owner,
        declared_by_classifier: true,
        declared_parameters: parameters.clone(),
        declared_result: result,
        applied_parameters: parameters,
        applied_result: result,
        parameter_identities: identities,
        suspend: implementation.suspend,
    }
}

fn inherited_slot_function(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    owner: TypeName,
    target: ResolvedFunctionOverrideTarget,
    declared: &FunctionInfo,
    applied: &FunctionInfo,
) -> Option<ResolvedSlotFunction> {
    let (declared_parameters, declared_result) =
        realized_overridden_declaration_signature(source, declared, target)?;
    let applied_parameters = applied_parameters_with_receiver(applied);
    Some(ResolvedSlotFunction {
        target,
        owner,
        declared_by_classifier: false,
        parameter_identities: overridden_parameter_identities(
            index,
            target,
            declared,
            declared_parameters.len(),
        ),
        declared_parameters,
        declared_result,
        applied_parameters: resolved_types(
            applied_parameters.iter().copied(),
            "applied slot function parameters",
        ),
        applied_result: resolved_ty(
            applied.ret.apply(applied.callable.ret).canonical_semantic(),
            "applied slot function result",
        ),
        suspend: applied.flags.suspend,
    })
}
