//! The type-parameter view of a declaration built from its resolved bounds: each parameter's
//! primary bound, with a bound that names another parameter followed through to that parameter's
//! own bound, and the bounds it inherits through such a chain.

use crate::types::{Ty, TyKind};

use super::TParams;

pub(super) fn symbolic_from_resolved_bounds(
    declared_names: &[String],
    provisional: &TParams,
    resolved_bounds: &[(String, Ty)],
) -> TParams {
    let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let direct = resolved_bounds.iter().fold(
        crate::fx_hash::HashMap::<&str, Vec<Ty>>::default(),
        |mut direct, (owner, bound)| {
            direct.entry(owner).or_default().push(*bound);
            direct
        },
    );
    let source_by_semantic = declared_names
        .iter()
        .filter_map(|source| {
            provisional
                .bound(source)
                .ty_param_name()
                .map(|semantic| (semantic, source.as_str()))
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();

    fn parameter_reference(
        semantic: &'static str,
        resolved_upper_bound: Ty,
        direct: &crate::fx_hash::HashMap<&str, Vec<Ty>>,
        source_by_semantic: &crate::fx_hash::HashMap<&'static str, &str>,
        visiting: &mut crate::fx_hash::HashSet<&'static str>,
        implicit: Ty,
    ) -> Ty {
        let Some(source) = source_by_semantic.get(semantic) else {
            return Ty::ty_param(semantic, resolved_upper_bound);
        };
        if !visiting.insert(semantic) {
            return Ty::ty_param(semantic, implicit);
        }
        let bound = direct
            .get(source)
            .and_then(|bounds| bounds.first())
            .copied()
            .map(|bound| match bound.kind() {
                TyKind::TyParam(next, upper_bound) | TyKind::Var(next, upper_bound) => {
                    parameter_reference(
                        next.as_str(),
                        upper_bound,
                        direct,
                        source_by_semantic,
                        visiting,
                        implicit,
                    )
                }
                _ => bound,
            })
            .unwrap_or(resolved_upper_bound);
        visiting.remove(semantic);
        Ty::ty_param(semantic, bound)
    }

    fn inherited_bounds(
        source: &str,
        direct: &crate::fx_hash::HashMap<&str, Vec<Ty>>,
        source_by_semantic: &crate::fx_hash::HashMap<&'static str, &str>,
        visiting: &mut crate::fx_hash::HashSet<String>,
        inherited: &mut Vec<Ty>,
    ) {
        if !visiting.insert(source.to_owned()) {
            return;
        }
        for bound in direct.get(source).into_iter().flatten() {
            if let TyKind::TyParam(semantic, _) | TyKind::Var(semantic, _) = bound.kind() {
                if let Some(source) = source_by_semantic.get(semantic.as_str()) {
                    inherited_bounds(source, direct, source_by_semantic, visiting, inherited);
                    continue;
                }
            }
            if !inherited.contains(bound) {
                inherited.push(*bound);
            }
        }
        visiting.remove(source);
    }

    let mut symbolic = TParams::default();
    for name in declared_names {
        let semantic = provisional
            .bound(name)
            .ty_param_name()
            .expect("provisional declaration type parameter must retain its semantic identity");
        let declared = direct.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let primary = declared.first().copied().unwrap_or(implicit);
        let primary = match primary.kind() {
            TyKind::TyParam(parameter, upper_bound) | TyKind::Var(parameter, upper_bound) => {
                parameter_reference(
                    parameter.as_str(),
                    upper_bound,
                    &direct,
                    &source_by_semantic,
                    &mut crate::fx_hash::HashSet::default(),
                    implicit,
                )
            }
            _ => primary,
        };
        let mut extra = declared.iter().copied().skip(1).collect::<Vec<_>>();
        for bound in declared {
            let (TyKind::TyParam(parameter, _) | TyKind::Var(parameter, _)) = bound.kind() else {
                continue;
            };
            let Some(source) = source_by_semantic.get(parameter.as_str()) else {
                continue;
            };
            let mut inherited = Vec::new();
            inherited_bounds(
                source,
                &direct,
                &source_by_semantic,
                &mut crate::fx_hash::HashSet::default(),
                &mut inherited,
            );
            for bound in inherited {
                if bound != primary && !extra.contains(&bound) {
                    extra.push(bound);
                }
            }
        }
        symbolic.insert_binding(name, Ty::ty_param(semantic, primary), extra);
    }
    symbolic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_bare_parameter_bound_keeps_the_inherited_intersection() {
        let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let t = crate::types::intern("test.T");
        let x = crate::types::intern("test.X");
        let a = Ty::obj("test/A");
        let b = Ty::obj("test/B");
        let mut provisional = super::super::TParams::default();
        provisional.insert_binding("T", Ty::ty_param(t, implicit), Vec::new());
        provisional.insert_binding("X", Ty::ty_param(x, implicit), Vec::new());
        let resolved = vec![
            ("T".to_string(), Ty::ty_param(x, implicit)),
            ("X".to_string(), a),
            ("X".to_string(), b),
        ];

        let symbolic = symbolic_from_resolved_bounds(
            &["T".to_string(), "X".to_string()],
            &provisional,
            &resolved,
        );

        assert_eq!(symbolic.bound("T"), Ty::ty_param(t, Ty::ty_param(x, a)));
        assert_eq!(symbolic.extra_bounds_of("T"), vec![a, b]);
        assert_eq!(symbolic.extra_bounds_of("X"), vec![b]);
    }

    #[test]
    fn resolved_enclosing_parameter_bound_keeps_its_resolved_upper_bound() {
        let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let own = crate::types::intern("test.S");
        let outer = crate::types::intern("test.OuterT");
        let outer_bound = Ty::obj("test/OuterBound");
        let mut provisional = super::super::TParams::default();
        provisional.insert_binding("S", Ty::ty_param(own, implicit), Vec::new());

        let symbolic = symbolic_from_resolved_bounds(
            &["S".to_string()],
            &provisional,
            &[("S".to_string(), Ty::ty_param(outer, outer_bound))],
        );

        assert_eq!(
            symbolic.bound("S"),
            Ty::ty_param(own, Ty::ty_param(outer, outer_bound))
        );
        assert!(symbolic.extra_bounds_of("S").is_empty());
    }
}
