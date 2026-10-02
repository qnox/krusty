//! Selection of one semantic member-property declaration through an applied receiver hierarchy.

use super::{
    declared_callables, direct_supertypes, hierarchy_projection, member_is_inheritable, PropKind,
    SelectedMemberProperty, SymbolResolver, Ty,
};
use crate::symbol_source::SymbolSource;

impl SymbolResolver<'_> {
    /// The declared type of the member property `name` on `recv` — the property itself, with no
    /// accessor in the answer. Provider-normalized declarations precede synthetic Java bean
    /// properties; applicability stays at the use site because protected access depends on the
    /// lexical class and receiver type.
    pub fn select_member_property(&self, recv: Ty, name: &str) -> Option<SelectedMemberProperty> {
        self.select_member_property_applicable_where(recv, name, |property| {
            (property.context_count == 0).then_some((true, 0))
        })
    }

    pub(crate) fn select_member_property_where(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<SelectedMemberProperty> {
        self.select_member_property_applicable_where(recv, name, |property| {
            (property.context_count == 0).then_some((true, 0))
        })
    }

    pub(crate) fn select_member_property_applicable_where(
        &self,
        recv: Ty,
        name: &str,
        property_applicable: impl Fn(&crate::libraries::PropertyInfo) -> Option<(bool, usize)>,
    ) -> Option<SelectedMemberProperty> {
        if recv.is_nullable()
            || (!matches!(recv.non_null(), Ty::Intersection(_))
                && recv.kotlin_class_internal().is_none())
        {
            return None;
        }
        // Component order is the selection order. A later component's direct property must not
        // hide a property the earlier component only inherits, and a `var` overrides a `val`.
        if let Some(parts) = hierarchy_projection::intersection_components(recv) {
            return self.intersection_member_property(parts, name, &property_applicable);
        }
        self.select_classifier_member_property(recv, name, &property_applicable)
    }

    fn select_classifier_member_property(
        &self,
        recv: Ty,
        name: &str,
        property_applicable: &dyn Fn(&crate::libraries::PropertyInfo) -> Option<(bool, usize)>,
    ) -> Option<SelectedMemberProperty> {
        let mut queue = std::collections::VecDeque::from([(recv, 0u32)]);
        let mut seen = std::collections::HashSet::new();
        let mut nearer: Vec<(std::sync::Arc<crate::libraries::LibraryType>, Ty)> = Vec::new();
        let mut synthetic_fallback: Option<(SelectedMemberProperty, bool)> = None;
        let mut inaccessible_declaration: Option<SelectedMemberProperty> = None;
        while let Some((current, depth)) = queue.pop_front() {
            if let Some(parts) = hierarchy_projection::intersection_components(current) {
                queue.extend(parts.iter().copied().map(|part| (part, depth)));
                continue;
            }
            let Some(internal) = current.kotlin_class_internal() else {
                continue;
            };
            if !seen.insert(current) {
                continue;
            }
            let Some(shape) = self.src.classifier(internal) else {
                continue;
            };
            let (_, mut local_properties) =
                declared_callables(&self.src, &shape, current, name).into_parts();
            local_properties.overloads.extend(
                self.lib
                    .inherited_accessor_properties(&self.src, current, name)
                    .overloads,
            );
            if depth > 0 {
                local_properties
                    .overloads
                    .retain(|property| member_is_inheritable(property.visibility));
            }
            crate::trace_compiler!(
                "resolve",
                "member property rung receiver={recv:?} current={current:?} owner={internal} name={name} properties={:?}",
                local_properties
                    .overloads
                    .iter()
                    .map(|property| (property.owner, property.kind, property.context_count))
                    .collect::<Vec<_>>(),
            );
            let local_property = local_properties
                .overloads
                .into_iter()
                .filter(|property| property.kind == PropKind::Member && property.receiver_rank == 0)
                .filter_map(|property| {
                    property_applicable(&property).map(|priority| (priority, property))
                })
                .max_by_key(|(priority, property)| (*priority, !property.accessor_derived()));
            if let Some(((accessible, _), mut property)) = local_property {
                crate::trace_compiler!(
                    "resolve",
                    "member property candidate receiver={current:?} owner={} name={name} getter={} classifier_formals={:?} declared={:?}",
                    property.owner,
                    property.getter.name,
                    shape.type_params,
                    property.ty,
                );
                // `declared_callables` already applied `current`. Reapplying classifier bindings
                // would erase a caller-owned type parameter to `Any` as an apparently unbound one.
                let declared_ty = property.ty;
                let ty = nearer
                    .iter()
                    .find_map(|(shape, applied)| {
                        declared_callables(
                            &self.src,
                            shape.as_ref(),
                            *applied,
                            &property.getter.name,
                        )
                        .into_parts()
                        .0
                        .overloads
                        .into_iter()
                        .find(|function| function.semantic_params().is_empty())
                        .map(|function| function.callable.ret)
                    })
                    .unwrap_or(declared_ty);
                property.ty = ty;
                property.getter.ret = ty;
                crate::trace_compiler!(
                    "resolve",
                    "member property selected receiver={current:?} owner={} name={name} ty={ty:?} visibility={:?}",
                    property.owner,
                    property.visibility,
                );
                let interface = self
                    .src
                    .classifier(property.owner)
                    .is_some_and(|owner| owner.is_interface());
                let accessor_derived = property.accessor_derived();
                let selected = SelectedMemberProperty {
                    owner: property.owner,
                    ty,
                    interface,
                    visibility: property.visibility,
                    property: Some(property),
                };
                if accessor_derived {
                    synthetic_fallback.get_or_insert((selected, accessible));
                } else if accessible {
                    return Some(selected);
                } else if synthetic_fallback
                    .as_ref()
                    .is_some_and(|(_, accessible)| *accessible)
                {
                    return synthetic_fallback.map(|(property, _)| property);
                } else {
                    // An inaccessible Java declaration is not inherited and cannot hide an
                    // accessible semantic property from a supertype.
                    inaccessible_declaration.get_or_insert(selected);
                }
            }
            if shape.hidden_member_properties.contains(name) {
                return synthetic_fallback
                    .filter(|(_, accessible)| *accessible)
                    .map(|(property, _)| property)
                    .or(inaccessible_declaration);
            }
            nearer.push((shape, current));
            queue.extend(
                direct_supertypes(&self.src, current)
                    .into_iter()
                    .map(|supertype| (supertype, depth + 1)),
            );
        }
        synthetic_fallback
            .filter(|(_, accessible)| *accessible)
            .map(|(property, _)| property)
            .or(inaccessible_declaration)
    }

    /// The property a value read or callable reference uses. An intersection walks each canonical
    /// component to completion; the earliest `var` wins over every `val`, and otherwise the
    /// earliest declaration wins, including one that component only inherits.
    fn intersection_member_property(
        &self,
        parts: &[Ty],
        name: &str,
        property_applicable: &dyn Fn(&crate::libraries::PropertyInfo) -> Option<(bool, usize)>,
    ) -> Option<SelectedMemberProperty> {
        let mut selected_val = None;
        for part in parts.iter().copied() {
            let Some(selected) =
                self.select_classifier_member_property(part, name, property_applicable)
            else {
                continue;
            };
            if selected
                .property
                .as_ref()
                .is_some_and(|property| property.setter.is_some())
            {
                return Some(selected);
            }
            if selected_val.is_none() {
                selected_val = Some(selected);
            }
        }
        selected_val
    }

    pub(super) fn declared_member_property(
        &self,
        receiver: Ty,
        name: &str,
        callables: &crate::libraries::Callables,
    ) -> Option<crate::libraries::PropertyInfo> {
        if hierarchy_projection::intersection_components(receiver.non_null()).is_some() {
            return self
                .select_member_property(receiver, name)
                .and_then(|selected| selected.property);
        }
        super::member_property_from_callables(callables)
    }
}
