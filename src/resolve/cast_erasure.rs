//! Whether a runtime type test can check its whole target (FIR's `isCastErased`).
//!
//! The JVM checks only a classifier, so `x is C<A>` is valid only when the operand's static type
//! already proves the rest of `C<A>`. kotlinc reports CANNOT_CHECK_FOR_ERASED otherwise. The
//! operand's type is the one FIR's `FirCastOperatorsChecker` uses: the declared type of an operand
//! that is not smart cast, and the intersection of everything known about one that is. A smart cast
//! that an earlier `as` produced therefore keeps both the declared type and the cast type.

use crate::assignable::{is_subtype, TyCtx};
use crate::types::Ty;

use super::{Checker, CheckerScope};
use crate::ast::ExprId;

impl Checker<'_> {
    /// The type FIR tests a type operator's operand at: the smart-cast intersection when flow has
    /// narrowed a stable path, else the operand's own type.
    pub(super) fn type_test_operand_ty(
        &self,
        scope: &CheckerScope<'_>,
        operand: ExprId,
        operand_ty: Ty,
    ) -> Ty {
        let Some(path) = self.expr_access_path(operand) else {
            return operand_ty;
        };
        let Some(declared) = self.stable_path_ty(scope, &path, self.span(operand)) else {
            return operand_ty;
        };
        // Smart-cast facts accumulate: a nested proof (`f is B` inside a region where an earlier
        // `f as A` holds) adds to the outer facts rather than replacing them. A write to the path
        // drops its facts from every frame, so each fact still on the chain holds here.
        let mut known = scope
            .ancestors()
            .flat_map(|rung| rung.intersection_narrowing(&path))
            .collect::<Vec<_>>();
        if known.is_empty() && operand_ty == declared {
            return operand_ty;
        }
        known.push(operand_ty);
        let nullable = operand_ty.is_nullable();
        let mut parts = vec![declared.non_null()];
        for part in known {
            let part = part.non_null();
            if part != Ty::Error && !parts.contains(&part) {
                parts.push(part);
            }
        }
        // FIR's intersection keeps only the most specific constituents.
        let context = TyCtx::new();
        let minimal = parts
            .iter()
            .copied()
            .filter(|&part| {
                !parts.iter().any(|&other| {
                    other != part
                        && is_subtype(&context, self, other, part)
                        && !is_subtype(&context, self, part, other)
                })
            })
            .collect::<Vec<_>>();
        let intersection = if minimal.len() == 1 {
            minimal[0]
        } else {
            Ty::intersection(&minimal)
        };
        if nullable {
            Ty::nullable(intersection)
        } else {
            intersection
        }
    }

    /// FIR's `isCastErased(supertype, subtype)`: whether testing a value of type `supertype` for
    /// `subtype` leaves part of `subtype` unchecked. `reified_target` says whether a type-parameter
    /// target is reified.
    pub(super) fn is_cast_erased(&self, supertype: Ty, subtype: Ty, reified_target: bool) -> bool {
        let supertype = crate::symbol_resolver::nominal_type(supertype);
        let subtype = crate::symbol_resolver::nominal_type(subtype);
        let target_parameter = match subtype {
            Ty::Nullable(inner) => *inner,
            other => other,
        };
        let is_non_reified_parameter =
            matches!(target_parameter, Ty::TyParam(..)) && !reified_target;
        let is_upcast = self.is_upcast(supertype, subtype);
        if is_non_reified_parameter && !is_upcast {
            // `x is T` for `x: T?` with a non-null `T` only checks for null.
            let nullable_to_definitely_not_null = !subtype.upper_bound_admits_null()
                && !subtype.is_nullable()
                && same_type_parameter(supertype.non_null(), subtype);
            if !nullable_to_definitely_not_null {
                return true;
            }
        }
        if (supertype != Ty::Error && supertype.is_nullable())
            || (subtype != Ty::Error && subtype.is_nullable())
        {
            return self.is_cast_erased(supertype.non_null(), subtype.non_null(), reified_target);
        }
        if is_upcast {
            return false;
        }
        if is_non_reified_parameter {
            return true;
        }
        if matches!(subtype, Ty::TyParam(..)) {
            return false;
        }
        let Ty::Obj(subtype_class, arguments) = subtype else {
            // Scalars and `String` are classifiers without type parameters, so each is its own
            // statically known subtype; an intersection or `T & Any` has no classifier.
            return matches!(subtype, Ty::Intersection(_) | Ty::DefinitelyNotNull(_));
        };
        // A classifier without type arguments is its own statically known subtype.
        if arguments.is_empty() {
            return false;
        }
        let source = self.fed_source();
        let check = |left: Ty, right: Ty| self.is_upcast(left, right);
        let Some(known) = crate::symbol_resolver::statically_known_subtype(
            &source,
            supertype,
            subtype_class,
            &check,
        ) else {
            return true;
        };
        !is_subtype(&TyCtx::new(), self, known, subtype)
    }

    /// FIR's `isUpcast`: the operand's type is already a subtype of the target.
    fn is_upcast(&self, candidate: Ty, target: Ty) -> bool {
        is_subtype(&TyCtx::new(), self, candidate, target)
    }
}

fn same_type_parameter(left: Ty, right: Ty) -> bool {
    matches!((left, right), (Ty::TyParam(a, _), Ty::TyParam(b, _)) if a == b)
}
