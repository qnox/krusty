//! Pass-1 source-contract extraction and semantic publication.
//!
//! A source contract affects callers, so it is declaration data rather than an ordinary body. The
//! active parser unit reads the first statement's call and its description into a compact
//! temporary payload. The signature environment then selects that call, every description call,
//! and every invocation kind through ordinary overload and scope resolution, and decodes the
//! contract from the declarations they selected before it enters `ResolvedModuleIndex`. Spelling
//! is only lookup input; no parser id, source range, or unresolved `TypeRef` crosses into Pass 2.

use std::collections::HashMap;

use crate::ast::{Decl, Expr, File, FunBody, Stmt, TypeRef};
use crate::contracts::{
    CallBinding, Description, DescriptionBinder, DescriptionOwner, InvocationKind, KindBinding,
    TermId, TermKind,
};
use crate::symbol_resolver::{CallArgKind, CandidateSelection};
use crate::types::{Ty, TypeName};

use super::ProductionSignatureSemantics;

#[derive(Clone, Debug)]
pub(crate) struct SourceContractCandidate {
    declaration: crate::fir::DeclarationId,
    source: crate::fir::SourceFileId,
    /// The first statement's callee: `contract` or a package-qualified `kotlin.contracts.contract`.
    callee: Vec<String>,
    name: String,
    params: Vec<String>,
    param_types: Vec<TypeRef>,
    receiver: Option<TypeRef>,
    description: Description,
}

/// Read the first statement of each function while the bounded Pass-1 parser unit is active.
/// Whether that statement declares a contract is deliberately deferred: aliases, imports, module
/// shadowing, and provider identity belong to the signature resolver below, not to syntax.
pub(crate) fn extract_source_contract_candidates(
    file: &File,
    source: crate::fir::SourceFileId,
    stubs: &[crate::fir::DeclarationStub],
) -> Vec<SourceContractCandidate> {
    // A member function declares a contract as well as a top-level one does; kotlinc rejects one
    // only on an open or overriding member, which the contract checks report.
    file.decls
        .iter()
        .flat_map(|&parser_declaration| match file.decl(parser_declaration) {
            Decl::Fun(function) => std::slice::from_ref(function),
            Decl::Class(class) => class.methods.as_slice(),
            Decl::Property(_) => &[],
        })
        .filter_map(|function| {
            let FunBody::Block(body) = function.body else {
                return None;
            };
            let Expr::Block { stmts, trailing } = file.expr(body) else {
                return None;
            };
            // Kotlin requires the contract declaration to be the function's first statement. A
            // single trailing expression is the same first statement in the parser's block form.
            let expression = stmts
                .first()
                .and_then(|statement| match file.stmt(*statement) {
                    Stmt::Expr(expression) => Some(*expression),
                    _ => None,
                })
                .or_else(|| stmts.is_empty().then_some(*trailing).flatten())?;
            let Expr::Call { callee, .. } = file.expr(expression) else {
                return None;
            };
            let callee = callee_path(file, *callee)?;
            // kotlinc's raw FIR only offers a first statement spelled `contract` for binding; an
            // import alias of the intrinsic is an ordinary call and a misplaced contract.
            if callee.last().map(String::as_str) != Some("contract") {
                return None;
            }
            let description = Description::read(file, expression)?;
            let declaration = stubs
                .iter()
                .find(|stub| {
                    stub.source == source
                        && stub.kind == crate::fir::DeclarationKind::Function
                        && stub.range == function.span
                })?
                .id;
            Some(SourceContractCandidate {
                declaration,
                source,
                callee,
                name: function.name.clone(),
                params: function
                    .params
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect(),
                param_types: function
                    .params
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect(),
                receiver: function.receiver.clone(),
                description,
            })
        })
        .collect()
}

fn callee_path(file: &File, callee: crate::ast::ExprId) -> Option<Vec<String>> {
    match file.expr(callee) {
        Expr::Name(name) => Some(vec![name.clone()]),
        Expr::Member { receiver, name } => {
            let mut path = callee_path(file, *receiver)?;
            path.push(name.clone());
            Some(path)
        }
        _ => None,
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn resolve_source_contracts(
        &self,
        candidates: &[SourceContractCandidate],
    ) -> Result<
        Vec<(
            crate::fir::DeclarationId,
            crate::contracts::ResolvedContract,
        )>,
        Vec<crate::fir::DeclarationId>,
    > {
        let mut resolved = Vec::new();
        let mut failed = Vec::new();
        for candidate in candidates {
            let scope = crate::fir::SignatureScope {
                owner: candidate.declaration,
                source: candidate.source,
            };
            let Some(builder) = self.selected_contract_builder(scope, candidate) else {
                continue;
            };
            let resolve_type = |reference: &TypeRef| {
                self.with_signature_type_scope(scope, |lexical| {
                    self.signature_type_ref(scope, lexical, reference)
                })
                .ok()
                .flatten()
                .filter(|ty| !ty.mentions_pending() && !ty.mentions_error())
            };
            let mut binder = SignatureDescriptionBinder {
                semantics: self,
                scope,
                builder,
                params: &candidate.params,
                param_types: candidate.param_types.iter().map(resolve_type).collect(),
                receiver: candidate.receiver.as_ref().and_then(resolve_type),
                selections: HashMap::new(),
            };
            // A description kotlinc rejects declares no contract; Pass 2 reports its findings
            // from the checked body.
            let Some(contract) = candidate
                .description
                .decode(
                    &DescriptionOwner {
                        params: &candidate.params,
                        name: &candidate.name,
                        has_receiver: candidate.receiver.is_some(),
                    },
                    &mut binder,
                )
                .contract
            else {
                continue;
            };
            let contract = contract.with_resolved_types(&mut |reference| resolve_type(reference));
            match crate::contracts::ResolvedContract::new(contract) {
                Ok(contract) => resolved.push((candidate.declaration, contract)),
                Err(error) => {
                    crate::trace_compiler!(
                        "signature",
                        "source contract {:?} is not publishable: {error:?}",
                        candidate.declaration,
                    );
                    failed.push(candidate.declaration);
                }
            }
        }
        if failed.is_empty() {
            Ok(resolved)
        } else {
            failed.sort_by_key(|declaration| declaration.raw());
            failed.dedup();
            Err(failed)
        }
    }

    /// The `ContractBuilder` receiver of the contract intrinsic the candidate's first statement
    /// selects, or `None` when ordinary resolution selects anything else. The call's only argument
    /// is a lambda. A parameter or an implicit receiver's member outranks every top-level function;
    /// among those, overload selection picks one declaration, identified by its full signature.
    fn selected_contract_builder(
        &self,
        scope: crate::fir::SignatureScope,
        candidate: &SourceContractCandidate,
    ) -> Option<Ty> {
        let arguments = [CallArgKind::LambdaLiteral(Ty::Error)];
        let (name, qualifier) = candidate.callee.split_last()?;
        let selected = if qualifier.is_empty() {
            if candidate.params.contains(name) {
                return None;
            }
            for receiver in self.implicit_receivers(scope) {
                let member = self.with_resolver(scope, |resolver| {
                    let callables = resolver.receiver_callables(receiver, name);
                    resolver.select_receiver_function_with_params(
                        receiver,
                        name,
                        &arguments,
                        &[],
                        &callables,
                    )
                });
                if member.is_ok() {
                    return None;
                }
            }
            self.with_resolver(scope, |resolver| {
                resolver.select_top_level_function_candidates(
                    name,
                    resolver.accessible_top_level_candidates(name),
                    &arguments,
                    &[],
                )
            })
        } else {
            self.with_qualified_package_resolver(scope, &qualifier.join("."), |_, resolver| {
                resolver
                    .select_top_level_function_candidates(
                        name,
                        resolver.accessible_top_level_candidates(name),
                        &arguments,
                        &[],
                    )
                    .ok_or_else(Self::failure)
            })
        };
        let (_, callable) = selected.ok()?;
        if !self.table.libraries.is_erased_contract_callable(&callable) {
            return None;
        }
        match callable.params.first() {
            Some(Ty::Fun(builder)) => builder.params.first().copied(),
            _ => None,
        }
    }
}

/// One selected description call: the declaration's owner, name, kind, and specialized shape.
#[derive(Clone)]
struct SelectedDslCall {
    owner: TypeName,
    name: String,
    kind: crate::libraries::FnKind,
    context_parameters: usize,
    /// The value parameter types, context parameters excluded.
    params: Vec<Ty>,
    ret: Ty,
}

impl SelectedDslCall {
    fn callable(&self) -> crate::contracts::SelectedDslCallable<'_> {
        crate::contracts::SelectedDslCallable {
            owner: self.owner,
            name: &self.name,
            dispatch_member: self.kind == crate::libraries::FnKind::Member,
            context_parameters: self.context_parameters,
            params: &self.params,
            ret: self.ret,
        }
    }
}

/// Binds a description in the declaring function's signature scope, where the lambda's implicit
/// receiver is the selected contract's builder.
struct SignatureDescriptionBinder<'s, 'a> {
    semantics: &'s ProductionSignatureSemantics<'a>,
    scope: crate::fir::SignatureScope,
    builder: Ty,
    params: &'s [String],
    param_types: Vec<Option<Ty>>,
    receiver: Option<Ty>,
    selections: HashMap<TermId, Option<SelectedDslCall>>,
}

impl SignatureDescriptionBinder<'_, '_> {
    fn select(&mut self, description: &Description, call: TermId) -> Option<SelectedDslCall> {
        if let Some(selected) = self.selections.get(&call) {
            return selected.clone();
        }
        let selected = self.select_uncached(description, call);
        self.selections.insert(call, selected.clone());
        selected
    }

    fn select_uncached(
        &mut self,
        description: &Description,
        call: TermId,
    ) -> Option<SelectedDslCall> {
        let TermKind::Call {
            receiver,
            name,
            args,
            ..
        } = &description.term(call).kind
        else {
            return None;
        };
        let receiver = match receiver {
            None => self.builder,
            Some(receiver) => self.select(description, *receiver)?.ret,
        };
        let arguments = args
            .iter()
            .map(|argument| CallArgKind::Typed(self.argument_type(description, *argument)))
            .collect::<Vec<_>>();
        self.semantics
            .with_resolver(self.scope, |resolver| {
                let callables = resolver.receiver_callables(receiver, name);
                match resolver.select_receiver_function_with_params_tracking(
                    receiver,
                    name,
                    &arguments,
                    &[],
                    &callables,
                    None,
                ) {
                    CandidateSelection::Selected((function, params, ret)) => {
                        Some(SelectedDslCall {
                            owner: function.callable.owner,
                            name: function.callable.name.clone(),
                            kind: function.kind,
                            context_parameters: function.context_count,
                            params: params.get(function.context_count..)?.to_vec(),
                            ret,
                        })
                    }
                    CandidateSelection::None | CandidateSelection::Ambiguous => None,
                }
            })
            .ok()
    }

    /// The type an argument of a description call has in the declaring function's scope.
    fn argument_type(&mut self, description: &Description, argument: TermId) -> Ty {
        match &description.term(argument).kind {
            TermKind::Bool(_)
            | TermKind::Is { .. }
            | TermKind::Equality { .. }
            | TermKind::And(..)
            | TermKind::Or(..) => Ty::Boolean,
            TermKind::Null => Ty::Null,
            TermKind::Call { .. } => self
                .select(description, argument)
                .map_or(Ty::Error, |selected| selected.ret),
            TermKind::Name(segments) => match segments.as_slice() {
                [name] if name == "this" || name.starts_with("this@") => {
                    self.receiver.unwrap_or(Ty::Error)
                }
                [name] if self.params.contains(name) => self
                    .params
                    .iter()
                    .position(|param| param == name)
                    .and_then(|index| self.param_types[index])
                    .unwrap_or(Ty::Error),
                _ => self
                    .selected_value(segments)
                    .map_or(Ty::Error, |selected| selected.result.get()),
            },
            TermKind::Other => Ty::Error,
        }
    }

    /// The value a description name selects in the declaring function's scope, through the
    /// ordinary value tower: a parameter root is a value, so its path names no classifier member.
    /// Contracts are bound before any inferred signature exists, so a value that would need one
    /// is not selected; Pass 2 reports what the checked body finds, so this records nothing.
    fn selected_value(&self, segments: &[String]) -> Option<super::SelectedValue> {
        if segments
            .first()
            .is_none_or(|root| self.params.contains(root))
        {
            return None;
        }
        let semantics = self.semantics;
        let recorded = semantics.diagnostics.borrow().len();
        let origin = semantics
            .signature_origins
            .get(&self.scope.owner)
            .copied()
            .unwrap_or(crate::fir::OriginId::from_raw(0));
        let selected = semantics.select_value_candidate(
            self.scope,
            &segments.join("."),
            origin,
            None,
            &mut |_| Err(ProductionSignatureSemantics::failure()),
        );
        semantics.diagnostics.borrow_mut().truncate(recorded);
        selected.ok()
    }
}

impl DescriptionBinder for SignatureDescriptionBinder<'_, '_> {
    fn bind_call(&mut self, description: &Description, call: TermId) -> CallBinding {
        let Some(selected) = self.select(description, call) else {
            return CallBinding::Unresolved;
        };
        let member = self
            .semantics
            .table
            .libraries
            .contract_dsl_member(&selected.callable());
        // Pass 1 only decides whether a contract exists; Pass 2 names a foreign declaration.
        member.map_or(CallBinding::Foreign(String::new()), CallBinding::Dsl)
    }

    fn bind_kind(&mut self, description: &Description, call: TermId, kind: TermId) -> KindBinding {
        let Some(expected) = self
            .select(description, call)
            .and_then(|selected| selected.params.get(1).copied())
            .and_then(Ty::kotlin_class_internal)
        else {
            return KindBinding::Unresolved;
        };
        let TermKind::Name(segments) = &description.term(kind).kind else {
            return KindBinding::Foreign(String::new());
        };
        match self
            .selected_value(segments)
            .and_then(|selected| selected.enum_entry)
        {
            Some(entry) if entry.classifier == expected => InvocationKind::of_entry(&entry.name)
                .map_or(KindBinding::Foreign(String::new()), KindBinding::Entry),
            _ => KindBinding::Foreign(String::new()),
        }
    }
}
