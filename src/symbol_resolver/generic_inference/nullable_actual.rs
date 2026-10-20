use crate::types::{Ty, TyKind};

/// Convert an actual type into the constraint contributed through a nullable formal position.
///
/// A covariant/star projection contributes its readable upper bound: `List<*>` reads as
/// `List<out Any?>`, and against `Iterable<T?>` the formal's `?` absorbs that nullability, leaving
/// `T = Any`. A contravariant projection must remain captured; opening `in String` to `String`
/// would invent a readable `String` result where Kotlin exposes only `Any?`.
pub(super) fn nullable_generic_actual(actual: Ty) -> Ty {
    let actual = match actual.kind() {
        TyKind::OutProjection(inner) | TyKind::StarProjection(inner) => inner,
        _ => actual,
    };
    if actual == Ty::Null
        || matches!(actual.kind(), TyKind::Nullable(inner) if inner == Ty::Nothing)
    {
        Ty::Nothing
    } else {
        // A nullable formal contributes the definitely-non-null form. `T : Any?` becomes `T & Any`,
        // which satisfies `R : Any`; a parameter already bounded by non-null `Any` stays that
        // parameter. Substituting the intersection back through `R?` reopens the original `T?`.
        actual.contributed_through_nullable_formal()
    }
}

/// Read one whole value argument against a parameter that is exactly a flexible type variable.
///
/// Kotlin reduces `Foo? <: T!` to `Foo? <: T`, so a nullable argument fixes `T` to that nullable
/// type instead of contributing the definitely-non-null lower face used by an ordinary flexible
/// shape. Nested flexible variables remain part of their enclosing structural constraint and do
/// not enter this root-only rule.
pub(super) fn flexible_variable_argument(shape: Ty, actual: Ty) -> Option<(Ty, Ty)> {
    let TyKind::PlatformNullable(variable @ Ty::TyParam(..)) = shape.kind() else {
        return None;
    };
    match actual.kind() {
        TyKind::Null => Some((variable, Ty::nullable(Ty::Nothing))),
        TyKind::Nullable(_) => Some((variable, actual)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullable_formal_reads_covariant_projections_but_preserves_contravariant_capture() {
        let any = Ty::obj("kotlin/Any");
        let number = Ty::obj("kotlin/Number");
        let string = Ty::obj("kotlin/String");

        assert_eq!(
            nullable_generic_actual(Ty::star_projection(Ty::nullable(any))),
            any
        );
        assert_eq!(
            nullable_generic_actual(Ty::out_projection(Ty::nullable(number))),
            number
        );
        assert_eq!(
            nullable_generic_actual(Ty::in_projection(string)),
            Ty::in_projection(string)
        );
    }

    #[test]
    fn a_nullable_formal_contributes_the_definitely_non_null_type_parameter() {
        let nullable_caller = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let non_null_caller = Ty::ty_param("T", Ty::obj("kotlin/Any"));

        assert_eq!(
            nullable_generic_actual(nullable_caller),
            nullable_caller.contributed_through_nullable_formal()
        );
        assert!(matches!(
            nullable_generic_actual(nullable_caller).kind(),
            TyKind::DefinitelyNotNull(_)
        ));
        assert_eq!(nullable_generic_actual(non_null_caller), non_null_caller);
        assert_eq!(
            non_null_caller.ty_param_bound(),
            Some(Ty::obj("kotlin/Any"))
        );
    }

    #[test]
    fn null_only_actuals_constrain_the_inner_formal_to_nothing() {
        assert_eq!(nullable_generic_actual(Ty::Null), Ty::Nothing);
        assert_eq!(
            nullable_generic_actual(Ty::nullable(Ty::Nothing)),
            Ty::Nothing
        );
    }

    #[test]
    fn a_nullable_argument_fixes_a_root_flexible_variable_as_nullable() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let shape = Ty::platform_nullable(variable);
        let nullable = Ty::nullable(Ty::String);

        assert_eq!(
            flexible_variable_argument(shape, nullable),
            Some((variable, nullable))
        );
        assert_eq!(
            flexible_variable_argument(shape, Ty::Null),
            Some((variable, Ty::nullable(Ty::Nothing)))
        );
        assert_eq!(flexible_variable_argument(shape, Ty::String), None);
    }
}
