//! Shared symbolic upper-bound construction over declaration-owned type syntax.

use std::collections::HashSet;

use crate::types::Ty;

use super::TParams;

pub(super) type ParameterLookup<'a> = dyn FnMut(&str) -> Option<Ty> + 'a;
pub(super) type BoundSemantic<'a, B> = dyn Fn(&B, &mut ParameterLookup<'_>) -> Ty + 'a;

pub(super) fn erased_from_syntax<B>(
    names: &[String],
    bounds: &[(String, B)],
    chain_parameter: &dyn Fn(&B) -> Option<String>,
    bound_erasure: &dyn Fn(&B) -> Ty,
) -> TParams {
    let any = Ty::obj("kotlin/Any");
    let erasure = names
        .iter()
        .map(|name| {
            let mut current = name.clone();
            let mut seen = HashSet::new();
            let erased = loop {
                let Some(bound) = bounds
                    .iter()
                    .find_map(|(owner, bound)| (owner == &current).then_some(bound))
                else {
                    break any;
                };
                let chained = chain_parameter(bound)
                    .filter(|candidate| names.iter().any(|name| name == candidate));
                match chained {
                    Some(parameter) if seen.insert(current) => current = parameter,
                    Some(_) => break any,
                    None => break bound_erasure(bound),
                }
            };
            (name.clone(), erased)
        })
        .collect();
    let extra_bounds = names
        .iter()
        .filter_map(|name| {
            let rest = bounds
                .iter()
                .filter(|(owner, _)| owner == name)
                .skip(1)
                .map(|(_, bound)| bound_erasure(bound))
                .filter(|ty| *ty != any)
                .collect::<Vec<_>>();
            (!rest.is_empty()).then(|| (name.clone(), rest))
        })
        .collect();
    TParams {
        erasure,
        extra_bounds,
    }
}

pub(super) fn erased_from_syntax_with_primary_class<B: Clone>(
    names: &[String],
    bounds: &[(String, B)],
    chain_parameter: &dyn Fn(&B) -> Option<String>,
    bound_erasure: &dyn Fn(&B) -> Ty,
    concrete_class_bound: &dyn Fn(&B) -> bool,
) -> TParams {
    let mut ordered = Vec::with_capacity(bounds.len());
    for name in names {
        let mut declared = bounds
            .iter()
            .filter(|(owner, _)| owner == name)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(primary) = declared
            .iter()
            .position(|(_, bound)| concrete_class_bound(bound))
        {
            ordered.push(declared.remove(primary));
        }
        ordered.extend(declared);
    }
    erased_from_syntax(names, &ordered, chain_parameter, bound_erasure)
}

pub(super) fn enclosing_bound_erasure<B>(
    enclosing: &TParams,
    name: &str,
    local_names: &[String],
    bounds: &[(String, B)],
    enclosing_parameter: &dyn Fn(&B) -> Option<String>,
) -> Option<Ty> {
    let mut current = name.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let bound = bounds
            .iter()
            .find_map(|(owner, bound)| (owner == &current).then_some(bound))?;
        let parameter = enclosing_parameter(bound)?;
        if local_names.iter().any(|local| local == &parameter) {
            current = parameter;
            continue;
        }
        let mut erased = enclosing.erasure.get(&parameter).copied()?;
        let mut bound_names = HashSet::new();
        while let Ty::TyParam(parameter, upper) = erased {
            if !bound_names.insert(parameter) {
                return None;
            }
            erased = *upper;
        }
        return Some(erased);
    }
}

pub(super) fn erased_extended_from_syntax<B>(
    enclosing: &TParams,
    names: &[String],
    bounds: &[(String, B)],
    chain_parameter: &dyn Fn(&B) -> Option<String>,
    enclosing_parameter: &dyn Fn(&B) -> Option<String>,
    bound_erasure: &dyn Fn(&B) -> Ty,
) -> TParams {
    let mut out = enclosing.clone();
    let mut declared = erased_from_syntax(names, bounds, chain_parameter, bound_erasure);
    for name in names {
        if let Some(bound) =
            enclosing_bound_erasure(enclosing, name, names, bounds, enclosing_parameter)
        {
            declared.erasure.insert(name.clone(), bound);
        }
    }
    out.erasure.extend(declared.erasure);
    out.extra_bounds.extend(declared.extra_bounds);
    out
}

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
