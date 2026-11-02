//! Subtype proof over authenticated caller formals and explicit class evidence.

use super::klib_types::KlibTypeParameters;
use crate::assignable::{is_subtype, TyCtx, TypeOracle};
use crate::types::{Ty, TyKind, TypeName, TypeVariance};

struct NoClassEvidence;

impl TypeOracle for NoClassEvidence {
    fn direct_supertypes(&self, _ty: Ty) -> Vec<Ty> {
        Vec::new()
    }
}

struct CallerBounds<'a> {
    classes: &'a dyn TypeOracle,
    parameters: &'a KlibTypeParameters,
}

impl TypeOracle for CallerBounds<'_> {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        self.classes.direct_supertypes(ty)
    }
    fn same_class_name(&self, left: TypeName, right: TypeName) -> bool {
        self.classes.same_class_name(left, right)
    }
    fn type_param_variance(&self, owner: TypeName, index: usize) -> TypeVariance {
        self.classes.type_param_variance(owner, index)
    }
    fn type_param_upper_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        self.classes.type_param_upper_bounds(owner, index)
    }
    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        self.classes.platform_flexible_upper_bound(lower)
    }
    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        match self.parameters.declared_bounds(name) {
            // Even an empty vector is known declaration evidence (the implicit Any? bound).
            Some(bounds) => bounds.to_vec(),
            None => self.classes.type_variable_bounds(name),
        }
    }
}

pub(super) fn satisfies(
    classes: Option<&dyn TypeOracle>,
    parameters: &KlibTypeParameters,
    argument: Ty,
    bound: Ty,
) -> bool {
    let oracle = CallerBounds {
        classes: classes.unwrap_or(&NoClassEvidence),
        parameters,
    };
    is_subtype(&TyCtx::new(), &oracle, argument, bound)
}

/// Ordinary conversions require the calling compilation's explicit class model. Without it,
/// preserve identity only; an absent model must not manufacture hierarchy evidence.
pub(super) fn ordinary_subtype(
    classes: Option<&dyn TypeOracle>,
    parameters: &KlibTypeParameters,
    sub: Ty,
    sup: Ty,
) -> bool {
    sub == sup || classes.is_some_and(|classes| satisfies(Some(classes), parameters, sub, sup))
}

/// Whether `left` and `right` are one type once every projection that only restates its
/// parameter's declaration-site variance is dropped: `Iterator<out T>` is `Iterator<T>` because
/// `Iterator` declares its parameter `out`. Without the class model only identity is proven.
pub(super) fn same_type(classes: Option<&dyn TypeOracle>, left: Ty, right: Ty) -> bool {
    left == right
        || classes.is_some_and(|classes| {
            without_restated_variance(classes, left) == without_restated_variance(classes, right)
        })
}

fn without_restated_variance(classes: &dyn TypeOracle, ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Nullable(inner) => Ty::nullable(without_restated_variance(classes, inner)),
        TyKind::InProjection(inner) => Ty::in_projection(without_restated_variance(classes, inner)),
        TyKind::OutProjection(inner) => {
            Ty::out_projection(without_restated_variance(classes, inner))
        }
        TyKind::Obj(owner, arguments) if !arguments.is_empty() => {
            let arguments = arguments
                .iter()
                .enumerate()
                .map(|(index, &argument)| {
                    let argument =
                        match (argument.kind(), classes.type_param_variance(owner, index)) {
                            (TyKind::OutProjection(inner), TypeVariance::Out)
                            | (TyKind::InProjection(inner), TypeVariance::In) => inner,
                            _ => argument,
                        };
                    without_restated_variance(classes, argument)
                })
                .collect::<Vec<_>>();
            Ty::obj_args_name(owner, &arguments)
        }
        _ => ty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_subtyping_consumes_the_explicit_applied_hierarchy() {
        struct Classes;
        impl TypeOracle for Classes {
            fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
                if ty.kotlin_class_internal() == Some(crate::types::type_name("fixture/Child")) {
                    vec![Ty::obj_args("fixture/Parent", ty.type_args())]
                } else {
                    Vec::new()
                }
            }
        }
        let classes = Classes;
        let parameters = KlibTypeParameters::default();
        let child = Ty::obj_args("fixture/Child", &[Ty::String]);
        let parent = Ty::obj_args("fixture/Parent", &[Ty::String]);
        let unrelated = Ty::obj_args("fixture/Parent", &[Ty::Int]);
        assert!(ordinary_subtype(Some(&classes), &parameters, child, parent));
        assert!(!ordinary_subtype(
            Some(&classes),
            &parameters,
            child,
            unrelated
        ));
        assert!(!ordinary_subtype(None, &parameters, child, parent));
    }

    #[test]
    fn ordinary_subtyping_requires_evidence_and_preserves_all_formal_bounds() {
        let classes = NoClassEvidence;
        let mut parameters = KlibTypeParameters::default();
        let a = Ty::obj("fixture/A");
        let b = Ty::obj("fixture/B");
        let c = Ty::obj("fixture/C");
        let formal = Ty::ty_param("dependency/T", a);
        assert!(parameters.record_bounds(formal, vec![a, b]));
        for (sub, sup, expected) in [
            (formal, a, true),
            (formal, b, true),
            (formal, c, false),
            (Ty::String, formal, false),
        ] {
            assert_eq!(
                ordinary_subtype(Some(&classes), &parameters, sub, sup),
                expected
            );
            assert!(!ordinary_subtype(None, &parameters, sub, sup));
        }
        assert!(ordinary_subtype(None, &parameters, formal, formal));
    }

    #[test]
    fn ordinary_subtyping_preserves_array_projections_and_argument_arity() {
        let classes = NoClassEvidence;
        let parameters = KlibTypeParameters::default();
        let strings = Ty::obj_args("kotlin/Array", &[Ty::String]);
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        for (sup, expected) in [
            (Ty::obj_args("kotlin/Array", &[any]), false),
            (
                Ty::obj_args("kotlin/Array", &[Ty::out_projection(any)]),
                true,
            ),
            (Ty::obj_args("kotlin/Array", &[Ty::String, Ty::Int]), false),
        ] {
            assert_eq!(
                ordinary_subtype(Some(&classes), &parameters, strings, sup),
                expected
            );
            assert!(!ordinary_subtype(None, &parameters, strings, sup));
        }
    }
}
