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

use std::collections::HashMap;

use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::fir::ResolvedParameterIdentity;
use crate::ir::{FnParamInfo, FunId, IrFile, IrFunction, IrParameterIdentity};
use crate::libraries::KlibBodyCallable;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction, KlibIrParameter};
use crate::metadata::klib_ir::{KlibIrSymbol, KlibIrSymbolKind};
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
    if function.dispatch_receiver.is_some()
        || selected.is_some_and(|selected| !selected.is_top_level())
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a dispatch receiver",
        ));
    }
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
        Some(selected) => cross_check(selected, function, &params, ret, identities)?,
    };
    let mut values = HashMap::new();
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
    if params.len() != selected.params().len()
        || params.len() != selected.parameter_identities().len()
    {
        return Err(mismatch(format!(
            "{} serialized parameters, {} selected parameters with {} identities",
            params.len(),
            selected.params().len(),
            selected.parameter_identities().len()
        )));
    }
    if function.context_parameters.len() != selected.context_count() {
        return Err(mismatch(format!(
            "{} serialized context parameters, {} selected",
            function.context_parameters.len(),
            selected.context_count()
        )));
    }
    for (index, ((ty, chosen), identity)) in params
        .iter()
        .zip(selected.params())
        .zip(serialized)
        .enumerate()
    {
        if ty != chosen {
            return Err(mismatch(format!(
                "parameter {index} is {ty:?} serialized and {chosen:?} selected"
            )));
        }
        let chosen = &selected.parameter_identities()[index];
        if let Some(identity) = identity.filter(|identity| identity != chosen) {
            return Err(mismatch(format!(
                "parameter {index} is {identity:?} serialized and {chosen:?} selected"
            )));
        }
    }
    if ret != selected.ret() {
        return Err(mismatch(format!(
            "the result is {ret:?} serialized and {:?} selected",
            selected.ret()
        )));
    }
    Ok(selected.parameter_identities().to_vec())
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
    function
}
