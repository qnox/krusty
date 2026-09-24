//! Keeping an overload alive while its postponed lambdas are still unshaped.
//!
//! Before a lambda argument has a type, overload selection can only ask whether each already-typed
//! argument could still fit a candidate once the call's type variables are inferred. A parameter
//! that mentions a type variable therefore postpones the constraint on that variable, but never on
//! the fixed type constructor around it: `Array<out R>` can infer `R` later, yet no binding of `R`
//! makes it accept a `List<Long>`. Keeping such a candidate alive lets it shape the lambda before
//! real overload selection runs, so a `zip(other) { a, b -> … }` lambda was typed against the array
//! overload and its unbound `R` leaked into the call's result.

use super::*;

impl Checker<'_> {
    /// Whether a typed argument `actual` can still satisfy the parameter `declared`, which mentions
    /// at least one of the call's `formals`, for SOME binding of those formals.
    pub(super) fn postponed_argument_fits(
        &self,
        declared: Ty,
        actual: Ty,
        formals: &[String],
    ) -> bool {
        match declared.non_null() {
            // `() -> T?` can infer `T` later, but it can never accept a known non-function value
            // (`generateSequence(1) { ... }` must not shape `1` as the seed function overload).
            Ty::Fun(_) => actual.non_null().fun_arity().is_some(),
            // A function value may still reach a generic functional interface by SAM conversion;
            // that conversion is judged by final selection, not by the constructor shape.
            _ if actual.non_null().fun_arity().is_some() => true,
            Ty::Obj(..) => {
                let shape = constructor_shape(declared, formals);
                crate::assignable::is_assignable(
                    &crate::assignable::TyCtx::new(),
                    self,
                    actual,
                    shape,
                )
            }
            _ => true,
        }
    }
}

/// `declared` with every type argument that depends on an inferable formal replaced by a star
/// projection, keeping the classifier and nullability fixed.
fn constructor_shape(declared: Ty, formals: &[String]) -> Ty {
    match declared {
        Ty::Nullable(inner) => Ty::nullable(constructor_shape(*inner, formals)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(constructor_shape(*inner, formals)),
        Ty::Obj(name, arguments) => {
            let arguments = arguments
                .iter()
                .map(|&argument| {
                    if ty_mentions_param(argument, formals) {
                        Ty::star_projection(Ty::nullable(Ty::obj_name(crate::types::wk::any())))
                    } else {
                        argument
                    }
                })
                .collect::<Vec<_>>();
            Ty::obj_args_name(name, &arguments)
        }
        other => other,
    }
}
