//! Body-local inferred member results and their overlay onto already-selected callables.
//!
//! A local or anonymous class is checked inside its enclosing Pass-2 body, so a member whose
//! result is inferred from its body has no result in the module's signature index. The active
//! checker records each such result by its stable declaration (or, for a legacy whole-file
//! checker, its `SourceMember`), and every later selection in the same body reads it from here.

use super::*;

impl Checker<'_> {
    pub(super) fn record_checked_source_member_result(
        &mut self,
        source: crate::libraries::SourceMember,
        stable: Option<crate::fir::DeclarationId>,
        ty: Ty,
    ) {
        if let Some(declaration) = stable.or_else(|| self.active_source_member_declaration(source))
        {
            self.inferred_stable_declaration_rets
                .insert(declaration, ty);
        } else {
            // Legacy whole-file checker callers have no stable active declaration inventory.
            self.inferred_source_member_rets.insert(source, ty);
        }
    }

    pub(super) fn remove_checked_source_member_result(
        &mut self,
        source: crate::libraries::SourceMember,
        stable: Option<crate::fir::DeclarationId>,
    ) {
        if let Some(declaration) = stable.or_else(|| self.active_source_member_declaration(source))
        {
            self.inferred_stable_declaration_rets.remove(&declaration);
        } else {
            self.inferred_source_member_rets.remove(&source);
        }
    }

    pub(super) fn has_finalized_signature(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> bool {
        declaration.is_some_and(|declaration| {
            self.resolved_index
                .is_some_and(|index| index.signature(declaration).is_some())
        })
    }

    pub(super) fn checked_source_member_result(
        &self,
        source: Option<crate::libraries::SourceMember>,
        stable: Option<crate::fir::DeclarationId>,
    ) -> Option<Ty> {
        if let Some(declaration) = stable {
            // A Pass-1 signature is the declaration's public semantic result. Pass 2 may compute a
            // more precise expression-body type while checking the declaration (`fun make() =
            // object { ... }`), but that implementation type must not leak through a later call in
            // the same bounded unit when Pass 1 deliberately approximated the stable result to
            // `Any`. Body-local declarations have no finalized signature yet and therefore fall
            // through to the checked overlay below.
            // The legacy candidate table has already projected that result into its own aligned
            // generic-formal identities. Return no overlay here rather than injecting the raw index
            // type, whose declaration-owned formals cannot be substituted by the candidate's call
            // bindings.
            if self.has_finalized_signature(Some(declaration)) {
                return None;
            }
            // A stable candidate that is outside the active bounded unit has no body-local overlay
            // in this checker. Its legacy `SourceMember` coordinates are parser-arena identities;
            // interpreting them through the active unit can alias an unrelated member at the same
            // class/method indices (`I.call` and a later unit's `A.foo` both appear as owner 0,
            // method 0 after reparsing). Stable identity is authoritative once present.
            let stable_result = self
                .inferred_stable_declaration_rets
                .get(&declaration)
                .copied();
            if stable_result.is_some() || self.active_declarations.is_some() {
                return stable_result;
            }
            // Same-parse compatibility checkers have no active stable binding table, but their
            // retained AST and `SourceMember` coordinates still belong to this exact file arena.
            // Consume the checked overlay there; production Pass 2 always has
            // `active_declarations` and therefore never takes this transient-coordinate path.
            return source
                .and_then(|source| self.inferred_source_member_rets.get(&source).copied());
        }
        source
            .and_then(|source| self.active_source_member_declaration(source))
            .and_then(|declaration| {
                self.inferred_stable_declaration_rets
                    .get(&declaration)
                    .copied()
            })
            .or_else(|| {
                source.and_then(|source| self.inferred_source_member_rets.get(&source).copied())
            })
    }

    /// Apply a body-local inferred property result to the already selected stable declaration.
    ///
    /// Local and anonymous classes are checked inside their enclosing Pass-2 body, so their
    /// expression-derived property types cannot live in the module's Pass-1 signature index. The
    /// active checker records those results by `SourceMember`; every later selection in the same
    /// body consumes that identity-keyed overlay. Keep the accessor handles coherent with the
    /// logical property type so checked FIR never receives a selected getter or setter containing
    /// the registration-time `Ty::Error` marker. The setter's declared parameter takes the
    /// declaration's own result, before the receiver's substitution.
    pub(super) fn apply_checked_source_property_result(
        &self,
        receiver: Ty,
        property: &mut crate::libraries::PropertyInfo,
    ) -> Option<Ty> {
        let declared =
            self.checked_source_member_result(property.source_member, property.stable_declaration)?;
        // The overlay stores the declaration-owned result. A read may already have selected that
        // declaration through an applied generic receiver (`Outer<Int>.ref`), so publish the same
        // receiver substitution as the ordinary property-selection path before replacing its
        // logical/accessor type. Otherwise a fresh local-body result can regress a correctly
        // specialized `Inner<Int, Int>` back to the declaration shape `Inner<Int, A>`.
        let ty = self
            .resolver()
            .classifier(property.owner)
            .map(|classifier| {
                let bindings = crate::symbol_resolver::classifier_bindings(&classifier, receiver);
                crate::symbol_resolver::ty_subst_keep_unbound(declared, &bindings)
            })
            .unwrap_or(declared);
        crate::trace_compiler!(
            "resolve",
            "checked property overlay name={} receiver={receiver:?} declared={declared:?} applied={ty:?}",
            property.name,
        );
        property.ty = ty;
        property.getter.ret = ty;
        property.getter.physical_ret = ty;
        if let Some(setter) = property.setter.as_mut() {
            let declared_slot = setter.declared_params.as_deref_mut().unwrap_or_default();
            let slots = [
                &mut setter.params[..],
                &mut setter.physical_params[..],
                declared_slot,
            ];
            for (slot, value) in slots.into_iter().zip([ty, ty, declared]) {
                if let Some(parameter) = slot.last_mut() {
                    *parameter = value;
                }
            }
        }
        Some(ty)
    }

    /// Apply a body-local inferred method result to an already-selected callable target.
    ///
    /// Local classes are checked inside the enclosing Pass-2 body, after their compact module
    /// registration has necessarily used `Ty::Error` for an expression-derived member result. The
    /// active checker owns the exact `SourceMember` result; substitute it through the already-
    /// selected receiver before publishing checked callable-reference semantics.
    pub(super) fn apply_checked_source_callable_result(
        &self,
        receiver: Ty,
        member: &mut crate::libraries::LibraryMember,
    ) -> Option<Ty> {
        let bindings = self.member_receiver_type_bindings(receiver, member);
        let inferred = self
            .checked_source_member_result(member.source_member, member.stable_declaration)
            .or_else(|| {
                // Legacy same-parse signature checking has not yet installed the stable body-local
                // result map. Accept only the unique declaration-shaped result; production Pass 2
                // carries the stable declaration above and never enters this compatibility path.
                (!self.has_finalized_signature(member.stable_declaration))
                    .then(|| {
                        let owner = member.owner?;
                        let mut matches = self
                            .inferred_method_rets
                            .iter()
                            .filter(|((candidate_owner, name, parameters), _)| {
                                *candidate_owner == owner
                                    && name == &member.name
                                    && parameters.len() == member.params.len()
                                    && parameters.iter().zip(&member.params).all(
                                        |(declared, selected)| {
                                            crate::symbol_resolver::ty_subst_keep_unbound(
                                                *declared, &bindings,
                                            ) == *selected
                                        },
                                    )
                            })
                            .map(|(_, result)| *result);
                        let result = matches.next()?;
                        matches.next().is_none().then_some(result)
                    })
                    .flatten()
            })?;
        let inferred = crate::symbol_resolver::ty_subst_keep_unbound(inferred, &bindings);
        let parameters = member
            .generic_sig
            .as_ref()
            .map(|signature| signature.params.clone())
            .filter(|parameters| parameters.len() == member.params.len())
            .unwrap_or_else(|| member.params.clone());
        member.params = parameters
            .into_iter()
            .map(|parameter| crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings))
            .collect();
        member.ret = inferred;
        member.physical_ret = inferred;
        Some(inferred)
    }
}
