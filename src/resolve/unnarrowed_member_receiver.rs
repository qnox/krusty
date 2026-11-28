//! The dispatch receiver of a member selected through a smart-cast receiver.
//!
//! A smart cast adds the narrowed type's member scope to the receiver's own. When the member it
//! selects is the very declaration the receiver's declared type already exposes, with the same
//! signature, kotlinc keeps that member of the declared type and its unnarrowed receiver: no cast
//! is needed to call it. `x: A; if (x is B) x.f()` calls `A.f` on `x` when `B` only inherits `f`;
//! a real override in the narrowed class (`D.f`) still needs the cast. So does a member of a generic
//! classifier: the narrowed class inherits it through a substitution of that classifier's type
//! parameters, which is a different member from the one the declared type exposes, even when the
//! substituted signature is unchanged (`x: Base<String>; if (x is Leaf) x.plain()` calls
//! `Leaf.plain`). And so does a protected member that only the narrowed receiver may access from
//! the calling class (`a: A; if (a is B) a.f()` inside `B`).

use super::scope::PathRoot;
use super::stable_path::StablePathRead;
use super::{Checker, CheckerScope, ExprId, MemberCallTowerRung, Ty};
use crate::fir::ResolvedDelegatedMember;
use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

impl Checker<'_> {
    /// The receiver's declared type when it exposes exactly the member `selected` was selected
    /// through the smart-cast receiver type `narrowed`.
    pub(super) fn unnarrowed_member_receiver(
        &self,
        scope: &CheckerScope<'_>,
        tower_rung: &MemberCallTowerRung,
        narrowed: Ty,
        selected: &crate::libraries::FunctionInfo,
    ) -> Option<Ty> {
        let declared = match (tower_rung.receiver, tower_rung.explicit_receiver) {
            (Some(receiver), _) => receiver.declared_ty,
            (None, Some(receiver)) => self.smart_cast_declared_receiver(scope, receiver)?,
            (None, None) => return None,
        };
        // A nullable declared type has no member scope of its own: the call needs the narrowing.
        if declared == narrowed
            || declared.is_nullable()
            || declared.mentions_error()
            || self.declared_by_generic_classifier(
                selected
                    .callable
                    .declaration_owner
                    .map_or(selected.callable.owner, |owner| owner.name()),
            )
        {
            return None;
        }
        let exposes_selected = self
            .stable_receiver_callables(declared, &selected.callable.name)
            .functions()
            .iter()
            .any(|candidate| {
                candidate.kind == crate::libraries::FnKind::Member
                    && crate::symbol_resolver::same_callable_declaration(candidate, selected)
            });
        // A protected member accessed through a smart cast to the accessing class is accessible
        // only through that narrowed receiver, so the call keeps it.
        let accessible = self.receiver_callable_accessible(
            selected.visibility,
            selected.callable.owner,
            declared,
            selected.callable.external_identity,
        );
        let delegated = self.delegated_below(
            declared,
            narrowed,
            &selected.callable.name,
            selected.callable.owner,
        );
        (exposes_selected && accessible && !delegated).then_some(declared)
    }

    /// The type an explicit receiver read had before the smart cast that narrowed it.
    fn smart_cast_declared_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) -> Option<Ty> {
        let path = self.expr_access_path(receiver)?;
        match (&path.root, path.segments.is_empty()) {
            (PathRoot::Receiver(identity), true) => self
                .implicit_receivers(scope)
                .into_iter()
                .find(|candidate| candidate.identity == *identity)
                .map(|candidate| candidate.declared_ty),
            (PathRoot::Value(identity), true) => self
                .local_properties
                .declared_before_initializer(*identity)
                .or_else(|| StablePathRead::new(self).declared_ty(scope, &path)),
            _ => StablePathRead::new(self).declared_ty(scope, &path),
        }
    }
}

impl Checker<'_> {
    /// [`Self::unnarrowed_member_receiver`] for the property `name` read or written through the
    /// smart-cast explicit receiver of type `narrowed`, recorded for checked FIR under the access.
    /// `(access, receiver)` names the access expression and its receiver.
    pub(super) fn unnarrowed_explicit_property_receiver(
        &mut self,
        scope: &CheckerScope<'_>,
        (access, receiver): (ExprId, ExprId),
        narrowed: Ty,
        name: &str,
        write: bool,
    ) -> Option<Ty> {
        let declared = self
            .smart_cast_declared_receiver(scope, receiver)
            .and_then(|declared| {
                self.unnarrowed_property_receiver(declared, narrowed, name, write)
            });
        match declared {
            Some(declared) => self.unnarrowed_dispatch_calls.insert(access, declared),
            None => self.unnarrowed_dispatch_calls.remove(&access),
        };
        declared
    }

    /// `declared` when a smart cast of it to `narrowed` exposes no property `name` of its own:
    /// both types select the same declaration with the same type, accessible through `declared`.
    pub(super) fn unnarrowed_property_receiver(
        &self,
        declared: Ty,
        narrowed: Ty,
        name: &str,
        write: bool,
    ) -> Option<Ty> {
        if declared == narrowed || declared.is_nullable() || declared.mentions_error() {
            return None;
        }
        let resolver = self.resolver();
        let select = |receiver| {
            if write {
                resolver.select_member_property_write_applicable_where(receiver, name, |property| {
                    (property.context_count == 0).then_some((true, 0))
                })
            } else {
                resolver.select_member_property(receiver, name)
            }
        };
        let through_declared = select(declared)?;
        let through_narrowed = select(narrowed)?;
        let (Some(left), Some(right)) = (&through_declared.property, &through_narrowed.property)
        else {
            return None;
        };
        let same_declaration = match (left.stable_declaration, right.stable_declaration) {
            (Some(left), Some(right)) => left == right,
            _ => {
                left.owner == right.owner
                    && left.name == right.name
                    && left.getter.descriptor == right.getter.descriptor
            }
        };
        let visibility = if write {
            left.setter_visibility
        } else {
            left.visibility
        };
        let accessible = self.receiver_property_accessible(
            visibility,
            left.owner,
            declared,
            left.getter.external_identity,
        );
        let accessor = if write {
            left.setter.as_deref()?
        } else {
            &left.getter
        };
        (same_declaration
            && accessible
            && !self.java_accessor_override(declared, narrowed, accessor)
            && !self.delegated_below(declared, narrowed, name, left.owner)
            && through_declared.ty == through_narrowed.ty
            && !self.declared_by_generic_classifier(
                left.getter
                    .declaration_owner
                    .map_or(left.owner, |owner| owner.name()),
            ))
        .then_some(declared)
    }

    /// Whether a Java class between `declared` and `narrowed` overrides the property `accessor`
    /// with its own method (`NullPointerException.getMessage` for `Throwable.message`): a Java
    /// method of the accessor's JVM name and arity that `narrowed` exposes and `declared` does not.
    /// kotlinc reads it as an override of the property, so the access needs the narrowed class.
    fn java_accessor_override(
        &self,
        declared: Ty,
        narrowed: Ty,
        accessor: &crate::libraries::LibraryCallable,
    ) -> bool {
        let name = accessor.physical_name.as_deref().unwrap_or(&accessor.name);
        let methods = |receiver| {
            self.stable_receiver_callables(receiver, name)
                .functions()
                .iter()
                .filter(|method| {
                    method.kind == crate::libraries::FnKind::Member
                        && method.callable.params.len() == accessor.params.len()
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let through_declared = methods(declared);
        methods(narrowed).iter().any(|method| {
            !through_declared
                .iter()
                .any(|exposed| crate::symbol_resolver::same_callable_declaration(exposed, method))
        })
    }

    /// Whether a source class in `narrowed`'s hierarchy, below `declared`'s classifier, delegates
    /// an interface whose member `name` of `owner` it so overrides (`class IC(i: I) : I by i`
    /// overrides `I.ok`): the generated forwarder is the narrowed class's own declaration.
    fn delegated_below(&self, declared: Ty, narrowed: Ty, name: &str, owner: TypeName) -> bool {
        let (Some(index), Some(root)) = (self.resolved_index, narrowed.obj_internal()) else {
            return false;
        };
        let top = declared.nominal_classifier();
        let mut pending = vec![root];
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(classifier) = pending.pop() {
            if Some(classifier) == top || !seen.insert(classifier) {
                continue;
            }
            let header = index
                .classifier_declaration(classifier)
                .and_then(|declaration| index.classifier_header(declaration));
            let delegates = header.is_some_and(|header| {
                header.interface_delegations.iter().any(|delegation| {
                    delegation.members.iter().any(|member| match member {
                        ResolvedDelegatedMember::Function(function) => {
                            &*function.name == name
                                && function.overridden.iter().any(|over| over.owner == owner)
                        }
                        ResolvedDelegatedMember::Property(property) => {
                            &*property.name == name
                                && property.overridden.iter().any(|over| over.owner == owner)
                        }
                    })
                })
            });
            if delegates {
                return true;
            }
            if let Some(info) = self.fed_source().classifier(classifier) {
                pending.extend(info.supertypes.iter());
            }
        }
        false
    }

    /// Whether `owner`, the classifier declaring a selected member, has type parameters. An
    /// unknown owner counts as generic, so the narrowed receiver is kept.
    fn declared_by_generic_classifier(&self, owner: TypeName) -> bool {
        self.fed_source()
            .classifier(owner)
            .is_none_or(|classifier| !classifier.type_params.is_empty())
    }
}

impl Checker<'_> {
    /// The view of the implicit `receiver` a bare read of property `name` goes through: its
    /// declared type when a smart cast adds no property of that name, else the flow intersection's
    /// projection.
    pub(super) fn implicit_property_receiver_view(
        &self,
        receiver: super::ImplicitReceiver,
        name: &str,
    ) -> super::ImplicitReceiver {
        match self.unnarrowed_property_receiver(receiver.declared_ty, receiver.ty, name, false) {
            Some(declared) => super::ImplicitReceiver {
                ty: declared,
                ..receiver
            },
            None => self.implicit_intersection_property_view(receiver, name),
        }
    }
}
