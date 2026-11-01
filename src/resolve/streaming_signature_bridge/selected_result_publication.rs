//! Publication facts retained from compact-signature call selection.
//!
//! Selection records one provider-neutral declaration contract by origin. The compact graph later
//! supplies its recursively published operands and the evaluator's final result; this collaborator
//! projects the declaration interval without lookup or overload reselection.

use super::ProductionSignatureSemantics;
use crate::resolve::selected_result_publication::CompactSelectedResultPublication;
use crate::types::{Ty, TyKind};

/// What a selection applied its declaration to: the receiver it was selected on and its argument
/// types in source order.
#[derive(Clone, Copy, Default)]
pub(super) struct SelectionInputs<'a> {
    pub(super) receiver: Option<Ty>,
    pub(super) arguments: &'a [Ty],
}

fn whole_array_arguments(arguments: &[crate::fir::ResolvedSigCallArgument<'_>]) -> Box<[bool]> {
    arguments
        .iter()
        .map(|argument| argument.spread || argument.name.is_some())
        .collect()
}

impl ProductionSignatureSemantics<'_> {
    /// The abbreviation a call or read's result carries because its selected declaration spelled
    /// the result through a typealias (`fun names(): Names`, `val foo: Foo<T>?`): the alias
    /// application selection recorded, its arguments bound as the selection bound them.
    fn declared_result_abbreviation(
        &self,
        origin: crate::fir::OriginId,
    ) -> Option<crate::fir::ResolvedTypeAbbreviation> {
        let alias = self.selected_calls.result_alias(origin)?;
        Some(crate::fir::ResolvedTypeAbbreviation::root(
            alias.alias(),
            alias.arguments(),
            alias.marked_nullable(),
        ))
    }

    /// The classifier `owner` applied to its own type parameters, as its members' declared
    /// signatures mention them.
    fn declaring_self(&self, owner: crate::types::TypeName) -> Option<Ty> {
        if let Some(class) = self.table.class_by_type_name(owner) {
            return Some(super::semantic_classifier_self(class));
        }
        let classifier =
            crate::symbol_source::SymbolSource::classifier(&*self.table.libraries, owner)?;
        let arguments = classifier
            .type_params()
            .iter()
            .enumerate()
            .map(|(ordinal, parameter)| {
                let bound = classifier
                    .type_param_bounds()
                    .get(ordinal)
                    .and_then(|bounds| bounds.first().copied())
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Ty::ty_param(parameter, bound)
            })
            .collect::<Vec<_>>();
        Some(Ty::obj_args_name(owner, &arguments))
    }

    /// The applied supertype of `actual` declared by `owner`, with its arguments: the hierarchy
    /// view publication binds declared parameters through.
    fn applied_supertype(
        &self,
        actual: Ty,
        owner: crate::types::TypeName,
    ) -> Option<(Ty, Vec<Ty>)> {
        let applied = self
            .table
            .applied_hierarchy(actual)
            .into_iter()
            .find(|(candidate, _, _)| *candidate == owner)
            .map(|(_, applied, _)| applied)?;
        let TyKind::Obj(applied_owner, arguments) = applied.non_null().kind() else {
            return None;
        };
        (applied_owner == owner).then(|| (applied, arguments.to_vec()))
    }

    pub(super) fn forget_selected_result_publication(&self, origin: crate::fir::OriginId) {
        self.selected_calls.forget_publication(origin);
    }

    pub(super) fn record_selected_result_publication(
        &self,
        origin: crate::fir::OriginId,
        publication: CompactSelectedResultPublication,
        inputs: SelectionInputs<'_>,
    ) {
        let alias = publication.applied_result_alias(
            inputs.receiver,
            inputs.arguments,
            |owner| self.declaring_self(owner),
            |actual, owner| self.applied_supertype(actual, owner),
        );
        self.selected_calls.record_result_alias(origin, alias);
        self.selected_calls.record_publication(origin, publication);
    }

    pub(super) fn selected_callable_publication(
        &self,
        callable: &crate::libraries::LibraryCallable,
        parameter_by_argument: Box<[Option<u32>]>,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        explicit_type_arguments: bool,
    ) -> CompactSelectedResultPublication {
        CompactSelectedResultPublication::callable(
            callable,
            parameter_by_argument,
            whole_array_arguments(arguments),
            explicit_type_arguments,
        )
    }

    pub(super) fn selected_member_publication(
        &self,
        member: &crate::libraries::LibraryMember,
        receiver: Option<Ty>,
        parameter_by_argument: Box<[Option<u32>]>,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        explicit_type_arguments: bool,
    ) -> CompactSelectedResultPublication {
        CompactSelectedResultPublication::member(
            member,
            receiver,
            parameter_by_argument,
            whole_array_arguments(arguments),
            explicit_type_arguments,
        )
    }

    pub(super) fn record_selected_constructor_publication(
        &self,
        origin: crate::fir::OriginId,
        declaration: &crate::symbol_resolver::SelectedConstructorDeclaration,
        selected: Ty,
        parameter_by_argument: Box<[Option<u32>]>,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        explicit_type_arguments: bool,
    ) {
        let Some(owner) = selected.non_null().obj_internal() else {
            return;
        };
        let parameters = declaration.declaration.generic_sig.as_ref().map_or_else(
            || declaration.declaration.params.clone(),
            |signature| signature.params.clone(),
        );
        let bounds = declaration
            .type_parameters
            .type_param_bounds
            .iter()
            .map(|bounds| {
                bounds
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
            })
            .collect();
        self.selected_calls.record_publication(
            origin,
            CompactSelectedResultPublication::constructor(
                owner,
                declaration.type_parameters.type_params.clone(),
                bounds,
                parameters,
                selected,
                parameter_by_argument,
                declaration.declaration.call_sig.vararg_index,
                whole_array_arguments(arguments),
                explicit_type_arguments,
            ),
        );
    }

    /// [`Self::record_final_selected_publication`] for a selection of a source declaration, whose
    /// result alias its published signature carries.
    pub(super) fn record_source_selected_publication(
        &self,
        origin: crate::fir::OriginId,
        publication: CompactSelectedResultPublication,
        signature: &crate::fir::ResolvedSignature,
        result: crate::fir::ResolvedTy,
        inputs: SelectionInputs<'_>,
    ) {
        let publication = publication.with_result_alias(signature.result_alias());
        self.record_final_selected_publication(origin, publication, result, inputs);
    }

    pub(super) fn record_final_selected_publication(
        &self,
        origin: crate::fir::OriginId,
        publication: CompactSelectedResultPublication,
        result: crate::fir::ResolvedTy,
        inputs: SelectionInputs<'_>,
    ) {
        let publication = publication.with_selected(result.get());
        self.record_selected_result_publication(origin, publication, inputs);
    }

    pub(super) fn finish_selected_member_publication(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: crate::fir::ResolvedTy,
        type_arguments: &[Ty],
        expected: Option<crate::fir::ResolvedTy>,
        result: Ty,
        member: Option<crate::libraries::LibraryMember>,
        source: Option<(u32, u32)>,
        source_declaration: Option<crate::fir::DeclarationId>,
        argument_types: Vec<Ty>,
        publication: CompactSelectedResultPublication,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedMemberCall, crate::fir::DiagnosticId> {
        let inputs = SelectionInputs {
            receiver: Some(receiver.get()),
            arguments: &argument_types,
        };
        if let Some(member) = member.as_ref() {
            if let Some(declaration) = member.stable_declaration {
                if self
                    .headers
                    .stub(declaration)
                    .is_some_and(|stub| stub.signature_inference.is_some())
                {
                    let signature = demand(declaration)?;
                    let parameters = signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    self.record_scoped_member_constraints(
                        scope,
                        receiver.get(),
                        member,
                        &parameters,
                        &argument_types,
                    );
                    let ty = self.apply_demanded_member_expecting(
                        receiver.get(),
                        member,
                        &signature,
                        &argument_types,
                        type_arguments,
                        expected.map(crate::fir::ResolvedTy::get),
                    )?;
                    self.record_source_selected_publication(
                        origin,
                        publication,
                        &signature,
                        ty,
                        inputs,
                    );
                    return Ok(crate::fir::ResolvedMemberCall {
                        ty: Some(ty),
                        declaration: Some(declaration),
                    });
                }
            }
            if let Some(signature) =
                self.demanded_member_signature(member.stable_declaration, demand)?
            {
                let parameters = signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                self.record_scoped_member_constraints(
                    scope,
                    receiver.get(),
                    member,
                    &parameters,
                    &argument_types,
                );
                let ty = self.apply_demanded_member_expecting(
                    receiver.get(),
                    member,
                    &signature,
                    &argument_types,
                    type_arguments,
                    expected.map(crate::fir::ResolvedTy::get),
                )?;
                self.record_source_selected_publication(
                    origin,
                    publication,
                    &signature,
                    ty,
                    inputs,
                );
                return Ok(crate::fir::ResolvedMemberCall {
                    ty: Some(ty),
                    declaration: member.stable_declaration,
                });
            }
            let parameters = member
                .generic_sig
                .as_ref()
                .map(|signature| signature.params.as_slice())
                .unwrap_or(&member.params);
            self.record_scoped_member_constraints(
                scope,
                receiver.get(),
                member,
                parameters,
                &argument_types,
            );
        }
        if let Some(source) = source {
            if let Some(signature) =
                self.demanded_source_signature(None, source_declaration, demand)?
            {
                let ty = self.apply_demanded_source_callable(
                    source,
                    Some(receiver.get()),
                    &signature,
                    &argument_types,
                    None,
                    type_arguments,
                    expected.map(crate::fir::ResolvedTy::get),
                )?;
                self.record_source_selected_publication(
                    origin,
                    publication,
                    &signature,
                    ty,
                    inputs,
                );
                return Ok(crate::fir::ResolvedMemberCall {
                    ty: Some(ty),
                    declaration: source_declaration,
                });
            }
        }
        let declaration = member.and_then(|member| member.stable_declaration);
        let ty = crate::fir::ResolvedTy::new(result).ok();
        if ty.is_none() && declaration.is_none() {
            return Err(Self::failure());
        }
        self.record_selected_result_publication(origin, publication, inputs);
        Ok(crate::fir::ResolvedMemberCall { ty, declaration })
    }
}

impl crate::fir::SelectedResultPublicationSemantics for ProductionSignatureSemantics<'_> {
    fn call_result_abbreviation(
        &self,
        origin: crate::fir::OriginId,
    ) -> Option<crate::fir::ResolvedTypeAbbreviation> {
        self.selected_calls
            .abbreviation(origin)
            .or_else(|| self.declared_result_abbreviation(origin))
    }

    fn published_call_result_abbreviation(
        &self,
        origin: crate::fir::OriginId,
        published: crate::fir::ResolvedTy,
    ) -> Option<crate::fir::ResolvedTypeAbbreviation> {
        let Some(abbreviation) = self.selected_calls.abbreviation(origin) else {
            return self.declared_result_abbreviation(origin);
        };
        let Some(inferred) = self.selected_calls.inferred_abbreviation(origin) else {
            return Some(abbreviation);
        };
        let mut bindings = crate::symbol_resolver::GSigBinds::default();
        crate::symbol_resolver::unify_ty(inferred.expansion, published.get(), &mut bindings);
        // A formal the published expansion leaves unbound keeps the argument ordinary selection
        // inferred for it.
        let arguments = inferred
            .formals
            .iter()
            .zip(abbreviation.arguments.iter())
            .map(|(formal, ordinary)| bindings.get(formal).copied().unwrap_or(*ordinary))
            .collect();
        Some(crate::fir::ResolvedTypeAbbreviation {
            arguments,
            ..abbreviation
        })
    }

    fn call_result_publication(
        &self,
        _scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Option<(crate::fir::ResolvedTy, crate::fir::ResolvedTy)>,
        arguments: &[(crate::fir::ResolvedTy, crate::fir::ResolvedTy)],
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let Some(publication) = self.selected_calls.publication(origin) else {
            return Ok(None);
        };
        let receiver = receiver.map(|(ordinary, published)| (ordinary.get(), published.get()));
        let arguments = arguments
            .iter()
            .map(|(ordinary, published)| (ordinary.get(), published.get()))
            .collect::<Vec<_>>();
        let published = publication.publish(receiver, &arguments, |ordinary, published, owner| {
            let ordinary = self
                .table
                .applied_hierarchy(ordinary)
                .into_iter()
                .find(|(candidate, _, _)| *candidate == owner)
                .map(|(_, applied, _)| applied)?;
            let published = self
                .table
                .applied_hierarchy(published)
                .into_iter()
                .find(|(candidate, _, _)| *candidate == owner)
                .map(|(_, applied, _)| applied)?;
            let TyKind::Obj(published_owner, arguments) = published.non_null().kind() else {
                return None;
            };
            (published_owner == owner).then_some((ordinary, arguments.to_vec()))
        });
        crate::fir::ResolvedTy::new(published)
            .map(Some)
            .map_err(|_| Self::failure())
    }
}
