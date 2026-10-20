//! The JVM shape of a member of this module that the backend knows only through checked facts,
//! such as an interface default a class inherits from another file.
//!
//! The declaring file's value-class pass names and erases the member itself. A forwarder emitted
//! elsewhere spells the same member from its declared signature, against the value classes the IR
//! already knows (`IrFile::value_class_underlying_name`), so both sides agree on kotlinc's hashed
//! name and carrier descriptor.

use super::member_names::vc_mangle;
use super::{erase, Under};
use crate::fir::ResolvedInheritedDefault;
use crate::ir::IrFile;
use crate::jvm::bridge_adaptations::ValueClassAdapter;
use crate::types::{Ty, TypeName};

/// kotlinc's JVM name for the member: `base`, with the value-class hash when its declared
/// signature mentions a value class, spelled as the declaring file spells it.
pub(crate) fn module_member_jvm_name(
    ir: &IrFile,
    base: &str,
    params: &[Ty],
    ret: &Ty,
    is_suspend: bool,
) -> String {
    vc_mangle(base, params, ret, ir, false, is_suspend)
}

/// The static a value class realizes an inherited member's box entry with, which kotlinc's
/// value-class lowering names like any member's replacement: the entry's own name when value-class
/// mangling already gave it a hash (`takes-LzhfEcQ`), otherwise `name-impl`. `declared` is the
/// member's name before mangling and `entry` its JVM name on the box.
pub(crate) fn inherited_member_impl_name(declared: &str, entry: &str) -> String {
    if entry == declared {
        format!("{declared}-impl")
    } else {
        entry.to_string()
    }
}

/// The type a declared parameter or result occupies in the member's JVM descriptor: a value class
/// is its carrier, as the declaring file's value-class pass lowers it; any other type is unchanged.
fn module_member_carrier(ir: &IrFile, ty: Ty) -> Ty {
    let mut declarations = Under::default();
    let mut pending = Vec::new();
    crate::ir::referenced_classifiers::collect_classifier_names(ty, &mut pending);
    while let Some(classifier) = pending.pop() {
        if declarations.contains_key(&classifier) {
            continue;
        }
        let Some(underlying) = super::boxed_value_class_underlying(ir, classifier) else {
            continue;
        };
        declarations.insert(
            classifier,
            underlying.scalar_value_repr().unwrap_or(underlying),
        );
        crate::ir::referenced_classifiers::collect_classifier_names(underlying, &mut pending);
    }
    erase(&ty, &declarations)
}

/// The parameter and result types a forwarder to an inherited member declares: the carriers of a
/// member this module declares elsewhere, as its declaring file's value-class pass lowers them, or
/// the recorded physical and semantic types of a dependency's member. kotlinc's forwarders declare,
/// guard and annotate the lowered member, so a value-class parameter or result of this module is
/// its carrier throughout.
pub(crate) struct ForwardedMemberTypes {
    pub(crate) physical_params: Vec<Ty>,
    pub(crate) physical_ret: Ty,
    pub(crate) semantic_params: Vec<Ty>,
    pub(crate) semantic_ret: Ty,
    /// Per parameter, the value class a supertype substitution carries unboxed there: the
    /// realization boxes it into the inherited declaration's erased slot.
    pub(crate) value_class_params: Vec<Option<ValueClassAdapter>>,
    /// The value class a substitution carries unboxed in the result: the realization unboxes the
    /// inherited declaration's erased result.
    pub(crate) value_class_ret: Option<ValueClassAdapter>,
}

/// The physical parameters that correspond to source-semantic parameters of a dependency member.
///
/// A normalized suspend member deliberately retains the classfile's trailing CPS `Continuation`
/// in `physical_params`, while `params` contains only source parameters. The forwarder emitters add
/// that CPS parameter themselves, together with its synthetic identity and nullability, so remove
/// exactly that declared ABI tail here. This is not an arity fallback: every remaining physical
/// parameter must still correspond one-for-one with a semantic parameter, preserving value-class
/// carriers that differ from their source types.
fn dependency_source_physical_params<'a>(
    physical: &'a [Ty],
    semantic: &[Ty],
    suspend: bool,
) -> &'a [Ty] {
    let source = if suspend {
        let (continuation, source) = physical
            .split_last()
            .expect("a normalized suspend member publishes its CPS continuation");
        assert_eq!(
            continuation.non_null().obj_internal(),
            Some(crate::types::wk::continuation()),
            "a normalized suspend member ends its physical parameters with Continuation"
        );
        source
    } else {
        physical
    };
    assert_eq!(
        source.len(),
        semantic.len(),
        "a normalized dependency member publishes one source physical type per semantic parameter"
    );
    source
}

pub(crate) fn forwarded_member_types(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    default: &crate::fir::ResolvedInheritedDefault,
) -> ForwardedMemberTypes {
    let shape = match &default.body {
        crate::fir::InheritedDefaultBody::Module => {
            // A function's descriptor erases a type-parameter parameter to its bound.
            let function = matches!(default.name, crate::fir::InheritedMemberName::Function(_));
            let ret = module_member_carrier(ir, default.result);
            return ForwardedMemberTypes {
                physical_params: default
                    .parameters
                    .iter()
                    .map(|ty| match ty.ty_param_bound() {
                        Some(bound) if function => bound.non_null(),
                        _ => *ty,
                    })
                    .map(|ty| module_member_carrier(ir, ty))
                    .collect(),
                physical_ret: ret,
                semantic_params: default
                    .parameters
                    .iter()
                    .map(|ty| module_member_carrier(ir, *ty))
                    .collect(),
                semantic_ret: ret,
                value_class_params: vec![None; default.parameters.len()],
                value_class_ret: None,
            };
        }
        crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(declaration)
        | crate::fir::InheritedDefaultBody::DependencyHolder(declaration) => callables
            .callable(*declaration)
            .expect("an inherited dependency default has frozen backend facts"),
        crate::fir::InheritedDefaultBody::JavaDefaultMethod => {
            unreachable!("a Java default method has no Kotlin forwarder")
        }
    };
    let physical_params = dependency_source_physical_params(
        &shape.physical_params,
        &default.parameters,
        default.suspend,
    );
    ForwardedMemberTypes {
        physical_params: physical_params.to_vec(),
        physical_ret: shape.physical_ret,
        semantic_params: default.parameters.to_vec(),
        semantic_ret: default.result,
        value_class_params: vec![None; default.parameters.len()],
        value_class_ret: None,
    }
}

/// The value class the classifier's applied supertype substitutes where the declaration has
/// another type (`T` of `I<T>` taken as `Z` for `I<Z>`).
fn substituted_value_class(ir: &IrFile, declared: &Ty, applied: &Ty) -> Option<TypeName> {
    if declared == applied {
        return None;
    }
    applied
        .non_null()
        .obj_internal()
        .filter(|&classifier| ir.is_value_class_name(classifier))
}

/// Whether the classifier's applied supertype substitutes a value class into the inherited
/// member's signature. kotlinc's fake override then has a value-class signature of its own: it is
/// named by that signature (`foo-<hash>`) and takes the carriers, and the declaration's own JVM
/// name and erased descriptor become a bridge to it. A suspend member keeps its declared CPS
/// shape.
pub(crate) fn substitutes_value_class(ir: &IrFile, default: &ResolvedInheritedDefault) -> bool {
    !default.suspend
        && default
            .parameters
            .iter()
            .zip(default.applied_parameters.iter())
            .chain(std::iter::once((&default.result, &default.applied_result)))
            .any(|(declared, applied)| substituted_value_class(ir, declared, applied).is_some())
}

/// The parameter and result types the classifier's own realization of an inherited member
/// declares: [`forwarded_member_types`] where the applied supertype leaves a position as declared,
/// and the substituted type where it does not (`f(value: String): String` for `I<String>`). A
/// substituted result whose declaration erases to a reference keeps a reference: kotlinc boxes it
/// (`Integer f(int)` for `I<Int>`) so it still overrides the declaration's erased result. A
/// substituted value class is its carrier (`foo-<hash>(I)` for `I<Z>`), result included: the
/// realization boxes and unboxes it across the declaration's erased slots.
pub(crate) fn specialized_member_types(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    default: &ResolvedInheritedDefault,
) -> ForwardedMemberTypes {
    let declared = forwarded_member_types(ir, callables, default);
    let module = matches!(default.body, crate::fir::InheritedDefaultBody::Module);
    let semantic = |ty: Ty| {
        if module {
            module_member_carrier(ir, ty)
        } else {
            ty
        }
    };
    let physical = |ty: Ty| match ty.ty_param_bound() {
        Some(bound) => bound.non_null(),
        None => ty,
    };
    // A substituted value class takes its carrier whichever module declares the member; the
    // carrier of a boxed representation (`Z?` over a primitive) is the box itself, which crosses
    // the erased slot as it is.
    let carried = |declared: &Ty, applied: Ty| {
        let value_class = substituted_value_class(ir, declared, &applied)?;
        let carrier = module_member_carrier(ir, applied);
        let adapter = (carrier.non_null().obj_internal() != Some(value_class))
            .then(|| ValueClassAdapter::new(value_class, applied.is_nullable()));
        Some((carrier, adapter))
    };
    let mut types = ForwardedMemberTypes {
        physical_params: Vec::with_capacity(declared.physical_params.len()),
        physical_ret: declared.physical_ret,
        semantic_params: Vec::with_capacity(declared.semantic_params.len()),
        semantic_ret: declared.semantic_ret,
        value_class_params: Vec::with_capacity(declared.semantic_params.len()),
        value_class_ret: None,
    };
    for (index, (declared_ty, applied)) in default
        .parameters
        .iter()
        .zip(default.applied_parameters.iter())
        .enumerate()
    {
        if declared_ty == applied {
            types.physical_params.push(declared.physical_params[index]);
            types.semantic_params.push(declared.semantic_params[index]);
            types.value_class_params.push(None);
        } else if let Some((carrier, adapter)) = carried(declared_ty, *applied) {
            types.physical_params.push(carrier);
            types.semantic_params.push(carrier);
            types.value_class_params.push(adapter);
        } else {
            let applied = semantic(*applied);
            types.physical_params.push(physical(applied));
            types.semantic_params.push(applied);
            types.value_class_params.push(None);
        }
    }
    if let Some((carrier, adapter)) = carried(&default.result, default.applied_result) {
        types.physical_ret = carrier;
        types.semantic_ret = carrier;
        types.value_class_ret = adapter;
    } else if default.result != default.applied_result {
        let applied = semantic(default.applied_result);
        let erased = crate::jvm::method_descriptors::jvm_declared_ty(&declared.physical_ret);
        let realized = physical(applied);
        let boxed = (erased.is_reference()
            && !crate::jvm::method_descriptors::jvm_declared_ty(&realized).is_reference())
        .then(|| crate::jvm::jvm_class_map::wrapper_type_name(realized))
        .flatten()
        .map(Ty::obj_name);
        types.physical_ret = boxed.unwrap_or(realized);
        types.semantic_ret = boxed.unwrap_or(applied);
    }
    types
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suspend_dependency_keeps_its_source_carrier_and_drops_only_the_cps_tail() {
        let semantic = Ty::obj("fixture/Ticket");
        let continuation = Ty::obj("kotlin/coroutines/Continuation");
        let physical = [Ty::Int, continuation];
        let source = dependency_source_physical_params(&physical, &[semantic], true);

        assert_eq!(source, &[Ty::Int]);
    }

    #[test]
    #[should_panic(expected = "publishes one source physical type per semantic parameter")]
    fn a_dependency_member_without_a_complete_physical_shape_is_rejected() {
        dependency_source_physical_params(&[], &[Ty::Int], false);
    }
}
