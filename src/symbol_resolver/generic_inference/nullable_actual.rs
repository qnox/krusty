use crate::types::Ty;

/// Convert an actual type into the constraint contributed through a nullable formal position.
///
/// A covariant/star projection contributes its readable upper bound: `List<*>` reads as
/// `List<out Any?>`, and against `Iterable<T?>` the formal's `?` absorbs that nullability, leaving
/// `T = Any`. A contravariant projection must remain captured; opening `in String` to `String`
/// would invent a readable `String` result where Kotlin exposes only `Any?`.
pub(super) fn nullable_generic_actual(actual: Ty) -> Ty {
    let actual = match actual {
        Ty::OutProjection(inner) | Ty::StarProjection(inner) => *inner,
        actual => actual,
    };
    if actual == Ty::Null || matches!(actual, Ty::Nullable(inner) if *inner == Ty::Nothing) {
        Ty::Nothing
    } else {
        actual.non_null()
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
    fn null_only_actuals_constrain_the_inner_formal_to_nothing() {
        assert_eq!(nullable_generic_actual(Ty::Null), Ty::Nothing);
        assert_eq!(
            nullable_generic_actual(Ty::nullable(Ty::Nothing)),
            Ty::Nothing
        );
    }
}
