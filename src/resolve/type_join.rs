use std::collections::HashSet;

use crate::assignable::TypeOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeVariance};

use super::semantic_common_supertype_inner;

fn joined_star_projection(star_upper_bound: Ty, left: Ty, right: Ty) -> Option<Ty> {
    (matches!(left, Ty::StarProjection(_)) || matches!(right, Ty::StarProjection(_)))
        .then(|| Ty::star_projection(star_upper_bound))
}

/// Returns the flexible operand when the pair differs only by one top-level PLATFORM wrapper.
///
/// This is intentionally exact. A nested PLATFORM occurrence belongs to the enclosing
/// classifier's own argument join; treating it as an outer flexible pair makes the result depend
/// on branch order and can discard a nested null-assertion obligation.
pub(super) fn exact_platform_flexible_pair(left: Ty, right: Ty) -> Option<Ty> {
    match (left, right) {
        (Ty::PlatformNullable(inner), plain) if *inner == plain => Some(left),
        (plain, Ty::PlatformNullable(inner)) if plain == *inner => Some(right),
        _ => None,
    }
}

pub(super) fn join_type_projection(
    source: &dyn SymbolSource,
    oracle: &dyn TypeOracle,
    declaration_variance: TypeVariance,
    star_upper_bound: Ty,
    left: Ty,
    right: Ty,
    visiting: &mut HashSet<(Ty, Ty)>,
) -> Option<Ty> {
    if left == right {
        return Some(left);
    }
    // Keep the PLATFORM side: its flexibility carries the null-assertion obligation that guarded
    // positions depend on.
    if let Some(platform) = exact_platform_flexible_pair(left, right) {
        return Some(platform);
    }
    // A STAR argument is the unknown one, and `C<*>` is the supertype of every `C<X>` — so a star
    // joined with anything is that star, in any variance position. Destructuring it into
    // `(out, <bound>)` and rejoining loses exactly that: the result comes back as `out Any?`, which
    // says the argument is BOUNDED by `Any?` rather than unknown, and an invariant expectation
    // written `C<*>` then rejects it. kotlinc reports `Resp<*>` where krusty reported
    // `Resp<out Any!>` for the same two arms.
    if let Some(star) = joined_star_projection(star_upper_bound, left, right) {
        return Some(star);
    }
    let projection = |argument| match argument {
        Ty::InProjection(inner) => (true, false, *inner),
        Ty::OutProjection(inner) | Ty::StarProjection(inner) => (false, true, *inner),
        other => (true, true, other),
    };
    let (left_in, left_out, left) = projection(left);
    let (right_in, right_out, right) = projection(right);
    let allows_out = declaration_variance != TypeVariance::In;
    if allows_out && left_out && right_out {
        if visiting.contains(&(left, right)) || visiting.contains(&(right, left)) {
            return Some(Ty::out_projection(star_upper_bound));
        }
        let joined = semantic_common_supertype_inner(source, oracle, left, right, visiting);
        return Some(match (declaration_variance, joined) {
            (TypeVariance::Out, Some(joined)) => joined,
            (_, Some(joined)) => Ty::out_projection(joined),
            (_, None) => Ty::out_projection(star_upper_bound),
        });
    }
    let allows_in = declaration_variance != TypeVariance::Out;
    if allows_in && left_in && right_in {
        let intersection = if crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            oracle,
            left,
            right,
        ) {
            left
        } else if crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            oracle,
            right,
            left,
        ) {
            right
        } else {
            Ty::Nothing
        };
        return Some(if declaration_variance == TypeVariance::In {
            intersection
        } else {
            Ty::in_projection(intersection)
        });
    }
    Some(Ty::out_projection(star_upper_bound))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_platform_pair_keeps_the_flexible_operand_in_both_orders() {
        let plain = Ty::obj("sample/Resp");
        let platform = Ty::platform_nullable(plain);

        assert_eq!(
            exact_platform_flexible_pair(platform, plain),
            Some(platform)
        );
        assert_eq!(
            exact_platform_flexible_pair(plain, platform),
            Some(platform)
        );
    }

    #[test]
    fn nested_platform_and_genuine_nullable_differences_are_not_outer_pairs() {
        let plain_argument = Ty::obj("kotlin/String");
        let platform_argument = Ty::platform_nullable(plain_argument);
        let plain = Ty::obj_args("sample/Box", &[plain_argument]);
        let nested_platform = Ty::obj_args("sample/Box", &[platform_argument]);

        assert_eq!(exact_platform_flexible_pair(plain, nested_platform), None);
        assert_eq!(exact_platform_flexible_pair(nested_platform, plain), None);
        assert_eq!(
            exact_platform_flexible_pair(Ty::nullable(plain), plain),
            None
        );
    }

    #[test]
    fn a_star_join_uses_the_declared_bound_in_both_orders() {
        let declared_bound = Ty::obj("sample/Top");
        let narrower_star = Ty::star_projection(Ty::obj("sample/Leaf"));
        let other_star = Ty::star_projection(Ty::obj("sample/OtherLeaf"));
        let ordinary = Ty::obj("sample/Value");
        let expected = Some(Ty::star_projection(declared_bound));

        assert_eq!(
            joined_star_projection(declared_bound, narrower_star, ordinary),
            expected
        );
        assert_eq!(
            joined_star_projection(declared_bound, ordinary, narrower_star),
            expected
        );
        assert_eq!(
            joined_star_projection(declared_bound, narrower_star, other_star),
            expected
        );
        assert_eq!(
            joined_star_projection(declared_bound, other_star, narrower_star),
            expected
        );
    }
}
