//! Temporary projection from compact pass-1 headers into the legacy semantic table.
//!
//! This module disappears when signature expressions are evaluated directly by the ordinary
//! resolver. It must not grow body checking or lowering responsibilities.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::{ClassDecl, Decl, DeclId, File, FunBody, FunDecl, PropDecl, TypeRef};
use crate::diag::Span;
use crate::types::{Ty, TypeName};

use super::{ClassSig, Signature, SymbolTable};

mod callable_references;
mod declaration_aliases;
mod declaration_conflicts;
mod declaration_spellings;
mod header_projection;
mod local_signatures;
mod lookups;
mod postponed_calls;
mod qualified_calls;
mod semantics;
mod source_contracts;

pub(super) use declaration_aliases::publish_compact_nested_aliases;
pub(crate) use declaration_conflicts::finalize_streamed_top_level_conflicts;
pub(super) use declaration_spellings::collect_compact_declared_spellings;
pub(super) use header_projection::*;

#[cfg(test)]
pub(crate) use local_signatures::publish_checked_local_signatures_in_active_root;
pub(crate) use local_signatures::{
    publish_checked_default_local_signatures, publish_checked_inline_local_signatures,
    publish_checked_local_signatures_in_pass_two_root,
};
#[cfg(test)]
pub(crate) use local_signatures::{
    publish_checked_local_signatures, publish_discovered_local_capture_declarations,
};
pub(crate) use source_contracts::{extract_source_contract_candidates, SourceContractCandidate};

enum SelectedTopLevelCall {
    Callable {
        callable: crate::libraries::LibraryCallable,
        source: Option<(u32, u32)>,
        declaration: Option<crate::fir::DeclarationId>,
    },
    Value(crate::libraries::PropertyInfo),
    /// Runtime value denoted by a classifier name (an object singleton or companion). Call syntax
    /// applies the ordinary `invoke` convention to this value before considering construction.
    ClassifierValue(Ty),
    Constructor(crate::libraries::LibraryMember),
    /// A fun-interface name applied to one function value (`I { … }`). The interface declares no
    /// constructor, so this is not a `Constructor` selection — the result is the interface itself.
    SamConstructor(crate::types::TypeName),
}

struct ProductionSignatureSemantics<'a> {
    headers: &'a crate::fir::StreamedHeaderModule,
    table: &'a SymbolTable,
    classifier_types: &'a HashMap<crate::fir::DeclarationId, crate::types::TypeName>,
    parameters: HashMap<crate::fir::DeclarationId, Box<[crate::fir::ResolvedTy]>>,
    extension_receivers: &'a HashMap<crate::fir::DeclarationId, crate::fir::ResolvedTy>,
    source_orders: HashMap<crate::fir::DeclarationId, u32>,
    signature_origins: HashMap<crate::fir::DeclarationId, crate::fir::OriginId>,
    scoped_receivers: RefCell<HashMap<crate::fir::DeclarationId, Vec<Ty>>>,
    scoped_constraint_inputs: RefCell<HashMap<crate::fir::DeclarationId, Vec<Vec<Ty>>>>,
    scoped_constraints:
        RefCell<HashMap<crate::fir::DeclarationId, Vec<crate::symbol_resolver::GSigBinds>>>,
    completed_scoped_constraints:
        RefCell<HashMap<crate::fir::DeclarationId, crate::symbol_resolver::GSigBinds>>,
    diagnostics: RefCell<Vec<ProductionSignatureDiagnostic>>,
}

#[derive(Clone)]
struct ProductionSignatureDiagnostic {
    declaration: crate::fir::DeclarationId,
    file: u32,
    span: Span,
    message: String,
}

struct ExplicitContextCall {
    candidates: Vec<crate::libraries::FunctionInfo>,
    arguments: Vec<crate::symbol_resolver::CallArgKind>,
    argument_types: Vec<Ty>,
}

const RECURSIVE_INFERENCE_MESSAGE: &str = "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.";

impl ProductionSignatureSemantics<'_> {
    fn failure() -> crate::fir::DiagnosticId {
        crate::fir::DiagnosticId::from_raw(0)
    }

    fn classifier_signature(&self, declaration: crate::fir::DeclarationId) -> Option<&ClassSig> {
        self.classifier_types
            .get(&declaration)
            .and_then(|classifier| self.table.class_by_type_name(*classifier))
    }

    fn enclosing_constructor_parameter(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<Ty> {
        let declaration = self.headers.declarations.anchor(scope.owner)?;
        // A primary-constructor parameter is a lexical value while class property initializers are
        // evaluated. Member functions and accessors see only a generated property when the
        // parameter is declared `val`/`var`; exposing the raw parameter there loses the stable
        // property target and, for bounded type parameters, their member-resolution semantics.
        if declaration.kind != crate::fir::DeclarationKind::Property {
            return None;
        }
        let owner = declaration.owner.filter(|owner| {
            self.headers
                .declarations
                .anchor(*owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::Classifier)
        })?;
        let signature = self.classifier_signature(owner)?;
        signature
            .ctor_param_names
            .iter()
            .position(|(candidate, _)| candidate == spelling)
            .and_then(|index| signature.ctor_params.get(index).copied())
    }

    fn demanded_enclosing_constructor_parameter(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<Ty>, crate::fir::DiagnosticId> {
        let declaration = match self.headers.declarations.anchor(scope.owner) {
            Some(declaration) if declaration.kind == crate::fir::DeclarationKind::Property => {
                declaration
            }
            Some(_) | None => return Ok(None),
        };
        let Some(owner) = declaration.owner.filter(|owner| {
            self.headers
                .declarations
                .anchor(*owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::Classifier)
        }) else {
            return Ok(None);
        };
        let Some(signature) = self.classifier_signature(owner) else {
            return Ok(None);
        };
        let Some(index) = signature
            .ctor_param_names
            .iter()
            .position(|(candidate, _)| candidate == spelling)
        else {
            return Ok(None);
        };
        let primary = self.headers.stubs.iter().find(|stub| {
            stub.kind == crate::fir::DeclarationKind::Constructor
                && self
                    .headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(owner) && anchor.sibling == 0)
        });
        if let Some(primary) = primary {
            let signature = demand(primary.id)?;
            if let Some(parameter) = signature.parameters.get(index) {
                return Ok(Some(parameter.get()));
            }
        }
        Ok(signature.ctor_params.get(index).copied())
    }

    /// Stable declaration selected by an enum-entry property's own member scope.
    ///
    /// Entry-body properties belong to the anonymous entry subclass rather than the parent enum's
    /// nominal member table. Compact signature evaluation therefore follows the header ownership
    /// edge directly and demands the selected declaration, preserving lazy dependencies such as
    /// `override val value = value3` without retaining the entry AST.
    fn enclosing_enum_entry_property(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::fir::DeclarationId> {
        let entry = self.enclosing_enum_entry(scope)?;
        self.headers.stubs.iter().find_map(|stub| {
            let anchor = self.headers.declarations.anchor(stub.id)?;
            (stub.kind == crate::fir::DeclarationKind::Property
                && anchor.owner == Some(entry)
                && stub
                    .lookup_name
                    .and_then(|name| self.headers.lookup_names.get(name))
                    == Some(spelling))
            .then_some(stub.id)
        })
    }

    fn enclosing_enum_entry_callables(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Vec<crate::fir::DeclarationId> {
        let Some(entry) = self.enclosing_enum_entry(scope) else {
            return Vec::new();
        };
        self.headers
            .stubs
            .iter()
            .filter_map(|stub| {
                let anchor = self.headers.declarations.anchor(stub.id)?;
                (stub.kind == crate::fir::DeclarationKind::Function
                    && anchor.owner == Some(entry)
                    && stub
                        .lookup_name
                        .and_then(|name| self.headers.lookup_names.get(name))
                        == Some(spelling))
                .then_some(stub.id)
            })
            .collect()
    }

    fn enclosing_enum_entry(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Option<crate::fir::DeclarationId> {
        let mut owner = Some(scope.owner);
        loop {
            let declaration = owner?;
            let anchor = self.headers.declarations.anchor(declaration)?;
            if anchor.kind == crate::fir::DeclarationKind::EnumEntry {
                return Some(declaration);
            }
            owner = anchor.owner;
        }
    }

    /// Source receiver denoted by an enclosing enum-entry label (`this@X`). The entry itself has no
    /// classifier type in headers; its parent enum is the semantic receiver retained by signatures.
    fn enclosing_enum_entry_receiver(
        &self,
        scope: crate::fir::SignatureScope,
        label: &str,
    ) -> Option<Ty> {
        let entry = self.enclosing_enum_entry(scope)?;
        let stub = self.headers.stubs.iter().find(|stub| stub.id == entry)?;
        if stub
            .lookup_name
            .and_then(|name| self.headers.lookup_names.get(name))
            != Some(label)
        {
            return None;
        }
        let owner = self.headers.declarations.anchor(entry)?.owner?;
        self.classifier_types.get(&owner).copied().map(Ty::obj_name)
    }

    fn contextual_callable_reference_type(
        &self,
        scope: crate::fir::SignatureScope,
        natural_parameters: &[Ty],
        result: Ty,
        expected: Option<crate::fir::ResolvedTy>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let Some(expected) = expected else {
            return Ok(None);
        };
        let Ty::Fun(expected) = expected.get().non_null() else {
            return Err(Self::failure());
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let context = crate::assignable::TyCtx::new();
        // A SAM/callable candidate can supply a still-open declaration type parameter as the
        // contextual shape. That parameter is a constraint slot, not a concrete target against
        // which the referenced declaration must already be assignable. Materialize those open
        // slots from the reference's natural signature before compatibility; the resulting
        // function then carries the evidence used to infer the enclosing call.
        let mut parameters = expected.params.clone();
        for (parameter, natural) in parameters.iter_mut().zip(natural_parameters) {
            if matches!(parameter.non_null(), Ty::TyParam(..)) {
                *parameter = *natural;
            }
        }
        let result = if matches!(expected.ret.non_null(), Ty::TyParam(..)) {
            result
        } else {
            expected.ret
        };
        let Ty::Fun(contextual) = Ty::fun_with_shape(
            parameters.clone(),
            result,
            expected.context_count,
            expected.has_receiver,
            expected.suspend,
        ) else {
            unreachable!("a contextual callable-reference shape is a function type")
        };
        if !super::callable_reference_selection::is_compatible(
            natural_parameters,
            result,
            false,
            contextual,
            true,
            |actual, target| crate::assignable::is_subtype(&context, &oracle, actual, target),
        ) {
            return Err(Self::failure());
        };
        crate::fir::ResolvedTy::new(Ty::fun_with_shape(
            parameters,
            result,
            expected.context_count,
            expected.has_receiver,
            expected.suspend,
        ))
        .map(Some)
        .map_err(|_| Self::failure())
    }

    fn callable_signature(&self, declaration: crate::fir::DeclarationId) -> Option<&Signature> {
        self.table
            .funs
            .values()
            .flatten()
            .chain(
                self.table
                    .ext_funs
                    .values()
                    .flat_map(HashMap::values)
                    .flatten(),
            )
            .chain(
                self.table
                    .classes
                    .values()
                    .flat_map(|class| class.methods.values().flatten()),
            )
            .chain(
                self.table
                    .classes
                    .values()
                    .flat_map(|class| class.member_ext_funs.values().flatten())
                    .map(|function| function.signature()),
            )
            .find(|signature| signature.stable_declaration == Some(declaration))
    }

    fn declaration_extension_receiver(&self, declaration: crate::fir::DeclarationId) -> Option<Ty> {
        self.callable_signature(declaration)
            .and_then(|signature| signature.source_receiver)
            .or_else(|| {
                self.table
                    .ext_props
                    .values()
                    .flatten()
                    .find(|property| property.stable_declaration == Some(declaration))
                    .map(|property| property.receiver)
            })
            .or_else(|| {
                self.table
                    .classes
                    .values()
                    .flat_map(|class| class.member_ext_props.values().flatten())
                    .find(|property| property.stable_declaration() == Some(declaration))
                    .map(|property| property.receiver_ty())
            })
    }

    /// The `thisRef` passed to delegated-property conventions belongs to the property declaration,
    /// not to the last receiver in its scope tower. An extension property's receiver wins; an
    /// ordinary member uses its nearest classifier owner; a top-level property has a null receiver.
    fn delegate_this_ref(&self, declaration: crate::fir::DeclarationId) -> Ty {
        if let Some(receiver) = self.extension_receivers.get(&declaration) {
            return receiver.get();
        }
        let mut owner = self
            .headers
            .declarations
            .anchor(declaration)
            .and_then(|anchor| anchor.owner);
        while let Some(declaration) = owner {
            let Some(anchor) = self.headers.declarations.anchor(declaration) else {
                break;
            };
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                return self
                    .classifier_signature(declaration)
                    .map(semantic_classifier_self)
                    .unwrap_or(Ty::Null);
            }
            owner = anchor.owner;
        }
        Ty::Null
    }

    fn header_type_parameters(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> &[crate::fir::HeaderTypeParameter] {
        let Some(header) = self.headers.syntax.declaration(declaration) else {
            return &[];
        };
        let range = match header.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::Property {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::Classifier {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters, ..
            } => type_parameters,
            crate::fir::HeaderDeclarationKind::Constructor { .. } => return &[],
        };
        self.headers.syntax.type_parameters(range)
    }

    fn header_classifier_captures(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> &[crate::fir::HeaderTypeParameter] {
        let Some(header) = self.headers.syntax.declaration(declaration) else {
            return &[];
        };
        let crate::fir::HeaderDeclarationKind::Classifier {
            lexical_type_parameter_captures,
            ..
        } = header.kind
        else {
            return &[];
        };
        self.headers
            .syntax
            .type_parameters(lexical_type_parameter_captures)
    }

    fn declare_semantic_type_parameters(
        &self,
        scope: &super::CheckerScope<'_>,
        declaration: crate::fir::DeclarationId,
        semantic_names: &[String],
        semantic_bounds: &[Vec<Ty>],
    ) {
        let packed = self.header_type_parameters(declaration);
        let mut parameters = super::TParams::default();
        let mut source_names = Vec::with_capacity(packed.len());
        for (ordinal, parameter) in packed.iter().enumerate() {
            let Some(source_name) = self.headers.lookup_names.get(parameter.name) else {
                continue;
            };
            let semantic_name = semantic_names
                .get(ordinal)
                .map(String::as_str)
                .unwrap_or(source_name);
            let bounds = semantic_bounds
                .get(ordinal)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let bound = bounds
                .first()
                .copied()
                .filter(|bound| !bound.mentions_error())
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            parameters.insert_binding(
                source_name,
                Ty::ty_param(semantic_name, bound),
                bounds.iter().copied().skip(1).collect(),
            );
            source_names.push(source_name.to_string());
        }
        scope.declare_tparams(&source_names, &parameters, |source_name| {
            packed.iter().any(|parameter| {
                parameter.flags.is_reified()
                    && self.headers.lookup_names.get(parameter.name) == Some(source_name)
            })
        });
    }

    fn record_recursive_inference(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> crate::fir::DiagnosticId {
        let mut diagnostics = self.diagnostics.borrow_mut();
        if let Some(index) = diagnostics
            .iter()
            .position(|diagnostic| diagnostic.declaration == declaration)
        {
            return crate::fir::DiagnosticId::from_raw(index as u32 + 1);
        }
        let Some(stub) = self
            .headers
            .stubs
            .iter()
            .find(|stub| stub.id == declaration)
        else {
            return Self::failure();
        };
        let span = self
            .signature_origins
            .get(&declaration)
            .and_then(|origin| match self.headers.signature_origins.get(*origin) {
                Some(crate::fir::Origin::Source { span, .. }) => Some(span),
                Some(crate::fir::Origin::Synthetic { cause, .. }) => {
                    match self.headers.signature_origins.get(cause) {
                        Some(crate::fir::Origin::Source { span, .. }) => Some(span),
                        Some(crate::fir::Origin::Synthetic { .. }) | None => None,
                    }
                }
                None => None,
            })
            .unwrap_or(stub.range);
        diagnostics.push(ProductionSignatureDiagnostic {
            declaration,
            file: stub.source.raw(),
            span,
            message: RECURSIVE_INFERENCE_MESSAGE.to_string(),
        });
        crate::fir::DiagnosticId::from_raw(diagnostics.len() as u32)
    }

    fn record_eager_forward_reference(
        &self,
        owner: crate::fir::DeclarationId,
        target: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
    ) -> crate::fir::DiagnosticId {
        let Some(stub) = self.headers.stubs.iter().find(|stub| stub.id == target) else {
            return Self::failure();
        };
        let spelling = stub
            .lookup_name
            .and_then(|name| self.headers.lookup_names.get(name))
            .unwrap_or("<unknown>");
        let (file, span) = match self.headers.signature_origins.get(origin) {
            Some(crate::fir::Origin::Source { file, span }) => (file.raw(), span),
            Some(crate::fir::Origin::Synthetic { cause, .. }) => {
                match self.headers.signature_origins.get(cause) {
                    Some(crate::fir::Origin::Source { file, span }) => (file.raw(), span),
                    Some(crate::fir::Origin::Synthetic { .. }) | None => {
                        (stub.source.raw(), stub.range)
                    }
                }
            }
            None => (stub.source.raw(), stub.range),
        };
        let message = format!("variable '{spelling}' must be initialized.");
        let mut diagnostics = self.diagnostics.borrow_mut();
        if let Some(index) = diagnostics.iter().position(|diagnostic| {
            diagnostic.file == file && diagnostic.span == span && diagnostic.message == message
        }) {
            return crate::fir::DiagnosticId::from_raw(index as u32 + 1);
        }
        diagnostics.push(ProductionSignatureDiagnostic {
            declaration: owner,
            file,
            span,
            message,
        });
        crate::fir::DiagnosticId::from_raw(diagnostics.len() as u32)
    }

    fn record_source_diagnostic(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        message: String,
    ) -> crate::fir::DiagnosticId {
        let fallback = self
            .headers
            .stubs
            .iter()
            .find(|stub| stub.id == declaration)
            .map(|stub| (stub.source.raw(), stub.range));
        let location = match self.headers.signature_origins.get(origin) {
            Some(crate::fir::Origin::Source { file, span }) => Some((file.raw(), span)),
            Some(crate::fir::Origin::Synthetic { cause, .. }) => {
                match self.headers.signature_origins.get(cause) {
                    Some(crate::fir::Origin::Source { file, span }) => Some((file.raw(), span)),
                    Some(crate::fir::Origin::Synthetic { .. }) | None => None,
                }
            }
            None => None,
        }
        .or(fallback);
        let Some((file, span)) = location else {
            return Self::failure();
        };
        let mut diagnostics = self.diagnostics.borrow_mut();
        if let Some(index) = diagnostics.iter().position(|diagnostic| {
            diagnostic.file == file && diagnostic.span == span && diagnostic.message == message
        }) {
            return crate::fir::DiagnosticId::from_raw(index as u32 + 1);
        }
        diagnostics.push(ProductionSignatureDiagnostic {
            declaration,
            file,
            span,
            message,
        });
        crate::fir::DiagnosticId::from_raw(diagnostics.len() as u32)
    }

    fn record_unresolved_reference(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        self.record_source_diagnostic(
            declaration,
            origin,
            format!("unresolved reference '{spelling}'."),
        )
    }

    fn record_unresolved_reference_at(
        &self,
        declaration: crate::fir::DeclarationId,
        source: crate::fir::SourceFileId,
        span: Span,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        self.record_source_diagnostic_at(
            declaration,
            source,
            span,
            format!("unresolved reference '{spelling}'."),
        )
    }

    fn record_top_level_context_call_failure(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
    ) -> Option<crate::fir::DiagnosticId> {
        let file = self.headers.scopes.file(scope.source)?;
        let names = arguments
            .iter()
            .filter_map(|argument| argument.name)
            .collect::<std::collections::HashSet<_>>();
        let candidates = self
            .with_resolver(scope, |resolver| {
                Some(resolver.top_level_candidates(spelling))
            })
            .ok()?;
        if !file.explicit_context_arguments && !names.is_empty() {
            if let Some((candidate, parameter, name)) = candidates.iter().find_map(|candidate| {
                let context_count = candidate
                    .context_count
                    .min(candidate.semantic_params().len());
                candidate
                    .call_sig
                    .param_names
                    .iter()
                    .take(context_count)
                    .enumerate()
                    .find(|(_, parameter)| names.contains(parameter.as_str()))
                    .map(|(parameter, name)| (candidate, parameter, name.as_str()))
            }) {
                let implicit_available = !self
                    .implicit_context_candidates(scope, [candidate.clone()])
                    .is_empty();
                let first = if implicit_available {
                    None
                } else {
                    let ty = candidate.semantic_params()[parameter].source_name();
                    Some(self.record_source_diagnostic(
                        scope.owner,
                        origin,
                        format!("no context argument for '{name}: {ty}' found."),
                    ))
                };
                let named = self.record_source_diagnostic(
                    scope.owner,
                    origin,
                    format!("no parameter with name '{name}' found."),
                );
                return Some(first.unwrap_or(named));
            }
        }

        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self.implicit_receivers(scope);
        for candidate in candidates {
            let mut signature = candidate.semantic_signature().into_owned();
            let context_count = candidate.context_count.min(signature.params.len());
            if context_count == 0 {
                continue;
            }
            let visible = (context_count..signature.params.len()).collect::<Vec<_>>();
            let mut projected = candidate.clone();
            projected.call_sig = super::call_sig_for_parameters(&candidate.call_sig, &visible);
            projected.context_count = 0;
            signature.params = signature.params[context_count..].to_vec();
            projected.generic_sig = Some(signature);
            if Self::mapped_call_arguments(&[projected], arguments, trailing_lambda).is_none() {
                continue;
            }
            let parameters = candidate.semantic_params();
            let missing = parameters[..context_count]
                .iter()
                .enumerate()
                .find(|(_, parameter)| {
                    super::context_argument_types(
                        &receivers,
                        std::slice::from_ref(parameter),
                        &crate::symbol_resolver::SourceOracle(&source),
                    )
                    .is_none()
                });
            let Some((parameter, ty)) = missing else {
                continue;
            };
            let name = candidate
                .call_sig
                .param_names
                .get(parameter)
                .filter(|name| !name.is_empty())
                .map(String::as_str)
                .unwrap_or("context");
            return Some(self.record_source_diagnostic(
                scope.owner,
                origin,
                format!(
                    "no context argument for '{name}: {}' found.",
                    ty.source_name()
                ),
            ));
        }
        None
    }

    fn record_source_diagnostic_at(
        &self,
        declaration: crate::fir::DeclarationId,
        source: crate::fir::SourceFileId,
        span: Span,
        message: String,
    ) -> crate::fir::DiagnosticId {
        let file = source.raw();
        let mut diagnostics = self.diagnostics.borrow_mut();
        if let Some(index) = diagnostics.iter().position(|diagnostic| {
            diagnostic.file == file && diagnostic.span == span && diagnostic.message == message
        }) {
            return crate::fir::DiagnosticId::from_raw(index as u32 + 1);
        }
        diagnostics.push(ProductionSignatureDiagnostic {
            declaration,
            file,
            span,
            message,
        });
        crate::fir::DiagnosticId::from_raw(diagnostics.len() as u32)
    }

    /// Return a diagnostic already produced while resolving one component of this compact type.
    /// A failed nested alias application deliberately returns `Ty::Error` through the recursive
    /// type builder; the outer `resolve_type` must preserve that precise diagnostic instead of
    /// replacing it with a generic unresolved-reference report for the enclosing classifier.
    fn recorded_type_diagnostic(
        &self,
        declaration: crate::fir::DeclarationId,
        source: crate::fir::SourceFileId,
        enclosing: Span,
    ) -> Option<crate::fir::DiagnosticId> {
        self.diagnostics
            .borrow()
            .iter()
            .enumerate()
            .rev()
            .find(|(_, diagnostic)| {
                diagnostic.declaration == declaration
                    && diagnostic.file == source.raw()
                    && enclosing.lo <= diagnostic.span.lo
                    && diagnostic.span.hi <= enclosing.hi
            })
            .map(|(index, _)| crate::fir::DiagnosticId::from_raw(index as u32 + 1))
    }

    fn record_ambiguous_member(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        self.record_source_diagnostic(
            declaration,
            origin,
            format!("overload resolution ambiguity for member '{spelling}'"),
        )
    }

    fn record_inapplicable_member_call(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        spelling: &str,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> crate::fir::DiagnosticId {
        let mut displays = candidates
            .iter()
            .map(|candidate| {
                let parameters = candidate
                    .semantic_params()
                    .iter()
                    .enumerate()
                    .map(|(ordinal, parameter)| {
                        let name = candidate
                            .call_sig
                            .param_names
                            .get(ordinal)
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| format!("p{ordinal}"));
                        format!("{name}: {}", parameter.source_name())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "fun {spelling}({parameters}): {}",
                    candidate.callable.ret.source_name()
                )
            })
            .collect::<Vec<_>>();
        displays.sort_unstable();
        displays.dedup();
        displays.truncate(64);
        let mut message = String::from("none of the following candidates is applicable:");
        for (ordinal, display) in displays.into_iter().enumerate() {
            message.push_str(if ordinal == 0 { "\n\n" } else { "\n" });
            message.push_str(&display);
        }
        self.record_source_diagnostic(declaration, origin, message)
    }

    fn record_member_call_selection_failure(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        let candidates = self
            .with_resolver(scope, |resolver| {
                Some(
                    resolver
                        .receiver_callables(receiver, spelling)
                        .functions()
                        .to_vec(),
                )
            })
            .unwrap_or_default();
        if candidates.is_empty() {
            self.record_unresolved_reference(scope.owner, origin, spelling)
        } else {
            self.record_inapplicable_member_call(scope.owner, origin, spelling, &candidates)
        }
    }

    fn with_resolver<T>(
        &self,
        scope: crate::fir::SignatureScope,
        select: impl FnOnce(&crate::symbol_resolver::SymbolResolver<'_>) -> Option<T>,
    ) -> Result<T, crate::fir::DiagnosticId> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let imports = self.function_import_scope(scope.source)?;
        let file = self
            .headers
            .scopes
            .file(scope.source)
            .ok_or_else(Self::failure)?;
        let package = self
            .headers
            .scopes
            .path(file.package)
            .iter()
            .map(|segment| self.headers.lookup_names.get(*segment))
            .collect::<Option<Vec<_>>>()
            .map(|segments| {
                if segments.is_empty() {
                    crate::types::TypeName::ROOT
                } else {
                    crate::types::type_name(&segments.join("/"))
                }
            })
            .ok_or_else(Self::failure)?;
        let mut lexical_classes = Vec::new();
        let mut owner = Some(scope.owner);
        while let Some(declaration) = owner {
            if let Some(classifier) = self.classifier_types.get(&declaration).copied() {
                lexical_classes.push(classifier);
            }
            owner = self
                .headers
                .declarations
                .anchor(declaration)
                .and_then(|anchor| anchor.owner);
        }
        let resolver = crate::symbol_resolver::SymbolResolver::new_import_scoped_with_module(
            self.table.libraries.as_ref(),
            &module,
            &imports,
        )
        .with_access_context(package, scope.source.raw(), lexical_classes);
        select(&resolver).ok_or_else(Self::failure)
    }

    fn classifier_is_singleton(&self, classifier: crate::types::TypeName) -> bool {
        self.table
            .classes
            .get(&classifier)
            .is_some_and(ClassSig::is_object)
            || self
                .table
                .libraries
                .classifier(classifier)
                .is_some_and(|declaration| declaration.is_object())
    }

    fn classifier_is_enum(&self, classifier: crate::types::TypeName) -> bool {
        self.table.enum_entries_of(classifier).is_some()
            || self
                .table
                .libraries
                .classifier(classifier)
                .is_some_and(|declaration| declaration.is_enum())
    }

    /// An enum-entry body is an anonymous subclass scope whose source receiver remains the enum
    /// type. If the declaration ownership chain reaches an entry before another classifier, bare
    /// `super` therefore selects that enum declaration itself. Keeping this decision on stable
    /// header ownership lets compact signature evaluation agree with checked-body resolution
    /// without inventing or retaining an anonymous entry classifier.
    fn enum_entry_direct_super(
        &self,
        scope: crate::fir::SignatureScope,
        current: Ty,
    ) -> Option<Ty> {
        let mut owner = self.headers.declarations.anchor(scope.owner)?.owner;
        let mut inside_entry = false;
        while let Some(declaration) = owner {
            let anchor = self.headers.declarations.anchor(declaration)?;
            match anchor.kind {
                crate::fir::DeclarationKind::EnumEntry => inside_entry = true,
                crate::fir::DeclarationKind::Classifier => {
                    let classifier = self.classifier_types.get(&declaration).copied()?;
                    return (inside_entry && current.obj_internal() == Some(classifier))
                        .then_some(current);
                }
                _ => {}
            }
            owner = anchor.owner;
        }
        None
    }

    fn classifier_has_enum_entry(
        &self,
        classifier: crate::types::TypeName,
        spelling: &str,
    ) -> bool {
        self.table
            .enum_entries_of(classifier)
            .is_some_and(|entries| entries.iter().any(|entry| entry == spelling))
            || self
                .table
                .libraries
                .classifier(classifier)
                .is_some_and(|declaration| declaration.is_enum_entry(spelling))
    }

    fn function_import_scope(
        &self,
        source_id: crate::fir::SourceFileId,
    ) -> Result<crate::symbol_resolver::FunctionImportScope, crate::fir::DiagnosticId> {
        let file = self
            .headers
            .scopes
            .file(source_id)
            .ok_or_else(Self::failure)?;
        let path = |range| {
            self.headers
                .scopes
                .path(range)
                .iter()
                .map(|segment| self.headers.lookup_names.get(*segment))
                .collect::<Option<Vec<_>>>()
                .map(|segments| segments.join("/"))
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, source_id.raw());
        let symbols = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            self.table.libraries.as_ref() as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut explicit = std::collections::HashMap::new();
        let mut stars = Vec::new();
        for import in self.headers.scopes.imports(file.imports) {
            let imported = path(import.path).ok_or_else(Self::failure)?;
            if import.wildcard {
                let owner = match super::qualifier_path(&imported, &symbols, None)
                    .map_err(|_| Self::failure())?
                {
                    super::ResolvedQualifier::Package(package) => package,
                    super::ResolvedQualifier::Classifier(classifier) => classifier,
                    super::ResolvedQualifier::Value => return Err(Self::failure()),
                };
                stars.push(owner);
                continue;
            }
            let (parent, declared_name) =
                imported.rsplit_once('/').unwrap_or(("", imported.as_str()));
            let owner = if parent.is_empty() {
                crate::symbol_source::SymbolNamespace::Package(crate::types::TypeName::ROOT)
            } else {
                match super::qualifier_path(parent, &symbols, None).map_err(|_| Self::failure())? {
                    super::ResolvedQualifier::Package(package) => {
                        crate::symbol_source::SymbolNamespace::Package(package)
                    }
                    super::ResolvedQualifier::Classifier(classifier) => {
                        crate::symbol_source::SymbolNamespace::Classifier(classifier)
                    }
                    super::ResolvedQualifier::Value => return Err(Self::failure()),
                }
            };
            let visible_name = import
                .alias
                .and_then(|alias| self.headers.lookup_names.get(alias))
                .unwrap_or(declared_name);
            explicit.insert(
                visible_name.to_owned(),
                crate::symbol_resolver::CallableImport::new(owner, declared_name.to_owned()),
            );
        }
        let own_package = path(file.package).ok_or_else(Self::failure)?;
        let kotlin_defaults = super::KOTLIN_DEFAULT_IMPORT_PACKAGES
            .iter()
            .map(|package| crate::types::type_name(&package.replace('.', "/")))
            .collect();
        let platform_defaults = self
            .table
            .libraries
            .platform_default_import_packages()
            .into_iter()
            .map(|package| crate::types::type_name(&package.replace('.', "/")))
            .collect();
        Ok(crate::symbol_resolver::FunctionImportScope::new(
            explicit,
            [
                vec![crate::types::type_name(&own_package)],
                stars,
                kotlin_defaults,
                platform_defaults,
            ],
        ))
    }

    fn explicit_imported_classifier_callable(
        &self,
        source: crate::fir::SourceFileId,
        spelling: &str,
    ) -> Option<(TypeName, String)> {
        let imports = self.function_import_scope(source).ok()?;
        let (namespace, declared_name) = imports.explicit_target(spelling)?;
        let crate::symbol_source::SymbolNamespace::Classifier(owner) = namespace else {
            return None;
        };
        Some((owner, declared_name))
    }

    fn selected_implicit_classifier_property(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
    ) -> Option<Ty> {
        self.with_resolver(scope, |resolver| {
            let property = resolver
                .classifier(classifier)?
                .classifier_property(classifier, spelling)?;
            match property.operation {
                crate::libraries::ImplicitClassifierProperty::EnumEntries => Some(property.ty),
            }
        })
        .ok()
    }

    /// Bind implicit context parameters from the compact receiver tower, then expose only the
    /// declaration's source-visible value parameters to ordinary argument mapping and overload
    /// selection. Context values are inference evidence, but never positional call arguments.
    fn implicit_context_candidates(
        &self,
        scope: crate::fir::SignatureScope,
        candidates: impl IntoIterator<Item = crate::libraries::FunctionInfo>,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self.implicit_receivers(scope);
        candidates
            .into_iter()
            .filter_map(|mut candidate| {
                let mut signature = candidate.semantic_signature().into_owned();
                let context_count = candidate.context_count.min(signature.params.len());
                if context_count == 0 {
                    return Some(candidate);
                }
                let context_parameters = &signature.params[..context_count];
                let actual = super::context_argument_types(
                    &receivers,
                    context_parameters,
                    &crate::symbol_resolver::SourceOracle(&source),
                );
                crate::trace_compiler!(
                    "signature",
                    "context candidate name={} receivers={receivers:?} parameters={context_parameters:?} actual={actual:?}",
                    candidate.callable.name,
                );
                let actual = actual?;
                let mut bindings = crate::symbol_resolver::GSigBinds::new();
                for (&parameter, actual) in context_parameters.iter().zip(actual) {
                    crate::symbol_resolver::unify_inferred_ty(parameter, actual, &mut bindings);
                }
                signature.params = signature.params[context_count..]
                    .iter()
                    .map(|parameter| {
                        crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                    })
                    .collect();
                signature.ret =
                    crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
                let visible =
                    (context_count..candidate.call_sig.param_names.len()).collect::<Vec<_>>();
                candidate.call_sig = super::call_sig_for_parameters(&candidate.call_sig, &visible);
                candidate.generic_sig = Some(signature);
                candidate.context_count = 0;
                crate::trace_compiler!(
                    "signature",
                    "context-projected candidate name={} receivers={receivers:?} params={:?} formals={:?} type_bindings={bindings:?}",
                    candidate.callable.name,
                    candidate.semantic_params(),
                    candidate
                        .generic_sig
                        .as_ref()
                        .map(|signature| signature.formals.as_slice())
                        .unwrap_or_default(),
                );
                Some(candidate)
            })
            .collect()
    }

    /// The resolver-facing kind for one evaluated signature argument. An integer LITERAL keeps its
    /// folded value so overload resolution can let it adopt a narrower integer parameter type.
    fn call_argument_kind(
        argument: &crate::fir::ResolvedSigCallArgument<'_>,
    ) -> crate::symbol_resolver::CallArgKind {
        if argument.spread {
            return crate::symbol_resolver::CallArgKind::Spread(argument.ty.get());
        }
        if argument.lambda {
            return crate::symbol_resolver::CallArgKind::LambdaLiteral(argument.ty.get());
        }
        match argument.integer_literal {
            Some(value) => {
                crate::symbol_resolver::CallArgKind::integer_literal(argument.ty.get(), value)
            }
            None => crate::symbol_resolver::CallArgKind::Typed(argument.ty.get()),
        }
    }

    fn mapped_call_arguments(
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
    ) -> Option<(Vec<crate::symbol_resolver::CallArgKind>, Vec<Ty>)> {
        let ordinary = || {
            let kinds = arguments
                .iter()
                .map(Self::call_argument_kind)
                .collect::<Vec<_>>();
            let types = arguments
                .iter()
                .map(|argument| {
                    let ty = argument.ty.get();
                    if argument.spread {
                        ty.array_read_elem().unwrap_or(ty)
                    } else {
                        ty
                    }
                })
                .collect::<Vec<_>>();
            (kinds, types)
        };
        let has_named = arguments.iter().any(|argument| argument.name.is_some());
        if !has_named && candidates.is_empty() {
            return Some(ordinary());
        }
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let Some(mapping) =
            Self::mapped_call_slots(candidates, &names, arguments.len(), trailing_lambda)
        else {
            return (!has_named).then(ordinary);
        };
        let reordered = mapping
            .iter()
            .map(|source| source.and_then(|source| arguments.get(source).copied()))
            .collect::<Vec<_>>();
        let kinds = reordered
            .iter()
            .map(|argument| match argument {
                Some(argument) => Self::call_argument_kind(argument),
                None => crate::symbol_resolver::CallArgKind::OmittedDefault,
            })
            .collect();
        let types = reordered
            .iter()
            .map(|argument| {
                argument.map_or(Ty::Error, |argument| {
                    let ty = argument.ty.get();
                    if argument.spread {
                        ty.array_read_elem().unwrap_or(ty)
                    } else {
                        ty
                    }
                })
            })
            .collect();
        Some((kinds, types))
    }

    /// Map one constructor call's source arguments into declaration-parameter order. Constructor
    /// declarations are not part of the top-level `FunctionInfo` family, but named/default/trailing
    /// arguments have the same source semantics. A unique mapping is required before type-based
    /// overload selection; distinct mappings remain ambiguous instead of silently treating source
    /// order as parameter order.
    fn mapped_constructor_arguments(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        classifier: crate::types::TypeName,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
    ) -> Option<(Vec<crate::symbol_resolver::CallArgKind>, Vec<Ty>)> {
        let declaration = resolver.classifier(classifier)?;
        let source_indices = (0..arguments.len()).collect::<Vec<_>>();
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let mut mappings = declaration
            .constructors
            .iter()
            .filter_map(|candidate| {
                let slots = crate::libraries::map_call_args(
                    &source_indices,
                    Some(&names),
                    &candidate.call_sig.param_names,
                    candidate.params.len(),
                    candidate.call_sig.required,
                    &candidate.call_sig.param_defaults,
                    candidate.call_sig.vararg_index,
                    trailing_lambda,
                )
                .ok()?;
                source_indices
                    .iter()
                    .all(|source| slots.iter().any(|slot| slot == &Some(*source)))
                    .then_some(slots)
            })
            .collect::<Vec<_>>();
        mappings.sort_unstable();
        mappings.dedup();
        let [mapping] = mappings.as_slice() else {
            return None;
        };
        let reordered = mapping
            .iter()
            .map(|source| source.and_then(|source| arguments.get(source).copied()))
            .collect::<Vec<_>>();
        let kinds = reordered
            .iter()
            .map(|argument| match argument {
                Some(argument) => Self::call_argument_kind(argument),
                None => crate::symbol_resolver::CallArgKind::OmittedDefault,
            })
            .collect();
        let types = reordered
            .iter()
            .map(|argument| {
                argument.map_or(Ty::Error, |argument| {
                    let ty = argument.ty.get();
                    if argument.spread {
                        ty.array_read_elem().unwrap_or(ty)
                    } else {
                        ty
                    }
                })
            })
            .collect();
        Some((kinds, types))
    }

    /// Project a callable family to the parameter order visible at a call which explicitly names at
    /// least one context parameter. Ordinary value parameters stay first; named context parameters
    /// follow them and remain named-only. The projected candidates are consumed only by the normal
    /// resolver during temporary signature evaluation.
    fn explicit_context_call(
        enabled: bool,
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
    ) -> Option<ExplicitContextCall> {
        if !enabled || arguments.iter().all(|argument| argument.name.is_none()) {
            return None;
        }
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let explicitly_names_context = candidates.iter().any(|candidate| {
            candidate
                .call_sig
                .param_names
                .iter()
                .take(candidate.context_count)
                .any(|parameter| names.iter().flatten().any(|argument| argument == parameter))
        });
        if !explicitly_names_context {
            return None;
        }

        let source_indices = (0..arguments.len()).collect::<Vec<_>>();
        let mut projected = Vec::new();
        for candidate in candidates {
            let semantic = candidate.semantic_signature().into_owned();
            let context_count = candidate.context_count.min(semantic.params.len());
            let explicitly_named = |parameter: usize| {
                candidate
                    .call_sig
                    .param_names
                    .get(parameter)
                    .is_some_and(|parameter| {
                        names.iter().flatten().any(|argument| argument == parameter)
                    })
            };
            let parameter_indices = (context_count..semantic.params.len())
                .chain((0..context_count).filter(|&parameter| explicitly_named(parameter)))
                .collect::<Vec<_>>();
            let call_sig = super::call_sig_for_parameters(&candidate.call_sig, &parameter_indices);
            let slots = crate::libraries::map_call_args(
                &source_indices,
                Some(&names),
                &call_sig.param_names,
                parameter_indices.len(),
                call_sig.required,
                &call_sig.param_defaults,
                call_sig.vararg_index,
                trailing_lambda,
            )
            .ok()?;
            if !source_indices
                .iter()
                .all(|source| slots.iter().any(|slot| slot == &Some(*source)))
            {
                continue;
            }
            let mut candidate = candidate.clone();
            let mut signature = semantic;
            signature.params = parameter_indices
                .iter()
                .map(|&parameter| signature.params[parameter])
                .collect();
            candidate.generic_sig = Some(signature);
            candidate.call_sig = call_sig;
            candidate.context_count = 0;
            projected.push((slots, candidate));
        }
        if projected.is_empty() {
            return None;
        }
        let first_slots = projected.first()?.0.clone();
        if projected.iter().any(|(slots, _)| *slots != first_slots) {
            return None;
        }
        let reordered = first_slots
            .iter()
            .map(|source| source.and_then(|source| arguments.get(source).copied()))
            .collect::<Vec<_>>();
        let call_arguments = reordered
            .iter()
            .map(|argument| match argument {
                Some(argument) if argument.spread => {
                    crate::symbol_resolver::CallArgKind::Spread(argument.ty.get())
                }
                Some(argument) => crate::symbol_resolver::CallArgKind::Typed(argument.ty.get()),
                None => crate::symbol_resolver::CallArgKind::OmittedDefault,
            })
            .collect();
        let argument_types = reordered
            .iter()
            .map(|argument| argument.map_or(Ty::Error, |argument| argument.ty.get()))
            .collect();
        Some(ExplicitContextCall {
            candidates: projected
                .into_iter()
                .map(|(_, candidate)| candidate)
                .collect(),
            arguments: call_arguments,
            argument_types,
        })
    }

    fn candidate_call_slots(
        candidate: &crate::libraries::FunctionInfo,
        names: &[Option<String>],
        argument_count: usize,
        trailing_lambda: bool,
    ) -> Option<Vec<Option<usize>>> {
        let source_indices = (0..argument_count).collect::<Vec<_>>();
        let slots = crate::libraries::map_call_args(
            &source_indices,
            Some(names),
            &candidate.call_sig.param_names,
            candidate.semantic_params().len(),
            candidate.call_sig.required,
            &candidate.call_sig.param_defaults,
            candidate.call_sig.vararg_index,
            trailing_lambda,
        )
        .ok()?;
        source_indices
            .iter()
            .all(|source| slots.iter().any(|slot| slot == &Some(*source)))
            .then_some(slots)
    }

    fn mapped_call_slots(
        candidates: &[crate::libraries::FunctionInfo],
        names: &[Option<String>],
        argument_count: usize,
        trailing_lambda: bool,
    ) -> Option<Vec<Option<usize>>> {
        let mut mappings = candidates
            .iter()
            .filter_map(|candidate| {
                Self::candidate_call_slots(candidate, names, argument_count, trailing_lambda)
            })
            .collect::<Vec<_>>();
        mappings.sort_unstable();
        mappings.dedup();
        let [mapping] = mappings.as_slice() else {
            return None;
        };
        Some(mapping.clone())
    }

    /// A postponed lambda/reference may prevent type applicability from selecting an overload, but
    /// declaration-owned argument mapping can still leave exactly one arity/name/default shape.
    /// That unique declaration supplies only the contextual argument shape; final overload
    /// selection runs again after the postponed argument is materialized.
    fn uniquely_mapped_candidate(
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        trailing_lambda: bool,
    ) -> Option<crate::libraries::FunctionInfo> {
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let mut matching = candidates.iter().filter(|candidate| {
            Self::candidate_call_slots(candidate, &names, arguments.len(), trailing_lambda)
                .is_some()
        });
        let selected = matching.next()?.clone();
        matching.next().is_none().then_some(selected)
    }

    fn probe_call_arguments(
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        trailing_lambda: bool,
    ) -> Option<(Vec<crate::symbol_resolver::CallArgKind>, Vec<Option<usize>>)> {
        if arguments.iter().any(|argument| {
            matches!(
                argument,
                crate::fir::SigCallArgumentProbe::PostponedLambda { spread: true, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        spread: true,
                        ..
                    }
            )
        }) {
            return None;
        }
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let requires_mapping = trailing_lambda || names.iter().any(Option::is_some);
        let slots = if requires_mapping {
            Self::mapped_call_slots(candidates, &names, arguments.len(), trailing_lambda)?
        } else {
            (0..arguments.len()).map(Some).collect()
        };
        let kinds = slots
            .iter()
            .map(|source| {
                source
                    .and_then(|source| arguments.get(source))
                    .map(Self::probe_argument_kind)
                    .unwrap_or(crate::symbol_resolver::CallArgKind::OmittedDefault)
            })
            .collect();
        Some((kinds, slots))
    }

    fn probe_argument_kind(
        argument: &crate::fir::SigCallArgumentProbe<'_>,
    ) -> crate::symbol_resolver::CallArgKind {
        match argument {
            crate::fir::SigCallArgumentProbe::Typed(argument) if argument.spread => {
                crate::symbol_resolver::CallArgKind::Spread(argument.ty.get())
            }
            crate::fir::SigCallArgumentProbe::Typed(argument)
                if argument.contextual_call && argument.ty.get().mentions_ty_param() =>
            {
                crate::symbol_resolver::CallArgKind::Typed(Ty::Error)
            }
            crate::fir::SigCallArgumentProbe::Typed(argument) => Self::call_argument_kind(argument),
            crate::fir::SigCallArgumentProbe::PostponedLambda {
                parameter_count,
                implicit_it,
                ..
            } => {
                let probe = if *implicit_it {
                    Ty::Error
                } else {
                    Ty::fun(vec![Ty::Error; *parameter_count as usize], Ty::Error)
                };
                crate::symbol_resolver::CallArgKind::LambdaLiteral(probe)
            }
            crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. } => {
                crate::symbol_resolver::CallArgKind::LambdaLiteral(Ty::Error)
            }
        }
    }

    fn postponed_expectations(
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        slots: &[Option<usize>],
        parameters: &[Ty],
    ) -> Box<[Option<crate::fir::ResolvedTy>]> {
        let mut expectations = vec![None; arguments.len()];
        for (slot, source) in slots.iter().enumerate() {
            let Some(source) = *source else {
                continue;
            };
            let contextual_call = matches!(
                arguments.get(source),
                Some(crate::fir::SigCallArgumentProbe::Typed(argument))
                    if argument.contextual_call
            );
            let postponed_callable = matches!(
                arguments.get(source),
                Some(
                    crate::fir::SigCallArgumentProbe::PostponedLambda { .. }
                        | crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. },
                )
            );
            if contextual_call || postponed_callable {
                expectations[source] = parameters.get(slot).copied().and_then(|parameter| {
                    (contextual_call || matches!(parameter.non_null(), Ty::Fun(_)))
                        .then(|| crate::fir::ResolvedTy::new(parameter).ok())
                        .flatten()
                });
            }
        }
        expectations.into_boxed_slice()
    }

    /// Normalize selected declaration parameters into the callable shapes used to materialize
    /// postponed lambdas and references. Package, top-level, classifier, and receiver expectation
    /// paths all consume this operation; none may independently reinterpret SAMs or lambda receiver
    /// metadata.
    fn functional_parameter_shapes(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        selected: &crate::libraries::FunctionInfo,
        parameters: impl IntoIterator<Item = Ty>,
    ) -> Vec<Ty> {
        parameters
            .into_iter()
            .enumerate()
            .map(|(parameter_index, parameter)| {
                let expectation = resolver
                    .functional_expectation(parameter)
                    .unwrap_or(parameter);
                let Ty::Fun(signature) = expectation.non_null() else {
                    return expectation;
                };
                let has_receiver = selected
                    .call_sig
                    .lambda_receiver_params
                    .get(parameter_index)
                    .copied()
                    .unwrap_or(false);
                let context_count = selected
                    .call_sig
                    .lambda_context_counts
                    .get(parameter_index)
                    .copied()
                    .unwrap_or(signature.context_count);
                Ty::fun_with_shape(
                    signature.params.clone(),
                    signature.ret,
                    context_count,
                    has_receiver || signature.has_receiver,
                    signature.suspend,
                )
            })
            .collect()
    }

    /// Project the selected callable's parameter types back onto postponed source arguments. The
    /// shared argument mapper owns named/default/trailing-lambda placement; this inversion only
    /// preserves the many-source-arguments-to-one-vararg relationship which a parameter-slot vector
    /// cannot represent. Positional vararg arguments expect the element type, while named/spread
    /// arguments expect the declared array type.
    fn postponed_call_expectations(
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        parameters: &[Ty],
        call_sig: &crate::libraries::CallSig,
        trailing_lambda: bool,
    ) -> Option<Box<[Option<crate::fir::ResolvedTy>]>> {
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let sources = (0..arguments.len()).collect::<Vec<_>>();
        let slots = crate::libraries::map_call_args(
            &sources,
            Some(&names),
            &call_sig.param_names,
            parameters.len(),
            call_sig.required,
            &call_sig.param_defaults,
            call_sig.vararg_index,
            trailing_lambda,
        )
        .ok()?;
        let mut expectations = vec![None; arguments.len()];
        for (source, argument) in arguments.iter().enumerate() {
            let contextual_call = matches!(
                argument,
                crate::fir::SigCallArgumentProbe::Typed(argument) if argument.contextual_call
            );
            let postponed_callable = matches!(
                argument,
                crate::fir::SigCallArgumentProbe::PostponedLambda { .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. }
            );
            if !contextual_call && !postponed_callable {
                continue;
            }
            let parameter = slots
                .iter()
                .position(|slot| *slot == Some(source))
                .or(call_sig.vararg_index)?;
            let mut expected = *parameters.get(parameter)?;
            let whole_vararg = call_sig.vararg_index == Some(parameter)
                && match argument {
                    crate::fir::SigCallArgumentProbe::Typed(argument) => {
                        argument.name.is_some() || argument.spread
                    }
                    crate::fir::SigCallArgumentProbe::PostponedLambda { name, spread, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        name,
                        spread,
                    } => name.is_some() || *spread,
                };
            if call_sig.vararg_index == Some(parameter) && !whole_vararg {
                expected = expected.array_read_elem().unwrap_or(expected);
            }
            expectations[source] = (contextual_call || matches!(expected.non_null(), Ty::Fun(_)))
                .then(|| crate::fir::ResolvedTy::new(expected).ok())
                .flatten();
        }
        Some(expectations.into_boxed_slice())
    }

    fn demanded_source_signature(
        &self,
        from: Option<crate::fir::SignatureScope>,
        declaration: Option<crate::fir::DeclarationId>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedSignature>, crate::fir::DiagnosticId> {
        self.demanded_source_signature_at(from, declaration, None, demand)
    }

    fn demanded_source_signature_at(
        &self,
        from: Option<crate::fir::SignatureScope>,
        declaration: Option<crate::fir::DeclarationId>,
        origin: Option<crate::fir::OriginId>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedSignature>, crate::fir::DiagnosticId> {
        let Some(declaration) = declaration else {
            return Ok(None);
        };
        if let Some(from) = from {
            let source_stub = self.headers.stubs.iter().find(|stub| stub.id == from.owner);
            let target_stub = self
                .headers
                .stubs
                .iter()
                .find(|stub| stub.id == declaration);
            let eager_forward_reference =
                source_stub
                    .zip(target_stub)
                    .is_some_and(|(source, target)| {
                        let source_order = self.source_orders.get(&source.id);
                        let target_order = self.source_orders.get(&target.id);
                        source.source == target.source
                            && source_order
                                .zip(target_order)
                                .is_some_and(|(source, target)| source < target)
                            && self
                                .headers
                                .declarations
                                .anchor(source.id)
                                .is_some_and(|anchor| anchor.owner.is_none())
                            && matches!(
                                source.signature_inference,
                                Some(
                                    crate::fir::InferredSignatureKind::PropertyInitializer
                                        | crate::fir::InferredSignatureKind::BackingFieldInitializer
                                        | crate::fir::InferredSignatureKind::DelegatedProperty
                                )
                            )
                    });
            if eager_forward_reference {
                return Err(origin.map_or_else(Self::failure, |origin| {
                    self.record_eager_forward_reference(from.owner, declaration, origin)
                }));
            }
        }
        // Explicit signatures are already resolved states in the same solver. Demand them through
        // the stable declaration identity too: provider call shapes may be transitional/erased,
        // while the published signature preserves captured classifier arguments and every other
        // source-level semantic type decision.
        demand(declaration).map(Some)
    }

    fn demanded_member_signature(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedSignature>, crate::fir::DiagnosticId> {
        declaration.map_or(Ok(None), |declaration| demand(declaration).map(Some))
    }

    fn selected_member_property_type(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let implicit_receivers = self.implicit_receivers(scope);
        let selected = self.with_resolver(scope, |resolver| {
            resolver.select_member_property_applicable_where(receiver, spelling, |property| {
                let context_types = property.getter.params.get(..property.context_count)?;
                super::context_argument_types(
                    &implicit_receivers,
                    context_types,
                    &crate::symbol_resolver::SourceOracle(&source),
                )
                .map(|_| (true, property.context_count))
            })
        });
        crate::trace_compiler!(
            "signature",
            "member property selection receiver={receiver:?} name={spelling} result={:?}",
            selected.as_ref().map(|selected| selected.ty),
        );
        let selected = match selected {
            Ok(selected) => selected,
            Err(_) => {
                let extension = self.with_resolver(scope, |resolver| {
                    resolver
                        .select_extension_property(receiver, spelling)
                        .ok()
                        .flatten()
                });
                let Ok(extension) = extension else {
                    return Ok(None);
                };
                if let Some(source) = extension.source_key {
                    if let Some(signature) = self.demanded_source_signature(
                        Some(scope),
                        extension.stable_declaration,
                        demand,
                    )? {
                        return self
                            .apply_demanded_source_property(source, receiver, &signature)
                            .map(Some);
                    }
                }
                return crate::fir::ResolvedTy::new(extension.ty)
                    .map(Some)
                    .map_err(|_| Self::failure());
            }
        };
        if let Some(property) = selected.property.as_ref() {
            if let Some(declaration) = property.stable_declaration {
                if self
                    .headers
                    .stubs
                    .iter()
                    .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
                {
                    let result = self
                        .apply_dispatch_owner(
                            receiver,
                            property.owner,
                            None,
                            demand(declaration)?.result.get(),
                        )
                        .projection_read_ty();
                    return crate::fir::ResolvedTy::new(result)
                        .map(Some)
                        .map_err(|_| Self::failure());
                }
            }
            if let Some(signature) =
                self.demanded_member_signature(property.stable_declaration, demand)?
            {
                let result = self
                    .apply_dispatch_owner(receiver, property.owner, None, signature.result.get())
                    .projection_read_ty();
                return crate::fir::ResolvedTy::new(result)
                    .map(Some)
                    .map_err(|_| Self::failure());
            }
        }
        crate::fir::ResolvedTy::new(selected.ty)
            .map(Some)
            .map_err(|_| Self::failure())
    }

    fn apply_dispatch_receiver(
        &self,
        receiver: Ty,
        member: &crate::libraries::LibraryMember,
        result: Ty,
    ) -> Ty {
        let Some(owner) = member.owner else {
            return result;
        };
        self.apply_dispatch_owner(receiver, owner, member.generic_sig.as_ref(), result)
    }

    fn apply_dispatch_owner(
        &self,
        receiver: Ty,
        owner: crate::types::TypeName,
        generic_signature: Option<&crate::libraries::GenericSig>,
        result: Ty,
    ) -> Ty {
        let Some((_, applied, _)) = self
            .table
            .applied_type_hierarchy(receiver.non_null())
            .into_iter()
            .find(|(candidate, _, _)| *candidate == owner)
        else {
            return result;
        };
        let Some(class) = self.table.class_by_type_name(owner) else {
            return result;
        };
        let mut bindings = class.type_parameter_bindings(applied);
        if let Some(signature) = generic_signature {
            for formal in &signature.formals {
                bindings.remove(formal);
            }
        }
        let specialized = crate::symbol_resolver::ty_subst_keep_unbound(result, &bindings);
        crate::trace_compiler!(
            "signature",
            "apply dispatch receiver={receiver:?} owner={owner:?} applied={applied:?} result={result:?} bindings={bindings:?} specialized={specialized:?}",
        );
        specialized
    }

    fn apply_demanded_member(
        &self,
        receiver: Ty,
        member: &crate::libraries::LibraryMember,
        signature: &crate::fir::ResolvedSignature,
        arguments: &[Ty],
        explicit_type_arguments: &[Ty],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let source_file = member
            .source_member
            .map(crate::libraries::SourceMember::file)
            .or_else(|| {
                member.stable_declaration.and_then(|declaration| {
                    self.headers
                        .stubs
                        .iter()
                        .find(|stub| stub.id == declaration)
                        .map(|stub| stub.source.raw())
                })
            })
            .unwrap_or(0);
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, source_file);
        let semantic_source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        if let Some(generic) = member.generic_sig.as_ref() {
            bindings.extend(
                generic
                    .formals
                    .iter()
                    .cloned()
                    .zip(explicit_type_arguments.iter().copied()),
            );
        }
        let context_count = member.context_count.min(signature.parameters.len());
        for (parameter, argument) in signature.parameters[context_count..].iter().zip(arguments) {
            if *argument == Ty::Error {
                continue;
            }
            let parameter = self.apply_dispatch_receiver(receiver, member, parameter.get());
            crate::symbol_resolver::unify_inferred_ty_with_source(
                &semantic_source,
                parameter,
                *argument,
                &mut bindings,
            );
        }
        let result = self.apply_dispatch_receiver(receiver, member, signature.result.get());
        crate::trace_compiler!(
            "signature",
            "apply demanded member receiver={receiver:?} member_owner={:?} arguments={arguments:?} bindings={bindings:?} signature_result={:?} dispatch_result={result:?}",
            member.owner,
            signature.result.get(),
        );
        crate::fir::ResolvedTy::new(
            crate::symbol_resolver::ty_subst_keep_unbound(result, &bindings).projection_read_ty(),
        )
        .map_err(|_| Self::failure())
    }

    fn apply_demanded_source_callable(
        &self,
        source: (u32, u32),
        receiver: Option<Ty>,
        signature: &crate::fir::ResolvedSignature,
        arguments: &[Ty],
        argument_kinds: Option<&[crate::symbol_resolver::CallArgKind]>,
        explicit_type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let declaration = DeclId(source.1);
        let Some(callable) = self
            .table
            .funs
            .values()
            .flatten()
            .chain(
                self.table
                    .ext_funs
                    .values()
                    .flat_map(HashMap::values)
                    .flatten(),
            )
            .find(|callable| {
                callable.source_file == Some(source.0) && callable.source_decl == Some(declaration)
            })
        else {
            crate::trace_compiler!(
                "signature",
                "demanded source callable missing source={source:?}",
            );
            return Err(Self::failure());
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, source.0);
        let semantic_source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        if let Some(generic) = callable.generic_sig.as_ref() {
            bindings.extend(
                generic
                    .formals
                    .iter()
                    .cloned()
                    .zip(explicit_type_arguments.iter().copied()),
            );
        }
        if let (Some(generic), Some(expected)) = (callable.generic_sig.as_ref(), expected) {
            let oracle = crate::symbol_resolver::SourceOracle(&semantic_source);
            if let Some(inferred) =
                crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                    &semantic_source,
                    generic,
                    expected,
                    |actual, bound| {
                        crate::assignable::is_assignable(
                            &crate::assignable::TyCtx::new(),
                            &oracle,
                            actual,
                            bound,
                        )
                    },
                )
            {
                crate::symbol_resolver::merge_generic_upper_bindings(
                    generic,
                    explicit_type_arguments,
                    &mut bindings,
                    inferred,
                    |actual, bound| {
                        crate::assignable::is_assignable(
                            &crate::assignable::TyCtx::new(),
                            &oracle,
                            actual,
                            bound,
                        )
                    },
                );
            }
        }
        if let (Some(declared), Some(actual)) = (callable.source_receiver, receiver) {
            crate::symbol_resolver::unify_inferred_ty_with_source(
                &semantic_source,
                declared,
                actual,
                &mut bindings,
            );
        }
        let context_count = callable.context_count.min(signature.parameters.len());
        let visible_vararg = callable
            .vararg_index
            .and_then(|index| index.checked_sub(context_count));
        for (argument_index, argument) in arguments.iter().enumerate() {
            if *argument == Ty::Error {
                continue;
            }
            let visible_parameter = visible_vararg
                .filter(|vararg| argument_index >= *vararg)
                .unwrap_or(argument_index);
            let Some(parameter) = signature.parameters.get(context_count + visible_parameter)
            else {
                continue;
            };
            let parameter = if visible_vararg == Some(visible_parameter) {
                parameter.get().array_read_elem().unwrap_or(parameter.get())
            } else {
                parameter.get()
            };
            let applied_parameter =
                crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings);
            let argument = argument_kinds
                .and_then(|arguments| arguments.get(argument_index))
                .map_or(*argument, |kind| {
                    // `arguments` is already mapped to the vararg element type. The spread probe
                    // retains the source array only for applicability; feeding that array back
                    // into result substitution would bind `T = Array<T>` after selection.
                    if kind.is_spread() {
                        *argument
                    } else {
                        kind.type_for(applied_parameter)
                    }
                });
            crate::symbol_resolver::unify_inferred_ty_with_source(
                &semantic_source,
                parameter,
                argument,
                &mut bindings,
            );
        }
        let result =
            crate::symbol_resolver::ty_subst_keep_unbound(signature.result.get(), &bindings);
        crate::trace_compiler!(
            "signature",
            "apply demanded source callable source={source:?} expected={expected:?} arguments={arguments:?} bindings={bindings:?} result={result:?}",
        );
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }

    fn apply_demanded_source_property(
        &self,
        source: (u32, u32),
        receiver: Ty,
        signature: &crate::fir::ResolvedSignature,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let Some(property) = self
            .table
            .ext_props
            .values()
            .flatten()
            .find(|property| property.source == source)
        else {
            crate::trace_compiler!(
                "signature",
                "demanded source extension property missing source={source:?}",
            );
            return Err(Self::failure());
        };
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_inferred_ty(property.receiver, receiver, &mut bindings);
        crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
            signature.result.get(),
            &bindings,
        ))
        .map_err(|_| Self::failure())
    }

    fn apply_demanded_function(
        &self,
        receiver: Ty,
        selected: &crate::libraries::FunctionInfo,
        signature: &crate::fir::ResolvedSignature,
        arguments: &[Ty],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        let dispatch_member = selected.kind == crate::libraries::FnKind::Member;
        if !dispatch_member {
            if let Some(declared) = selected.semantic_receiver() {
                crate::symbol_resolver::unify_inferred_ty(declared, receiver, &mut bindings);
            }
        }
        let specialize_dispatch = |ty| {
            if dispatch_member {
                self.apply_dispatch_owner(
                    receiver,
                    selected.callable.owner,
                    selected.generic_sig.as_ref(),
                    ty,
                )
            } else {
                ty
            }
        };
        let context_count = selected.context_count.min(signature.parameters.len());
        for (parameter, argument) in signature.parameters[context_count..].iter().zip(arguments) {
            crate::symbol_resolver::unify_inferred_ty(
                specialize_dispatch(parameter.get()),
                *argument,
                &mut bindings,
            );
        }
        crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
            specialize_dispatch(signature.result.get()),
            &bindings,
        ))
        .map_err(|_| Self::failure())
    }

    fn selected_convention_result(
        &self,
        receiver: Ty,
        selected: &crate::libraries::FunctionInfo,
        result: Ty,
        arguments: &[Ty],
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if let Some(signature) =
            self.demanded_member_signature(selected.stable_declaration, demand)?
        {
            return self.apply_demanded_function(receiver, selected, &signature, arguments);
        }
        if let Some(signature) =
            self.demanded_source_signature(None, selected.stable_declaration, demand)?
        {
            crate::trace_compiler!(
                "signature",
                "demanded convention receiver={receiver:?} parameters={:?} result={:?}",
                signature.parameters,
                signature.result,
            );
            return self.apply_demanded_function(receiver, selected, &signature, arguments);
        }
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }

    fn constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        member: &crate::libraries::LibraryMember,
        arguments: &[Ty],
        bound_outer: Option<Ty>,
        explicit_type_arguments: &[Ty],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let owner = member.owner.ok_or_else(Self::failure)?;
        let Some(class) = self.table.class_by_type_name(owner) else {
            let result = if member.ret.obj_internal() == Some(owner) {
                member.ret
            } else {
                Ty::obj_name(owner)
            };
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        };
        // Constructor selection has already applied explicit arguments, literal adaptation,
        // generic bounds, and overload mapping to `member.ret`. Seed those selected classifier
        // arguments before filling any still-unbound variable from the raw expression types; doing
        // the latter first would incorrectly turn `C<T : Long>(0)` into `C<Int>`.
        let selected_arguments = (member.ret.obj_internal() == Some(owner))
            .then(|| member.ret.type_args())
            .unwrap_or_default();
        let mut bindings = class
            .type_params
            .iter()
            .cloned()
            .zip(selected_arguments.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        // A selected constructor result can contain an early partial solution (for example the
        // first `T` occurrence in `PairBox("x", null)`). It is not a completed classifier result
        // until every argument occurrence has contributed. Infer into a fresh map, then replace
        // only classifier slots not fixed by a written type argument; this preserves
        // `C<Number>(1)` while completing `PairBox("x", null)` to `PairBox<String?>`.
        let declared_parameters: std::borrow::Cow<'_, [Ty]> = member
            .generic_sig
            .as_ref()
            .filter(|signature| signature.params.len() == arguments.len())
            .map(|signature| std::borrow::Cow::Borrowed(signature.params.as_slice()))
            .unwrap_or_else(|| {
                std::borrow::Cow::Owned(
                    class
                        .ctor_param_shapes
                        .iter()
                        .map(|parameter| parameter.0)
                        .collect(),
                )
            });
        let mut inferred = crate::symbol_resolver::GSigBinds::new();
        for (parameter, argument) in declared_parameters.iter().zip(arguments) {
            crate::symbol_resolver::unify_inferred_ty(*parameter, *argument, &mut inferred);
        }
        let mut completed_bindings = bindings.clone();
        for (index, formal) in class.type_params.iter().enumerate() {
            let explicitly_fixed = explicit_type_arguments
                .get(index)
                .is_some_and(|argument| *argument != Ty::Error);
            if !explicitly_fixed {
                if let Some(inferred) = inferred.get(formal).copied() {
                    completed_bindings.insert(formal.clone(), inferred);
                }
            }
        }
        // The fresh completion above sees compact expression result types, but no longer carries
        // literal provenance. It may therefore infer `Int` from the raw `0` after constructor
        // selection correctly adapted that literal to a declared `T : Long` parameter. Accept the
        // completed map only when it still satisfies the constructor's declaration bounds;
        // otherwise the selected semantic result remains authoritative. This is generic
        // constructor inference—value classes use the same path as ordinary classes.
        let completed_is_valid = member.generic_sig.as_ref().is_none_or(|signature| {
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            crate::symbol_resolver::generic_bindings_satisfy_bounds(
                signature,
                &completed_bindings,
                |actual, bound| {
                    crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &oracle,
                        actual,
                        bound,
                    )
                },
            )
        });
        if completed_is_valid {
            bindings = completed_bindings;
        }
        let mut result_arguments = class
            .type_params
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                bindings.get(formal).copied().unwrap_or_else(|| {
                    class
                        .type_param_bounds
                        .get(index)
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                })
            })
            .collect::<Vec<_>>();
        if !class.captured_type_parameters.type_params.is_empty() {
            let receivers = bound_outer
                .into_iter()
                .chain(self.implicit_receivers(scope));
            let receiver_bindings = receivers
                .flat_map(|receiver| self.table.applied_type_hierarchy(receiver.non_null()))
                .filter_map(|(receiver_owner, applied, _)| {
                    self.table
                        .class_by_type_name(receiver_owner)
                        .map(|declaration| declaration.type_parameter_bindings(applied))
                })
                .collect::<Vec<_>>();
            for (ordinal, captured) in class
                .captured_type_parameters
                .type_params
                .iter()
                .enumerate()
            {
                let argument = receiver_bindings
                    .iter()
                    .find_map(|bindings| bindings.get(captured).copied())
                    .unwrap_or_else(|| {
                        Ty::ty_param(
                            captured,
                            class
                                .captured_type_parameters
                                .type_param_bounds
                                .get(ordinal)
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        )
                    });
                result_arguments.push(argument);
            }
        }
        let result = Ty::obj_args_name(owner, &result_arguments);
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }

    fn direct_super_receivers(
        &self,
        scope: crate::fir::SignatureScope,
        super_spelling: &str,
    ) -> Result<(Option<Ty>, Vec<Ty>), crate::fir::DiagnosticId> {
        let receivers = self.implicit_receivers(scope);
        let label = super_spelling.rsplit_once('@').map(|(_, label)| label);
        let current = match label {
            Some(label) => receivers.into_iter().find(|receiver| {
                receiver
                    .obj_internal()
                    .is_some_and(|classifier| classifier.nested_segment_ref() == label)
            }),
            None => receivers.into_iter().next(),
        }
        .ok_or_else(Self::failure)?;
        let current_internal = current.obj_internal().ok_or_else(Self::failure)?;
        let class = self
            .table
            .class_by_type_name(current_internal)
            .ok_or_else(Self::failure)?;
        let qualifier = super_spelling
            .split('@')
            .next()
            .and_then(|spelling| spelling.strip_prefix("super<"))
            .and_then(|spelling| spelling.strip_suffix('>'));
        let matches_qualifier = |owner: crate::types::TypeName| {
            qualifier.is_none_or(|name| owner.qualifier_matches(name))
        };
        let applied = |owner: crate::types::TypeName, arguments: &[Ty]| {
            if arguments.is_empty() {
                Ty::obj_name(owner)
            } else {
                Ty::obj_args_name(owner, arguments)
            }
        };

        // An enum-entry body has the enum declaration itself as its anonymous subclass's sole
        // direct superclass. Its source receiver is nevertheless spelled as the enum type.
        if qualifier.is_none() {
            if let Some(receiver) = self.enum_entry_direct_super(scope, current) {
                return Ok((Some(receiver), Vec::new()));
            }
        }
        let class_super = class
            .super_internal
            .filter(|owner| matches_qualifier(*owner))
            .map(|owner| applied(owner, &class.super_type_args))
            .or_else(|| {
                let any = crate::types::type_name("kotlin/Any");
                (!class.is_interface() && matches_qualifier(any)).then(|| Ty::obj_name(any))
            });
        let interface_supers = class
            .interfaces
            .iter_ids()
            .enumerate()
            .filter(|(_, owner)| matches_qualifier(*owner))
            .map(|(index, owner)| {
                applied(
                    owner,
                    class
                        .interface_type_args
                        .get(index)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            })
            .collect();
        Ok((class_super, interface_supers))
    }

    fn selected_super_member_property_result(
        &self,
        scope: crate::fir::SignatureScope,
        super_spelling: &str,
        member_spelling: &str,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let (class_super, interface_supers) = self.direct_super_receivers(scope, super_spelling)?;
        if let Some(receiver) = class_super {
            if let Some(result) =
                self.selected_member_property_type(scope, receiver, member_spelling, demand)?
            {
                return Ok(result);
            }
        }
        let mut selected = None;
        for receiver in interface_supers {
            let Some(result) =
                self.selected_member_property_type(scope, receiver, member_spelling, demand)?
            else {
                continue;
            };
            if selected.is_some() {
                return Err(Self::failure());
            }
            selected = Some(result);
        }
        selected.ok_or_else(Self::failure)
    }

    fn selected_super_member_call_result(
        &self,
        scope: crate::fir::SignatureScope,
        super_spelling: &str,
        member_spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let (class_super, interface_supers) = self.direct_super_receivers(scope, super_spelling)?;
        // `super.Inner(args)` is a constructor call on an inner classifier inherited through the
        // selected direct super receiver, not a method named `Inner`. Classifier construction is
        // its own scope-tower facet, so select it before collecting callable members.
        if let Some(receiver) = class_super {
            if let Some(result) = self.bound_inner_constructor_result(
                scope,
                receiver,
                member_spelling,
                arguments,
                type_arguments,
            )? {
                return Ok(result);
            }
        } else {
            let mut constructors =
                interface_supers.iter().copied().filter_map(|receiver| {
                    match self.bound_inner_constructor_result(
                        scope,
                        receiver,
                        member_spelling,
                        arguments,
                        type_arguments,
                    ) {
                        Ok(Some(result)) => Some(Ok(result)),
                        Ok(None) => None,
                        Err(diagnostic) => Some(Err(diagnostic)),
                    }
                });
            if let Some(selected) = constructors.next() {
                let selected = selected?;
                if constructors.next().is_some() {
                    return Err(Self::failure());
                }
                return Ok(selected);
            }
        }
        let select = |receiver: Ty| {
            self.with_resolver(scope, |resolver| {
                let callables = resolver.receiver_callables(receiver, member_spelling);
                let (selected_arguments, selected_argument_types) =
                    Self::mapped_call_arguments(callables.functions(), arguments, trailing_lambda)?;
                let crate::symbol_resolver::CandidateSelection::Selected((
                    selected,
                    parameters,
                    result,
                )) = resolver.select_receiver_function_with_params_tracking(
                    receiver,
                    member_spelling,
                    &selected_arguments,
                    type_arguments,
                    &callables,
                )
                else {
                    return None;
                };
                if selected.kind != crate::libraries::FnKind::Member
                    || selected.callable.is_abstract
                {
                    return None;
                }
                let mut member = selected.member_with_return(result);
                member.params = parameters;
                Some((receiver, member, selected_argument_types))
            })
            .ok()
        };
        let selected = class_super.and_then(select).or_else(|| {
            let mut selected = interface_supers.into_iter().filter_map(select);
            let candidate = selected.next()?;
            selected.next().is_none().then_some(candidate)
        });
        let (receiver, member, argument_types) = selected.ok_or_else(Self::failure)?;
        if let Some(declaration) = member.stable_declaration {
            if self
                .headers
                .stubs
                .iter()
                .any(|stub| stub.id == declaration && stub.signature_inference.is_some())
            {
                let signature = demand(declaration)?;
                return self.apply_demanded_member(
                    receiver,
                    &member,
                    &signature,
                    &argument_types,
                    type_arguments,
                );
            }
        }
        if let Some(signature) =
            self.demanded_member_signature(member.stable_declaration, demand)?
        {
            return self.apply_demanded_member(
                receiver,
                &member,
                &signature,
                &argument_types,
                type_arguments,
            );
        }
        crate::fir::ResolvedTy::new(member.ret).map_err(|_| Self::failure())
    }

    /// Select an inner classifier inherited by a concrete outer receiver, then run its constructor
    /// through the ordinary resolver candidate family. This is the signature-graph counterpart of
    /// checked-body `outer.Inner(args)`/`super.Inner(args)` resolution; it returns only the compact
    /// result type and retains no constructor body or syntax identity.
    fn bound_inner_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
    ) -> Result<Option<crate::types::TypeName>, crate::fir::DiagnosticId> {
        let Some(outer) = receiver.kotlin_class_internal() else {
            return Ok(None);
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let selected = crate::symbol_resolver::inherited_nested_classifier_name(
            spelling,
            vec![outer],
            |owner| {
                crate::symbol_resolver::direct_supertypes(&source, Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect()
            },
            |candidate| {
                crate::symbol_resolver::inherited_classifier_shape(&source, candidate, outer)
                    .is_some()
            },
        );
        match selected {
            crate::symbol_resolver::InheritedNestedClassifier::NotFound => return Ok(None),
            crate::symbol_resolver::InheritedNestedClassifier::Ambiguous => {
                return Err(Self::failure())
            }
            crate::symbol_resolver::InheritedNestedClassifier::Found(internal) => {
                Ok(Some(internal))
            }
        }
    }

    fn bound_inner_constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[Ty],
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let Some(internal) = self.bound_inner_classifier(scope, receiver, spelling)? else {
            return Ok(None);
        };
        let argument_kinds = arguments
            .iter()
            .map(Self::call_argument_kind)
            .collect::<Vec<_>>();
        let argument_types = arguments
            .iter()
            .map(|argument| argument.ty.get())
            .collect::<Vec<_>>();
        let declaration = self.with_resolver(scope, |resolver| {
            resolver.select_constructor_declaration_with_type_arguments(
                internal,
                &argument_kinds,
                type_arguments,
            )
        })?;
        self.constructor_result(
            scope,
            &declaration,
            &argument_types,
            Some(receiver),
            type_arguments,
        )
        .map(Some)
    }

    fn checked_binary(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        operator: crate::ast::BinOp,
        lhs: Ty,
        rhs: Ty,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        // The operator's result type is algebra over the operands; only its value-class check needs a
        // lookup context, which signature evaluation supplies for the file being solved. Every
        // refusal is a decline here — the checker owns the wording, this path owns the decision.
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let _ = origin;
        let outcome = super::binary_result(&source, operator, lhs, rhs);
        crate::trace_compiler!(
            "signature",
            "binary op={operator:?} lhs={lhs:?} rhs={rhs:?} -> {outcome:?}"
        );
        let result = outcome.map_err(|_| Self::failure())?;
        if result.mentions_error() {
            return Err(Self::failure());
        }
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }

    fn implicit_receivers(&self, scope: crate::fir::SignatureScope) -> Vec<Ty> {
        let mut receivers = self
            .scoped_receivers
            .borrow()
            .get(&scope.owner)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .collect::<Vec<_>>();
        let mut declared = self.declaration_implicit_receivers(scope);
        // A demanded local/anonymous member is evaluated under its own stable declaration, while
        // receiver lambdas enclosing that declaration were entered under the non-local signature
        // currently being solved. Carry those active receiver rungs through the stable owner chain.
        // Insert them immediately before the ancestor declaration's ordinary receiver suffix:
        // `anonymous this`, then `with(Foo2)`, then the enclosing `Main` instance.
        let mut owner = self.declaration_semantic_parent(scope.owner);
        while let Some(declaration) = owner {
            if let Some(scoped) = self.scoped_receivers.borrow().get(&declaration) {
                let inherited = scoped.iter().rev().copied().collect::<Vec<_>>();
                let ancestor_scope = crate::fir::SignatureScope {
                    owner: declaration,
                    source: scope.source,
                };
                let ancestor_receivers = self.declaration_implicit_receivers(ancestor_scope);
                let insertion = declared
                    .iter()
                    .position(|receiver| ancestor_receivers.contains(receiver))
                    .unwrap_or(declared.len());
                declared.splice(insertion..insertion, inherited);
            }
            owner = self.declaration_semantic_parent(declaration);
        }
        receivers.extend(declared);
        receivers
    }

    fn declaration_semantic_parent(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<crate::fir::DeclarationId> {
        self.headers
            .declarations
            .anchor(declaration)
            .and_then(|anchor| anchor.owner)
            .or_else(|| self.headers.local_classifier_lexical_root(declaration))
    }

    fn classifier_receiver_chain(&self, mut classifier: crate::types::TypeName) -> Vec<Ty> {
        let mut receivers = Vec::new();
        loop {
            let Some(signature) = self.table.class_by_type_name(classifier) else {
                return receivers;
            };
            receivers.push(semantic_classifier_self(signature));
            let Some(outer) = signature.inner_of else {
                return receivers;
            };
            classifier = outer;
        }
    }

    fn classifier_context_receivers(
        &self,
        declaration: crate::fir::DeclarationId,
        source: crate::fir::SourceFileId,
    ) -> Vec<Ty> {
        let Some(crate::fir::HeaderDeclarationKind::Classifier {
            context_parameters, ..
        }) = self
            .headers
            .syntax
            .declaration(declaration)
            .map(|header| header.kind)
        else {
            return Vec::new();
        };
        let scope = crate::fir::SignatureScope {
            owner: declaration,
            source,
        };
        self.headers
            .syntax
            .parameters(context_parameters)
            .iter()
            .rev()
            .filter_map(|parameter| self.resolve_compact_header_type(scope, parameter.ty))
            .collect()
    }

    fn declaration_context_receivers(&self, scope: crate::fir::SignatureScope) -> Vec<Ty> {
        let compact_parameters =
            self.headers
                .syntax
                .declaration(scope.owner)
                .and_then(|declaration| match declaration.kind {
                    crate::fir::HeaderDeclarationKind::Callable {
                        parameters,
                        context_count,
                        ..
                    } => self
                        .headers
                        .syntax
                        .parameters(parameters)
                        .get(..context_count as usize)
                        .map(<[_]>::to_vec),
                    crate::fir::HeaderDeclarationKind::Property {
                        context_parameters, ..
                    } => Some(self.headers.syntax.parameters(context_parameters).to_vec()),
                    crate::fir::HeaderDeclarationKind::Classifier { .. }
                    | crate::fir::HeaderDeclarationKind::Constructor { .. }
                    | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
                });
        if let Some(parameters) = compact_parameters {
            return parameters
                .iter()
                .filter_map(|parameter| self.resolve_compact_header_type(scope, parameter.ty))
                .collect();
        }
        self.callable_signature(scope.owner)
            .map(|signature| {
                signature.params[..signature.context_count.min(signature.params.len())].to_vec()
            })
            .unwrap_or_default()
    }

    fn declaration_implicit_receivers(&self, scope: crate::fir::SignatureScope) -> Vec<Ty> {
        let Some(anchor) = self.headers.declarations.anchor(scope.owner) else {
            return Vec::new();
        };
        let context_receivers = self.declaration_context_receivers(scope);
        if let Some(mut owner) = self.declaration_semantic_parent(scope.owner) {
            let mut receivers = Vec::new();
            let mut direct_classifier = true;
            loop {
                let Some(owner_anchor) = self.headers.declarations.anchor(owner) else {
                    return Vec::new();
                };
                if owner_anchor.kind == crate::fir::DeclarationKind::Classifier {
                    let Some(signature) = self.classifier_signature(owner) else {
                        return Vec::new();
                    };
                    if direct_classifier {
                        if anchor.kind == crate::fir::DeclarationKind::Function {
                            if let Some(extension) = signature
                                .member_ext_funs
                                .values()
                                .flatten()
                                .find(|function| {
                                    function.signature().stable_declaration == Some(scope.owner)
                                })
                                .map(|function| function.receiver_ty())
                            {
                                receivers.push(extension);
                            }
                        } else if anchor.kind == crate::fir::DeclarationKind::Property {
                            if let Some(extension) = signature
                                .member_ext_props
                                .values()
                                .flatten()
                                .find(|property| property.stable_declaration() == Some(scope.owner))
                                .map(|property| property.receiver_ty())
                            {
                                receivers.push(extension);
                            }
                        }
                        receivers.extend(context_receivers.iter().copied());
                    }
                    let arguments = signature
                        .type_params
                        .iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let bound = signature
                                .type_param_bounds
                                .get(index)
                                .copied()
                                .filter(|bound| *bound != Ty::Error)
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                            Ty::ty_param(name, bound)
                        })
                        .collect::<Vec<_>>();
                    receivers.push(Ty::obj_args_name(signature.internal, &arguments));
                    if direct_classifier {
                        receivers.extend(self.classifier_context_receivers(owner, scope.source));
                    }
                    let Some(outer) = signature.inner_of else {
                        // A local/anonymous classifier captures the dispatch receiver of the
                        // member body that contains it even though it is not a nominal Kotlin
                        // `inner` class. Its stable owner chain ends at the local classifier, so
                        // recover that lexical rung from source containment. Add only the nearest
                        // enclosing non-local classifier; its own nominal `inner_of` chain supplies
                        // any further visible outer instances.
                        let is_local = self.headers.stubs.iter().any(|stub| {
                            stub.id == owner
                                && stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                        });
                        if is_local {
                            let lexical = self.lexical_class_names(scope);
                            if let Some(index) = lexical
                                .iter()
                                .position(|candidate| *candidate == signature.internal)
                            {
                                for enclosing in lexical.iter().skip(index + 1).copied() {
                                    for receiver in self.classifier_receiver_chain(enclosing) {
                                        if !receivers.contains(&receiver) {
                                            receivers.push(receiver);
                                        }
                                    }
                                    let enclosing_is_local = self
                                        .table
                                        .class_by_type_name(enclosing)
                                        .and_then(|classifier| classifier.stable_declaration)
                                        .is_some_and(|declaration| {
                                            self.headers.stubs.iter().any(|stub| {
                                                stub.id == declaration
                                                    && stub.flags.has(
                                                        crate::fir::DeclarationFlags::LOCAL_CLASS,
                                                    )
                                            })
                                        });
                                    if !enclosing_is_local {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(root) = self.headers.local_classifier_lexical_root(owner) {
                            owner = root;
                            direct_classifier = false;
                            continue;
                        }
                        return receivers;
                    };
                    let Some(outer_declaration) = self
                        .table
                        .classes
                        .get(&outer)
                        .and_then(|outer| outer.stable_declaration)
                    else {
                        return Vec::new();
                    };
                    owner = outer_declaration;
                    direct_classifier = false;
                    continue;
                }
                let Some(parent) = self.declaration_semantic_parent(owner) else {
                    return Vec::new();
                };
                owner = parent;
            }
        }
        // A top-level EXTENSION PROPERTY (`val A.z get() = this.x`) also has a receiver, and `this`
        // inside its accessor resolves to it. Only functions were consulted here, so the receiver
        // was invisible and the whole module's signatures declined with no diagnostic.
        if let Some(receiver) = self
            .table
            .ext_props
            .values()
            .flatten()
            .find(|property| property.stable_declaration == Some(scope.owner))
            .map(|property| property.receiver)
        {
            let mut receivers = vec![receiver];
            receivers.extend(context_receivers);
            return receivers;
        }
        let source_receiver = self
            .table
            .funs
            .values()
            .flatten()
            .chain(
                self.table
                    .ext_funs
                    .values()
                    .flat_map(HashMap::values)
                    .flatten(),
            )
            .find(|signature| signature.stable_declaration == Some(scope.owner))
            .and_then(|signature| signature.source_receiver);
        let mut receivers = source_receiver.into_iter().collect::<Vec<_>>();
        receivers.extend(context_receivers);
        receivers
    }
}

fn semantic_classifier_self(signature: &ClassSig) -> Ty {
    let own = signature
        .type_params
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            Ty::ty_param(
                parameter,
                signature
                    .type_param_bounds
                    .get(ordinal)
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        });
    let captured = signature
        .captured_type_parameters
        .type_params
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            Ty::ty_param(
                parameter,
                signature
                    .captured_type_parameters
                    .type_param_bounds
                    .get(ordinal)
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        });
    let arguments = own.chain(captured).collect::<Vec<_>>();
    Ty::obj_args_name(signature.internal, &arguments)
}

/// Complete source classifier applications emitted by the transitional checked-signature table.
/// Source syntax writes only an inner/local classifier's own arguments; the stable semantic type
/// carries captured declaration arguments after them. Compact explicit-header resolution will
/// eventually be the sole producer, but every signature crossing this bridge must already obey the
/// final representation invariant.
fn semantic_type_with_classifier_captures(table: &SymbolTable, ty: Ty) -> Ty {
    match ty {
        Ty::Obj(owner, arguments) => {
            let mut arguments = arguments
                .iter()
                .copied()
                .map(|argument| semantic_type_with_classifier_captures(table, argument))
                .collect::<Vec<_>>();
            if let Some(classifier) = table.class_by_type_name(owner) {
                let own = classifier.type_params.len();
                let captured = &classifier.captured_type_parameters;
                if arguments.len() >= own && arguments.len() < own + captured.type_params.len() {
                    arguments.extend(
                        captured
                            .type_params
                            .iter()
                            .enumerate()
                            .skip(arguments.len().saturating_sub(own))
                            .map(|(ordinal, parameter)| {
                                Ty::ty_param(
                                    parameter,
                                    captured
                                        .type_param_bounds
                                        .get(ordinal)
                                        .copied()
                                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                                )
                            }),
                    );
                }
            }
            Ty::obj_args_name(owner, &arguments)
        }
        Ty::Fun(function) => Ty::fun_with_shape(
            function
                .params
                .iter()
                .copied()
                .map(|parameter| semantic_type_with_classifier_captures(table, parameter))
                .collect(),
            semantic_type_with_classifier_captures(table, function.ret),
            function.context_count,
            function.has_receiver,
            function.suspend,
        ),
        Ty::Nullable(inner) => Ty::nullable(semantic_type_with_classifier_captures(table, *inner)),
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(semantic_type_with_classifier_captures(table, *inner))
        }
        Ty::InProjection(inner) => {
            Ty::in_projection(semantic_type_with_classifier_captures(table, *inner))
        }
        Ty::OutProjection(inner) => {
            Ty::out_projection(semantic_type_with_classifier_captures(table, *inner))
        }
        Ty::StarProjection(inner) => {
            Ty::star_projection(semantic_type_with_classifier_captures(table, *inner))
        }
        other => other,
    }
}

/// Revalidate the completed semantic parent graph after body-local aliases have been expanded.
/// The transitional collector cannot resolve a statement-local alias and therefore cannot see a
/// cycle expressed through one; conversely, its unresolved spelling must not cause a valid compact
/// edge to be discarded. Returning the exact cyclic compact edges lets publication distinguish
/// those two cases without retaining alias syntax beyond Pass 1.
fn compact_classifier_cycle_edges(
    table: &SymbolTable,
    classifier_types: &HashMap<crate::fir::DeclarationId, TypeName>,
    resolved: &HashMap<crate::fir::DeclarationId, (Option<Ty>, Vec<Ty>)>,
) -> HashSet<(crate::fir::DeclarationId, TypeName)> {
    let mut graph = super::supertype_graph(table);
    for (declaration, (superclass, interfaces)) in resolved {
        let Some(owner) = classifier_types.get(declaration).copied() else {
            continue;
        };
        let parents = superclass
            .iter()
            .chain(interfaces)
            .filter_map(|parent| parent.non_null().kotlin_class_internal())
            .collect::<Vec<_>>();
        graph.entry(owner).or_default().extend(parents);
    }
    let (component_of, cyclic_components) = super::supertype_components(&graph);
    let mut rejected = HashSet::new();
    for (declaration, (superclass, interfaces)) in resolved {
        let Some(component) = classifier_types
            .get(declaration)
            .and_then(|owner| component_of.get(owner))
            .copied()
            .filter(|component| cyclic_components.contains(component))
        else {
            continue;
        };
        for parent in superclass.iter().chain(interfaces) {
            let Some(parent) = parent.non_null().kotlin_class_internal() else {
                continue;
            };
            if component_of.get(&parent).copied() == Some(component) {
                rejected.insert((*declaration, parent));
            }
        }
    }
    rejected
}

/// Publish classifier inheritance from compact Pass-1 syntax. Every source-written edge is resolved
/// from its compact semantic type. The transitional collector contributes only language-defined
/// implicit parents, never a fallback spelling or type argument for a source edge.
fn compact_classifier_parents(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    declaration: crate::fir::DeclarationId,
    source: crate::fir::SourceFileId,
    classifier: &ClassSig,
    resolved_local: Option<&(Option<Ty>, Vec<Ty>)>,
    compact_cycle_edges: &HashSet<(crate::fir::DeclarationId, TypeName)>,
) -> Option<(Option<Ty>, Vec<Ty>, Vec<Ty>)> {
    let header = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Classifier {
        supertypes, base, ..
    } = header.kind
    else {
        return None;
    };
    let scope = crate::fir::SignatureScope {
        owner: declaration,
        source,
    };
    let explicit_base = resolved_local
        .and_then(|(base, _)| *base)
        .or_else(|| base.and_then(|syntax| semantics.resolve_compact_header_type(scope, syntax)));
    if base.is_some() && explicit_base.is_none() {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} failed explicit base resolved_local={resolved_local:?}",
        );
        return None;
    }
    let source_syntax = headers.syntax.type_operands(supertypes);
    if resolved_local.is_some_and(|(_, parents)| parents.len() != source_syntax.len()) {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} compact/source supertype count mismatch compact={} source={}",
            resolved_local.map_or(0, |(_, parents)| parents.len()),
            source_syntax.len(),
        );
        return None;
    }
    let resolved_source_supertypes = source_syntax
        .iter()
        .enumerate()
        .map(|(ordinal, syntax)| {
            resolved_local
                .and_then(|(_, parents)| parents.get(ordinal).copied())
                .or_else(|| semantics.resolve_compact_header_type(scope, *syntax))
        })
        .collect::<Option<Vec<_>>>()?;

    let parent_is_interface = |parent: Ty| {
        if matches!(parent.non_null(), Ty::Fun(_)) {
            return Some(true);
        }
        let owner = parent.non_null().kotlin_class_internal()?;
        semantics
            .table
            .class_by_type_name(owner)
            .map(ClassSig::is_interface)
            .or_else(|| {
                semantics
                    .table
                    .libraries
                    .classifier(owner)
                    .map(|classifier| classifier.is_interface())
            })
    };
    let source_superclass = if explicit_base.is_some() {
        None
    } else {
        resolved_source_supertypes
            .iter()
            .position(|parent| parent_is_interface(*parent) == Some(false))
    };
    if resolved_source_supertypes
        .iter()
        .enumerate()
        .any(|(ordinal, parent)| {
            parent_is_interface(*parent).is_none()
                || parent_is_interface(*parent) == Some(false) && Some(ordinal) != source_superclass
        })
    {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} has invalid source parents={resolved_source_supertypes:?} classifications={:?} selected_superclass={source_superclass:?}",
            resolved_source_supertypes
                .iter()
                .map(|parent| parent_is_interface(*parent))
                .collect::<Vec<_>>(),
        );
        return None;
    }

    // Signature collection has already validated the complete module hierarchy and removed only
    // edges that participate in a source cycle. Compact type resolution above recovers the applied
    // source shapes, but must not resurrect one of those rejected nominal edges merely because its
    // syntax still exists in the header inventory.
    let retained_parent = |parent: Ty| {
        if matches!(parent.non_null(), Ty::Fun(_)) {
            return true;
        }
        parent
            .non_null()
            .kotlin_class_internal()
            .is_some_and(|owner| {
                if resolved_local.is_some() {
                    // A body-local alias is visible only while its bounded Pass-1 unit is live. The
                    // compact graph has already expanded and resolved that edge; the transitional
                    // collector can retain only the unresolvable alias spelling and therefore cannot
                    // validate its identity. Reject semantic cycle edges using the completed compact
                    // graph, otherwise make its resolved parent authoritative.
                    return !compact_cycle_edges.contains(&(declaration, owner));
                }
                {
                    classifier.super_internal == Some(owner)
                        || classifier
                            .interfaces
                            .iter_ids()
                            .any(|parent| parent == owner)
                }
            })
    };
    let superclass = explicit_base
        .or_else(|| source_superclass.map(|ordinal| resolved_source_supertypes[ordinal]))
        .filter(|parent| retained_parent(*parent))
        .or_else(|| {
            let owner = classifier.super_internal?;
            let implicit = semantic_type_with_classifier_captures(
                semantics.table,
                Ty::obj_args_name(owner, &classifier.super_type_args),
            );
            (!implicit.mentions_error()).then_some(implicit)
        });

    let mut interfaces = resolved_source_supertypes
        .iter()
        .enumerate()
        .filter_map(|(ordinal, parent)| (Some(ordinal) != source_superclass).then_some(*parent))
        .filter(|parent| retained_parent(*parent))
        .collect::<Vec<_>>();
    // Add validated implicit language parents from ClassSig. Source interfaces already carry their
    // compact applied arguments above; compare by resolved identity rather than relying on an
    // ordinal, because cycle removal can shrink the validated source-interface list.
    for (ordinal, owner) in classifier.interfaces.iter_ids().enumerate() {
        if interfaces
            .iter()
            .any(|parent| parent.non_null().kotlin_class_internal() == Some(owner))
        {
            continue;
        }
        let implicit = semantic_type_with_classifier_captures(
            semantics.table,
            Ty::obj_args_name(
                owner,
                classifier
                    .interface_type_args
                    .get(ordinal)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            ),
        );
        if implicit.mentions_error() {
            crate::trace_compiler!(
                "signature",
                "classifier parent resolution declaration={declaration:?} implicit interface {owner} is unpublishable as {implicit:?}",
            );
            return None;
        }
        interfaces.push(implicit);
    }
    Some((superclass, interfaces, resolved_source_supertypes))
}

/// Reapply a compact header's star-projection syntax to a provisionally resolved legacy type.
///
/// The legacy collector has no classifier-shape input while resolving a `TypeRef`, so it represents
/// every `*` as `out Any?`. The compact header still owns the exact star bit and the completed symbol
/// table owns the target parameter bound. Combine those stable Pass-1 facts at publication time;
/// an explicitly written `out Any?` has no star bit and is therefore never rewritten.
fn compact_header_star_bounds(table: &SymbolTable, syntax: &TypeRef, resolved: Ty) -> Ty {
    match resolved {
        Ty::Nullable(inner) => Ty::nullable(compact_header_star_bounds(table, syntax, *inner)),
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(compact_header_star_bounds(table, syntax, *inner))
        }
        Ty::InProjection(inner) if !syntax.is_star_projection() => {
            Ty::in_projection(compact_header_star_bounds(table, syntax, *inner))
        }
        Ty::OutProjection(inner) if !syntax.is_star_projection() => {
            Ty::out_projection(compact_header_star_bounds(table, syntax, *inner))
        }
        Ty::Obj(owner, resolved_arguments) if !syntax.targs.is_empty() => {
            let classifier = table.class_by_type_name(owner);
            let mut arguments = resolved_arguments.to_vec();
            for index in 0..syntax.targs.len().min(arguments.len()) {
                let argument_syntax = &syntax.targs[index];
                if !argument_syntax.is_star_projection() {
                    arguments[index] =
                        compact_header_star_bounds(table, argument_syntax, arguments[index]);
                    continue;
                }
                let bindings = classifier
                    .into_iter()
                    .flat_map(|classifier| classifier.type_params.iter())
                    .zip(arguments.iter())
                    .enumerate()
                    .filter_map(|(ordinal, (formal, actual))| {
                        (ordinal != index).then_some((formal.clone(), *actual))
                    })
                    .collect::<crate::symbol_resolver::GSigBinds>();
                let upper_bound = classifier
                    .and_then(|classifier| classifier.type_param_bounds.get(index))
                    .copied()
                    .map(|bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings))
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                arguments[index] = Ty::star_projection(upper_bound);
            }
            Ty::obj_args_name(owner, &arguments)
        }
        Ty::Fun(function) if syntax.name == "<fun>" => {
            let parameters = syntax
                .fun_params
                .iter()
                .zip(function.params.iter())
                .map(|(syntax, resolved)| compact_header_star_bounds(table, syntax, *resolved))
                .collect::<Vec<_>>();
            let result = syntax
                .arg
                .as_deref()
                .map(|syntax| compact_header_star_bounds(table, syntax, function.ret))
                .unwrap_or(function.ret);
            Ty::fun_with_shape(
                parameters,
                result,
                function.context_count,
                function.has_receiver,
                function.suspend,
            )
        }
        other => other,
    }
}

/// Give capture storage owned by retained Pass-1 bodies stable, non-source-visible identities.
/// The declaration is needed only by signature publication for the retained inline/default unit;
/// ordinary Pass-2 captures stay in checked FIR as classifier/field coordinates and never enter the
/// module header index.
pub(crate) fn install_streamed_anonymous_capture_declarations(
    files: &[File],
    headers: &mut crate::fir::StreamedHeaderModule,
    table: &mut SymbolTable,
) {
    use crate::fir::{DeclarationAnchor, DeclarationFlags, DeclarationKind, DeclarationStub};

    let captures = table
        .anonymous_object_captures
        .iter()
        .map(|(&key, captures)| (key, captures.clone()))
        .collect::<Vec<_>>();
    for ((source, transient), captures) in captures {
        let Some(Decl::Class(class)) = files
            .get(source as usize)
            .and_then(|file| file.decl_arena.get(transient.0 as usize))
        else {
            continue;
        };
        let Some(classifier) = table
            .anonymous_object_types
            .get(&(source, transient))
            .copied()
        else {
            continue;
        };
        let Some(owner) = table
            .class_by_type_name(classifier)
            .and_then(|class| class.stable_declaration)
        else {
            continue;
        };

        for (ordinal, capture) in captures.iter().enumerate() {
            let synthetic_storage_property = table
                .class_by_type_name(classifier)
                .and_then(|class| class.declared_props.get(&capture.name))
                .is_some_and(|property| !property.source_visible);
            if !synthetic_storage_property {
                // A same-named source property keeps its own stable declaration. Capture storage
                // is addressed by its checked ordinal in FIR and may share that property's
                // physical field later; it must never replace the source-visible signature.
                continue;
            }
            let sibling = u32::MAX
                .checked_sub(u32::try_from(ordinal).expect("too many anonymous-object captures"))
                .expect("too many anonymous-object captures");
            let declaration = headers.declarations.intern(DeclarationAnchor {
                source: crate::fir::SourceFileId::from_raw(source),
                range: class.span,
                owner: Some(owner),
                kind: DeclarationKind::Property,
                sibling,
            });
            let name = headers.lookup_names.intern(&capture.name);
            headers.stubs.push(DeclarationStub {
                id: declaration,
                source: crate::fir::SourceFileId::from_raw(source),
                range: class.span,
                lookup_name: Some(name),
                body: None,
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Property,
                visibility: crate::types::Visibility::Private,
                flags: DeclarationFlags::default()
                    .with(DeclarationFlags::LOCAL_CLASS, true)
                    .with(DeclarationFlags::COMPILER_GENERATED, true)
                    .with(DeclarationFlags::FINAL, true),
            });
            if let Some(property) = table
                .class_by_type_name_mut(classifier)
                .and_then(|class| class.declared_props.get_mut(&capture.name))
            {
                debug_assert!(!property.source_visible);
                property.stable_declaration = Some(declaration);
            }
        }
    }
}

/// Give declarations contributed by frontend plugins stable module identities before the
/// temporary collection table is projected into [`crate::fir::ResolvedModuleIndex`]. Plugins
/// contribute complete semantic callable shapes, so these declarations need no compact source
/// syntax; they do need the same owner graph as written and language-generated declarations.
pub(crate) fn install_streamed_plugin_declarations(
    headers: &mut crate::fir::StreamedHeaderModule,
    table: &mut SymbolTable,
) {
    use crate::fir::{DeclarationAnchor, DeclarationFlags, DeclarationKind, DeclarationStub};

    fn unused_sibling(
        headers: &crate::fir::StreamedHeaderModule,
        owner: crate::fir::DeclarationId,
        kind: DeclarationKind,
    ) -> u32 {
        (0..=u32::MAX)
            .rev()
            .find(|candidate| {
                !headers.stubs.iter().any(|stub| {
                    headers.declarations.anchor(stub.id).is_some_and(|anchor| {
                        anchor.owner == Some(owner)
                            && anchor.kind == kind
                            && anchor.sibling == *candidate
                    })
                })
            })
            .expect("a declaration owner exhausted its structural ordinals")
    }

    let companion_owners = table
        .classes
        .values()
        .filter_map(|class| {
            Some((
                class.companion_internal?,
                (
                    class.stable_declaration?,
                    crate::fir::SourceFileId::from_raw(class.source_file),
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    let mut generated_owners = table
        .classes
        .values()
        .filter(|class| {
            class.methods.values().flatten().any(|signature| {
                signature.plugin_expression.is_some() && signature.stable_declaration.is_none()
            })
        })
        .map(|class| class.internal)
        .collect::<Vec<_>>();
    generated_owners.sort_by_key(|owner| owner.render());

    for internal in generated_owners {
        let stable_owner = match table
            .class_by_type_name(internal)
            .and_then(|class| class.stable_declaration)
        {
            Some(declaration) => declaration,
            None => {
                let Some(&(outer, source)) = companion_owners.get(&internal) else {
                    panic!("a plugin-generated classifier must have a stable enclosing owner")
                };
                let range = headers
                    .declarations
                    .anchor(outer)
                    .expect("a stable enclosing classifier must retain its Pass-1 range")
                    .range;
                let declaration = headers.declarations.intern(DeclarationAnchor {
                    source,
                    range,
                    owner: Some(outer),
                    kind: DeclarationKind::Classifier,
                    sibling: unused_sibling(headers, outer, DeclarationKind::Classifier),
                });
                headers.stubs.push(DeclarationStub {
                    id: declaration,
                    source,
                    range,
                    lookup_name: Some(headers.lookup_names.intern("Companion")),
                    body: None,
                    signature_inference: None,
                    initialization_order: None,
                    kind: DeclarationKind::Classifier,
                    visibility: crate::types::Visibility::Public,
                    flags: DeclarationFlags::default()
                        .with(DeclarationFlags::COMPANION, true)
                        .with(DeclarationFlags::FINAL, true)
                        .with(DeclarationFlags::COMPILER_GENERATED, true),
                });
                let constructor = headers.declarations.intern(DeclarationAnchor {
                    source,
                    range,
                    owner: Some(declaration),
                    kind: DeclarationKind::Constructor,
                    sibling: 0,
                });
                headers.stubs.push(DeclarationStub {
                    id: constructor,
                    source,
                    range,
                    lookup_name: None,
                    body: None,
                    signature_inference: None,
                    initialization_order: None,
                    kind: DeclarationKind::Constructor,
                    visibility: crate::types::Visibility::Private,
                    flags: DeclarationFlags::default()
                        .with(DeclarationFlags::COMPILER_GENERATED, true),
                });
                let generated = table
                    .class_by_type_name_mut(internal)
                    .expect("a generated plugin owner must remain in the collection table");
                generated.stable_declaration = Some(declaration);
                generated.primary_constructor_declaration = Some(constructor);
                declaration
            }
        };
        let owner_anchor = headers
            .declarations
            .anchor(stable_owner)
            .expect("a stable plugin owner must retain its Pass-1 range");
        let method_plans = {
            let class = table
                .class_by_type_name(internal)
                .expect("a generated plugin owner must remain in the collection table");
            let mut plans = Vec::new();
            for name in &class.declared_callable_order {
                if let Some(overloads) = class.methods.get(name) {
                    plans.extend(overloads.iter().enumerate().filter_map(
                        |(ordinal, signature)| {
                            if signature.plugin_expression.is_some()
                                && signature.stable_declaration.is_none()
                            {
                                Some((name.clone(), ordinal))
                            } else {
                                None
                            }
                        },
                    ));
                }
            }
            plans
        };
        for (name, ordinal) in method_plans {
            let sibling = unused_sibling(headers, stable_owner, DeclarationKind::Function);
            let declaration = headers.declarations.intern(DeclarationAnchor {
                source: owner_anchor.source,
                range: owner_anchor.range,
                owner: Some(stable_owner),
                kind: DeclarationKind::Function,
                sibling,
            });
            let (visibility, flags) = {
                let signature = table
                    .class_by_type_name_mut(internal)
                    .and_then(|class| class.methods.get_mut(&name))
                    .and_then(|overloads| overloads.get_mut(ordinal))
                    .expect("a planned plugin callable must remain in its owner");
                signature.stable_declaration = Some(declaration);
                (
                    signature.visibility,
                    DeclarationFlags::default()
                        .with(DeclarationFlags::INLINE, signature.is_inline())
                        .with(DeclarationFlags::FINAL, signature.is_final())
                        .with(DeclarationFlags::OVERRIDE, signature.is_override())
                        .with(DeclarationFlags::ABSTRACT, signature.is_abstract())
                        .with(DeclarationFlags::SUSPEND, signature.is_suspend())
                        .with(DeclarationFlags::OPERATOR, signature.is_operator())
                        .with(DeclarationFlags::INFIX, signature.is_infix())
                        .with(DeclarationFlags::COMPILER_GENERATED, true),
                )
            };
            headers.stubs.push(DeclarationStub {
                id: declaration,
                source: owner_anchor.source,
                range: owner_anchor.range,
                lookup_name: Some(headers.lookup_names.intern(&name)),
                body: None,
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Function,
                visibility,
                flags,
            });
        }
    }
}

pub(crate) struct StreamedSignatureIndex {
    pub(crate) index: crate::fir::ResolvedModuleIndex,
    pub(crate) failures: Vec<crate::fir::DeclarationId>,
}

pub(crate) fn finalized_streamed_signature_index(
    headers: &crate::fir::StreamedHeaderModule,
    table: &mut SymbolTable,
    extracted: crate::fir::SignatureConstraintExtractor,
    source_contracts: Vec<SourceContractCandidate>,
    diags: &mut crate::diag::DiagSink,
) -> StreamedSignatureIndex {
    use crate::fir::DeclarationKind;

    fn semantic_parameters(signature: &Signature) -> Vec<Ty> {
        signature
            .generic_sig
            .as_ref()
            .map(|generic| generic.params.clone())
            .unwrap_or_else(|| signature.params.clone())
    }

    fn semantic_result(signature: &Signature) -> Ty {
        signature
            .generic_sig
            .as_ref()
            .map_or(signature.ret, |generic| generic.ret)
    }

    /// Seed a declaration shape directly from compact syntax when the transitional symbol table
    /// has no corresponding member. Every written type is resolved below by the ordinary semantic
    /// header resolver; `Pending` here is only an internal slot value and is replaced before a
    /// `ResolvedSignature` can be published. An inferred result remains a signature-graph output.
    fn compact_header_seed(
        headers: &crate::fir::StreamedHeaderModule,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Vec<Ty>, Ty, Option<Ty>)> {
        let declaration = headers.syntax.declaration(declaration)?;
        match declaration.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                result,
                ..
            } => Some((
                vec![Ty::Pending; headers.syntax.parameters(parameters).len()],
                match result {
                    crate::fir::HeaderResultType::ImplicitUnit => Ty::Unit,
                    crate::fir::HeaderResultType::Explicit(_)
                    | crate::fir::HeaderResultType::Inferred => Ty::Pending,
                },
                receiver.map(|_| Ty::Pending),
            )),
            crate::fir::HeaderDeclarationKind::Property {
                receiver,
                context_parameters,
                ..
            } => Some((
                vec![Ty::Pending; headers.syntax.parameters(context_parameters).len()],
                Ty::Pending,
                receiver.map(|_| Ty::Pending),
            )),
            crate::fir::HeaderDeclarationKind::Classifier { .. }
            | crate::fir::HeaderDeclarationKind::Constructor { .. }
            | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
        }
    }

    fn stable_function<'a>(
        table: &'a SymbolTable,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(&'a Signature, Option<Ty>)> {
        if let Some(signature) = table
            .funs
            .values()
            .flatten()
            .chain(table.ext_funs.values().flat_map(HashMap::values).flatten())
            .chain(
                table
                    .classes
                    .values()
                    .flat_map(|class| class.methods.values().flatten()),
            )
            .find(|signature| signature.stable_declaration == Some(declaration))
        {
            return Some((signature, signature.source_receiver));
        }
        table
            .classes
            .values()
            .flat_map(|class| class.member_ext_funs.values().flatten())
            .find(|function| function.signature().stable_declaration == Some(declaration))
            .map(|function| {
                let receiver = function
                    .signature()
                    .generic_sig
                    .as_ref()
                    .and_then(|generic| generic.receiver)
                    .unwrap_or_else(|| function.receiver_ty());
                (function.signature(), Some(receiver))
            })
    }

    fn stable_property(
        table: &SymbolTable,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Vec<Ty>, Ty, Option<Ty>)> {
        if let Some(property) = table
            .source_props
            .values()
            .find(|property| property.stable_declaration == Some(declaration))
        {
            return Some((property.context_params.clone(), property.ty, None));
        }
        if let Some(property) = table
            .ext_props
            .values()
            .flatten()
            .find(|property| property.stable_declaration == Some(declaration))
        {
            return Some((
                property.context_params.clone(),
                property.ty,
                Some(property.receiver),
            ));
        }
        for class in table.classes.values() {
            if let Some((name, property)) = class
                .declared_props
                .iter()
                .find(|(_, property)| property.stable_declaration == Some(declaration))
            {
                let ty = class
                    .generic_property_shapes
                    .get(name)
                    .copied()
                    // Direct nullable type parameters use the legacy class signature's dedicated
                    // nullable-parameter table rather than `generic_property_shapes`. Stable FIR
                    // must nevertheless publish the same symbolic type as the primary constructor,
                    // not the erased `Any?` member-selection view. The constructor's declaration
                    // shape is the authoritative source for a property parameter.
                    .or_else(|| {
                        class
                            .ctor_param_names
                            .iter()
                            .position(|(parameter, _)| parameter == name)
                            .and_then(|ordinal| class.ctor_param_shapes.get(ordinal))
                            .map(|(shape, _)| *shape)
                    })
                    .unwrap_or(property.ty);
                return Some((property.context_params.clone(), ty, None));
            }
            if let Some(property) = class
                .contextual_props
                .values()
                .flatten()
                .find(|property| property.stable_declaration == Some(declaration))
            {
                return Some((property.context_params.clone(), property.ty, None));
            }
            if let Some(property) = class
                .member_ext_props
                .values()
                .flatten()
                .find(|property| property.stable_declaration() == Some(declaration))
            {
                return Some((
                    property.context_params().to_vec(),
                    property.ret(),
                    Some(property.receiver_ty()),
                ));
            }
        }
        None
    }

    fn stable_property_annotations(
        table: &SymbolTable,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&[TypeName]> {
        if let Some(property) = table
            .source_props
            .values()
            .find(|property| property.stable_declaration == Some(declaration))
        {
            return Some(&property.annotations);
        }
        if let Some(property) = table
            .ext_props
            .values()
            .flatten()
            .find(|property| property.stable_declaration == Some(declaration))
        {
            return Some(&property.annotations);
        }
        for class in table.classes.values() {
            if let Some(property) = class
                .declared_props
                .values()
                .find(|property| property.stable_declaration == Some(declaration))
            {
                return Some(&property.annotations);
            }
            if let Some(property) = class
                .contextual_props
                .values()
                .flatten()
                .find(|property| property.stable_declaration == Some(declaration))
            {
                return Some(&property.annotations);
            }
        }
        None
    }

    fn stable_constructor(
        table: &SymbolTable,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Vec<Ty>, Ty, Option<Ty>)> {
        table.classes.values().find_map(|class| {
            if class.primary_constructor_declaration == Some(declaration) {
                return Some((
                    class
                        .ctor_param_shapes
                        .iter()
                        .map(|(parameter, _)| *parameter)
                        .collect(),
                    semantic_classifier_self(class),
                    None,
                ));
            }
            class
                .secondary_constructor_declarations
                .iter()
                .position(|candidate| *candidate == Some(declaration))
                .and_then(|ordinal| class.secondary_ctor_shapes.get(ordinal).cloned())
                .map(|parameters| (parameters, semantic_classifier_self(class), None))
        })
    }

    fn stable_constructor_annotations(
        table: &SymbolTable,
        declaration: crate::fir::DeclarationId,
    ) -> Option<&[TypeName]> {
        table.classes.values().find_map(|class| {
            if class.primary_constructor_declaration == Some(declaration) {
                return Some(class.primary_constructor_annotations.as_slice());
            }
            class
                .secondary_constructor_declarations
                .iter()
                .position(|candidate| *candidate == Some(declaration))
                .and_then(|ordinal| class.secondary_constructor_annotations.get(ordinal))
                .map(Vec::as_slice)
        })
    }

    fn stable_constructor_implicit_integer_coercion(
        table: &SymbolTable,
        declaration: crate::fir::DeclarationId,
        ordinal: usize,
    ) -> bool {
        table.classes.values().any(|class| {
            if class.primary_constructor_declaration == Some(declaration) {
                return class
                    .ctor_implicit_integer_coercion
                    .get(ordinal)
                    .copied()
                    .unwrap_or(false);
            }
            class
                .secondary_constructor_declarations
                .iter()
                .position(|candidate| *candidate == Some(declaration))
                .and_then(|constructor| class.secondary_ctor_call_sigs.get(constructor))
                .and_then(|signature| signature.implicit_integer_coercion.get(ordinal))
                .copied()
                .unwrap_or(false)
        })
    }

    fn generated_function<'a>(
        table: &'a SymbolTable,
        stub: &crate::fir::DeclarationStub,
    ) -> Option<(&'a Signature, Option<Ty>)> {
        if let Some(signature) = table
            .classes
            .values()
            .flat_map(|class| class.methods.values().flatten())
            .find(|signature| signature.stable_declaration == Some(stub.id))
        {
            return Some((signature, signature.source_receiver));
        }
        table
            .classes
            .values()
            .flat_map(|class| class.member_ext_funs.values().flatten())
            .find(|function| function.signature().stable_declaration == Some(stub.id))
            .map(|function| {
                let receiver = function
                    .signature()
                    .generic_sig
                    .as_ref()
                    .and_then(|generic| generic.receiver)
                    .unwrap_or_else(|| function.receiver_ty());
                (function.signature(), Some(receiver))
            })
    }

    let (graph, extraction_failures) = extracted.into_parts();
    let mut required = Vec::new();
    let mut explicit = Vec::new();
    let mut inferred_parameters = HashMap::new();
    let mut backing_field_declarations = Vec::new();
    let mut explicit_backing_field_types = HashMap::new();
    let mut resolved_receivers = HashMap::new();
    let mut failed = extraction_failures
        .into_iter()
        .map(|failure| {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: extraction refused form {:?}",
                failure.declaration,
                failure.form,
            );
            failure.declaration
        })
        .collect::<Vec<_>>();
    let classifier_types = table
        .classes
        .values()
        .filter_map(|signature| {
            signature
                .stable_declaration
                .map(|declaration| (declaration, signature.internal))
        })
        .collect::<HashMap<_, _>>();
    // Enum entries are declaration headers, not bodies. Publish their compact inventory into the
    // transitional module table before any inferred companion/member signature is solved, so the
    // ordinary shared resolver exposes entries and the synthetic `values`/`valueOf` callables. The
    // legacy collector also fills this table when it owns a full class AST; bounded Pass 1 must not
    // depend on that implementation accident.
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == DeclarationKind::EnumEntry)
    {
        let Some(spelling) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            continue;
        };
        let mut owner = headers
            .declarations
            .anchor(stub.id)
            .and_then(|anchor| anchor.owner);
        let classifier = loop {
            let Some(declaration) = owner else { break None };
            let Some(anchor) = headers.declarations.anchor(declaration) else {
                break None;
            };
            if anchor.kind == DeclarationKind::Classifier {
                if let Some(classifier) = classifier_types.get(&declaration).copied() {
                    break Some(classifier);
                }
            }
            owner = anchor.owner;
        };
        let Some(classifier) = classifier else {
            continue;
        };
        let entries = table.enums.entry(classifier).or_default();
        if !entries.iter().any(|entry| entry == spelling) {
            entries.push(spelling.to_owned());
        }
    }
    let empty_extension_receivers = HashMap::new();
    let explicit_type_semantics = ProductionSignatureSemantics {
        headers,
        table,
        classifier_types: &classifier_types,
        parameters: HashMap::new(),
        extension_receivers: &empty_extension_receivers,
        source_orders: HashMap::new(),
        signature_origins: HashMap::new(),
        scoped_receivers: RefCell::new(HashMap::new()),
        scoped_constraint_inputs: RefCell::new(HashMap::new()),
        scoped_constraints: RefCell::new(HashMap::new()),
        completed_scoped_constraints: RefCell::new(HashMap::new()),
        diagnostics: RefCell::new(Vec::new()),
    };
    for stub in &headers.stubs {
        let block_getter_requires_explicit_type = stub.kind == DeclarationKind::Property
            && stub.signature_inference.is_none()
            && stub.flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER)
            && headers
                .syntax
                .declaration(stub.id)
                .is_some_and(|declaration| {
                    matches!(
                        declaration.kind,
                        crate::fir::HeaderDeclarationKind::Property {
                            declared_type: None,
                            getter_type: None,
                            ..
                        }
                    )
                });
        if block_getter_requires_explicit_type {
            diags.set_file(stub.source.raw());
            diags.error(
                stub.range,
                "this property must have an explicit type, be initialized, or be delegated.",
            );
            failed.push(stub.id);
            continue;
        }
        // Capture fields are synthesized only after retained-inline/active-body capture analysis;
        // they are storage declarations, not source signatures in the compact graph.
        if stub.kind == DeclarationKind::Property
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
        {
            continue;
        }
        let anchor = headers
            .declarations
            .anchor(stub.id)
            .expect("a compact stub must retain its stable anchor");
        let owns_signature_default =
            headers
                .syntax
                .declaration(stub.id)
                .is_some_and(|declaration| {
                    let parameters = match declaration.kind {
                        crate::fir::HeaderDeclarationKind::Callable { parameters, .. }
                        | crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } => {
                            Some(parameters)
                        }
                        crate::fir::HeaderDeclarationKind::Classifier { .. }
                        | crate::fir::HeaderDeclarationKind::Property { .. }
                        | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
                    };
                    parameters.is_some_and(|parameters| {
                        headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .any(|parameter| parameter.flags.has_default())
                    })
                });
        let undemanded_ordinary_local = stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            && !stub.flags.has(crate::fir::DeclarationFlags::INLINE)
            && !owns_signature_default
            && graph.constraint(stub.id).is_none()
            && graph.explicit_signature_types(stub.id).is_none();
        let signature = if stub.kind == DeclarationKind::Function
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
        {
            generated_function(table, stub).map(|(signature, receiver)| {
                (
                    semantic_parameters(signature),
                    semantic_result(signature),
                    receiver,
                )
            })
        } else {
            match stub.kind {
                DeclarationKind::Function => {
                    let enum_entry = anchor.owner.and_then(|owner| {
                        headers
                            .declarations
                            .anchor(owner)
                            .is_some_and(|owner| owner.kind == DeclarationKind::EnumEntry)
                            .then_some(owner)
                    });
                    if enum_entry.is_some() {
                        compact_header_seed(headers, stub.id)
                    } else {
                        stable_function(table, stub.id).map(|(signature, receiver)| {
                            (
                                semantic_parameters(signature),
                                semantic_result(signature),
                                receiver,
                            )
                        })
                    }
                }
                DeclarationKind::Property => {
                    let enum_entry = anchor.owner.and_then(|owner| {
                        headers
                            .declarations
                            .anchor(owner)
                            .is_some_and(|owner| owner.kind == DeclarationKind::EnumEntry)
                            .then_some(owner)
                    });
                    if enum_entry.is_some() {
                        compact_header_seed(headers, stub.id)
                    } else {
                        stable_property(table, stub.id)
                    }
                }
                DeclarationKind::Constructor => stable_constructor(table, stub.id).or_else(|| {
                    (anchor.sibling == 0)
                        .then_some(anchor.owner)
                        .flatten()
                        .and_then(|owner| classifier_types.get(&owner))
                        .and_then(|owner| table.class_by_type_name(*owner))
                        .map(|class| {
                            (
                                class
                                    .ctor_param_shapes
                                    .iter()
                                    .map(|(parameter, _)| *parameter)
                                    .collect(),
                                semantic_classifier_self(class),
                                None,
                            )
                        })
                }),
                DeclarationKind::Classifier
                | DeclarationKind::TypeAlias
                | DeclarationKind::Accessor
                | DeclarationKind::Initializer
                | DeclarationKind::EnumEntry
                | DeclarationKind::Script => continue,
            }
        };
        let Some((mut parameters, mut result, mut receiver)) = signature else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: no signature for kind {:?}",
                stub.id,
                stub.kind,
            );
            failed.push(stub.id);
            continue;
        };
        if undemanded_ordinary_local
            && parameters
                .iter()
                .copied()
                .chain(std::iter::once(result))
                .chain(receiver)
                .any(|ty| crate::fir::ResolvedTy::new(ty).is_err())
        {
            // The transitional symbol table cannot resolve a body-local type outside its lexical
            // rung. Preserve the stable declaration header and let Pass 2 either publish the
            // checked signature or report the source diagnostic; do not turn it into a silent
            // module-finalization failure.
            continue;
        }
        let constructor_property_vararg = (stub.kind == DeclarationKind::Property)
            .then_some(anchor.owner)
            .flatten()
            .and_then(|owner| headers.syntax.declaration(owner))
            .and_then(|owner| match owner.kind {
                crate::fir::HeaderDeclarationKind::Classifier {
                    primary_parameters, ..
                } => headers
                    .syntax
                    .parameters(primary_parameters)
                    .get(anchor.sibling as usize)
                    .map(|parameter| parameter.flags.is_property() && parameter.flags.is_vararg()),
                crate::fir::HeaderDeclarationKind::Callable { .. }
                | crate::fir::HeaderDeclarationKind::Property { .. }
                | crate::fir::HeaderDeclarationKind::Constructor { .. }
                | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
            })
            .unwrap_or(false);
        let compact_declaration = headers.syntax.declaration(stub.id).map(|value| value.kind);
        let has_compact_declaration = compact_declaration.is_some();
        let (parameter_types, result_type, receiver_type, backing_field_type) =
            match compact_declaration {
                Some(crate::fir::HeaderDeclarationKind::Callable {
                    receiver,
                    parameters,
                    result,
                    ..
                }) => (
                    headers
                        .syntax
                        .parameters(parameters)
                        .iter()
                        .map(|parameter| (parameter.ty, parameter.flags.is_vararg()))
                        .collect::<Vec<_>>(),
                    match result {
                        crate::fir::HeaderResultType::Explicit(result) => Some(result),
                        crate::fir::HeaderResultType::ImplicitUnit
                        | crate::fir::HeaderResultType::Inferred => None,
                    },
                    receiver,
                    None,
                ),
                Some(crate::fir::HeaderDeclarationKind::Property {
                    receiver,
                    context_parameters,
                    declared_type,
                    getter_type,
                    backing_field_type,
                    ..
                }) => (
                    headers
                        .syntax
                        .parameters(context_parameters)
                        .iter()
                        .map(|parameter| (parameter.ty, false))
                        .collect::<Vec<_>>(),
                    declared_type.or(getter_type),
                    receiver,
                    backing_field_type,
                ),
                Some(crate::fir::HeaderDeclarationKind::Constructor {
                    context_parameters,
                    parameters,
                }) => {
                    let mut all_parameters = headers
                        .syntax
                        .parameters(context_parameters)
                        .iter()
                        .map(|parameter| (parameter.ty, false))
                        .collect::<Vec<_>>();
                    all_parameters.extend(
                        headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .map(|parameter| (parameter.ty, parameter.flags.is_vararg())),
                    );
                    (all_parameters, None, None, None)
                }
                Some(crate::fir::HeaderDeclarationKind::Classifier { .. })
                | Some(crate::fir::HeaderDeclarationKind::TypeAlias { .. })
                | None => (Vec::new(), None, None, None),
            };
        let scope = crate::fir::SignatureScope {
            owner: stub.id,
            source: stub.source,
        };
        // Non-local explicit headers are resolved from compact syntax regardless of whether the
        // transitional collector produced a superficially publishable type. Syntaxless generated
        // declarations keep their authoritative semantic seed; an empty syntax projection must
        // not erase generated parameters. Local-class headers use the graph expressions below
        // because those already expanded statement-local aliases while their lexical rung was live.
        if !stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS) && has_compact_declaration {
            let mut header_resolution_failed = false;
            let mut resolved_backing_field_type = None;
            let mut resolved_parameters = Vec::with_capacity(parameter_types.len());
            for (syntax, is_vararg) in parameter_types.iter().copied() {
                match explicit_type_semantics.resolve_explicit_header_type(scope, syntax) {
                    Ok(resolved) => {
                        crate::trace_compiler!(
                            "signature",
                            "explicit header parameter declaration={:?} syntax={:?} resolved={resolved:?}",
                            stub.id,
                            headers
                                .syntax
                                .transient_type_ref(syntax, &headers.lookup_names),
                        );
                        resolved_parameters
                            .push(super::semantic_value_parameter_ty(resolved, is_vararg));
                    }
                    Err(_) => {
                        header_resolution_failed = true;
                        break;
                    }
                }
            }
            if !header_resolution_failed {
                parameters = resolved_parameters;
            }
            if let Some(syntax) = result_type {
                match explicit_type_semantics.resolve_explicit_header_type(scope, syntax) {
                    Ok(resolved) => {
                        result = super::semantic_value_parameter_ty(
                            resolved,
                            constructor_property_vararg,
                        )
                    }
                    Err(_) => header_resolution_failed = true,
                }
            }
            if let Some(syntax) = receiver_type {
                match explicit_type_semantics.resolve_explicit_header_type(scope, syntax) {
                    Ok(resolved) => receiver = Some(resolved),
                    Err(_) => header_resolution_failed = true,
                }
            }
            if let Some(syntax) = backing_field_type {
                match explicit_type_semantics.resolve_explicit_header_type(scope, syntax) {
                    Ok(resolved) => resolved_backing_field_type = Some(resolved),
                    Err(_) => header_resolution_failed = true,
                }
            }
            if header_resolution_failed {
                failed.push(stub.id);
                continue;
            }
            if let Some(storage) = resolved_backing_field_type {
                let storage = semantic_type_with_classifier_captures(table, storage);
                let Ok(storage) = crate::fir::ResolvedTy::new(storage) else {
                    failed.push(stub.id);
                    continue;
                };
                explicit_backing_field_types.insert(stub.id, storage);
            }
        }
        for (parameter, (syntax, _)) in parameters.iter_mut().zip(parameter_types) {
            if let Some(syntax) = headers
                .syntax
                .transient_type_ref(syntax, &headers.lookup_names)
            {
                *parameter = compact_header_star_bounds(table, &syntax, *parameter);
            }
        }
        if let Some(syntax) = result_type.and_then(|syntax| {
            headers
                .syntax
                .transient_type_ref(syntax, &headers.lookup_names)
        }) {
            result = compact_header_star_bounds(table, &syntax, result);
        }
        if let Some((syntax, resolved)) = receiver_type
            .and_then(|syntax| {
                headers
                    .syntax
                    .transient_type_ref(syntax, &headers.lookup_names)
            })
            .zip(receiver)
        {
            receiver = Some(compact_header_star_bounds(table, &syntax, resolved));
        }
        let compact_explicit_types = stub
            .flags
            .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            .then(|| graph.explicit_signature_types(stub.id))
            .flatten();
        crate::trace_compiler!(
            "signature",
            "compact explicit declaration={:?} kind={:?} types={compact_explicit_types:?}",
            stub.id,
            stub.kind,
        );
        if let Some(compact) = compact_explicit_types {
            let compact_parameters = graph.operands(compact.parameters);
            if compact_parameters.len() > parameters.len() {
                crate::trace_compiler!(
                    "fir",
                    "signature finalization declined {:?}: compact explicit parameters exceed the semantic shape",
                    stub.id,
                );
                failed.push(stub.id);
                continue;
            }
            let mut replacement_failed = false;
            for (parameter, expression) in parameters.iter_mut().zip(compact_parameters) {
                let Some(resolved) =
                    explicit_type_semantics.resolve_compact_graph_type(&graph, *expression)
                else {
                    replacement_failed = true;
                    break;
                };
                *parameter = resolved;
            }
            if replacement_failed {
                failed.push(stub.id);
                continue;
            }
            if let Some(expression) = compact.result {
                let Some(resolved) =
                    explicit_type_semantics.resolve_compact_graph_type(&graph, expression)
                else {
                    failed.push(stub.id);
                    continue;
                };
                result = super::semantic_value_parameter_ty(resolved, constructor_property_vararg);
            }
            if let Some(expression) = compact.receiver {
                let Some(resolved) =
                    explicit_type_semantics.resolve_compact_graph_type(&graph, expression)
                else {
                    failed.push(stub.id);
                    continue;
                };
                receiver = Some(resolved);
            }
            if let Some(expression) = compact.storage {
                let Some(resolved) =
                    explicit_type_semantics.resolve_compact_graph_type(&graph, expression)
                else {
                    failed.push(stub.id);
                    continue;
                };
                let resolved = semantic_type_with_classifier_captures(table, resolved);
                let Ok(resolved) = crate::fir::ResolvedTy::new(resolved) else {
                    failed.push(stub.id);
                    continue;
                };
                explicit_backing_field_types.insert(stub.id, resolved);
            }
        }
        let parameters = parameters
            .into_iter()
            .map(|parameter| semantic_type_with_classifier_captures(table, parameter))
            .collect::<Vec<_>>();
        let result = semantic_type_with_classifier_captures(table, result);
        let receiver =
            receiver.map(|receiver| semantic_type_with_classifier_captures(table, receiver));
        crate::trace_compiler!(
            "signature",
            "streamed declaration={:?} name={:?} kind={:?} parameters={parameters:?} result={result:?} receiver={receiver:?}",
            stub.id,
            stub.lookup_name.and_then(|name| headers.lookup_names.get(name)),
            stub.kind,
        );
        if let Some(receiver) = receiver {
            let Ok(receiver) = crate::fir::ResolvedTy::new(receiver) else {
                crate::trace_compiler!(
                    "fir",
                    "signature finalization declined {:?}: receiver type is unpublishable",
                    stub.id,
                );
                failed.push(stub.id);
                continue;
            };
            resolved_receivers.insert(stub.id, receiver);
        }
        // An ordinary local inferred result is not a module signature root. It is published in
        // Pass 2 while its real lexical context is active; semantic override plans for its local
        // classifier are completed immediately after that publication.
        let deferred_local_signature = stub.signature_inference.is_some()
            && stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS);
        if !deferred_local_signature {
            required.push(stub.id);
        }
        if stub.signature_inference.is_some() {
            if graph.constraint(stub.id).is_none() {
                if deferred_local_signature {
                    // The stable declaration header is enough for Pass 1. Its result depends on
                    // the ordinary body's live lexical context, so Pass 2 publishes the checked
                    // signature immediately before checking/lowering that local declaration.
                    continue;
                }
                crate::trace_compiler!(
                    "fir",
                    "signature finalization declined {:?}: inferred declaration has no extracted constraint",
                    stub.id,
                );
                failed.push(stub.id);
                continue;
            }
            let parameters = parameters
                .into_iter()
                .map(crate::fir::ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>();
            let Ok(parameters) = parameters else {
                crate::trace_compiler!(
                    "fir",
                    "signature finalization declined {:?}: inferred parameter types are unpublishable",
                    stub.id,
                );
                failed.push(stub.id);
                continue;
            };
            inferred_parameters.insert(stub.id, parameters.into_boxed_slice());
            if stub.signature_inference
                == Some(crate::fir::InferredSignatureKind::BackingFieldInitializer)
            {
                let Ok(signature) = crate::fir::ResolvedSignature::new(
                    inferred_parameters
                        .get(&stub.id)
                        .expect("just inserted")
                        .iter()
                        .map(|parameter| parameter.get()),
                    result,
                ) else {
                    failed.push(stub.id);
                    continue;
                };
                explicit.push((stub.id, signature));
                backing_field_declarations.push(stub.id);
            }
        } else {
            let Ok(signature) = crate::fir::ResolvedSignature::new(parameters, result) else {
                crate::trace_compiler!(
                    "fir",
                    "signature finalization declined {:?}: explicit signature is unpublishable",
                    stub.id,
                );
                failed.push(stub.id);
                continue;
            };
            inferred_parameters.insert(stub.id, signature.parameters.clone());
            explicit.push((stub.id, signature));
        }
    }
    // A body-local member constraint can still be demanded by a surviving non-local or retained
    // inline signature. Seed its resolved parameter headers so that demand uses the same graph.
    // Undemanded ordinary-local results remain Pass-2 lexical work and are published immediately
    // before their checked FIR is built.
    for constraint in graph.constraints() {
        if inferred_parameters.contains_key(&constraint.declaration) {
            continue;
        }
        let Some(stub) = headers.stubs.iter().find(|stub| {
            stub.id == constraint.declaration
                && stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
        }) else {
            continue;
        };
        let parameters = match stub.kind {
            DeclarationKind::Function => {
                stable_function(table, stub.id).map(|(signature, _)| semantic_parameters(signature))
            }
            DeclarationKind::Property => {
                stable_property(table, stub.id).map(|(parameters, _, _)| parameters)
            }
            DeclarationKind::Classifier
            | DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Constructor
            | DeclarationKind::Accessor
            | DeclarationKind::Initializer
            | DeclarationKind::Script => None,
        };
        let Some(parameters) = parameters else {
            failed.push(stub.id);
            continue;
        };
        let parameters = parameters
            .into_iter()
            .map(|parameter| {
                crate::fir::ResolvedTy::new(semantic_type_with_classifier_captures(
                    table, parameter,
                ))
            })
            .collect::<Result<Vec<_>, _>>();
        match parameters {
            Ok(parameters) => {
                inferred_parameters.insert(stub.id, parameters.into_boxed_slice());
            }
            Err(_) => failed.push(stub.id),
        }
    }
    let mut resolved_classifier_parents = HashMap::new();
    for (declaration, parents) in graph.explicit_classifier_parents() {
        let superclass = match parents.superclass {
            Some(expression) => {
                let Some(parent) =
                    explicit_type_semantics.resolve_compact_graph_type(&graph, expression)
                else {
                    failed.push(declaration);
                    continue;
                };
                Some(parent)
            }
            None => None,
        };
        let resolved_supertypes = graph
            .operands(parents.supertypes)
            .iter()
            .map(|expression| {
                explicit_type_semantics.resolve_compact_graph_type(&graph, *expression)
            })
            .collect::<Option<Vec<_>>>();
        let Some(resolved_supertypes) = resolved_supertypes else {
            failed.push(declaration);
            continue;
        };
        resolved_classifier_parents.insert(declaration, (superclass, resolved_supertypes));
    }
    let compact_cycle_edges =
        compact_classifier_cycle_edges(table, &classifier_types, &resolved_classifier_parents);
    failed.sort_by_key(|declaration| declaration.raw());
    failed.dedup();
    if !failed.is_empty() {
        crate::trace_compiler!(
            "fir",
            "signature finalization rejected declarations before solver: {failed:?}",
        );
        for diagnostic in explicit_type_semantics.diagnostics.borrow().iter() {
            diags.set_file(diagnostic.file);
            diags.error(diagnostic.span, diagnostic.message.clone());
        }
    }

    let signature_origins = graph
        .constraints()
        .iter()
        .map(|constraint| (constraint.declaration, constraint.origin))
        .collect();
    let source_orders = headers
        .declaration_inventory()
        .iter()
        .copied()
        .enumerate()
        .map(|(order, declaration)| {
            (
                declaration,
                u32::try_from(order).expect("too many stable declarations"),
            )
        })
        .collect();
    let semantics = ProductionSignatureSemantics {
        headers,
        table,
        classifier_types: &classifier_types,
        parameters: inferred_parameters,
        extension_receivers: &resolved_receivers,
        source_orders,
        signature_origins,
        scoped_receivers: RefCell::new(HashMap::new()),
        scoped_constraint_inputs: RefCell::new(HashMap::new()),
        scoped_constraints: RefCell::new(HashMap::new()),
        completed_scoped_constraints: RefCell::new(HashMap::new()),
        diagnostics: RefCell::new(Vec::new()),
    };
    let evaluator = crate::fir::ResolverBackedSignatureEvaluator::new(&semantics);
    let mut solver = crate::fir::SignatureSolver::new(graph, required);
    for (declaration, signature) in explicit {
        solver.publish_explicit(declaration, signature);
    }
    let mut backing_field_types = explicit_backing_field_types;
    let mut auxiliary_failures = Vec::new();
    for declaration in backing_field_declarations {
        match solver.evaluate_auxiliary_constraint(declaration, &evaluator) {
            Ok(signature) => {
                backing_field_types.insert(declaration, signature.result);
            }
            Err(diagnostic) => auxiliary_failures.push((declaration, Some(diagnostic))),
        }
    }
    let (mut index, mut finalization_failures) = solver.finalize_recovering(&evaluator);
    finalization_failures.extend(auxiliary_failures);
    finalization_failures.sort_by_key(|(declaration, _)| declaration.raw());
    finalization_failures.dedup_by_key(|(declaration, _)| *declaration);
    if !failed.is_empty() || !finalization_failures.is_empty() {
        crate::trace_compiler!(
            "fir",
            "signature publication declined before_solver={failed:?} solver={finalization_failures:?}",
        );
        let failed_declarations = finalization_failures
            .iter()
            .map(|(declaration, _)| *declaration)
            .collect::<std::collections::HashSet<_>>();
        let selected_diagnostics = finalization_failures
            .iter()
            .filter_map(|(_, diagnostic)| *diagnostic)
            .collect::<std::collections::HashSet<_>>();
        for (index, diagnostic) in semantics.diagnostics.borrow().iter().enumerate() {
            let identity = crate::fir::DiagnosticId::from_raw(index as u32 + 1);
            if !failed_declarations.contains(&diagnostic.declaration)
                && !selected_diagnostics.contains(&identity)
            {
                continue;
            }
            diags.set_file(diagnostic.file);
            diags.error(diagnostic.span, diagnostic.message.clone());
        }
        failed.extend(
            finalization_failures
                .into_iter()
                .map(|(declaration, _)| declaration),
        );
        failed.sort_by_key(|declaration| declaration.raw());
        failed.dedup();
    }
    macro_rules! stop_with_failure {
        ($declaration:expr) => {{
            failed.push($declaration);
            failed.sort_by_key(|declaration| declaration.raw());
            failed.dedup();
            return StreamedSignatureIndex {
                index,
                failures: failed,
            };
        }};
    }
    // Publish the compact semantic declaration/classifier graph before header syntax and lookup
    // spellings are destroyed. This is declaration-scaled persistent state; it contains neither
    // parser IDs nor unresolved type syntax.
    let mut deferred_interface_delegations = Vec::new();
    for stub in &headers.stubs {
        let anchor = headers
            .declarations
            .anchor(stub.id)
            .expect("a compact stub must retain its stable anchor");
        index.publish_declaration_header(
            stub.id,
            crate::fir::ResolvedDeclarationHeader {
                kind: stub.kind,
                owner: anchor.owner,
                name: None,
                visibility: stub.visibility,
                flags: stub.flags,
                initialization_order: stub.initialization_order,
            },
            stub.lookup_name
                .and_then(|name| headers.lookup_names.get(name)),
        );
        let annotations = match stub.kind {
            DeclarationKind::Function => stable_function(table, stub.id)
                .map(|(signature, _)| signature.annotations.as_slice()),
            DeclarationKind::Property => stable_property_annotations(table, stub.id),
            DeclarationKind::Constructor => stable_constructor_annotations(table, stub.id),
            DeclarationKind::Classifier => table
                .classes
                .values()
                .find(|class| class.stable_declaration == Some(stub.id))
                .map(|class| class.annotations.as_slice()),
            _ => None,
        };
        if let Some(annotations) = annotations {
            index.publish_declaration_annotations(stub.id, annotations.iter().copied());
            for (ordinal, _) in annotations.iter().enumerate() {
                index.publish_declaration_annotation_string_arguments(
                    stub.id,
                    ordinal as u32,
                    headers
                        .annotation_string_arguments(stub.id, ordinal)
                        .iter()
                        .cloned(),
                );
            }
        }
    }
    // Retained inline/default anonymous bodies need their captured values during the same Pass-1
    // checked-FIR construction. Capture discovery synthesized these declarations after signature
    // extraction, so publish their already-resolved semantic property types directly; they are not
    // lazy graph nodes and contain no source syntax or target storage decision.
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == DeclarationKind::Property
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
    }) {
        let Some((parameters, result, receiver)) = stable_property(table, stub.id) else {
            stop_with_failure!(stub.id);
        };
        if receiver.is_some()
            || !parameters.is_empty()
            || index
                .publish_signature(stub.id, parameters, result)
                .is_err()
        {
            stop_with_failure!(stub.id);
        }
        let mutable = headers
            .declarations
            .anchor(stub.id)
            .and_then(|anchor| anchor.owner)
            .and_then(|owner| classifier_types.get(&owner))
            .and_then(|classifier| {
                table
                    .anonymous_object_types
                    .iter()
                    .find_map(|(source, candidate)| {
                        (*candidate == *classifier && source.0 == stub.source.raw())
                            .then_some(source)
                    })
            })
            .and_then(|source| table.anonymous_object_captures.get(source))
            .and_then(|captures| {
                let name = stub
                    .lookup_name
                    .and_then(|name| headers.lookup_names.get(name))?;
                captures
                    .iter()
                    .find(|capture| capture.name == name)
                    .map(|capture| capture.shared_cell)
            })
            .unwrap_or(false);
        index.publish_property_shape(
            crate::fir::PropertyId::from_raw(stub.id.raw()),
            stub.id,
            0,
            0,
            None,
            mutable,
        );
    }
    // Classifier publication consumes exact own-member override facts while it closes interface
    // delegation. Publish every declaration header first so source order cannot affect that query.
    for stub in &headers.stubs {
        if stub.kind != DeclarationKind::Classifier {
            continue;
        }
        let Some(classifier) = classifier_types
            .get(&stub.id)
            .and_then(|classifier| table.class_by_type_name(*classifier))
        else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: classifier stub has no collected classifier signature",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        if stub
            .flags
            .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
            && headers.syntax.declaration(stub.id).is_none()
        {
            if index
                .publish_classifier_header(
                    stub.id,
                    classifier.internal,
                    None,
                    std::iter::empty(),
                    std::iter::empty(),
                    std::iter::empty(),
                    std::iter::empty(),
                )
                .is_err()
            {
                stop_with_failure!(stub.id);
            }
            continue;
        }
        let declaration = headers
            .syntax
            .declaration(stub.id)
            .expect("a classifier stub must retain compact syntax");
        let crate::fir::HeaderDeclarationKind::Classifier {
            context_parameters,
            delegations,
            ..
        } = declaration.kind
        else {
            unreachable!("a classifier stub must own a classifier header")
        };
        let Some((superclass, interfaces, source_supertypes)) = compact_classifier_parents(
            headers,
            &semantics,
            stub.id,
            stub.source,
            classifier,
            resolved_classifier_parents.get(&stub.id),
            &compact_cycle_edges,
        ) else {
            // An ordinary body-local classifier is not a Pass-1 semantic root. Its header can use
            // statement-local aliases and other lexical declarations that intentionally exist
            // only while the containing body is checked in Pass 2. Preserve its stable declaration
            // inventory, but do not turn the absence of an undemanded semantic header into module
            // finalization failure. A local classifier reached from an inferred non-local
            // signature has compact parents in `resolved_classifier_parents` and must still
            // finalize here.
            if stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                && !resolved_classifier_parents.contains_key(&stub.id)
            {
                index.publish_classifier_identity(stub.id, classifier.internal);
                crate::trace_compiler!(
                    "fir",
                    "signature finalization deferred ordinary local classifier {:?} ({}) to Pass 2",
                    stub.id,
                    classifier.internal,
                );
                continue;
            }
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?} ({}): compact classifier inheritance did not resolve",
                stub.id,
                classifier.internal,
            );
            stop_with_failure!(stub.id);
        };
        let interface_delegations = headers
            .syntax
            .interface_delegations(delegations)
            .iter()
            .map(|delegation| {
                let interface = *source_supertypes.get(delegation.supertype as usize)?;
                let source = match delegation.source {
                    crate::fir::HeaderInterfaceDelegateSource::ConstructorParameter(parameter) => {
                        crate::fir::ResolvedInterfaceDelegateSource::ConstructorParameter(parameter)
                    }
                    crate::fir::HeaderInterfaceDelegateSource::ConstructorBodyInitializer => {
                        crate::fir::ResolvedInterfaceDelegateSource::ConstructorBodyInitializer
                    }
                };
                Some((interface, source))
            })
            .collect::<Option<Vec<_>>>();
        let Some(interface_delegations) = interface_delegations else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: interface-delegation closure did not resolve",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        deferred_interface_delegations.push((
            stub.id,
            stub.source.raw(),
            classifier.internal,
            interface_delegations,
        ));
        let sealed_subclasses = classifier
            .is_sealed()
            .then(|| table.subclass_names_of(classifier.internal))
            .unwrap_or_default();
        let signature_scope = crate::fir::SignatureScope {
            owner: stub.id,
            source: stub.source,
        };
        let context_parameters = headers
            .syntax
            .parameters(context_parameters)
            .iter()
            .map(|parameter| {
                Some((
                    headers
                        .lookup_names
                        .get(parameter.name)
                        .filter(|name| *name != "_")
                        .map(|name| Box::<str>::from(name)),
                    semantics.resolve_compact_header_type(signature_scope, parameter.ty)?,
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(context_parameters) = context_parameters else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: classifier context parameter did not resolve",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        crate::trace_compiler!(
            "signature",
            "classifier header declaration={:?} classifier={} superclass={superclass:?} interfaces={interfaces:?}",
            stub.id,
            classifier.internal,
        );
        if index
            .publish_classifier_header(
                stub.id,
                classifier.internal,
                superclass,
                interfaces,
                std::iter::empty(),
                context_parameters,
                sealed_subclasses,
            )
            .is_err()
        {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: classifier header publication was rejected",
                stub.id,
            );
            stop_with_failure!(stub.id);
        }
    }
    for stub in &headers.stubs {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let (type_parameters, bounds, declaration_start) = match declaration.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                type_parameters,
                bounds,
                signature_start,
                ..
            } => (type_parameters, Some(bounds), signature_start),
            crate::fir::HeaderDeclarationKind::Property {
                type_parameters,
                bounds,
                ..
            }
            | crate::fir::HeaderDeclarationKind::Classifier {
                type_parameters,
                bounds,
                ..
            } => (type_parameters, Some(bounds), stub.range.lo),
            crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters, ..
            } => (type_parameters, None, stub.range.lo),
            crate::fir::HeaderDeclarationKind::Constructor { .. } => continue,
        };
        let packed = headers.syntax.type_parameters(type_parameters);
        if packed.is_empty() {
            continue;
        }
        let declared_names = packed
            .iter()
            .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_owned))
            .collect::<Option<Vec<_>>>();
        let Some(declared_names) = declared_names else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: type parameter name is not interned",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        let declared_bounds = bounds
            .map(|bounds| headers.syntax.bounds(bounds))
            .unwrap_or_default()
            .iter()
            .map(|bound| {
                Some((
                    headers.lookup_names.get(bound.parameter)?.to_owned(),
                    headers
                        .syntax
                        .transient_type_ref(bound.ty, &headers.lookup_names)?,
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(declared_bounds) = declared_bounds else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: type parameter bound is not interned",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        let symbolic =
            super::TParams::symbolic_from_decl_with(&declared_names, &declared_bounds, &|name| {
                table.class_names.get(name)
            })
            .alpha_renamed_declaration(
                &declared_names,
                table.compilation_id,
                stub.source.raw(),
                declaration_start,
            );
        for (ordinal, (source_name, parameter)) in declared_names.iter().zip(packed).enumerate() {
            let semantic = symbolic.bound(source_name);
            let semantic_name = semantic.ty_param_name().unwrap_or(source_name);
            let has_explicit_bound = declared_bounds
                .iter()
                .any(|(owner, _)| owner == source_name);
            let resolved_bounds = has_explicit_bound
                .then(|| {
                    let mut bounds = vec![semantic
                        .ty_param_bound()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))];
                    bounds.extend(symbolic.extra_bounds_of(source_name));
                    bounds
                })
                .unwrap_or_default()
                .into_iter()
                .map(|bound| {
                    let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                        table
                            .classes
                            .get(&owner)
                            .is_some_and(|classifier| classifier.is_interface())
                            || table
                                .libraries
                                .classifier(owner)
                                .is_some_and(|classifier| classifier.is_interface())
                    });
                    (bound, is_interface)
                })
                .collect::<Vec<_>>();
            let variance = if parameter.flags.is_in() {
                crate::types::TypeVariance::In
            } else if parameter.flags.is_out() {
                crate::types::TypeVariance::Out
            } else {
                crate::types::TypeVariance::Invariant
            };
            if index
                .publish_type_parameter(
                    stub.id,
                    u32::try_from(ordinal).expect("too many declaration type parameters"),
                    source_name,
                    semantic_name,
                    crate::fir::ResolvedTypeParameterFlags::new(
                        variance,
                        parameter.flags.is_non_null(),
                        parameter.flags.is_reified(),
                    ),
                    resolved_bounds,
                )
                .is_err()
            {
                stop_with_failure!(stub.id);
            }
        }
    }
    // Syntaxless frontend-plugin callables already carry their complete semantic generic shape.
    // Publish those formals on the generated stable declaration just as compact syntax publishes
    // written callable formals above.
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == DeclarationKind::Function
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
            && headers.syntax.declaration(stub.id).is_none()
    }) {
        let Some((signature, _)) = stable_function(table, stub.id) else {
            stop_with_failure!(stub.id);
        };
        let Some(generic) = signature.generic_sig.as_ref() else {
            continue;
        };
        for (ordinal, semantic_name) in generic.formals.iter().enumerate() {
            let bounds = generic
                .formal_bounds
                .get(ordinal)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|bound| {
                    let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                        table
                            .classes
                            .get(&owner)
                            .is_some_and(|classifier| classifier.is_interface())
                            || table
                                .libraries
                                .classifier(owner)
                                .is_some_and(|classifier| classifier.is_interface())
                    });
                    (bound, is_interface)
                });
            if index
                .publish_type_parameter(
                    stub.id,
                    u32::try_from(ordinal).expect("too many generated callable type parameters"),
                    crate::types::type_parameter_source_name(semantic_name),
                    semantic_name,
                    crate::fir::ResolvedTypeParameterFlags::new(
                        crate::types::TypeVariance::Invariant,
                        false,
                        false,
                    ),
                    bounds,
                )
                .is_err()
            {
                stop_with_failure!(stub.id);
            }
        }
    }
    // A typealias has no executable body, but its resolved expansion is part of the stable
    // declaration header. Publish it before compact syntax and the legacy semantic table are
    // released so common lowering and dependency metadata never have to reopen source text.
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == DeclarationKind::TypeAlias)
    {
        let Some(name) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            stop_with_failure!(stub.id);
        };
        let anchor = headers
            .declarations
            .anchor(stub.id)
            .expect("a compact type-alias stub must retain its stable owner");
        let owner = match anchor.owner {
            Some(owner) => {
                let Some(owner_header) = index.declaration_header(owner) else {
                    stop_with_failure!(stub.id);
                };
                // A local classifier's alias is lexical-only and cannot be published to another
                // module. Its Pass-2 uses resolve through the local scope and need no class metadata.
                if owner_header
                    .flags
                    .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                {
                    continue;
                }
                let Some(classifier) = index.classifier_header(owner) else {
                    stop_with_failure!(stub.id);
                };
                let mut nested = Vec::new();
                let mut current = classifier.classifier;
                while let Some(parent) = current.nested_owner() {
                    nested.push(current.nested_segment_ref());
                    current = parent;
                }
                nested.push(current.segment_ref());
                nested.reverse();
                let mut semantic_owner = classifier.classifier.namespace();
                for segment in nested {
                    semantic_owner = crate::types::type_name_child(semantic_owner, segment);
                }
                semantic_owner
            }
            None => {
                let Some(file_scope) = headers.scopes.file(stub.source) else {
                    stop_with_failure!(stub.id);
                };
                let mut package = crate::types::TypeName::ROOT;
                for segment in headers.scopes.path(file_scope.package) {
                    let Some(segment) = headers.lookup_names.get(*segment) else {
                        stop_with_failure!(stub.id);
                    };
                    package = crate::types::type_name_child(package, segment);
                }
                package
            }
        };
        let identity = crate::types::type_name_child(owner, name);
        let Some((_, expansion)) = table.source_alias_expansions.get(&identity) else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: resolved type-alias expansion is missing",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        let expansion_spelling = table
            .alias_expansion_spellings
            .get(&identity)
            .map(|(spelling, _, _)| spelling.clone())
            .unwrap_or_default();
        if index
            .publish_type_alias_header(stub.id, identity, *expansion, expansion_spelling)
            .is_err()
        {
            stop_with_failure!(stub.id);
        }
    }
    // An applied nested/inner classifier carries both its own type arguments and any lexically
    // captured declaration arguments. Publish their stable semantic identities once so checked FIR
    // can attach substitutions without reconstructing ownership from source spelling or AST IDs.
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == DeclarationKind::Classifier)
    {
        // An undemanded body-local classifier whose lexical header is deferred above receives its
        // applied type-parameter layout together with its checked header in Pass 2. The index does
        // not admit a layout without the classifier identity it describes.
        if index.classifier_header(stub.id).is_none() {
            continue;
        }
        let Some(classifier) = classifier_types
            .get(&stub.id)
            .and_then(|classifier| table.class_by_type_name(*classifier))
        else {
            stop_with_failure!(stub.id);
        };
        let own = (0..classifier.type_parameters.type_params().len())
            .map(|ordinal| {
                index.type_parameter(
                    stub.id,
                    u32::try_from(ordinal).expect("too many classifier type parameters"),
                )
            })
            .collect::<Option<Vec<_>>>();
        let own_count = classifier.type_parameters.type_params().len();
        let mut captured = Vec::new();
        for (captured_ordinal, (semantic_name, bound)) in classifier
            .captured_type_parameters
            .type_params()
            .iter()
            .zip(
                classifier
                    .captured_type_parameters
                    .type_param_bounds()
                    .iter()
                    .copied(),
            )
            .enumerate()
        {
            if let Some(parameter) = index.type_parameter_by_semantic_name(semantic_name) {
                captured.push(parameter);
                continue;
            }
            // A classifier nested in a generic local function captures a semantic formal whose
            // declaring local function has no module-level callable header. Materialize that formal
            // as a classifier-owned captured slot now; waiting for checked body publication would
            // leave the supposedly finalized module index incomplete at the Pass-2 boundary.
            let ordinal = own_count + captured_ordinal;
            let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                table
                    .classes
                    .get(&owner)
                    .is_some_and(|classifier| classifier.is_interface())
                    || table
                        .libraries
                        .classifier(owner)
                        .is_some_and(|classifier| classifier.is_interface())
            });
            if index
                .publish_type_parameter(
                    stub.id,
                    u32::try_from(ordinal).expect("too many captured classifier type parameters"),
                    crate::types::type_parameter_source_name(semantic_name),
                    semantic_name,
                    crate::fir::ResolvedTypeParameterFlags::new(
                        crate::types::TypeVariance::Invariant,
                        false,
                        false,
                    ),
                    [(bound, is_interface)],
                )
                .is_err()
            {
                stop_with_failure!(stub.id);
            }
            let Some(parameter) = index.type_parameter(stub.id, ordinal as u32) else {
                stop_with_failure!(stub.id);
            };
            captured.push(parameter);
        }
        let Some(mut parameters) = own else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: classifier type-argument identity is missing",
                stub.id,
            );
            stop_with_failure!(stub.id);
        };
        parameters.extend(captured);
        index.publish_classifier_type_arguments(
            stub.id,
            u32::try_from(own_count).expect("too many own classifier type parameters"),
            parameters,
        );
    }
    // An invalid inferred result must not erase the callable declaration during Pass-2 diagnostic
    // recovery. Retain its coordinate-free arity/default/receiver header without inventing a
    // semantic result type; ModuleSymbols supplies a transient Error result only while checking the
    // already-invalid module, and valid FIR/lowering still require `index.signature(declaration)`.
    let failed_declarations = failed
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == DeclarationKind::Function && failed_declarations.contains(&stub.id)
    }) {
        if index.signature(stub.id).is_some() {
            continue;
        }
        let Some(name) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            continue;
        };
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let crate::fir::HeaderDeclarationKind::Callable {
            parameters,
            context_count,
            ..
        } = declaration.kind
        else {
            continue;
        };
        let packed_parameters = headers.syntax.parameters(parameters);
        let context_value_count = packed_parameters
            .iter()
            .take(context_count as usize)
            .filter(|parameter| {
                headers
                    .lookup_names
                    .get(parameter.name)
                    .is_some_and(|name| name != "_")
            })
            .count() as u32;
        let callable = crate::fir::CallableId::from_raw(stub.id.raw());
        index.publish_failed_function_shape(
            callable,
            stub.id,
            name,
            crate::fir::ResolvedCallableShape {
                context_parameter_count: context_count,
                context_value_count,
                extension_receiver: resolved_receivers.get(&stub.id).copied(),
            },
        );
        index.publish_callable_parameters(
            callable,
            packed_parameters.iter().map(|parameter| {
                let name = headers
                    .lookup_names
                    .get(parameter.name)
                    .expect("a compact failed callable parameter must retain its spelling");
                (
                    name,
                    crate::fir::ResolvedValueParameterFlags::new(
                        parameter.flags.is_vararg(),
                        parameter.flags.has_default(),
                        parameter.flags.is_property(),
                        parameter.flags.is_mutable_property(),
                    ),
                )
            }),
        );
    }
    for stub in headers.stubs.iter().filter(|stub| {
        matches!(
            stub.kind,
            DeclarationKind::Function | DeclarationKind::Constructor
        )
    }) {
        if index.signature(stub.id).is_none() {
            continue;
        }
        let callable = crate::fir::CallableId::from_raw(stub.id.raw());
        match stub.kind {
            DeclarationKind::Function => {
                let name = stub
                    .lookup_name
                    .and_then(|name| headers.lookup_names.get(name))
                    .expect("a function stub must retain its Pass-1 emission spelling");
                if stub
                    .flags
                    .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
                {
                    let Some((signature, receiver)) = generated_function(table, stub) else {
                        stop_with_failure!(stub.id);
                    };
                    let extension_receiver =
                        match receiver.map(crate::fir::ResolvedTy::new).transpose() {
                            Ok(receiver) => receiver,
                            Err(_) => stop_with_failure!(stub.id),
                        };
                    index.publish_function_shape(
                        callable,
                        stub.id,
                        name,
                        crate::fir::ResolvedCallableShape {
                            context_parameter_count: signature.context_count as u32,
                            context_value_count: signature
                                .param_names
                                .iter()
                                .take(signature.context_count)
                                .filter(|name| name.as_str() != "_")
                                .count() as u32,
                            extension_receiver,
                        },
                        false,
                    );
                    index.publish_callable_parameters(
                        callable,
                        signature
                            .param_names
                            .iter()
                            .enumerate()
                            .map(|(ordinal, name)| {
                                (
                                    name.as_str(),
                                    crate::fir::ResolvedValueParameterFlags::new(
                                        signature.vararg_index == Some(ordinal),
                                        signature
                                            .param_defaults
                                            .get(ordinal)
                                            .copied()
                                            .unwrap_or(false),
                                        false,
                                        false,
                                    )
                                    .with_implicit_integer_coercion(
                                        signature
                                            .implicit_integer_coercion
                                            .get(ordinal)
                                            .copied()
                                            .unwrap_or(false),
                                    )
                                    .with_exact(
                                        signature
                                            .exact_params
                                            .get(ordinal)
                                            .copied()
                                            .unwrap_or(false),
                                    )
                                    .with_no_infer(
                                        signature
                                            .no_infer_params
                                            .get(ordinal)
                                            .copied()
                                            .unwrap_or(false),
                                    ),
                                )
                            }),
                    );
                    index.publish_callable_behavior(
                        callable,
                        crate::fir::ResolvedCallableBehavior {
                            requires_splice: signature.requires_splice(),
                            projected_return_hazard: signature.projected_return_hazard,
                            plugin_expression: signature.plugin_expression,
                        },
                    );
                    if let Some(bound) = signature.equality_bound {
                        if index
                            .publish_callable_equality_bound(callable, bound)
                            .is_err()
                        {
                            stop_with_failure!(stub.id);
                        }
                    }
                    continue;
                }
                let declaration = headers
                    .syntax
                    .declaration(stub.id)
                    .expect("a function stub must retain its compact header");
                let crate::fir::HeaderDeclarationKind::Callable {
                    receiver: _,
                    parameters,
                    type_parameters,
                    context_count,
                    ..
                } = declaration.kind
                else {
                    unreachable!("a function stub must own a callable header")
                };
                let context_value_count = headers
                    .syntax
                    .parameters(parameters)
                    .iter()
                    .take(context_count as usize)
                    .filter(|parameter| {
                        headers
                            .lookup_names
                            .get(parameter.name)
                            .is_some_and(|name| name != "_")
                    })
                    .count() as u32;
                index.publish_function_shape(
                    callable,
                    stub.id,
                    name,
                    crate::fir::ResolvedCallableShape {
                        context_parameter_count: context_count,
                        context_value_count,
                        extension_receiver: resolved_receivers.get(&stub.id).copied(),
                    },
                    stub.flags.has(crate::fir::DeclarationFlags::INLINE),
                );
                let stable_signature =
                    stable_function(table, stub.id).map(|(signature, _)| signature);
                let parameters = headers
                    .syntax
                    .parameters(parameters)
                    .iter()
                    .enumerate()
                    .map(|(ordinal, parameter)| {
                        let name = headers
                            .lookup_names
                            .get(parameter.name)
                            .expect("a compact callable parameter must retain its spelling");
                        (
                            name,
                            crate::fir::ResolvedValueParameterFlags::new(
                                parameter.flags.is_vararg(),
                                parameter.flags.has_default(),
                                parameter.flags.is_property(),
                                parameter.flags.is_mutable_property(),
                            )
                            .with_implicit_integer_coercion(
                                stable_signature
                                    .and_then(|signature| {
                                        signature.implicit_integer_coercion.get(ordinal)
                                    })
                                    .copied()
                                    .unwrap_or(false),
                            )
                            .with_exact(
                                stable_signature
                                    .and_then(|signature| signature.exact_params.get(ordinal))
                                    .copied()
                                    .unwrap_or(false),
                            )
                            .with_no_infer(
                                stable_signature
                                    .and_then(|signature| signature.no_infer_params.get(ordinal))
                                    .copied()
                                    .unwrap_or(false),
                            ),
                        )
                    })
                    .collect::<Vec<_>>();
                index.publish_callable_parameters(callable, parameters);
                index.publish_callable_behavior(
                    callable,
                    crate::fir::ResolvedCallableBehavior {
                        requires_splice: stable_signature.map_or_else(
                            || {
                                headers
                                    .syntax
                                    .type_parameters(type_parameters)
                                    .iter()
                                    .any(|parameter| parameter.flags.is_reified())
                            },
                            Signature::requires_splice,
                        ),
                        projected_return_hazard: stable_signature
                            .is_some_and(|signature| signature.projected_return_hazard),
                        plugin_expression: stable_signature
                            .and_then(|signature| signature.plugin_expression),
                    },
                );
                let equality_bound =
                    stable_signature.and_then(|signature| signature.equality_bound);
                if let Some(bound) = equality_bound {
                    if index
                        .publish_callable_equality_bound(callable, bound)
                        .is_err()
                    {
                        stop_with_failure!(stub.id);
                    }
                }
            }
            DeclarationKind::Constructor => {
                if stub
                    .flags
                    .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
                    && headers.syntax.declaration(stub.id).is_none()
                {
                    index.publish_constructor_shape(
                        callable,
                        stub.id,
                        crate::fir::ResolvedCallableShape {
                            context_parameter_count: 0,
                            context_value_count: 0,
                            extension_receiver: None,
                        },
                    );
                    index.publish_callable_parameters(
                        callable,
                        std::iter::empty::<(&str, crate::fir::ResolvedValueParameterFlags)>(),
                    );
                    continue;
                }
                let declaration = headers
                    .syntax
                    .declaration(stub.id)
                    .expect("a constructor stub must retain its compact header");
                let crate::fir::HeaderDeclarationKind::Constructor {
                    context_parameters,
                    parameters,
                } = declaration.kind
                else {
                    unreachable!("a constructor stub must own a constructor header")
                };
                let packed_context_parameters = headers.syntax.parameters(context_parameters);
                let context_parameter_count = u32::try_from(packed_context_parameters.len())
                    .expect("too many constructor context parameters");
                let context_value_count = u32::try_from(
                    packed_context_parameters
                        .iter()
                        .filter(|parameter| {
                            headers
                                .lookup_names
                                .get(parameter.name)
                                .is_some_and(|name| name != "_")
                        })
                        .count(),
                )
                .expect("too many named constructor context parameters");
                index.publish_constructor_shape(
                    callable,
                    stub.id,
                    crate::fir::ResolvedCallableShape {
                        context_parameter_count,
                        context_value_count,
                        extension_receiver: None,
                    },
                );
                let mut published_parameters = packed_context_parameters
                    .iter()
                    .map(|parameter| {
                        let name = headers
                            .lookup_names
                            .get(parameter.name)
                            .expect("a compact context parameter must retain its spelling");
                        (
                            name,
                            crate::fir::ResolvedValueParameterFlags::new(
                                false, false, false, false,
                            ),
                        )
                    })
                    .collect::<Vec<_>>();
                published_parameters.extend(
                    headers
                        .syntax
                        .parameters(parameters)
                        .iter()
                        .enumerate()
                        .map(|(ordinal, parameter)| {
                            let name = headers
                                .lookup_names
                                .get(parameter.name)
                                .expect("a compact constructor parameter must retain its spelling");
                            (
                                name,
                                crate::fir::ResolvedValueParameterFlags::new(
                                    parameter.flags.is_vararg(),
                                    parameter.flags.has_default(),
                                    parameter.flags.is_property(),
                                    parameter.flags.is_mutable_property(),
                                )
                                .with_implicit_integer_coercion(
                                    stable_constructor_implicit_integer_coercion(
                                        table, stub.id, ordinal,
                                    ),
                                ),
                            )
                        }),
                );
                let anchor = headers
                    .declarations
                    .anchor(stub.id)
                    .expect("a constructor stub must retain its stable anchor");
                if anchor.sibling == 0 {
                    let anonymous_captures = anchor
                        .owner
                        .and_then(|owner| classifier_types.get(&owner))
                        .and_then(|owner| {
                            table
                                .anonymous_object_types
                                .iter()
                                .find_map(|(source, ty)| {
                                    (*ty == *owner && source.0 == stub.source.raw())
                                        .then_some(source)
                                })
                        })
                        .and_then(|source| table.anonymous_object_captures.get(source));
                    published_parameters.extend(anonymous_captures.into_iter().flatten().map(
                        |capture| {
                            (
                                capture.name.as_str(),
                                crate::fir::ResolvedValueParameterFlags::new(
                                    false, false, true, false,
                                ),
                            )
                        },
                    ));
                }
                index.publish_callable_parameters(callable, published_parameters);
            }
            DeclarationKind::Property
            | DeclarationKind::Classifier
            | DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Accessor
            | DeclarationKind::Initializer
            | DeclarationKind::Script => unreachable!("callable publication filter"),
        }
    }
    // A body-local classifier may expose a callable to an earlier-streamed ordinary body through
    // an already-finalized non-local signature (`Nested().foo().toString()`, where `foo` returns an
    // anonymous classifier). Publish local FUNCTION headers that are independently pending-free;
    // their bodies and classifier storage still remain Pass-2 work. Headers whose types depend on
    // the lexical body retain Error/Pending and are deliberately left for bounded local publication.
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == DeclarationKind::Function
            && stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
    }) {
        if index.signature(stub.id).is_some() {
            continue;
        }
        let Some((signature, receiver)) = stable_function(table, stub.id) else {
            continue;
        };
        let parameters = semantic_parameters(signature);
        let result = semantic_result(signature);
        if parameters
            .iter()
            .chain(std::iter::once(&result))
            .chain(receiver.iter())
            .any(|ty| ty.mentions_pending() || ty.mentions_error())
        {
            continue;
        }
        if index
            .publish_signature(stub.id, parameters, result)
            .is_err()
        {
            continue;
        }
        let Some(name) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            continue;
        };
        let callable = crate::fir::CallableId::from_raw(stub.id.raw());
        let extension_receiver = match receiver.map(crate::fir::ResolvedTy::new).transpose() {
            Ok(receiver) => receiver,
            Err(_) => stop_with_failure!(stub.id),
        };
        index.publish_function_shape(
            callable,
            stub.id,
            name,
            crate::fir::ResolvedCallableShape {
                context_parameter_count: signature.context_count as u32,
                context_value_count: signature
                    .param_names
                    .iter()
                    .take(signature.context_count)
                    .filter(|name| name.as_str() != "_")
                    .count() as u32,
                extension_receiver,
            },
            stub.flags.has(crate::fir::DeclarationFlags::INLINE),
        );
        index.publish_callable_parameters(
            callable,
            signature
                .param_names
                .iter()
                .enumerate()
                .map(|(ordinal, name)| {
                    (
                        name.as_str(),
                        crate::fir::ResolvedValueParameterFlags::new(
                            signature.vararg_index == Some(ordinal),
                            signature
                                .param_defaults
                                .get(ordinal)
                                .copied()
                                .unwrap_or(false),
                            false,
                            false,
                        )
                        .with_implicit_integer_coercion(
                            signature
                                .implicit_integer_coercion
                                .get(ordinal)
                                .copied()
                                .unwrap_or(false),
                        )
                        .with_exact(
                            signature
                                .exact_params
                                .get(ordinal)
                                .copied()
                                .unwrap_or(false),
                        )
                        .with_no_infer(
                            signature
                                .no_infer_params
                                .get(ordinal)
                                .copied()
                                .unwrap_or(false),
                        ),
                    )
                }),
        );
        index.publish_callable_behavior(
            callable,
            crate::fir::ResolvedCallableBehavior {
                requires_splice: signature.requires_splice(),
                projected_return_hazard: signature.projected_return_hazard,
                plugin_expression: signature.plugin_expression,
            },
        );
    }
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == DeclarationKind::Property
            && !stub
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
    }) {
        if index.signature(stub.id).is_none() {
            continue;
        }
        let declaration = headers
            .syntax
            .declaration(stub.id)
            .expect("a property stub must retain its compact header");
        let crate::fir::HeaderDeclarationKind::Property {
            receiver: _,
            context_parameters,
            mutable,
            ..
        } = declaration.kind
        else {
            unreachable!("a property stub must own a property header")
        };
        let property = crate::fir::PropertyId::from_raw(stub.id.raw());
        index.publish_property_shape(
            property,
            stub.id,
            u32::try_from(headers.syntax.parameters(context_parameters).len())
                .expect("too many property context parameters"),
            u32::try_from(
                headers
                    .syntax
                    .parameters(context_parameters)
                    .iter()
                    .filter(|parameter| {
                        headers
                            .lookup_names
                            .get(parameter.name)
                            .is_some_and(|name| name != "_")
                    })
                    .count(),
            )
            .expect("too many named property context parameters"),
            resolved_receivers.get(&stub.id).copied(),
            mutable,
        );
        if let Some(storage) = backing_field_types.get(&stub.id).copied() {
            index.publish_property_storage_type(property, storage);
        }
    }
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == DeclarationKind::Accessor)
    {
        let anchor = headers
            .declarations
            .anchor(stub.id)
            .expect("an accessor stub must retain its stable anchor");
        let property_declaration = anchor
            .owner
            .expect("an accessor must retain its property owner");
        let property = index
            .property_for_declaration(property_declaration)
            .and_then(|property| index.property(property));
        let Some(property) = property else {
            // An inferred ordinary-local property is intentionally deferred to its Pass-2 lexical
            // unit. Its accessor identity is published alongside that checked property below.
            continue;
        };
        let property_signature = index
            .signature(property_declaration)
            .cloned()
            .expect("an accessor owner must have a resolved property signature");
        let property_name = index
            .declaration_name(property_declaration)
            .expect("an accessor owner must retain its emission name")
            .to_owned();
        let is_setter = anchor.sibling == 1;
        let mut parameters = property_signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if is_setter {
            parameters.push(property_signature.result.get());
        }
        if index
            .publish_signature(
                stub.id,
                parameters,
                if is_setter {
                    Ty::Unit
                } else {
                    property_signature.result.get()
                },
            )
            .is_err()
        {
            stop_with_failure!(stub.id);
        }
        let callable = crate::fir::CallableId::from_raw(stub.id.raw());
        let callable_name = if is_setter {
            super::property_setter_name(&property_name)
        } else {
            super::property_getter_name(&property_name)
        };
        index.publish_function_shape(
            callable,
            stub.id,
            &callable_name,
            crate::fir::ResolvedCallableShape {
                context_parameter_count: property.context_parameter_count,
                context_value_count: property.context_value_count,
                extension_receiver: property.extension_receiver,
            },
            stub.flags.has(crate::fir::DeclarationFlags::INLINE),
        );
        let declaration = headers
            .syntax
            .declaration(property_declaration)
            .expect("an accessor owner must retain compact property syntax");
        let crate::fir::HeaderDeclarationKind::Property {
            context_parameters, ..
        } = declaration.kind
        else {
            unreachable!("an accessor owner must own a property header")
        };
        let mut parameter_names = headers
            .syntax
            .parameters(context_parameters)
            .iter()
            .map(|parameter| {
                (
                    headers
                        .lookup_names
                        .get(parameter.name)
                        .expect("an accessor context parameter must retain its spelling"),
                    crate::fir::ResolvedValueParameterFlags::new(false, false, false, false),
                )
            })
            .collect::<Vec<_>>();
        if is_setter {
            parameter_names.push((
                "value",
                crate::fir::ResolvedValueParameterFlags::new(false, false, false, false),
            ));
        }
        index.publish_callable_parameters(callable, parameter_names);
    }
    // The forwarding surface is a classifier-header fact, but its selected module targets are the
    // stable callable/property identities published above. Close it only now; lowering will consume
    // the finished plan without reopening this symbol source.
    for (declaration, source_file, classifier, delegations) in deferred_interface_delegations {
        let delegations = delegations
            .into_iter()
            .map(|(interface, delegate_source)| {
                super::interface_delegation::resolve_interface_delegation(
                    table,
                    &index,
                    source_file,
                    classifier,
                    interface,
                    delegate_source,
                )
            })
            .collect::<Option<Vec<_>>>();
        let Some(delegations) = delegations else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {declaration:?}: interface-delegation closure did not resolve",
            );
            stop_with_failure!(declaration);
        };
        index.publish_interface_delegations(declaration, delegations);
    }
    let resolved_contracts = match semantics.resolve_source_contracts(&source_contracts) {
        Ok(contracts) => contracts,
        Err(mut declarations) => {
            failed.append(&mut declarations);
            failed.sort_by_key(|declaration| declaration.raw());
            failed.dedup();
            return StreamedSignatureIndex {
                index,
                failures: failed,
            };
        }
    };
    for (declaration, contract) in resolved_contracts {
        index.publish_contract(declaration, contract);
    }
    for (&classifier, &retention) in &table.annotation_retentions {
        if index.classifier_declaration(classifier).is_some() {
            index.publish_annotation_policy(
                classifier,
                retention,
                table.annotation_targets.get(&classifier).copied(),
            );
        }
    }
    StreamedSignatureIndex {
        index,
        failures: failed,
    }
}
