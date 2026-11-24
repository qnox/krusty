//! Concrete enum classifiers selected by the standard-library reflection intrinsics.
//!
//! `enumValues`, `enumValueOf`, and the zero-argument `enumEntries` become the enum's own
//! statics once the call's result names a concrete classifier. A type parameter stays
//! unrealized until an inline expansion substitutes it.

use crate::fir::{FirClassifierCallable, FirIntrinsic, ResolvedTy};
use crate::libraries::CompilerIntrinsic;
use crate::types::{Ty, TypeName};

/// `enumValues` of a reified type parameter, which only an inline template holds. It stays a
/// compiler operation on that parameter: the builtin declaration has no body to expand, and inline
/// specialization substitutes the enum the call site names.
pub(super) fn reified_parameter_operation<E>(
    intrinsic: Option<CompilerIntrinsic>,
    ret: Ty,
    resolve: impl FnOnce(Ty) -> Result<ResolvedTy, E>,
) -> Result<Option<FirIntrinsic>, E> {
    let Some(CompilerIntrinsic::EnumValues) = intrinsic else {
        return Ok(None);
    };
    let Some(classifier) = ret.array_elem().filter(|ty| ty.non_null().is_ty_param()) else {
        return Ok(None);
    };
    Ok(Some(FirIntrinsic::EnumValues {
        classifier: resolve(classifier)?,
    }))
}

pub(super) fn selected_classifier_call(
    intrinsic: Option<CompilerIntrinsic>,
    ret: Ty,
) -> Option<(TypeName, FirClassifierCallable)> {
    let concrete_classifier = |ty: Ty| {
        (!ty.mentions_ty_param())
            .then(|| ty.kotlin_class_internal())
            .flatten()
    };
    match intrinsic {
        Some(CompilerIntrinsic::EnumValues) => ret
            .array_elem()
            .and_then(concrete_classifier)
            .map(|classifier| (classifier, FirClassifierCallable::EnumValues)),
        Some(CompilerIntrinsic::EnumValueOf) => concrete_classifier(ret)
            .map(|classifier| (classifier, FirClassifierCallable::TopLevelEnumValueOf)),
        Some(CompilerIntrinsic::EnumEntries) => ret
            .non_null()
            .type_args()
            .first()
            .copied()
            .and_then(concrete_classifier)
            .map(|classifier| (classifier, FirClassifierCallable::EnumEntries)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn enum_entries_selects_the_type_argument() {
        let ret = Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj("Color")]);
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumEntries), ret),
            Some((type_name("Color"), FirClassifierCallable::EnumEntries))
        );
    }

    #[test]
    fn nullable_enum_entries_still_selects_the_type_argument() {
        let ret = Ty::nullable(Ty::obj_args(
            "kotlin/enums/EnumEntries",
            &[Ty::obj("Color")],
        ));
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumEntries), ret),
            Some((type_name("Color"), FirClassifierCallable::EnumEntries))
        );
    }

    #[test]
    fn enum_entries_of_a_type_parameter_stays_unrealized() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Enum"));
        let ret = Ty::obj_args("kotlin/enums/EnumEntries", &[parameter]);
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumEntries), ret),
            None
        );
    }

    #[test]
    fn enum_values_selects_the_array_element() {
        let ret = Ty::array(Ty::obj("Color"));
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumValues), ret),
            Some((type_name("Color"), FirClassifierCallable::EnumValues))
        );
    }

    #[test]
    fn enum_value_of_selects_the_return_class() {
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumValueOf), Ty::obj("Color")),
            Some((
                type_name("Color"),
                FirClassifierCallable::TopLevelEnumValueOf
            ))
        );
    }

    #[test]
    fn a_type_parameter_result_stays_unrealized() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Enum"));
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumValueOf), parameter),
            None
        );
        assert_eq!(
            selected_classifier_call(Some(CompilerIntrinsic::EnumValues), Ty::array(parameter)),
            None
        );
    }

    fn resolved(ty: Ty) -> Result<ResolvedTy, ()> {
        ResolvedTy::new(ty).map_err(|_| ())
    }

    #[test]
    fn enum_values_of_a_reified_parameter_is_a_template_operation() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Enum"));
        assert_eq!(
            reified_parameter_operation(
                Some(CompilerIntrinsic::EnumValues),
                Ty::array(parameter),
                resolved
            ),
            Ok(Some(FirIntrinsic::EnumValues {
                classifier: ResolvedTy::new(parameter).unwrap()
            }))
        );
    }

    #[test]
    fn a_concrete_enum_is_no_template_operation() {
        assert_eq!(
            reified_parameter_operation(
                Some(CompilerIntrinsic::EnumValues),
                Ty::array(Ty::obj("Color")),
                resolved
            ),
            Ok(None)
        );
    }
}
