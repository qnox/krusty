//! A KLIB function as a common-IR function declaration.
//!
//! The serialized declaration header describes the function: its parameters, in their roles
//! (context parameters, extension receiver, value parameters), and its result, converted
//! structurally into semantic types. A dependency-only callee, one no checked call of the module
//! selected, is declared from that header alone. When a provider record describes the same
//! identity too ([`KlibBodyCallable`], the declaration a checked call selected and the backend
//! handoff froze), both views must agree parameter by parameter in role, identity and type, and in
//! the result; a disagreement is an internal inconsistency and declines rather than picking either
//! side. Agreement also maps each serialized parameter symbol to the value index the body reads it
//! by.

use crate::fx_hash::HashMap;

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{classifier_name, semantic_type, KlibTypeParameters};
use crate::fir::ResolvedParameterIdentity;
use crate::ir::{FnParamInfo, FunId, IrFile, IrFunction, IrParameterIdentity};
use crate::libraries::KlibBodyCallable;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrExprId, KlibIrFunction, KlibIrParameter, KlibIrTypeId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{Ty, TyKind, TypeName};

mod declared_type_parameters;
pub(super) mod vararg_parameters;

/// One parameter as the body reads it.
#[derive(Clone, Copy, Debug)]
pub(super) struct ParameterValue {
    pub(super) index: u32,
    /// Whether a read publishes the binding as stable. Checked FIR lowering publishes it for a
    /// read of a value or context parameter, and not for a receiver, which is read as `this`.
    pub(super) stable_read: bool,
    /// The type the body reads the parameter at when that is the erasure of its declared type,
    /// as in a prepared-inline copy; `None` when the body reads the declared type itself.
    pub(super) erased: Option<Ty>,
}

/// The declaration a body is lowered into, before the body exists.
pub(super) struct FunctionHeader {
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    identities: Vec<IrParameterIdentity>,
    context_count: usize,
    extension_receiver: bool,
    dispatch_receiver: Option<Ty>,
    /// A member template's dispatch receiver, which the body reads in the value slot ahead of the
    /// parameters, as a same-module member body reads `this`.
    receiver_slot: Option<TypeName>,
    singleton_extension_unread_receiver: Option<Ty>,
    values: HashMap<KlibIrSymbol, ParameterValue>,
    /// Each parameter's serialized default, aligned with [`Self::params`].
    defaults: Vec<Option<KlibIrExprId>>,
    type_parameters: KlibTypeParameters,
    /// Each declared type parameter's erasure, by name, for a body serialized with its type
    /// parameters erased; empty for one that names them.
    erasures: HashMap<String, Ty>,
    /// The type the body returns when that is the erasure of the declared result.
    erased_result: Option<Ty>,
}

impl FunctionHeader {
    pub(super) fn result(&self) -> Ty {
        self.ret
    }

    pub(super) fn value(&self, symbol: &KlibIrSymbol) -> Option<ParameterValue> {
        self.values.get(symbol).copied()
    }

    /// How many value slots the parameters take; a body's local variables follow them.
    pub(super) fn parameter_count(&self) -> usize {
        self.params.len() + usize::from(self.receiver_slot.is_some())
    }

    /// The declared type of the value in `slot`: the dispatch receiver of a member template, then
    /// the parameters.
    pub(super) fn slot_type(&self, slot: u32) -> Ty {
        match (self.receiver_slot, slot) {
            (Some(receiver), 0) => Ty::obj_name(receiver),
            (Some(_), slot) => self.params[slot as usize - 1],
            (None, slot) => self.params[slot as usize],
        }
    }

    /// The classifier whose instance a member template's body reads as its dispatch receiver.
    pub(super) fn receiver_slot(&self) -> Option<TypeName> {
        self.receiver_slot
    }

    /// Each parameter's serialized default, aligned with [`Self::params`].
    pub(super) fn defaults(&self) -> &[Option<KlibIrExprId>] {
        &self.defaults
    }

    pub(super) fn linked_signature(&self) -> (&[Ty], Ty, usize, bool, Option<Ty>) {
        (
            &self.params,
            self.ret,
            self.context_count,
            self.extension_receiver,
            self.dispatch_receiver,
        )
    }

    pub(super) fn singleton_extension_unread_receiver(&self) -> Option<Ty> {
        self.singleton_extension_unread_receiver
    }

    /// The type parameters the body may name.
    pub(super) fn type_parameters(&self) -> &KlibTypeParameters {
        &self.type_parameters
    }

    /// The declared parameter types, context parameters and extension receiver included.
    pub(super) fn params(&self) -> &[Ty] {
        &self.params
    }

    /// The position of the extension receiver among [`Self::params`], when there is one.
    pub(super) fn extension_receiver_index(&self) -> Option<usize> {
        self.extension_receiver.then_some(self.context_count)
    }

    /// `ty` with each declared type parameter replaced by its erasure: the type an erased body
    /// uses for it. `ty` itself for a body that names its type parameters.
    pub(super) fn erased(&self, ty: Ty) -> Ty {
        if self.erasures.is_empty() {
            return ty;
        }
        crate::types::ty_subst(ty, &self.erasures)
    }

    /// The type the body's own returns carry.
    pub(super) fn body_result(&self) -> Ty {
        self.erased_result.unwrap_or(self.ret)
    }
}

/// The role a serialized parameter list gives each position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SerializedRole {
    Context,
    ExtensionReceiver,
    Value,
}

/// The declaration of the serialized function `function`, cross-checked against `selected`, the
/// provider record of the same identity, when there is one.
pub(super) fn function_header(
    selected: Option<KlibBodyCallable<'_>>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    function_header_with_emitted_dispatch(selected, None, arena, function)
}

/// The declaration of `function`, the prepared-inline copy of `selected` that a call expands
/// from, whose `i`th type parameter is the semantic type parameter `formals[i]` of the provider
/// record. Kotlin serializes the copy with its type parameters erased and its extension receiver as
/// a leading value parameter. The header is the selected declaration; each serialized parameter
/// and the result must be either its selected type or that type's erasure, which the body then
/// reads at. A function-typed parameter is read at its selected type, and its invocations convert
/// the erased operands instead.
pub(super) fn prepared_inline_header(
    selected: KlibBodyCallable<'_>,
    formals: &[Ty],
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    if function.base.symbol.kind != KlibIrSymbolKind::Function {
        return Err(mismatch(
            "the serialized declaration's symbol is not a function".to_owned(),
        ));
    }
    if function.constructor {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor",
        ));
    }
    if function.companion_extension_class.is_some() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a companion extension",
        ));
    }
    // Kotlin's prepared copy of a member declares the dispatch receiver as its leading value
    // parameter, ahead of the selected declaration's parameters; a copy may instead keep it in
    // its own slot.
    let flattened_dispatch = selected.is_member()
        && function.dispatch_receiver.is_none()
        && function.context_parameters.is_empty()
        && function.extension_receiver.is_none()
        && function.regular_parameters.len() == selected.params().len() + 1;
    let dispatch_receiver = if flattened_dispatch {
        function.regular_parameters.first()
    } else {
        function.dispatch_receiver.as_ref()
    };
    vararg_parameters::reject_dispatch_marker(dispatch_receiver)?;
    match (dispatch_receiver.is_some(), selected.is_member()) {
        (true, true) => {}
        (false, false) if selected.is_top_level() => {}
        (false, false) => {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a declaration neither top-level nor a member",
            ))
        }
        _ => {
            return Err(mismatch(
                "the serialized dispatch receiver disagrees with the selected declaration"
                    .to_owned(),
            ))
        }
    }
    if selected.name() != function.name {
        return Err(mismatch(format!(
            "the declaration is `{}` serialized and `{}` selected",
            function.name,
            selected.name()
        )));
    }
    if formals.len() != function.type_parameters.len() {
        return Err(mismatch(format!(
            "{} serialized type parameters, {} selected",
            function.type_parameters.len(),
            formals.len()
        )));
    }
    let mut type_parameters = KlibTypeParameters::default();
    let mut erasures = HashMap::default();
    for (parameter, formal) in function.type_parameters.iter().zip(formals) {
        let TyKind::TyParam(name, bound) = formal.kind() else {
            return Err(mismatch(format!(
                "the type parameter `{}` is selected as {formal:?}",
                parameter.name
            )));
        };
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter
            || !type_parameters.bind(&parameter.base.symbol, *formal)
        {
            return Err(mismatch(format!(
                "the type parameter `{}` has no identity of its own",
                parameter.name
            )));
        }
        // A bound names only earlier formals here; a later one stays and fails the comparison.
        let erasure = crate::types::ty_subst_keep_unbound(bound, &erasures);
        erasures.insert(name.as_str().to_owned(), erasure);
    }
    // The extension receiver is serialized either in its own slot or as the leading value
    // parameter; both orders follow the selected declaration's.
    let regular = &function.regular_parameters[usize::from(flattened_dispatch)..];
    let serialized = function
        .context_parameters
        .iter()
        .chain(&function.extension_receiver)
        .chain(regular)
        .collect::<Vec<_>>();
    let declared = selected.params();
    let identities = selected.parameter_identities();
    if serialized.len() != declared.len() || identities.len() != declared.len() {
        return Err(mismatch(format!(
            "{} serialized parameters, {} selected parameters with {} identities",
            serialized.len(),
            declared.len(),
            identities.len()
        )));
    }
    if function.context_parameters.len() != selected.context_count() {
        return Err(mismatch(format!(
            "{} serialized context parameters, {} selected",
            function.context_parameters.len(),
            selected.context_count()
        )));
    }
    let read_type = |serialized: KlibIrTypeId, declared: Ty| -> Result<Option<Ty>, _> {
        let read = semantic_type(arena, &type_parameters, serialized)?;
        let erased = crate::types::ty_subst(declared, &erasures);
        match read {
            read if read == declared => Ok(None),
            read if read == erased => Ok(Some(read)),
            // The erasure also drops a function type's use-site annotations, which mark its
            // context parameters and receiver: the copy keeps the plain function class.
            read if read == plain_function_type(erased) => Ok(Some(read)),
            read => Err(mismatch(format!(
                "{read:?} is serialized for the selected {declared:?}"
            ))),
        }
    };
    let mut values = HashMap::default();
    // A member's body reads its dispatch receiver in the slot ahead of the parameters. Its type
    // names the selected classifier, without type arguments of its own.
    let receiver_slot = match dispatch_receiver {
        Some(receiver) => {
            let TyKind::Obj(classifier, arguments) =
                semantic_type(arena, &type_parameters, receiver.ty)?.kind()
            else {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a dispatch receiver of a generic or non-class type",
                ));
            };
            if !arguments.is_empty() {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a dispatch receiver of a generic or non-class type",
                ));
            }
            // Metadata is authoritative for the member's owner: the copy must be its member.
            if selected.declaration_owner()
                != Some(crate::types::SemanticCallableOwner::Classifier(classifier))
            {
                return Err(mismatch(format!(
                    "a prepared inline copy is a member of {classifier:?}, selected as one of {:?}",
                    selected.declaration_owner()
                )));
            }
            values.insert(
                receiver.base.symbol.clone(),
                ParameterValue {
                    index: 0,
                    stable_read: false,
                    erased: None,
                },
            );
            Some(classifier)
        }
        None => None,
    };
    let first_slot = u32::from(receiver_slot.is_some());
    let mut defaults = Vec::with_capacity(serialized.len());
    let mut extension_receiver = false;
    let mut varargs = vararg_parameters::VarargParameters::default();
    for (index, ((parameter, &ty), identity)) in
        serialized.iter().zip(declared).zip(identities).enumerate()
    {
        if let Some(element) = parameter.vararg_element_type {
            varargs.record(
                index,
                matches!(
                    identity,
                    ResolvedParameterIdentity::Source(_)
                        | ResolvedParameterIdentity::Unnamed { .. }
                ) && parameter.base.symbol.kind == KlibIrSymbolKind::ValueParameter,
                semantic_type(arena, &type_parameters, parameter.ty)?,
                semantic_type(arena, &type_parameters, element)?,
            )?;
        }
        defaults.push(parameter.default_value);
        let receiver = matches!(identity, ResolvedParameterIdentity::ExtensionReceiver);
        if receiver && index != selected.context_count() {
            return Err(mismatch(format!(
                "the extension receiver is selected at parameter {index}"
            )));
        }
        extension_receiver |= receiver;
        let erased = read_type(parameter.ty, ty)?.filter(|_| !ty.is_fun());
        let stable_read = !matches!(
            identity,
            ResolvedParameterIdentity::ExtensionReceiver
                | ResolvedParameterIdentity::LegacyContextReceiver { .. }
        );
        let index = u32::try_from(index).expect("a parameter list fits u32") + first_slot;
        let value = ParameterValue {
            index,
            stable_read,
            erased,
        };
        if values
            .insert(parameter.base.symbol.clone(), value)
            .is_some()
        {
            return Err(mismatch(format!(
                "parameter {index} repeats another serialized parameter's identity"
            )));
        }
    }
    varargs.authenticate(Some(selected), false)?;
    let erased_result = read_type(function.return_type, selected.ret())?;
    Ok(FunctionHeader {
        name: function.name.clone(),
        params: declared.to_vec(),
        ret: selected.ret(),
        identities: identities
            .iter()
            .map(IrParameterIdentity::resolved)
            .collect(),
        context_count: selected.context_count(),
        extension_receiver,
        dispatch_receiver: None,
        receiver_slot,
        singleton_extension_unread_receiver: None,
        values,
        defaults,
        type_parameters,
        erasures,
        erased_result,
    })
}

/// Cross-check a header linked to an earlier support unit. That unit's selected singleton is part
/// of the frozen semantic contract even when this source module did not itself select the member.
pub(super) fn function_header_with_emitted_dispatch(
    selected: Option<KlibBodyCallable<'_>>,
    emitted_singleton_dispatch: Option<Ty>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    raw_function_header(
        selected,
        HeaderPurpose::Body(emitted_singleton_dispatch),
        arena,
        function,
    )
}

/// An external call retains its declaration's dispatch operand. It does not emit the raw body,
/// so the receiver-erasure/body-read constraints of support function emission do not apply.
#[derive(Clone, Copy)]
pub(super) enum HeaderPurpose {
    Body(Option<Ty>),
    External,
}

pub(super) fn raw_function_header(
    selected: Option<KlibBodyCallable<'_>>,
    purpose: HeaderPurpose,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    raw_function_header_with_class(
        selected,
        purpose,
        arena,
        function,
        KlibTypeParameters::default(),
    )
}

pub(super) fn raw_function_header_with_class(
    selected: Option<KlibBodyCallable<'_>>,
    purpose: HeaderPurpose,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    class_parameters: KlibTypeParameters,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    if function.base.symbol.kind != KlibIrSymbolKind::Function {
        return Err(mismatch(
            "the serialized declaration's symbol is not a function".to_owned(),
        ));
    }
    if function.constructor {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor",
        ));
    }
    if let Some(modifiers) = selected.and_then(KlibBodyCallable::declaration_modifiers) {
        // Serialized FunctionFlags reuse the function declaration prefix. Accessor metadata has
        // a different inline/external layout and has already been normalized by its provider.
        use crate::metadata::function_flags;
        let flags = function.base.flags;
        if modifiers.is_inline != (flags & function_flags::IS_INLINE != 0)
            || modifiers.is_suspend != (flags & function_flags::IS_SUSPEND != 0)
            || modifiers.is_external != (flags & function_flags::IS_EXTERNAL != 0)
            || modifiers.modality != crate::metadata::semantic::declaration_modality(flags)
        {
            return Err(mismatch(
                "the serialized function modifiers disagree with its selected declaration"
                    .to_owned(),
            ));
        }
    }
    let type_parameters =
        declared_type_parameters::bind(selected, arena, function, class_parameters)?;
    header(selected, purpose, type_parameters, arena, function)
}

fn header(
    selected: Option<KlibBodyCallable<'_>>,
    purpose: HeaderPurpose,
    type_parameters: KlibTypeParameters,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    let types = &type_parameters;
    vararg_parameters::reject_dispatch_marker(function.dispatch_receiver.as_ref())?;
    let emitted_singleton_dispatch = match purpose {
        HeaderPurpose::Body(singleton) => singleton,
        HeaderPurpose::External => None,
    };
    // Validate the producer's qualified marker even for a header-only transitive callee. Such a
    // callee can be exported with its unread-receiver effect and selected by a later source
    // module, so deferring validation until a provider selection is present would publish an
    // effect from malformed metadata.
    let companion_extension_classifier = companion_extension_classifier(function)?;
    if let Some(classifier) = companion_extension_classifier {
        let Some(receiver) = function.extension_receiver.as_ref() else {
            return Err(mismatch(
                "the serialized companion extension marker has no extension receiver".to_owned(),
            ));
        };
        if semantic_type(arena, types, receiver.ty)? != Ty::obj_name(classifier) {
            return Err(mismatch(
                "the serialized companion extension classifier disagrees with its receiver"
                    .to_owned(),
            ));
        }
    }
    let dispatch_receiver = if matches!(purpose, HeaderPurpose::External) {
        let receiver = function
            .dispatch_receiver
            .as_ref()
            .map(|receiver| semantic_type(arena, types, receiver.ty))
            .transpose()?;
        if let Some(selected) = selected {
            match (receiver, selected.is_member(), selected.declaration_owner()) {
                (
                    Some(receiver),
                    true,
                    Some(crate::types::SemanticCallableOwner::Classifier(owner)),
                ) if receiver.kotlin_class_internal() == Some(owner) => {}
                (None, false, _) if selected.is_top_level() => {}
                _ => return Err(mismatch(
                    "the external dispatch receiver disagrees with its selected declaration owner"
                        .to_owned(),
                )),
            }
        }
        receiver
    } else {
        match (function.dispatch_receiver.as_ref(), selected) {
            (None, Some(selected)) if !selected.is_top_level() => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a dispatch receiver",
                ));
            }
            (None, _) if emitted_singleton_dispatch.is_some() => {
                return Err(mismatch(
                    "the emitted singleton contract has no serialized dispatch receiver".to_owned(),
                ));
            }
            (Some(_), Some(selected)) if !selected.is_member() => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a dispatch receiver",
                ));
            }
            (Some(_), None) if emitted_singleton_dispatch.is_none() => {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "a dispatch receiver",
                ));
            }
            (Some(receiver), selected) => {
                // A member body can be emitted with an erased dispatch slot only when its serialized
                // body never observes that slot. The backend still accepts a source edge only when its
                // exact dispatch expression is the provider-selected singleton; ordinary instances,
                // inherited fake overrides, and bodies which read `this` remain unsupported.
                if arena.body_reads_value(function, &receiver.base.symbol) {
                    return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                        "a used dispatch receiver",
                    ));
                }
                let serialized = semantic_type(arena, types, receiver.ty)?;
                let selected_singleton = selected.and_then(|selected| {
                    selected
                        .is_member()
                        .then(|| selected.singleton_dispatch())
                        .flatten()
                });
                if let (Some(selected), Some(emitted)) =
                    (selected_singleton, emitted_singleton_dispatch)
                {
                    if selected != emitted {
                        return Err(mismatch(
                            "the selected and emitted singleton dispatch receivers disagree"
                                .to_owned(),
                        ));
                    }
                }
                let expected = selected_singleton.or(emitted_singleton_dispatch);
                if expected != Some(serialized) {
                    return Err(mismatch(
                    "the serialized dispatch receiver disagrees with the selected or emitted singleton"
                        .to_owned(),
                ));
                }
                Some(serialized)
            }
            (None, _) => None,
        }
    };
    let serialized = function
        .context_parameters
        .iter()
        .map(|parameter| (SerializedRole::Context, parameter))
        .chain(
            function
                .extension_receiver
                .iter()
                .map(|parameter| (SerializedRole::ExtensionReceiver, parameter)),
        )
        .chain(
            function
                .regular_parameters
                .iter()
                .map(|parameter| (SerializedRole::Value, parameter)),
        )
        .collect::<Vec<_>>();
    // A support body which never reads its exact extension-receiver parameter can accept an inert
    // ABI value when the checked call's receiver is an exact singleton value. Ordinary receiver
    // expressions remain evaluated by the target. This covers both a serialized
    // classifier-associated marker and an ordinary extension on a companion object, as Wasm's
    // primitive `fromBits` declarations are encoded.
    let singleton_extension_unread_receiver = match function.extension_receiver.as_ref() {
        Some(receiver) if !arena.body_reads_value(function, &receiver.base.symbol) => {
            Some(semantic_type(arena, types, receiver.ty)?)
        }
        _ => None,
    };
    let mut params = Vec::with_capacity(serialized.len());
    let mut identities = Vec::with_capacity(serialized.len());
    let mut varargs = vararg_parameters::VarargParameters::default();
    for (index, (role, parameter)) in serialized.iter().enumerate() {
        let packed = semantic_type(arena, types, parameter.ty)?;
        if let Some(element) = parameter.vararg_element_type {
            varargs.record(
                index,
                *role == SerializedRole::Value
                    && parameter.base.symbol.kind == KlibIrSymbolKind::ValueParameter,
                packed,
                semantic_type(arena, types, element)?,
            )?;
        }
        if parameter.default_value.is_some() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a parameter default",
            ));
        }
        params.push(packed);
        identities.push(serialized_identity(*role, parameter));
    }
    varargs.authenticate(selected, function.companion_extension_class.is_some())?;
    let ret = semantic_type(arena, types, function.return_type)?;
    let identities = match selected {
        None => identities.into_iter().collect::<Option<Vec<_>>>().ok_or(
            KlibBodyDeclineReason::UnsupportedDeclaration(
                "a context parameter no selected declaration describes",
            ),
        )?,
        Some(selected) => cross_check(
            selected,
            function,
            companion_extension_classifier,
            &params,
            ret,
            identities,
        )?,
    };
    let mut values = HashMap::default();
    for (index, ((role, parameter), identity)) in serialized.iter().zip(&identities).enumerate() {
        let stable_read = parameter_role(*role, identity).ok_or_else(|| {
            mismatch(format!(
                "parameter {index} is a {role:?} parameter serialized and {identity:?} selected"
            ))
        })?;
        let index = u32::try_from(index).expect("a parameter list fits u32");
        if values
            .insert(
                parameter.base.symbol.clone(),
                ParameterValue {
                    index,
                    stable_read,
                    erased: None,
                },
            )
            .is_some()
        {
            return Err(mismatch(format!(
                "parameter {index} repeats another serialized parameter's identity"
            )));
        }
    }
    Ok(FunctionHeader {
        name: function.name.clone(),
        params,
        ret,
        identities: identities
            .iter()
            .map(IrParameterIdentity::resolved)
            .collect(),
        context_count: function.context_parameters.len(),
        extension_receiver: function.extension_receiver.is_some(),
        dispatch_receiver,
        receiver_slot: None,
        singleton_extension_unread_receiver,
        values,
        defaults: vec![None; serialized.len()],
        type_parameters,
        erasures: HashMap::default(),
        erased_result: None,
    })
}

/// The identity a serialized parameter has by its role alone: an extension receiver is the
/// receiver, a value parameter its source name. A context parameter's role among the context
/// kinds (named, anonymous, legacy receiver) is recorded only by a provider, so the header alone
/// gives it none.
fn serialized_identity(
    role: SerializedRole,
    parameter: &KlibIrParameter,
) -> Option<ResolvedParameterIdentity> {
    match role {
        SerializedRole::Context => None,
        SerializedRole::ExtensionReceiver => Some(ResolvedParameterIdentity::ExtensionReceiver),
        SerializedRole::Value => Some(ResolvedParameterIdentity::Source(
            parameter.name.as_str().into(),
        )),
    }
}

/// The parameter identities of `selected`, after checking that it describes the serialized
/// declaration: the same name, parameter list, parameter types and identities, and result.
fn cross_check(
    selected: KlibBodyCallable<'_>,
    function: &KlibIrFunction,
    marker: Option<crate::types::TypeName>,
    params: &[Ty],
    ret: Ty,
    serialized: Vec<Option<ResolvedParameterIdentity>>,
) -> Result<Vec<ResolvedParameterIdentity>, KlibBodyDeclineReason> {
    if selected.name() != function.name {
        return Err(mismatch(format!(
            "the declaration is `{}` serialized and `{}` selected",
            function.name,
            selected.name()
        )));
    }
    let companion_receiver = selected.companion_receiver();
    let expected_serialized = selected.params().len() + usize::from(companion_receiver.is_some());
    if params.len() != expected_serialized
        || selected.params().len() != selected.parameter_identities().len()
    {
        return Err(mismatch(format!(
            "{} serialized parameters, {} selected parameters with {} identities{}",
            params.len(),
            selected.params().len(),
            selected.parameter_identities().len(),
            if companion_receiver.is_some() {
                " plus a companion receiver"
            } else {
                ""
            }
        )));
    }
    if function.context_parameters.len() != selected.context_count() {
        return Err(mismatch(format!(
            "{} serialized context parameters, {} selected",
            function.context_parameters.len(),
            selected.context_count()
        )));
    }
    match (companion_receiver, marker) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(mismatch(
                "the serialized companion extension has no selected receiver contract".to_owned(),
            ));
        }
        (Some(_), None) => {
            return Err(mismatch(
                "the selected companion receiver has no serialized classifier marker".to_owned(),
            ));
        }
        (Some(receiver), Some(marker)) if marker != receiver.classifier() => {
            return Err(mismatch(
                "the serialized companion extension classifier disagrees with the selected one"
                    .to_owned(),
            ));
        }
        (Some(_), Some(_)) => {}
    }
    let receiver_index = companion_receiver.map(|_| selected.context_count());
    if let Some(receiver) = companion_receiver {
        if function.extension_receiver.is_none()
            || serialized.get(receiver_index.expect("a companion receiver has an index"))
                != Some(&Some(receiver.identity().clone()))
        {
            return Err(mismatch(
                "the serialized companion receiver is absent from its selected coordinate"
                    .to_owned(),
            ));
        }
    }
    let mut checked = Vec::with_capacity(params.len());
    for (index, (ty, identity)) in params.iter().zip(serialized).enumerate() {
        if receiver_index == Some(index) {
            checked.push(
                companion_receiver
                    .expect("a receiver coordinate has a contract")
                    .identity()
                    .clone(),
            );
            continue;
        }
        let selected_index = index - usize::from(receiver_index.is_some_and(|slot| index > slot));
        let chosen = &selected.params()[selected_index];
        if ty != chosen {
            return Err(mismatch(format!(
                "parameter {index} is {ty:?} serialized and {chosen:?} selected"
            )));
        }
        let chosen = &selected.parameter_identities()[selected_index];
        if let Some(identity) = identity.filter(|identity| identity != chosen) {
            return Err(mismatch(format!(
                "parameter {index} is {identity:?} serialized and {chosen:?} selected"
            )));
        }
        checked.push(chosen.clone());
    }
    if ret != selected.ret() {
        return Err(mismatch(format!(
            "the result is {ret:?} serialized and {:?} selected",
            selected.ret()
        )));
    }
    Ok(checked)
}

/// The exact classifier carried by a serialized companion-extension marker. A malformed marker
/// is a disagreement with the selected provider contract, not a type the body may reinterpret.
fn companion_extension_classifier(
    function: &KlibIrFunction,
) -> Result<Option<crate::types::TypeName>, KlibBodyDeclineReason> {
    let Some(marker) = function.companion_extension_class.as_ref() else {
        return Ok(None);
    };
    if marker.kind != KlibIrSymbolKind::Class {
        return Err(mismatch(
            "the serialized companion extension marker is not a class".to_owned(),
        ));
    }
    let KlibIrSignature::Public(signature) = &marker.signature else {
        return Err(mismatch(
            "the serialized companion extension marker is not a public classifier".to_owned(),
        ));
    };
    if signature.member_id().is_some() || signature.declaration().is_empty() {
        return Err(mismatch(
            "the serialized companion extension marker has no classifier identity".to_owned(),
        ));
    }
    classifier_name(signature).map(Some).map_err(|_| {
        mismatch(
            "the serialized companion extension marker has an invalid classifier identity"
                .to_owned(),
        )
    })
}

/// Whether the selected identity has the serialized role, and if so whether a read of it is a
/// stable binding read.
fn parameter_role(role: SerializedRole, identity: &ResolvedParameterIdentity) -> Option<bool> {
    match (role, identity) {
        (SerializedRole::Context, ResolvedParameterIdentity::LegacyContextReceiver { .. }) => {
            Some(false)
        }
        (
            SerializedRole::Context,
            ResolvedParameterIdentity::ContextValue { .. }
            | ResolvedParameterIdentity::AnonymousContextParameter { .. },
        ) => Some(true),
        (SerializedRole::ExtensionReceiver, ResolvedParameterIdentity::ExtensionReceiver) => {
            Some(false)
        }
        (
            SerializedRole::Value,
            ResolvedParameterIdentity::Source(_) | ResolvedParameterIdentity::Unnamed { .. },
        ) => Some(true),
        _ => None,
    }
}

/// The function class application `ty` is, without the annotations that mark its context
/// parameters and receiver; any other type unchanged.
pub(super) fn plain_function_type(ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Fun(signature) => Ty::fun_with_shape(
            signature.params.to_vec(),
            signature.ret,
            0,
            false,
            signature.suspend,
        ),
        TyKind::Nullable(inner) => Ty::nullable(plain_function_type(inner)),
        _ => ty,
    }
}

fn mismatch(detail: String) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail)
}

/// Declare the function to `ir`, with the parameter facts checked FIR lowering publishes for a
/// top-level function. Its body is attached once lowered: a call cycle reaches the declaration
/// before its body exists.
pub(super) fn declare_function(ir: &mut IrFile, header: &FunctionHeader) -> FunId {
    let function = ir.add_fun(IrFunction {
        name: header.name.clone(),
        // A dependency declaration's parameters were checked where it was compiled; the
        // entry guards a backend adds are those of the module's own visible functions.
        param_checks: vec![None; header.params.len()],
        params: header.params.clone(),
        ret: header.ret,
        body: None,
        is_static: true,
        dispatch_receiver: None,
    });
    ir.fn_source_names.insert(function, header.name.clone());
    ir.fn_params
        .insert(function, FnParamInfo::identities(header.identities.clone()));
    if header.extension_receiver {
        ir.extension_receiver_fns.insert(function);
    }
    if header.context_count != 0 {
        ir.fn_context_counts.insert(function, header.context_count);
    }
    if let Some(receiver) = header.singleton_extension_unread_receiver() {
        ir.singleton_extension_unread_receivers
            .insert(function, receiver);
    }
    function
}
