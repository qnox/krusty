//! Which `@Metadata` function a JVM method realizes.
//!
//! kotlinc omits a function's `method_signature` when the descriptor is the default it computes, so
//! a reader pairs each metadata function with its method by aligning the metadata parameter types
//! with the descriptor. A type parameter aligns as the bound it erases to, found among the
//! function's own type parameters and those of its enclosing classifiers.

use super::value_class_erasure::{
    meta_descriptor_position, meta_param_exact, metadata_carrier_matches, metadata_parameter_class,
    metadata_value_class_bound_carrier, MetadataFormals,
};
use super::{meta_param_compat, ClassMeta};
use crate::types::{type_name, Ty, TyKind, TypeName};

/// Whether metadata callable `c` corresponds to a JVM method with these descriptor parameter types. An
/// EXTENSION's receiver — a separate attribute, emitted as the leading JVM parameter — must match, then
/// the value parameters align in order. Returns `(kept-param end, exact-match count)` — `end` is the count
/// of SOURCE parameters (where the synthetic tail — a `suspend` Continuation, a `$default` mask — begins),
/// and `exact` counts the value params matching by EQUAL erased descriptor (not through the loose
/// type-variable rule), so the caller prefers the most-specific overload (`plusAssign(element: T)` binds
/// the `Object` descriptor, `plusAssign(elements: Iterable)` the `Iterable` one).
pub(super) fn function_parameter_erasure_matches(
    signature: &crate::types::FnSig,
    composable: bool,
    descriptor: Ty,
    classifier_arity: &dyn Fn(Ty) -> Option<usize>,
) -> bool {
    // A suspend function value implements Function(N+1): the additional JVM argument is its
    // Continuation. A receiver is already the first semantic parameter, so it needs no separate
    // adjustment (`suspend R.() -> T` has one semantic parameter and erases to Function2).
    let semantic = signature.params.len() + usize::from(signature.suspend);
    // A composable function value takes the composer and its `$changed` words after its own
    // parameters. The Compose compiler counts one changed word per ten slots of the parameters
    // and the composer (`changedParamCount(parameters, 1)`), so ten parameters need two.
    let expected = if composable {
        semantic + 1 + (semantic + 1).div_ceil(10)
    } else {
        semantic
    };
    // `Function22` is the last arity with a class of its own; wider function values erase to
    // `FunctionN`, whose `invoke` takes a vararg.
    if expected > MAX_FUNCTION_CLASS_ARITY {
        return descriptor.non_null().obj_internal()
            == Some(type_name("kotlin/jvm/functions/FunctionN"));
    }
    classifier_arity(descriptor) == Some(expected)
}

/// The largest `N` for which `kotlin.jvm.functions.FunctionN` is a distinct class.
const MAX_FUNCTION_CLASS_ARITY: usize = 22;

/// Descriptor slot of the metadata parameter at `index`, where `parameters()` yields the context
/// parameters first and the value parameters after them. Kotlin signs a context extension
/// `(contexts…, receiver, values…)`, so only parameters PAST the context prefix are pushed along by
/// the receiver.
/// JVM carrier of a metadata type parameter whose class bound is a value class.
///
/// The signature parameter stays the type parameter, so it has no classifier of its own. A value-class
/// class bound (`UInt` ahead of `Comparable<UInt>`) is what the descriptor erases to. Any other bound
/// keeps the ordinary reference alignment.
fn meta_callable_aligns(
    f: &crate::jvm::metadata::MetaFn,
    enclosing: &[crate::jvm::metadata::ScopedTypeParameter],
    desc_params: &[Ty],
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    classifier_arity: &dyn Fn(Ty) -> Option<usize>,
    is_interface: &dyn Fn(TypeName) -> bool,
) -> Option<(usize, usize)> {
    let extension = f.is_extension();
    let context_count = f.context_count();
    // Context parameters LEAD the JVM descriptor, ahead of the extension receiver; metadata keeps
    // them out of `value_parameter` (field 13 instead), so the aligned descriptor span is
    // context + receiver + value params and the two groups are not contiguous.
    let end = usize::from(extension) + context_count + f.value_params.len();
    if end > desc_params.len() {
        return None;
    }
    let aligned =
        |index: usize| &desc_params[meta_descriptor_position(index, context_count, extension)];
    let receiver_ok = !extension || {
        let receiver = &desc_params[context_count];
        match f.receiver_class {
            Some(rc) => meta_param_compat(Some(rc), false, receiver, value_underlying),
            None => receiver.is_reference(),
        }
    };
    if !receiver_ok
        || !f
            .parameters()
            .enumerate()
            .map(|(index, m)| ((index, m), aligned(index)))
            .all(|((index, m), d)| {
                let signature_parameter = f
                    .generic_sig
                    .as_ref()
                    .and_then(|signature| signature.params.get(index))
                    .copied();
                if let Some(TyKind::Fun(signature)) =
                    signature_parameter.map(|ty| ty.non_null().kind())
                {
                    return function_parameter_erasure_matches(
                        signature,
                        m.composable_fun(),
                        *d,
                        classifier_arity,
                    );
                }
                if let Some(carrier) = signature_parameter
                    .zip(f.generic_sig.as_ref())
                    .filter(|(parameter, _)| parameter.non_null().is_ty_param())
                    .and_then(|(parameter, signature)| {
                        metadata_value_class_bound_carrier(
                            parameter,
                            MetadataFormals {
                                callable: signature,
                                enclosing,
                            },
                            is_interface,
                            value_underlying,
                        )
                    })
                {
                    return metadata_carrier_matches(carrier, d, false, value_underlying);
                }
                let class =
                    metadata_parameter_class(signature_parameter, f, enclosing, is_interface);
                if class.is_none()
                    && signature_parameter
                        .is_some_and(|parameter| parameter.non_null().is_ty_param())
                {
                    // A type parameter no declaration in scope declares has no erasure to pair.
                    return false;
                }
                meta_param_compat(class.or(m.ty), m.nullable(), d, value_underlying)
            })
    {
        return None;
    }
    let exact = f
        .parameters()
        .enumerate()
        .map(|(index, m)| ((index, m), aligned(index)))
        .filter(|((index, m), d)| {
            let signature_parameter = f
                .generic_sig
                .as_ref()
                .and_then(|signature| signature.params.get(*index))
                .copied();
            if let Some(TyKind::Fun(signature)) = signature_parameter.map(|ty| ty.non_null().kind())
            {
                return function_parameter_erasure_matches(
                    signature,
                    m.composable_fun(),
                    **d,
                    classifier_arity,
                );
            }
            if let Some(carrier) = signature_parameter
                .zip(f.generic_sig.as_ref())
                .filter(|(parameter, _)| parameter.non_null().is_ty_param())
                .and_then(|(parameter, signature)| {
                    metadata_value_class_bound_carrier(
                        parameter,
                        MetadataFormals {
                            callable: signature,
                            enclosing,
                        },
                        is_interface,
                        value_underlying,
                    )
                })
            {
                return metadata_carrier_matches(carrier, d, true, value_underlying);
            }
            let class =
                metadata_parameter_class(signature_parameter, f, enclosing, is_interface).or(m.ty);
            meta_param_exact(class, m.nullable(), d, value_underlying)
        })
        .count();
    Some((end, exact))
}

/// Pick the metadata function whose signature corresponds to the JVM method with `desc_params`, returning
/// `(kept-param end, index into `meta.fns`)`. Disambiguates OVERLOADS sharing a JVM name
/// (`any()` vs `any(predicate)`, `IntArray.any` vs `CharArray.any`) by receiver + value-parameter match,
/// preferring the longest alignment.
pub(super) fn aligned_meta_index(
    meta: &ClassMeta,
    fn_name: &str,
    desc_params: &[Ty],
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    classifier_arity: &dyn Fn(Ty) -> Option<usize>,
    is_interface: &dyn Fn(TypeName) -> bool,
) -> Option<(usize, usize)> {
    meta.fns_named(fn_name)
        .filter_map(|i| {
            let f = meta.fn_at(i as usize);
            let alignment = meta_callable_aligns(
                f,
                &meta.enclosing_formals,
                desc_params,
                value_underlying,
                classifier_arity,
                is_interface,
            );
            crate::trace_compiler!(
                "resolve",
                "metadata alignment {fn_name} desc={desc_params:?} candidate_value_classes={:?} candidate_signature={:?} alignment={alignment:?}",
                f.value_params.iter().map(|parameter| parameter.ty).collect::<Vec<_>>(),
                f.generic_sig.as_ref().map(|signature| &signature.params),
            );
            let (end, exact) = alignment?;
            let ret_match = f.ret_class.is_some_and(|rc| {
                meta_param_compat(Some(rc), f.ret_nullable(), desc_ret, value_underlying)
            });
            Some((end, exact, ret_match, i))
        })
        .max_by_key(|(end, exact, ret_match, _)| (*end, *exact, *ret_match))
        .map(|(end, _, _, i)| (end, i as usize))
}

pub(super) fn aligned_meta_callable<'a>(
    meta: &'a ClassMeta,
    fn_name: &str,
    desc_params: &[Ty],
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    classifier_arity: &dyn Fn(Ty) -> Option<usize>,
    is_interface: &dyn Fn(TypeName) -> bool,
) -> Option<(usize, &'a crate::jvm::metadata::MetaFn)> {
    aligned_meta_index(
        meta,
        fn_name,
        desc_params,
        desc_ret,
        value_underlying,
        classifier_arity,
        is_interface,
    )
    .map(|(end, i)| (end, meta.fn_at(i)))
}

#[cfg(test)]
mod tests {
    use super::function_parameter_erasure_matches;
    use crate::types::{Ty, TyKind};

    fn arity(ty: Ty) -> Option<usize> {
        ty.fun_arity().map(usize::from)
    }

    /// The erased `FunctionN` value of a function type with `arguments` parameters.
    fn erased(arguments: usize) -> Ty {
        if arguments > 22 {
            return Ty::obj("kotlin/jvm/functions/FunctionN");
        }
        Ty::fun(
            vec![Ty::obj("kotlin/Any"); arguments],
            Ty::obj("kotlin/Any"),
        )
    }

    fn matches(parameters: usize, composable: bool, erased_arguments: usize) -> bool {
        let TyKind::Fun(signature) = Ty::fun(vec![Ty::Int; parameters], Ty::Unit).kind() else {
            unreachable!()
        };
        function_parameter_erasure_matches(signature, composable, erased(erased_arguments), &arity)
    }

    #[test]
    fn suspend_receiver_function_metadata_matches_its_function_interface_erasure() {
        let TyKind::Fun(metadata) = Ty::fun_with_shape(
            vec![Ty::obj("kotlin/sequences/SequenceScope")],
            Ty::Unit,
            0,
            true,
            true,
        )
        .kind() else {
            unreachable!()
        };

        assert!(function_parameter_erasure_matches(
            metadata,
            false,
            erased(2),
            &arity,
        ));
    }

    /// Each row is the `Function` kotlinc 2.4.21's Compose plugin erases `@Composable (Int × n) ->
    /// Unit` to: `n` parameters, the composer, and one changed word per ten slots of the
    /// parameters and the composer.
    #[test]
    fn composable_function_metadata_matches_the_producers_erasure() {
        for (parameters, erased_arguments) in [
            (0, 2),
            (1, 3),
            (8, 10),
            (9, 11),
            (10, 13),
            (11, 14),
            (18, 21),
            (19, 22),
            (20, 24),
        ] {
            assert!(
                matches(parameters, true, erased_arguments),
                "{parameters} parameters erase to Function{erased_arguments}"
            );
            // Neither a neighbouring width nor the plain function value of the same parameters
            // is the composable erasure. Past `Function22` every width is `FunctionN`, so only
            // the widest class below it is a wrong width there.
            assert!(
                !matches(parameters, true, (erased_arguments - 1).min(22)) || erased_arguments > 23
            );
            assert!(!matches(parameters, true, erased_arguments + 1) || erased_arguments > 22);
            assert!(!matches(parameters, false, erased_arguments));
        }
    }

    /// A receiver counts as one of the parameters: `@Composable Int.(Int × n) -> Unit` is
    /// `Function11`, `Function13` and `Function14` for `n` of 8, 9 and 10.
    #[test]
    fn composable_receiver_function_counts_its_receiver_slot() {
        let TyKind::Fun(receiver) =
            Ty::fun_with_shape(vec![Ty::Int; 10], Ty::Unit, 0, true, false).kind()
        else {
            unreachable!()
        };

        assert!(function_parameter_erasure_matches(
            receiver,
            true,
            erased(13),
            &arity
        ));
        assert!(!function_parameter_erasure_matches(
            receiver,
            true,
            erased(12),
            &arity
        ));
    }
}
