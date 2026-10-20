//! Declaration contract and receiver-tower selection for member-extension properties.
//!
//! Providers normalize source and dependency declarations into [`MemberExtPropSig`]. Selection
//! then consumes only that semantic shape and stable declaration/accessor identities; callers
//! supply their receiver tower and context-argument binding policy.

use super::{
    applied_symbol_hierarchy, member_extension_selection, ImplicitReceiver,
    MemberExtensionPriority, ResolvedContextArgument,
};
use crate::fx_hash::HashMap;
use crate::libraries::GenericSig;
use crate::symbol_source::SymbolSource;
use crate::types::{ty_mentions_param, Ty, TypeName, Visibility};

/// A member extension property's declaration-time semantic shape.
#[derive(Clone, Debug)]
pub struct MemberExtPropSig {
    pub(super) receiver: Ty,
    pub(super) ret: Ty,
    pub(super) context_params: Vec<Ty>,
    pub(super) context_param_names: Vec<String>,
    pub(super) context_parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    pub(super) type_params: Vec<String>,
    pub(super) type_param_bounds: Vec<Ty>,
    pub(super) formal_bounds: Vec<Vec<Ty>>,
    pub(super) is_var: bool,
    pub(super) visibility: Visibility,
    /// Accessor visibility is independent from the property's visibility (`public var ... private
    /// set`). Keep it on the semantic signature, as ordinary declared properties do, so every
    /// implicit-dispatch use is checked before lowering and no backend/source-origin branch has to
    /// reconstruct it from syntax.
    pub(super) setter_visibility: Option<Visibility>,
    /// Resolved declaration annotation identities. A member extension property is a declared
    /// property like any other, so it publishes its annotations into `ResolvedModuleIndex` through
    /// the same stable declaration; it must not be the one property shape that drops them.
    pub(super) annotations: Vec<TypeName>,
    /// Provider-normalized declaration availability. `HIDDEN` keeps the binary declaration but
    /// removes it from source lookup; selectors must not reopen annotation syntax by origin.
    pub(super) deprecated_hidden: bool,
    /// Independent source availability of the getter and setter after accessor annotations have
    /// been folded. Setter presence remains represented by `is_var`.
    pub(super) accessor_availability: crate::libraries::PropertyAccessorAvailability,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(super) source_member: Option<crate::libraries::SourceMember>,
    pub(super) getter: Option<crate::libraries::LibraryCallable>,
    pub(super) setter: Option<crate::libraries::LibraryCallable>,
}

impl MemberExtPropSig {
    fn readable(&self) -> bool {
        !self.deprecated_hidden && self.accessor_availability.getter
    }

    fn write_selectable(&self) -> bool {
        !self.deprecated_hidden && (!self.is_var || self.accessor_availability.setter)
    }

    /// The declared extension receiver (`Int` in `val Int.x: T`).
    pub(crate) fn receiver_ty(&self) -> Ty {
        self.receiver
    }

    /// The property's type (with getter-body inference already applied).
    pub(crate) fn ret(&self) -> Ty {
        self.ret
    }

    /// The property's own type parameters (`val <T> T.x`); empty for the supported shape.
    pub(crate) fn type_params(&self) -> &[String] {
        &self.type_params
    }

    pub(crate) fn type_param_bounds(&self) -> &[Ty] {
        &self.type_param_bounds
    }

    /// The declaration-owned generic shape shared by source and dependency property providers.
    /// Accessor consumers must not reconstruct these bounds from the rendered property spelling.
    pub(crate) fn generic_signature(&self) -> Option<GenericSig> {
        (!self.type_params.is_empty()).then(|| GenericSig {
            formals: self.type_params.clone(),
            formal_bounds: self.formal_bounds.clone(),
            receiver: Some(self.receiver),
            params: self.context_params.clone(),
            ret: self.ret,
            return_policy: Default::default(),
        })
    }

    pub(crate) fn context_params(&self) -> &[Ty] {
        &self.context_params
    }

    pub(crate) fn context_param_names(&self) -> &[String] {
        &self.context_param_names
    }

    pub(crate) fn context_parameter_identities(&self) -> &[crate::fir::ResolvedParameterIdentity] {
        &self.context_parameter_identities
    }

    pub(crate) fn is_var(&self) -> bool {
        self.is_var
    }

    pub(crate) fn stable_declaration(&self) -> Option<crate::fir::DeclarationId> {
        self.stable_declaration
    }

    pub(crate) fn source_member(&self) -> Option<crate::libraries::SourceMember> {
        self.source_member
    }

    pub(crate) fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub(crate) fn setter_visibility(&self) -> Option<Visibility> {
        self.setter_visibility
    }

    pub(crate) fn deprecated_hidden(&self) -> bool {
        self.deprecated_hidden
    }

    pub(crate) fn accessor_availability(&self) -> crate::libraries::PropertyAccessorAvailability {
        self.accessor_availability
    }
}

/// One member extension property selected for a concrete receiver and implicit dispatch scope.
///
/// Naming the independently meaningful receiver, dispatch, property, and accessor fields prevents
/// a later handoff from checking public property visibility when a narrower setter governs a write.
#[derive(Clone)]
pub(super) struct MemberExtensionProperty {
    pub(super) ty: Ty,
    pub(super) is_var: bool,
    pub(super) visibility: Visibility,
    pub(super) setter_visibility: Option<Visibility>,
    pub(super) owner: TypeName,
    pub(super) dispatch_receiver: ImplicitReceiver,
    pub(super) declared_receiver: Ty,
    pub(super) context_params: Vec<Ty>,
    pub(super) context_args: Vec<ResolvedContextArgument>,
    /// Declaration-owned receiver/result shape retained on the selected property for metadata
    /// publication. It includes class-owner variables in `ret` even when the property declares no
    /// type parameters of its own.
    pub(super) publication_signature: Box<GenericSig>,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(super) getter: Option<crate::libraries::LibraryCallable>,
    pub(super) setter: Option<crate::libraries::LibraryCallable>,
}

impl super::Checker<'_> {
    /// Select a member-extension property from one combined provider/active declaration family
    /// using this lexical scope's receiver tower and context-argument policy.
    pub(super) fn member_extension_property(
        &self,
        scope: &super::CheckerScope<'_>,
        extension_receiver: Ty,
        name: &str,
    ) -> Result<Option<MemberExtensionProperty>, ()> {
        self.member_extension_property_for_access(
            scope,
            extension_receiver,
            name,
            crate::symbol_resolver::PropertyAccess::Read,
        )
    }

    pub(super) fn member_extension_property_for_write(
        &self,
        scope: &super::CheckerScope<'_>,
        extension_receiver: Ty,
        name: &str,
    ) -> Result<Option<MemberExtensionProperty>, ()> {
        self.member_extension_property_for_access(
            scope,
            extension_receiver,
            name,
            crate::symbol_resolver::PropertyAccess::Write,
        )
    }

    fn member_extension_property_for_access(
        &self,
        scope: &super::CheckerScope<'_>,
        extension_receiver: Ty,
        name: &str,
        access: crate::symbol_resolver::PropertyAccess,
    ) -> Result<Option<MemberExtensionProperty>, ()> {
        select(
            &self.fed_source(),
            self,
            &member_extension_selection::ordinary_dispatch_first(
                scope,
                self.implicit_receivers(scope),
            ),
            &|parameters| {
                self.select_context_arguments_with_types(scope, parameters)
                    .ok()
            },
            &|owner, name| {
                self.checked_local_member_extension_properties
                    .get(&owner)
                    .and_then(|properties| properties.get(name))
                    .cloned()
                    .unwrap_or_default()
            },
            extension_receiver,
            name,
            access,
        )
    }
}

/// Select a member-extension property against an explicit receiver tower. The tower and the
/// context-argument resolver are the only things a scope supplies here, so both are parameters:
/// the checker passes its scope's, while signature evaluation passes the declaration's.
pub(super) fn select(
    source: &dyn SymbolSource,
    oracle: &dyn crate::assignable::TypeOracle,
    implicit_receivers: &[ImplicitReceiver],
    context_arguments: &dyn Fn(&[Ty]) -> Option<Vec<(ResolvedContextArgument, Ty)>>,
    active_declarations: &dyn Fn(TypeName, &str) -> Vec<MemberExtPropSig>,
    extension_receiver: Ty,
    name: &str,
    access: crate::symbol_resolver::PropertyAccess,
) -> Result<Option<MemberExtensionProperty>, ()> {
    let mut candidates = Vec::new();
    for (dispatch_rank, dispatch_receiver) in implicit_receivers.iter().copied().enumerate() {
        // Member extensions are inherited across module boundaries exactly like ordinary members.
        // Keep the dispatch receiver's applied type while walking through the first classpath
        // boundary (for example `Derived : Foo<String>`), otherwise a declaration such as
        // `val T.prop` on `Foo<T>` is never reached or specialized for a downstream source class.
        for (owner, owner_ty, dispatch_depth) in
            applied_symbol_hierarchy(source, dispatch_receiver.ty)
        {
            // The immutable provider and the active body-local overlay expose the same normalized
            // declaration shape. Candidate union happens before applicability and specificity, so
            // active declarations never take a separate origin-specific selection path.
            let mut declarations = active_declarations(owner, name);
            declarations.retain(|property| match access {
                crate::symbol_resolver::PropertyAccess::Read => property.readable(),
                crate::symbol_resolver::PropertyAccess::Write => property.write_selectable(),
            });
            if let Some(classifier) = source.classifier(owner) {
                declarations.extend(
                    classifier
                        .declared_callables
                        .get(name)
                        .into_iter()
                        .flat_map(crate::libraries::Callables::properties)
                        .filter(|property| {
                            property.kind == crate::libraries::PropKind::MemberExtension
                                && (property.stable_declaration.is_some()
                                    || property.getter.external_property_identity.is_some())
                        })
                        .filter(|property| match access {
                            crate::symbol_resolver::PropertyAccess::Read => property.readable(),
                            crate::symbol_resolver::PropertyAccess::Write => {
                                property.write_selectable()
                            }
                        })
                        .filter_map(|property| {
                            let receiver = property.receiver?;
                            let generic = property.getter.generic_sig.as_deref();
                            let (type_params, formal_bounds) = if property.formals.is_empty() {
                                (Vec::new(), Vec::new())
                            } else {
                                let generic = generic?;
                                (generic.formals.clone(), generic.formal_bounds.clone())
                            };
                            let type_param_bounds = formal_bounds
                                .iter()
                                .map(|bounds| {
                                    bounds
                                        .first()
                                        .copied()
                                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                                })
                                .collect();
                            Some(MemberExtPropSig {
                                receiver,
                                ret: property.ty,
                                context_params: property
                                    .getter
                                    .params
                                    .iter()
                                    .take(property.context_count)
                                    .copied()
                                    .collect(),
                                context_param_names: property.context_param_names.clone(),
                                context_parameter_identities: property
                                    .context_parameter_identities
                                    .clone(),
                                type_params,
                                type_param_bounds,
                                formal_bounds,
                                is_var: property.setter.is_some(),
                                visibility: property.visibility,
                                setter_visibility: property
                                    .setter
                                    .as_ref()
                                    .map(|_| property.setter_visibility),
                                // A provider-normalized property carries no annotation identities
                                // yet: `PropertyInfo` has no annotation field. This selection list
                                // is transient and never publishes a declaration's annotations —
                                // `ClassSig::member_ext_props` does — so nothing reads these.
                                annotations: Vec::new(),
                                deprecated_hidden: property.deprecated_hidden,
                                accessor_availability: property.accessor_availability,
                                stable_declaration: property.stable_declaration,
                                source_member: property.source_member,
                                getter: Some(property.getter.clone()),
                                setter: property.setter.as_deref().cloned(),
                            })
                        }),
                );
            }
            let class_bindings = source
                .classifier(owner)
                .map_or_else(HashMap::default, |class| {
                    class
                        .type_params
                        .iter()
                        .cloned()
                        .zip(owner_ty.type_args().iter().copied())
                        .collect()
                });
            for sig in &declarations {
                let mut applicability_bindings = class_bindings.clone();
                crate::symbol_resolver::unify_ty(
                    sig.receiver,
                    extension_receiver,
                    &mut applicability_bindings,
                );
                let bounds_satisfied =
                    sig.type_params
                        .iter()
                        .zip(&sig.formal_bounds)
                        .all(|(parameter, bounds)| {
                            applicability_bindings.get(parameter).is_none_or(|actual| {
                                bounds.iter().copied().all(|bound| {
                                    crate::assignable::is_assignable(
                                        &crate::assignable::TyCtx::new(),
                                        oracle,
                                        *actual,
                                        crate::symbol_resolver::ty_subst(
                                            bound,
                                            &applicability_bindings,
                                        ),
                                    )
                                })
                            })
                        });
                if !bounds_satisfied {
                    continue;
                }
                for (parameter, bound) in sig.type_params.iter().zip(&sig.type_param_bounds) {
                    applicability_bindings
                        .entry(parameter.clone())
                        .or_insert(*bound);
                }
                let declared_receiver =
                    crate::symbol_resolver::ty_subst(sig.receiver, &applicability_bindings);
                let applicable = declared_receiver == extension_receiver
                    || declared_receiver.is_erased_top()
                    || crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        oracle,
                        extension_receiver,
                        declared_receiver,
                    );
                if !applicable {
                    continue;
                }
                let mut bindings = class_bindings.clone();
                for parameter in &sig.type_params {
                    bindings.remove(parameter);
                }
                crate::symbol_resolver::unify_ty(sig.receiver, extension_receiver, &mut bindings);
                let context_params = sig
                    .context_params
                    .iter()
                    .map(|parameter| crate::symbol_resolver::ty_subst(*parameter, &bindings))
                    .collect::<Vec<_>>();
                let Some(context_arguments) = context_arguments(&context_params) else {
                    continue;
                };
                for (parameter, (_, actual)) in sig.context_params.iter().zip(&context_arguments) {
                    crate::symbol_resolver::unify_ty(*parameter, *actual, &mut bindings);
                }
                for (parameter, bound) in sig.type_params.iter().zip(&sig.type_param_bounds) {
                    bindings.entry(parameter.clone()).or_insert(*bound);
                }
                let ty = crate::symbol_resolver::ty_subst(sig.ret, &bindings);
                let context_params = sig
                    .context_params
                    .iter()
                    .map(|parameter| crate::symbol_resolver::ty_subst(*parameter, &bindings))
                    .collect();
                let context_args = context_arguments
                    .into_iter()
                    .map(|(argument, _)| argument)
                    .collect();
                let generic_receiver = ty_mentions_param(sig.receiver, &sig.type_params);
                let receiver_domain = member_extension_selection::receiver_domain(
                    sig.receiver,
                    &sig.type_params,
                    &sig.type_param_bounds,
                    &class_bindings,
                );
                candidates.push((
                    MemberExtensionPriority {
                        dispatch_rank,
                        dispatch_depth,
                        owner,
                        declared_receiver,
                        generic_receiver,
                        receiver_domain,
                    },
                    MemberExtensionProperty {
                        ty,
                        is_var: sig.is_var,
                        visibility: sig.visibility,
                        setter_visibility: sig.setter_visibility,
                        owner,
                        dispatch_receiver,
                        declared_receiver,
                        context_params,
                        context_args,
                        publication_signature: Box::new(GenericSig {
                            formals: sig.type_params.clone(),
                            formal_bounds: sig.formal_bounds.clone(),
                            receiver: Some(sig.receiver),
                            params: sig.context_params.clone(),
                            ret: sig.ret,
                            return_policy: Default::default(),
                        }),
                        stable_declaration: sig.stable_declaration,
                        getter: sig.getter.clone(),
                        setter: sig.setter.clone(),
                    },
                ));
            }
        }
    }
    let maximal =
        member_extension_selection::maximal_member_extensions(oracle, &candidates, |candidate| {
            candidate.0
        });
    match maximal.as_slice() {
        [index] => Ok(Some(candidates[*index].1.clone())),
        [] => Ok(None),
        _ => Err(()),
    }
}
