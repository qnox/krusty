//! A selected KLIB function as a common-IR function declaration.
//!
//! The declaration's parameters and result are the ones its provider normalized and the backend
//! handoff froze ([`BackendCallableFact`]): the body is lowered against exactly the declaration a
//! call selected. The serialized declaration describes the same identity, so its parameter list
//! must agree with the frozen one, parameter by parameter in role and type; a disagreement is an
//! internal inconsistency and declines rather than picking either side. Agreement also maps each
//! serialized parameter symbol to the value index the body reads it by.

use std::collections::HashMap;

use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::backend::BackendCallableFact;
use crate::fir::ResolvedParameterIdentity;
use crate::ir::{ExprId, FnParamInfo, FunId, IrFile, IrFunction, IrParameterIdentity};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};
use crate::metadata::klib_ir::KlibIrSymbol;
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
}

/// The role a serialized parameter list gives each position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SerializedRole {
    Context,
    ExtensionReceiver,
    Value,
}

/// Join the frozen declaration of `fact` with its serialized declaration `function`.
pub(super) fn function_header(
    fact: &BackendCallableFact,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    if function.constructor {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor",
        ));
    }
    if function.dispatch_receiver.is_some() || !fact.is_top_level() {
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
    if serialized.len() != fact.params.len() || serialized.len() != fact.parameter_identities.len()
    {
        return Err(mismatch(format!(
            "{} serialized parameters, {} selected parameters with {} identities",
            serialized.len(),
            fact.params.len(),
            fact.parameter_identities.len()
        )));
    }
    if function.context_parameters.len() != fact.context_count {
        return Err(mismatch(format!(
            "{} serialized context parameters, {} selected",
            function.context_parameters.len(),
            fact.context_count
        )));
    }
    let mut values = HashMap::new();
    for (index, ((role, parameter), (selected, identity))) in serialized
        .into_iter()
        .zip(fact.params.iter().zip(fact.parameter_identities.iter()))
        .enumerate()
    {
        let stable_read = parameter_role(role, identity).ok_or_else(|| {
            mismatch(format!(
                "parameter {index} is a {role:?} parameter serialized and {identity:?} selected"
            ))
        })?;
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
        let ty = semantic_type(arena, parameter.ty)?;
        if ty != *selected {
            return Err(mismatch(format!(
                "parameter {index} is {ty:?} serialized and {selected:?} selected"
            )));
        }
        let index = u32::try_from(index).expect("a parameter list fits u32");
        values.insert(
            parameter.base.symbol.clone(),
            ParameterValue { index, stable_read },
        );
    }
    let ret = semantic_type(arena, function.return_type)?;
    if ret != fact.ret {
        return Err(mismatch(format!(
            "the result is {ret:?} serialized and {:?} selected",
            fact.ret
        )));
    }
    Ok(FunctionHeader {
        params: fact.params.clone(),
        ret,
        identities: fact
            .parameter_identities
            .iter()
            .map(IrParameterIdentity::resolved)
            .collect(),
        context_count: fact.context_count,
        extension_receiver: function.extension_receiver.is_some(),
        values,
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

/// Add the declaration with its lowered `body` to `ir`, with the parameter facts checked FIR
/// lowering publishes for a top-level function.
pub(super) fn add_function(
    ir: &mut IrFile,
    fact: &BackendCallableFact,
    header: FunctionHeader,
    body: ExprId,
) -> FunId {
    let FunctionHeader {
        params,
        ret,
        identities,
        context_count,
        extension_receiver,
        values: _,
    } = header;
    let function = ir.add_fun(IrFunction {
        name: fact.name.clone(),
        // A dependency declaration's parameters were checked where it was compiled; the
        // entry guards a backend adds are those of the module's own visible functions.
        param_checks: vec![None; params.len()],
        params,
        ret,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
    });
    ir.fn_source_names.insert(function, fact.name.clone());
    ir.fn_params
        .insert(function, FnParamInfo::identities(identities));
    if extension_receiver {
        ir.extension_receiver_fns.insert(function);
    }
    if context_count != 0 {
        ir.fn_context_counts.insert(function, context_count);
    }
    function
}
