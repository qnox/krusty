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
use super::klib_types::{classifier_name, semantic_type};
use crate::fir::ResolvedParameterIdentity;
use crate::ir::{FnParamInfo, FunId, IrFile, IrFunction, IrParameterIdentity};
use crate::libraries::KlibBodyCallable;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction, KlibIrParameter};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

/// One parameter as the body reads it.
#[derive(Clone, Copy, Debug)]
pub(super) struct ParameterValue {
    pub(super) index: u32,
    /// Whether a read publishes the binding as stable. Checked FIR lowering publishes it for a
    /// read of a value or context parameter, and not for a receiver, which is read as `this`.
    pub(super) stable_read: bool,
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
    singleton_extension_unread_receiver: Option<Ty>,
    values: HashMap<KlibIrSymbol, ParameterValue>,
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
        self.params.len()
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

/// Cross-check a header linked to an earlier support unit. That unit's selected singleton is part
/// of the frozen semantic contract even when this source module did not itself select the member.
pub(super) fn function_header_with_emitted_dispatch(
    selected: Option<KlibBodyCallable<'_>>,
    emitted_singleton_dispatch: Option<Ty>,
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
        if semantic_type(arena, receiver.ty)? != Ty::obj_name(classifier) {
            return Err(mismatch(
                "the serialized companion extension classifier disagrees with its receiver"
                    .to_owned(),
            ));
        }
    }
    let dispatch_receiver = match (function.dispatch_receiver.as_ref(), selected) {
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
            let serialized = semantic_type(arena, receiver.ty)?;
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
                        "the selected and emitted singleton dispatch receivers disagree".to_owned(),
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
    };
    if !function.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "type parameters",
        ));
    }
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
            Some(semantic_type(arena, receiver.ty)?)
        }
        _ => None,
    };
    let mut params = Vec::with_capacity(serialized.len());
    let mut identities = Vec::with_capacity(serialized.len());
    for (role, parameter) in &serialized {
        if parameter.vararg_element_type.is_some() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a vararg parameter",
            ));
        }
        if parameter.default_value.is_some() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a parameter default",
            ));
        }
        params.push(semantic_type(arena, parameter.ty)?);
        identities.push(serialized_identity(*role, parameter));
    }
    let ret = semantic_type(arena, function.return_type)?;
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
                ParameterValue { index, stable_read },
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
        singleton_extension_unread_receiver,
        values,
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
