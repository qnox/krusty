//! Shared symbolic upper-bound construction over declaration-owned type syntax.

use std::collections::HashSet;

use crate::types::Ty;

use super::TParams;

pub(super) type ParameterLookup<'a> = dyn FnMut(&str) -> Option<Ty> + 'a;
pub(super) type BoundSemantic<'a, B> = dyn Fn(&B, &mut ParameterLookup<'_>) -> Ty + 'a;

struct BoundBuilder<'a, B> {
    names: &'a [String],
    bounds: &'a [(String, B)],
    bare_parameter: &'a dyn Fn(&B) -> Option<String>,
    semantic: &'a BoundSemantic<'a, B>,
    enclosing: &'a dyn Fn(&str) -> Option<Ty>,
}

impl<B> BoundBuilder<'_, B> {
    fn implicit_bound() -> Ty {
        Ty::nullable(Ty::obj("kotlin/Any"))
    }

    fn declared_parameter(&self, bound: &B) -> Option<String> {
        (self.bare_parameter)(bound)
            .filter(|candidate| self.names.iter().any(|name| name == candidate))
    }

    fn semantic_bound(&self, bound: &B, visiting: &mut HashSet<String>) -> Ty {
        (self.semantic)(bound, &mut |candidate| {
            if let Some(ty) = (self.enclosing)(candidate) {
                return Some(ty);
            }
            self.names
                .iter()
                .any(|name| name == candidate)
                .then(|| Ty::ty_param(candidate, self.parameter_bound(candidate, visiting)))
        })
    }

    fn parameter_bound(&self, name: &str, visiting: &mut HashSet<String>) -> Ty {
        if !visiting.insert(name.to_string()) {
            return Self::implicit_bound();
        }
        let bound = self
            .bounds
            .iter()
            .find_map(|(owner, bound)| (owner == name).then_some(bound));
        let resolved = match bound.and_then(|bound| {
            self.declared_parameter(bound)
                .map(|parameter| (bound, parameter))
        }) {
            Some((_, parameter)) => {
                Ty::ty_param(&parameter, self.parameter_bound(&parameter, visiting))
            }
            None => bound
                .map(|bound| self.semantic_bound(bound, visiting))
                .unwrap_or_else(Self::implicit_bound),
        };
        visiting.remove(name);
        resolved
    }

    fn inherited_bounds(&self, current: &str, visiting: &mut HashSet<String>, out: &mut Vec<Ty>) {
        if !visiting.insert(current.to_string()) {
            return;
        }
        for bound in self
            .bounds
            .iter()
            .filter_map(|(parameter, bound)| (parameter == current).then_some(bound))
        {
            if let Some(inherited) = self.declared_parameter(bound) {
                self.inherited_bounds(&inherited, visiting, out);
            } else {
                let resolved = (self.semantic)(bound, &mut |_| None);
                if !out.contains(&resolved) {
                    out.push(resolved);
                }
            }
        }
        visiting.remove(current);
    }

    fn build(&self) -> TParams {
        let implicit = Self::implicit_bound();
        let mut out = TParams::default();
        for name in self.names {
            let direct_bounds = self
                .bounds
                .iter()
                .filter_map(|(owner, bound)| (owner == name).then_some(bound))
                .map(|bound| {
                    if let Some(parameter) = self.declared_parameter(bound) {
                        Ty::ty_param(
                            &parameter,
                            self.parameter_bound(&parameter, &mut HashSet::new()),
                        )
                    } else {
                        self.semantic_bound(bound, &mut HashSet::new())
                    }
                })
                .collect::<Vec<_>>();
            let bound = direct_bounds.first().copied().unwrap_or(implicit);
            out.erasure.insert(name.clone(), Ty::ty_param(name, bound));
            let mut extra_bounds = direct_bounds.into_iter().skip(1).collect::<Vec<_>>();
            // Direct bounds already contain every constraint written on this parameter. Flatten
            // additional constraints only through a bare parameter edge (`T : X`), otherwise a
            // direct generic bound such as `Comparable<T>` would be decoded twice.
            for inherited in self
                .bounds
                .iter()
                .filter_map(|(owner, bound)| {
                    (owner == name)
                        .then(|| self.declared_parameter(bound))
                        .flatten()
                })
                .flat_map(|inherited| {
                    let mut semantics = Vec::new();
                    self.inherited_bounds(&inherited, &mut HashSet::new(), &mut semantics);
                    semantics
                })
            {
                if inherited != bound && !extra_bounds.contains(&inherited) {
                    extra_bounds.push(inherited);
                }
            }
            if !extra_bounds.is_empty() {
                out.extra_bounds.insert(name.clone(), extra_bounds);
            }
        }
        out
    }
}

pub(super) fn symbolic_from_syntax_enclosing<B>(
    names: &[String],
    bounds: &[(String, B)],
    bare_parameter: &dyn Fn(&B) -> Option<String>,
    semantic: &BoundSemantic<'_, B>,
    enclosing: &dyn Fn(&str) -> Option<Ty>,
) -> TParams {
    BoundBuilder {
        names,
        bounds,
        bare_parameter,
        semantic,
        enclosing,
    }
    .build()
}
