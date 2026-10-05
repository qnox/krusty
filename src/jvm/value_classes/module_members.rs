//! The JVM shape of a member of this module that the backend knows only through checked facts,
//! such as an interface default a class inherits from another file.
//!
//! The declaring file's value-class pass names and erases the member itself. A forwarder emitted
//! elsewhere spells the same member from its declared signature, against the value classes the IR
//! already knows (`IrFile::value_class_underlying_name`), so both sides agree on kotlinc's hashed
//! name and carrier descriptor.

use super::member_names::vc_mangle;
use super::{erase, Under};
use crate::ir::IrFile;
use crate::types::Ty;

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
    let mut declarations = Under::new();
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
            };
        }
        crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(shape)
        | crate::fir::InheritedDefaultBody::DependencyHolder(shape, _) => shape,
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
    }
}

/// The parameter and result types the classifier's own realization of an inherited member
/// declares: [`forwarded_member_types`] where the applied supertype leaves a position as declared,
/// and the substituted type where it does not (`f(value: String): String` for `I<String>`). A
/// substituted result whose declaration erases to a reference keeps a reference: kotlinc boxes it
/// (`Integer f(int)` for `I<Int>`) so it still overrides the declaration's erased result. A
/// value-class substitute keeps the declared shape (a recorded gap: kotlinc writes a mangled typed
/// forwarder).
pub(crate) fn specialized_member_types(
    ir: &IrFile,
    default: &crate::fir::ResolvedInheritedDefault,
) -> ForwardedMemberTypes {
    let declared = forwarded_member_types(ir, default);
    // A value-class substitute (`I<Z>`) keeps the declaration's erased shape: kotlinc's typed
    // forwarder would take the mangled carrier signature, which this realization does not model.
    let substitutes_value_class = default
        .parameters
        .iter()
        .zip(default.applied_parameters.iter())
        .chain(std::iter::once((&default.result, &default.applied_result)))
        .filter(|(declared, applied)| declared != applied)
        .any(|(_, applied)| {
            applied
                .non_null()
                .obj_internal()
                .is_some_and(|classifier| ir.is_value_class_name(classifier))
        });
    if substitutes_value_class {
        return declared;
    }
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
    let mut types = ForwardedMemberTypes {
        physical_params: Vec::with_capacity(declared.physical_params.len()),
        physical_ret: declared.physical_ret,
        semantic_params: Vec::with_capacity(declared.semantic_params.len()),
        semantic_ret: declared.semantic_ret,
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
        } else {
            let applied = semantic(*applied);
            types.physical_params.push(physical(applied));
            types.semantic_params.push(applied);
        }
    }
    if default.result != default.applied_result {
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

    fn dependency_member(
        physical_params: &[Ty],
        params: &[Ty],
        suspend: bool,
    ) -> crate::fir::ResolvedInheritedDefault {
        let api = crate::types::type_name("fixture/Api");
        crate::fir::ResolvedInheritedDefault {
            name: crate::fir::InheritedMemberName::Function("run".into()),
            function: None,
            declaring_interface: api,
            dispatch_interface: api,
            parameters: params.into(),
            parameter_identities: Box::new([]),
            result: Ty::Unit,
            applied_parameters: params.into(),
            applied_result: Ty::Unit,
            suspend,
            vararg: false,
            body: crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(
                crate::fir::DependencyMemberShape {
                    physical_name: None,
                    physical_params: physical_params.into(),
                    physical_ret: Ty::Unit,
                },
            ),
        }
    }

    #[test]
    fn a_suspend_dependency_keeps_its_source_carrier_and_drops_only_the_cps_tail() {
        let semantic = Ty::obj("fixture/Ticket");
        let continuation = Ty::obj("kotlin/coroutines/Continuation");
        let member = dependency_member(&[Ty::Int, continuation], &[semantic], true);

        let types = forwarded_member_types(&IrFile::default(), &member);

        assert_eq!(types.physical_params, vec![Ty::Int]);
        assert_eq!(types.semantic_params, vec![semantic]);
    }

    #[test]
    #[should_panic(expected = "publishes one source physical type per semantic parameter")]
    fn a_dependency_member_without_a_complete_physical_shape_is_rejected() {
        let member = dependency_member(&[], &[Ty::Int], false);

        forwarded_member_types(&IrFile::default(), &member);
    }
}
