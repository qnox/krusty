//! kotlinc's `LambdaMetafactory` signature adaptation (`LambdaMetafactoryArguments`): for each slot
//! of the interface method a lambda implements, how the factory must adapt the implementation's
//! type to the slot's declared one, and whether it cannot. A conflict makes the lambda a class.

use krusty::ir::{FunId, IrFile};
use krusty::types::{Ty, TyKind};

/// kotlinc's `TypeAdaptationConstraint`: how `LambdaMetafactory` must adapt one slot of the
/// implementation (the adaptee) to one slot of an interface method the lambda implements
/// (`LambdaMetafactoryArguments.computeParameterTypeAdaptationConstraint`). Slots that need no
/// adaptation have no constraint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Adaptation {
    ForceBoxing,
    BoxPrimitive,
    KeepUnboxed,
    Conflict,
}

/// The constraint between an implementation slot of type `adaptee` and an interface slot declared
/// with `expected`. A type parameter is erased to its upper bound's class. `Nothing` has no
/// adaptee; a value class over a non-null reference cannot be boxed by the factory; and the
/// factory does not know the mangled method a value class in the interface slot implies.
fn parameter_adaptation(
    ir: &IrFile,
    classifiers: &dyn krusty::types::ClassifierFactSource,
    adaptee: Ty,
    expected: Ty,
) -> Option<Adaptation> {
    if matches!(adaptee.non_null(), Ty::Nothing | Ty::Null) {
        return Some(Adaptation::Conflict);
    }
    if super::is_primitive(adaptee) {
        return Some(if super::is_primitive(expected) {
            Adaptation::KeepUnboxed
        } else {
            Adaptation::BoxPrimitive
        });
    }
    let boxable = value_class_boxable(ir, classifiers, adaptee)?;
    Some(
        if !boxable || value_class_boxable(ir, classifiers, expected).is_some() {
            Adaptation::Conflict
        } else {
            Adaptation::ForceBoxing
        },
    )
}

/// The result's constraint: a `Unit` implementation returns `void`, which only a `Unit` slot
/// shares; any other result adapts as a parameter does.
fn result_adaptation(
    ir: &IrFile,
    classifiers: &dyn krusty::types::ClassifierFactSource,
    adaptee: Ty,
    expected: Ty,
) -> Option<Adaptation> {
    if adaptee == Ty::Unit {
        return Some(if expected == Ty::Unit {
            Adaptation::KeepUnboxed
        } else {
            Adaptation::ForceBoxing
        });
    }
    parameter_adaptation(ir, classifiers, adaptee, expected)
}

/// Whether the constraints one slot takes from each method it implements disagree, or any is a
/// conflict (`joinSignatureAdaptationConstraints`).
fn conflicting(constraints: impl IntoIterator<Item = Option<Adaptation>>) -> bool {
    let mut joined = None;
    for constraint in constraints.into_iter().flatten() {
        if constraint == Adaptation::Conflict || joined.is_some_and(|other| other != constraint) {
            return true;
        }
        joined = Some(constraint);
    }
    false
}

/// Whether `LambdaMetafactory` cannot adapt a function lambda's implementation to the generic
/// `FunctionN.invoke`. Every `FunctionN` slot is a type parameter, so only the implementation's
/// own types decide; the result is the body's inferred one where the frontend recorded it.
pub(super) fn function_conflict(
    ir: &IrFile,
    classifiers: &dyn krusty::types::ClassifierFactSource,
    fid: FunId,
    signature: &krusty::types::FnSig,
) -> bool {
    let generic = Ty::nullable(Ty::obj("kotlin/Any"));
    signature
        .params
        .iter()
        .map(|&parameter| parameter_adaptation(ir, classifiers, parameter, generic))
        .chain([result_adaptation(
            ir,
            classifiers,
            super::implementation_result(ir, fid, signature),
            generic,
        )])
        .any(|constraint| conflicting([constraint]))
}

/// Whether `LambdaMetafactory` cannot adapt a SAM-converted lambda to its interface method: the
/// method's instantiated slots are the adaptee, and the method's declaration, with every
/// declaration it overrides, gives the expected slots.
pub(super) fn sam_conflict(
    ir: &IrFile,
    classifiers: &dyn krusty::types::ClassifierFactSource,
    target: &krusty::ir::IrSamTarget,
) -> bool {
    target.parameters.len() != target.declared_parameters.len()
        || target
            .parameters
            .iter()
            .zip(&target.declared_parameters)
            .any(|(&adaptee, &expected)| {
                conflicting([parameter_adaptation(ir, classifiers, adaptee, expected)])
            })
        || conflicting(
            std::iter::once(target.declared_result)
                .chain(target.overridden_results.iter().copied())
                .map(|expected| result_adaptation(ir, classifiers, target.result, expected)),
        )
}

/// Whether a SAM conversion's lambda is one this pass realizes when the factory cannot build it:
/// a plain lambda literal converted to a non-suspend interface under `indy` SAM conversions.
pub(super) fn sam_candidate(
    target: &krusty::ir::IrSamTarget,
    modes: crate::ir_emit::LambdaModes,
) -> bool {
    modes.sam_conversions == crate::ir_emit::LambdaMode::Indy
        && !target.suspend
        && !target.source_suspend
        && !target.function_adapter
        && !target.wraps_function_value
        && !target.nullable
        && !crate::override_results::boxes_sam_result(target)
}

/// For a value class `ty` erases to (a type parameter through its upper bound), whether the
/// factory can box it: its declared underlying type is nullable or a primitive. `None` for any
/// other type.
fn value_class_boxable(
    ir: &IrFile,
    classifiers: &dyn krusty::types::ClassifierFactSource,
    ty: Ty,
) -> Option<bool> {
    let classifier = match ty.non_null().kind() {
        TyKind::TyParam(_, bound) => return value_class_boxable(ir, classifiers, bound),
        _ => ty.non_null().obj_internal()?,
    };
    if let Some(class) = ir
        .classes
        .iter()
        .find(|class| class.is_value && class.fq_name == classifier)
    {
        let field = class
            .fields
            .first()
            .expect("a value class has its underlying field before lambda-class realization");
        return Some(match &field.type_param {
            Some(name) => {
                field.ty.is_nullable()
                    || class
                        .type_param_bounds
                        .iter()
                        .find(|(candidate, _)| candidate == name)
                        .is_none_or(|(_, bound)| bound.is_nullable())
            }
            None => field.ty.is_nullable() || super::is_primitive(field.ty),
        });
    }
    let recorded = ir.external_value_class_name(classifier).copied();
    let published = classifiers.classifier_value_underlying(classifier);
    if let (Some(recorded), Some(published)) = (recorded, published) {
        assert_eq!(
            recorded.canonical_semantic(),
            published.canonical_semantic(),
            "checked providers disagreed about one value-class declaration"
        );
    }
    recorded
        .or(published)
        .map(|underlying| admits_null(underlying) || super::is_primitive(underlying))
}

/// Whether a value class's declared underlying type admits `null`: a nullable type, or a type
/// parameter whose bound does (`Result<T>(val a: T)` over `T : Any?`).
fn admits_null(underlying: Ty) -> bool {
    match underlying.kind() {
        TyKind::TyParam(_, bound) => admits_null(bound),
        _ => underlying.admits_null(),
    }
}
