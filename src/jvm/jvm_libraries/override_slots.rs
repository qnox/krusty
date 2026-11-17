//! JVM publication of provider-normalized override slots.
//!
//! Erasure, mapped classifier names, and value-class exclusions stay at this provider boundary.
//! Common hierarchy code consumes only [`ProviderOverrideSlot`] identity and ownership.

use super::JvmLibraries;
use crate::libraries::{ProviderOverrideSlot, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::TyKind;
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, PartialEq, Eq)]
struct JvmOverrideKey {
    effective_name: String,
    inputs: Box<[Ty]>,
}

fn normalized_input(libraries: &JvmLibraries, ty: Ty) -> Ty {
    let ty = libraries.library_value_form(ty);
    match ty.kind() {
        _ if ty.is_unsigned() => ty.scalar_value_repr().unwrap(),
        TyKind::STRING => Ty::obj("kotlin/String"),
        TyKind::Obj(name, arguments) if name == crate::types::wk::array() => {
            let element = arguments
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::obj_args("kotlin/Array", &[normalized_input(libraries, element)])
        }
        TyKind::Obj(name, _) => Ty::obj_name(name),
        TyKind::Null | TyKind::Nothing => Ty::obj("kotlin/Any"),
        // An unresolved frontend type is not a valid provider declaration fact. Preserve the
        // sentinel in the opaque key so it cannot accidentally match a real `Any` slot.
        TyKind::Error => Ty::Error,
        TyKind::Fun(signature) => Ty::obj(&crate::jvm::names::function_interface_internal_name(
            signature.params.len() + usize::from(signature.suspend),
        )),
        TyKind::Nullable(inner) => match inner {
            Ty::UInt => Ty::obj("kotlin/UInt"),
            Ty::ULong => Ty::obj("kotlin/ULong"),
            other if other.boxed_ref().is_some() => {
                normalized_input(libraries, other.boxed_ref().unwrap_or(other))
            }
            other => normalized_input(libraries, other),
        },
        TyKind::PlatformNullable(inner) => normalized_input(libraries, Ty::nullable(inner)),
        TyKind::MutabilityFlexible(inner) => normalized_input(libraries, inner),
        TyKind::TyParam(_, bound) | TyKind::DefinitelyNotNull(bound) => {
            normalized_input(libraries, bound)
        }
        _ => ty,
    }
}

/// A Java declaration's override inputs ([`crate::libraries::FunctionInfo::override_inputs`]).
/// kotlinc's Java override checker matches a raw Java input by its JVM erasure rather than by its
/// Kotlin type, so a raw `AtomicReference` overrides a Kotlin `AtomicReference<Int>`: the slot is
/// the Kotlin classifier that erases to the raw input's class. `None` when no input is raw.
pub(super) fn java_override_inputs(inputs: &[Ty]) -> Option<Box<[Option<TypeName>]>> {
    let slots = inputs
        .iter()
        .map(|input| raw_input_classifier(*input))
        .collect::<Box<[_]>>();
    slots.iter().any(Option::is_some).then_some(slots)
}

fn raw_input_classifier(ty: Ty) -> Option<TypeName> {
    match ty.kind() {
        TyKind::Obj(name, arguments)
            if !arguments.is_empty()
                && arguments
                    .iter()
                    .all(|argument| matches!(argument.kind(), TyKind::RawProjection(_))) =>
        {
            Some(crate::jvm::jvm_class_map::one_way_java_override_classifier(name).unwrap_or(name))
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner) => raw_input_classifier(inner),
        _ => None,
    }
}

pub(super) fn slot(
    libraries: &JvmLibraries,
    owner: TypeName,
    effective_name: String,
    inputs: impl IntoIterator<Item = Ty>,
) -> ProviderOverrideSlot {
    ProviderOverrideSlot::new(
        owner,
        JvmOverrideKey {
            effective_name,
            inputs: inputs
                .into_iter()
                .map(|input| normalized_input(libraries, input))
                .collect(),
        },
    )
}

pub(super) fn result_refines(
    libraries: &JvmLibraries,
    source: &dyn SymbolSource,
    implementation: Ty,
    declaration: Ty,
) -> bool {
    crate::symbol_resolver::resolution_subtype(
        source,
        normalized_input(libraries, implementation),
        normalized_input(libraries, declaration),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn libraries() -> Option<JvmLibraries> {
        let stdlib = crate::toolchain::stdlib_jar()?;
        JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .ok()
    }

    #[test]
    fn semantic_and_physical_function_inputs_share_the_numbered_carrier() {
        let Some(libraries) = libraries() else {
            return;
        };
        assert_eq!(
            normalized_input(&libraries, Ty::fun(vec![Ty::obj("kotlin/Any")], Ty::Unit)),
            normalized_input(&libraries, Ty::obj("kotlin/jvm/functions/Function1"))
        );
    }

    #[test]
    fn semantic_and_physical_high_arity_inputs_share_function_n() {
        let Some(libraries) = libraries() else {
            return;
        };
        assert_eq!(
            normalized_input(
                &libraries,
                Ty::fun(vec![Ty::obj("kotlin/Any"); 23], Ty::Unit)
            ),
            normalized_input(&libraries, Ty::obj("kotlin/jvm/functions/FunctionN"))
        );
    }

    #[test]
    fn an_unresolved_input_never_matches_a_real_any_slot() {
        let Some(libraries) = libraries() else {
            return;
        };
        assert_ne!(
            normalized_input(&libraries, Ty::Error),
            normalized_input(&libraries, Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn only_a_raw_java_input_overrides_through_the_classifier_it_erases_to() {
        let raw = |internal| {
            Ty::platform_nullable(Ty::obj_args(
                internal,
                &[Ty::raw_projection(Ty::platform_nullable(Ty::obj(
                    "kotlin/Any",
                )))],
            ))
        };
        let atomic = raw("java/util/concurrent/atomic/AtomicReference");
        let list = raw("java/util/ArrayList");
        assert_eq!(
            java_override_inputs(&[Ty::Int, atomic, list]).as_deref(),
            Some(
                &[
                    None,
                    Some(crate::types::type_name(
                        "kotlin/concurrent/atomics/AtomicReference"
                    )),
                    Some(crate::types::type_name("java/util/ArrayList")),
                ][..]
            )
        );
        // A non-generic Java input is never raw, so `AtomicInteger` keeps its Kotlin type and does
        // not override a Kotlin `AtomicInt`.
        assert_eq!(
            java_override_inputs(&[Ty::platform_nullable(Ty::obj(
                "java/util/concurrent/atomic/AtomicInteger"
            ))]),
            None
        );
    }
}
