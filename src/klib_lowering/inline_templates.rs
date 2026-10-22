//! Inline dependency functions as call-site templates.
//!
//! A call of an inline dependency function is expanded where it is made, as a call of a
//! same-module inline function is: common lowering copies the function's checked body into the
//! caller, splicing each lambda argument into its invocations. A KLIB serializes that body, so it
//! is lowered here into the calling file's common IR as an inline-only template function: the
//! same IR checked FIR lowering produces for an equivalent source inline function, with the
//! parameter facts it publishes (identities and each parameter's inline expansion mode). The
//! selected provider record describes the function, including the semantic type parameters its
//! types name; the serialized header must agree with it, and each serialized type parameter is
//! that record's. A template is never a target function itself: a call it makes of an ordinary
//! dependency function is the same external call a source call of that function is, through the
//! identity the calling compilation's provider gives the exact serialized callee, a member's with
//! its dispatch receiver, which the target dispatches as it dispatches that source call. A call of
//! another inline function declines, as does every form body lowering declines. A declined
//! template leaves the file as it was.

use super::body_lowering::{
    BodyLowering, CallLinker, LinkedAccessor, LinkedCallee, LinkedConstructor, LoweredFunction,
};
use super::decline::{KlibBodyDecline, KlibBodyDeclineReason};
use super::function_lowering::function_header;
use super::function_lowering::{declare_function, prepared_inline_header};
use super::ir_builtins::IrBuiltinOperators;
use super::klib_types::{member_owner, semantic_type, KlibTypeParameters};
use super::lowering_checkpoint::LoweringCheckpoint;
use crate::fir::ExternalCallableId;
use crate::ir::{Callee, FunId, IrFile};
use crate::klib_libraries::{
    KlibDeclarationBodies, KlibReferenceBinding, KlibReferenceBindings, KlibReferenceDispatch,
    KlibReferenceOrigin,
};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, KlibBodyCallable, KlibDeclarationSignature,
    KlibDependencyCallable,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::KlibIrBody;
use crate::types::SemanticCallableOwner;
use crate::types::{InlineExpansionMode, InlineParameterModifier, Ty};

/// An inline dependency function's template in a file.
#[derive(Clone, Debug)]
pub struct KlibInlineTemplate {
    /// The inline-only function holding the template.
    pub function: FunId,
    /// The semantic identities of the function's type parameters, in declaration order.
    pub type_parameters: Vec<String>,
}

/// The functions a template's body may call.
#[derive(Clone, Copy)]
pub struct TemplateCallees<'a> {
    /// The ordinary serialized declarations of the loaded KLIBs, which resolve each call's
    /// producer and describe its callee.
    pub declarations: &'a KlibDeclarationBodies,
    /// Already normalized reference identities. Template lowering performs no hierarchy lookup.
    pub references: &'a KlibReferenceBindings,
    /// The calling compilation's symbols: the provider identity of an exact serialized
    /// declaration, its property and classifier records, and the class hierarchy that decides the
    /// subtyping a body leaves implicit.
    pub source: &'a dyn crate::symbol_source::SymbolSource,
}

/// Lower the prepared-inline body of the dependency function `selected`, from `prepared`, into
/// `ir` as an inline-only template function. `None` when `selected` is not a KLIB declaration.
pub fn lower_inline_template(
    ir: &mut IrFile,
    selected: &ExternalCallableRealization,
    prepared: &KlibDeclarationBodies,
    callees: TemplateCallees<'_>,
) -> Option<Result<KlibInlineTemplate, KlibBodyDecline>> {
    let callable = selected.klib_body_callable()?;
    let (type_parameters, formals) = match selected.callable.generic_sig.as_deref() {
        None => (Vec::new(), Vec::new()),
        Some(generic) => (
            generic.formals.clone(),
            generic
                .formals
                .iter()
                .enumerate()
                .map(|(index, name)| Ty::ty_param(name, generic.primary_formal_bound(index)))
                .collect(),
        ),
    };
    let declaration = Declaration {
        callable,
        formals: &formals,
        inline_modifiers: &selected.callable.inline_modifiers,
        suspend: selected.callable.suspend,
    };
    let checkpoint = LoweringCheckpoint::of(ir);
    Some(
        lower(ir, declaration, prepared, callees)
            .map(|function| KlibInlineTemplate {
                function,
                type_parameters,
            })
            .map_err(|reason| {
                checkpoint.restore(ir);
                KlibBodyDecline::new(callable.signature().clone(), reason)
            }),
    )
}

/// The provider record of the function a template is lowered for.
#[derive(Clone, Copy)]
struct Declaration<'a> {
    callable: KlibBodyCallable<'a>,
    /// The semantic type parameters the record's types name, in declaration order.
    formals: &'a [Ty],
    /// Each value parameter's `noinline`/`crossinline` modifier, context parameters included and
    /// the extension receiver excluded, in declaration order.
    inline_modifiers: &'a [InlineParameterModifier],
    /// Whether the function suspends. Its template would have to suspend its caller.
    suspend: bool,
}

fn lower(
    ir: &mut IrFile,
    declaration: Declaration<'_>,
    bodies: &KlibDeclarationBodies,
    callees: TemplateCallees<'_>,
) -> Result<FunId, KlibBodyDeclineReason> {
    let callable = declaration.callable;
    let mut calls = TemplateCalls {
        module: &callable.identity().module,
        callees,
    };
    if declaration.suspend {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a suspend function",
        ));
    }
    let (arena, function) = bodies
        .function(callable.identity())
        .ok_or(KlibBodyDeclineReason::Unjoined)?;
    let statements = match &function.body {
        None => return Err(KlibBodyDeclineReason::NoBody),
        Some(KlibIrBody::Synthetic(_)) => return Err(KlibBodyDeclineReason::SyntheticBody),
        Some(KlibIrBody::Block(statements)) => statements,
    };
    let header = prepared_inline_header(callable, declaration.formals, arena, function)?;
    let modes = expansion_modes(
        header.params(),
        header.extension_receiver_index(),
        declaration.inline_modifiers,
    )?;
    let template = declare_function(ir, &header);
    if let Some(classifier) = header.receiver_slot() {
        let declared = &mut ir.functions[template as usize];
        declared.is_static = false;
        declared.dispatch_receiver = Some(classifier);
    }
    let info = ir
        .fn_params
        .get_mut(&template)
        .expect("a declared function publishes its parameters");
    info.inline_modifiers = modifiers_with_receiver(
        declaration.inline_modifiers,
        header.extension_receiver_index(),
    );
    info.expansion_modes = modes;
    let builtins = IrBuiltinOperators::new();
    let body = BodyLowering::new(
        arena,
        LoweredFunction {
            symbol: &function.base.symbol,
            function: template,
        },
        &header,
        &builtins,
        ir,
        &mut calls,
    )
    .body(statements)?;
    ir.functions[template as usize].body = Some(body);
    let mut defaults = Vec::with_capacity(header.params().len());
    for (default, &declared) in header.defaults().iter().zip(header.params()) {
        defaults.push(match *default {
            Some(default) => Some(
                BodyLowering::new(
                    arena,
                    LoweredFunction {
                        symbol: &function.base.symbol,
                        function: template,
                    },
                    &header,
                    &builtins,
                    ir,
                    &mut calls,
                )
                .default_value(default, declared)?,
            ),
            None => None,
        });
    }
    if defaults.iter().any(Option::is_some) {
        ir.fn_params
            .get_mut(&template)
            .expect("a declared function publishes its parameters")
            .defaults = Some(defaults);
    }
    ir.inline_fns.insert(template);
    ir.inline_only_fns.insert(template);
    Ok(template)
}

/// How a call expands each parameter, as checked FIR publishes it for a source inline function:
/// the extension receiver is a receiver, and a value or context parameter splices when it is a
/// non-null function type that did not write `noinline`.
fn expansion_modes(
    params: &[Ty],
    receiver: Option<usize>,
    modifiers: &[InlineParameterModifier],
) -> Result<Vec<InlineExpansionMode>, KlibBodyDeclineReason> {
    if modifiers.len() + usize::from(receiver.is_some()) != params.len() {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "{} parameters serialized, {} inline modifiers selected",
            params.len(),
            modifiers.len()
        )));
    }
    let modifiers = modifiers_with_receiver(modifiers, receiver);
    Ok(params
        .iter()
        .zip(modifiers)
        .enumerate()
        .map(|(index, (ty, modifier))| {
            InlineExpansionMode::declared(receiver == Some(index), ty.is_fun(), modifier)
        })
        .collect())
}

/// The modifiers aligned with every parameter: the extension receiver writes none.
fn modifiers_with_receiver(
    modifiers: &[InlineParameterModifier],
    receiver: Option<usize>,
) -> Vec<InlineParameterModifier> {
    let mut aligned = modifiers.to_vec();
    if let Some(receiver) = receiver {
        aligned.insert(receiver, InlineParameterModifier::None);
    }
    aligned
}

/// A template's calls. The template is copied into its callers rather than emitted, so each call
/// is linked as its caller would link it: an external call of the provider's identity for the
/// exact serialized callee.
struct TemplateCalls<'a> {
    /// The template's declaring module, whose producer graph resolves its calls.
    module: &'a str,
    callees: TemplateCallees<'a>,
}

impl TemplateCalls<'_> {
    /// The declaration a call of `callee` in the template's module names.
    fn identity(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
        self.origin(&KlibDeclarationSignature::Public(callee.clone()), callee)
    }

    /// The declaration `signature` names in the template's module; `callee` is the public
    /// signature a decline names it by.
    fn origin(
        &self,
        signature: &KlibDeclarationSignature,
        callee: &KlibPublicIdSignature,
    ) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
        self.binding(signature, callee)
            .map(|binding| binding.declaration)
    }

    fn binding(
        &self,
        signature: &KlibDeclarationSignature,
        callee: &KlibPublicIdSignature,
    ) -> Result<KlibReferenceBinding, KlibBodyDeclineReason> {
        match self.callees.references.get(self.module, signature) {
            KlibReferenceOrigin::Exact(binding) => Ok(binding),
            KlibReferenceOrigin::Ambiguous => Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(
                Box::new(callee.clone()),
            )),
            KlibReferenceOrigin::Missing => Err(KlibBodyDeclineReason::UndeclaredCallee(Box::new(
                callee.clone(),
            ))),
        }
    }
}

impl CallLinker for TemplateCalls<'_> {
    fn singleton(&self, classifier: crate::types::TypeName) -> Result<(), KlibBodyDeclineReason> {
        match self.callees.source.classifier(classifier) {
            Some(record) if record.is_object() => Ok(()),
            _ => Err(KlibBodyDeclineReason::SignatureMismatch(format!(
                "the serialized object {classifier:?} is selected as no object"
            ))),
        }
    }

    fn applied_supertype(&self, sub: Ty, owner: crate::types::TypeName) -> Option<Ty> {
        crate::assignable::applied_supertype(
            &crate::symbol_resolver::SourceOracle(self.callees.source),
            sub,
            Ty::obj_name(owner),
        )
    }

    fn is_subtype(&self, sub: Ty, sup: Ty) -> bool {
        crate::assignable::is_subtype(
            &crate::assignable::TyCtx::new(),
            &crate::symbol_resolver::SourceOracle(self.callees.source),
            sub,
            sup,
        )
    }

    fn link_constructor(
        &mut self,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedConstructor, KlibBodyDeclineReason> {
        let identity = self.identity(callee)?;
        let declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                KlibDeclarationSignature::Public(callee.clone()),
                reason,
            )))
        };
        let Some((target, realization)) = self
            .callees
            .source
            .external_callable_by_dependency(&identity)
        else {
            return Err(declined(KlibBodyDeclineReason::SignatureMismatch(
                "the provider realizes no declaration of the exact serialized callee".to_owned(),
            )));
        };
        let selected = realization
            .klib_body_callable()
            .filter(|selected| selected.identity() == &identity)
            .ok_or_else(|| {
                declined(KlibBodyDeclineReason::SignatureMismatch(
                    "the constructor provider rebound a different declaration".to_owned(),
                ))
            })?;
        let declaration =
            super::constructor_calls::authenticate(selected, self.callees.declarations)
                .map_err(declined)?;
        Ok(LinkedConstructor {
            target,
            declaration,
        })
    }

    fn constructor_bound(&self, parameters: &KlibTypeParameters, argument: Ty, bound: Ty) -> bool {
        let oracle = crate::symbol_resolver::SourceOracle(self.callees.source);
        super::constructor_bounds::satisfies(Some(&oracle), parameters, argument, bound)
    }

    fn link_accessor(
        &mut self,
        callee: &KlibAccessorIdSignature,
    ) -> Result<LinkedAccessor, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Accessor(callee.clone());
        let identity = self.origin(&signature, callee.property())?;
        let declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                signature.clone(),
                reason,
            )))
        };
        let (arena, declaration) = self
            .callees
            .declarations
            .function(&identity)
            .ok_or_else(|| declined(KlibBodyDeclineReason::Unjoined))?;
        let Some((target, realization)) = self
            .callees
            .source
            .external_callable_by_dependency(&identity)
        else {
            return Err(declined(KlibBodyDeclineReason::SignatureMismatch(
                "the provider realizes no declaration of the exact serialized callee".to_owned(),
            )));
        };
        let property = realization
            .callable
            .external_property_identity
            .and_then(|property| {
                Some((property, self.callees.source.external_property(property)?))
            });
        let Some((property, accessors)) = property else {
            return Err(declined(KlibBodyDeclineReason::SignatureMismatch(
                "the serialized accessor is selected as no property's accessor".to_owned(),
            )));
        };
        if realization.callable.inline.can_inline() {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "an access of an inline dependency property from an inline template",
            ));
        }
        accessor_access(
            self.callees.source,
            callee,
            arena,
            declaration,
            &realization,
        )
        .map(|receiver| {
            let (receiver, class_type_parameters) = receiver.unzip();
            LinkedAccessor {
                property,
                write: accessors.setter == Some(target),
                receiver,
                params: realization.callable.params.clone(),
                ret: realization.callable.ret,
                context_count: realization.callable.context_count,
                class_type_parameters: class_type_parameters.unwrap_or_default(),
            }
        })
        .map_err(declined)
    }

    fn requires_declaration(
        &self,
        _callee: &KlibPublicIdSignature,
    ) -> Result<bool, KlibBodyDeclineReason> {
        // No source edge needs a template's callee as target code.
        Ok(false)
    }

    fn link(
        &mut self,
        _ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let binding = self.binding(&signature, callee)?;
        let identity = &binding.declaration;
        let callee_declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                signature.clone(),
                reason,
            )))
        };
        let (arena, declaration) = self
            .callees
            .declarations
            .function(identity)
            .ok_or_else(|| callee_declined(KlibBodyDeclineReason::Unjoined))?;
        let Some((target, realization)) = self
            .callees
            .source
            .external_callable_by_dependency(identity)
        else {
            return Err(callee_declined(KlibBodyDeclineReason::SignatureMismatch(
                "the provider realizes no declaration of the exact serialized callee".to_owned(),
            )));
        };
        if realization.callable.inline.can_inline() {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of an inline dependency function from an inline template",
            ));
        }
        if let Some(receiver) = &declaration.dispatch_receiver {
            let KlibDeclarationSignature::Public(canonical) = &identity.declaration else {
                return Err(callee_declined(KlibBodyDeclineReason::Unjoined));
            };
            let member = MemberDeclaration {
                callee: canonical,
                arena,
                declaration,
                receiver,
                reference_dispatch: binding.dispatch.as_ref(),
            };
            return member_call(self.callees.source, member, target, &realization)
                .map_err(callee_declined);
        }
        let formals = realization
            .callable
            .generic_sig
            .as_deref()
            .map(|generic| generic.formals.clone())
            .unwrap_or_default();
        if !formals.is_empty() || !declaration.type_parameters.is_empty() {
            return generic_call(declaration, target, &realization, formals)
                .map_err(callee_declined);
        }
        let header = function_header(None, arena, declaration).map_err(callee_declined)?;
        let (params, ret, context_count, extension_receiver, receiver) = header.linked_signature();
        Ok(LinkedCallee {
            callee: Callee::External {
                target,
                default_provider: None,
                params: params.to_vec(),
                ret,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: extension_receiver.then_some(context_count as u32),
            },
            receiver,
            params: params.to_vec(),
            ret,
            context_count,
            singleton_dispatch: None,
            type_parameters: Vec::new(),
            class_type_parameters: Vec::new(),
        })
    }
}

/// A template's call of the generic top-level function or extension `declaration`, by the
/// provider's identity `target`. Its signature is the provider's, naming the provider's type
/// parameters `formals`, which each call binds to its type arguments.
fn generic_call(
    declaration: &crate::metadata::klib_ir::tree::KlibIrFunction,
    target: ExternalCallableId,
    realization: &ExternalCallableRealization,
    formals: Vec<String>,
) -> Result<LinkedCallee, KlibBodyDeclineReason> {
    if declaration.type_parameters.len() != formals.len() {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "{} serialized type parameters, {} selected",
            declaration.type_parameters.len(),
            formals.len()
        )));
    }
    let extension = declaration.extension_receiver.is_some();
    let expected_kind = if extension {
        ExternalCallableKind::Extension
    } else {
        ExternalCallableKind::TopLevel
    };
    let context_count = realization.callable.context_count;
    let params = realization.callable.params.clone();
    if realization.kind != expected_kind
        || context_count != declaration.context_parameters.len()
        || params.len()
            != context_count + usize::from(extension) + declaration.regular_parameters.len()
    {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "the serialized declaration is selected as a {:?} with {} parameters",
            realization.kind,
            params.len()
        )));
    }
    let ret = realization.callable.ret;
    Ok(LinkedCallee {
        callee: Callee::External {
            target,
            default_provider: None,
            params: params.clone(),
            ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: extension.then_some(context_count as u32),
        },
        receiver: None,
        params,
        ret,
        context_count,
        singleton_dispatch: None,
        type_parameters: formals,
        class_type_parameters: Vec::new(),
    })
}

/// A serialized member declaration a template calls, with the dispatch receiver it declares.
struct MemberDeclaration<'a> {
    callee: &'a KlibPublicIdSignature,
    arena: &'a crate::metadata::klib_ir::tree::KlibIrArena,
    declaration: &'a crate::metadata::klib_ir::tree::KlibIrFunction,
    receiver: &'a crate::metadata::klib_ir::tree::KlibIrParameter,
    reference_dispatch: Option<&'a KlibReferenceDispatch>,
}

/// A template's call of the member `callee`, by the provider's identity `target`. Its signature is
/// the provider's: the member declares no type parameters or extension receiver, and the
/// serialized declaration must belong to the class metadata names as its owner. The dispatch
/// receiver is read at its class's type, applied to the class's own type parameters when the class
/// is generic.
fn member_call(
    source: &dyn crate::symbol_source::SymbolSource,
    member: MemberDeclaration<'_>,
    target: ExternalCallableId,
    realization: &ExternalCallableRealization,
) -> Result<LinkedCallee, KlibBodyDeclineReason> {
    let MemberDeclaration {
        callee,
        arena,
        declaration,
        receiver,
        reference_dispatch,
    } = member;
    let generic = realization
        .callable
        .generic_sig
        .as_deref()
        .is_some_and(|generic| !generic.formals.is_empty());
    if !declaration.type_parameters.is_empty() || generic {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "type parameters",
        ));
    }
    if declaration.extension_receiver.is_some() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a member extension",
        ));
    }
    let owner = member_owner(callee)?;
    if realization.declaration_owner != Some(SemanticCallableOwner::Classifier(owner)) {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "the serialized member of {owner:?} is selected as one of {:?}",
            realization.declaration_owner
        )));
    }
    let (receiver, class_type_parameters) = if let Some(dispatch) = reference_dispatch {
        // The reference binder proved the applied parent before publication. This bounded slice
        // has no class-dependent parameter/result positions; keep the referenced receiver itself
        // rather than redoing a hierarchy walk while lowering its call.
        if dispatch.declaring_receiver.obj_internal() != Some(owner)
            || realization.callable.ret.mentions_ty_param()
            || !realization.callable.params.is_empty()
        {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an inherited reference disagrees with its frozen dispatch contract".to_owned(),
            ));
        }
        (dispatch.receiver, Vec::new())
    } else {
        class_receiver(source, owner, arena, receiver)?
    };
    let params = realization.callable.params.clone();
    let ret = realization.callable.ret;
    Ok(LinkedCallee {
        callee: Callee::External {
            target,
            default_provider: None,
            params: params.clone(),
            ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        receiver: Some(receiver),
        params,
        ret,
        context_count: realization.callable.context_count,
        singleton_dispatch: None,
        type_parameters: Vec::new(),
        class_type_parameters,
    })
}

/// The dispatch receiver an access through the non-generic accessor `declaration` reads, by the
/// provider's realization of it: a member's must belong to the class metadata names as its owner
/// and is read at the serialized receiver's type, the argument the call passes.
fn accessor_access(
    source: &dyn crate::symbol_source::SymbolSource,
    callee: &KlibAccessorIdSignature,
    arena: &crate::metadata::klib_ir::tree::KlibIrArena,
    declaration: &crate::metadata::klib_ir::tree::KlibIrFunction,
    realization: &ExternalCallableRealization,
) -> Result<Option<(Ty, Vec<String>)>, KlibBodyDeclineReason> {
    let generic = realization
        .callable
        .generic_sig
        .as_deref()
        .is_some_and(|generic| !generic.formals.is_empty());
    if !declaration.type_parameters.is_empty() || generic {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "type parameters",
        ));
    }
    let extension = declaration.extension_receiver.is_some();
    let declared = declaration.context_parameters.len()
        + usize::from(extension)
        + declaration.regular_parameters.len();
    if realization.callable.context_count != declaration.context_parameters.len()
        || realization.callable.params.len() != declared
    {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "the serialized accessor is selected with {} parameters",
            realization.callable.params.len()
        )));
    }
    let Some(receiver) = &declaration.dispatch_receiver else {
        return Ok(None);
    };
    if extension {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a member extension",
        ));
    }
    let owner = member_owner(callee.property())?;
    if realization.declaration_owner != Some(SemanticCallableOwner::Classifier(owner)) {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "the serialized member of {owner:?} is selected as one of {:?}",
            realization.declaration_owner
        )));
    }
    class_receiver(source, owner, arena, receiver).map(Some)
}

/// The type a member of `owner` reads its dispatch receiver at, with the class's type parameters
/// a call binds from its receiver: the serialized receiver's type for a non-generic class, and the
/// class applied to its own type parameters, as the provider names them, for a generic one.
fn class_receiver(
    source: &dyn crate::symbol_source::SymbolSource,
    owner: crate::types::TypeName,
    arena: &crate::metadata::klib_ir::tree::KlibIrArena,
    receiver: &crate::metadata::klib_ir::tree::KlibIrParameter,
) -> Result<(Ty, Vec<String>), KlibBodyDeclineReason> {
    let Some(record) = source.classifier(owner) else {
        return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
            "no class {owner:?} declares the serialized member"
        )));
    };
    let formals = record.type_params().clone();
    if formals.is_empty() {
        return Ok((
            semantic_type(arena, &KlibTypeParameters::default(), receiver.ty)?,
            formals,
        ));
    }
    let arguments = formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let bound = record
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first().copied())
                .unwrap_or(Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(formal, bound)
        })
        .collect::<Vec<_>>();
    Ok((Ty::obj_args_name(owner, &arguments), formals))
}

#[cfg(test)]
mod tests {
    use super::{expansion_modes, modifiers_with_receiver};
    use crate::klib_lowering::decline::KlibBodyDeclineReason;
    use crate::types::{InlineExpansionMode, InlineParameterModifier, Ty};

    fn function() -> Ty {
        Ty::fun_with_shape(vec![Ty::Int], Ty::Unit, 0, false, false)
    }

    #[test]
    fn a_selected_modifier_decides_each_function_parameter_and_the_receiver_writes_none() {
        let params = [
            Ty::Int,
            function(),
            function(),
            function(),
            Ty::nullable(function()),
        ];
        let modifiers = [
            InlineParameterModifier::None,
            InlineParameterModifier::Noinline,
            InlineParameterModifier::Crossinline,
            InlineParameterModifier::None,
        ];
        assert_eq!(
            expansion_modes(&params, Some(0), &modifiers),
            Ok(vec![
                InlineExpansionMode::Receiver,
                InlineExpansionMode::Splice,
                InlineExpansionMode::Materialize,
                InlineExpansionMode::Splice,
                InlineExpansionMode::Materialize,
            ])
        );
        assert_eq!(
            modifiers_with_receiver(&modifiers, Some(0)),
            [
                InlineParameterModifier::None,
                InlineParameterModifier::None,
                InlineParameterModifier::Noinline,
                InlineParameterModifier::Crossinline,
                InlineParameterModifier::None,
            ]
        );
    }

    #[test]
    fn modifiers_for_another_parameter_list_decline() {
        assert_eq!(
            expansion_modes(&[function()], None, &[]),
            Err(KlibBodyDeclineReason::SignatureMismatch(
                "1 parameters serialized, 0 inline modifiers selected".to_owned()
            ))
        );
    }
}
