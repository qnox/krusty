//! Constant default arguments, read from each declaration's serialized IR.
//!
//! Metadata says only that a parameter has a default; its value is the parameter's IR default
//! expression. A constant default is published as a [`DefaultValue`], which a call site that omits
//! the argument passes itself. Any other default (a call, a reference to another parameter) is
//! not a closed value and stays unpublished, so such a call is rejected rather than miscompiled.

use std::collections::HashMap;

use crate::libraries::DefaultValue;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrExprId, KlibIrExprKind, KlibIrFunction};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrModuleTrees, KlibIrSignature};

/// The constant default of each value parameter, by the declaration's linkable identity. Only
/// declarations with at least one constant default are listed.
#[derive(Default)]
pub(super) struct ParameterDefaults {
    defaults: HashMap<KlibIrSignature, Vec<Option<DefaultValue>>>,
}

impl ParameterDefaults {
    /// Read the defaults of every function `trees` declares.
    pub(super) fn add_library(&mut self, trees: &KlibIrModuleTrees) {
        for signature in trees.function_signatures() {
            let Some((arena, function)) = trees.function(signature) else {
                continue;
            };
            let defaults = value_parameter_defaults(arena, function);
            if defaults.iter().any(Option::is_some) {
                self.defaults.insert(signature.clone(), defaults);
            }
        }
    }

    /// The constant defaults of the declaration under `signature`, one per value parameter;
    /// empty when it has none.
    pub(super) fn of(&self, signature: &KlibIrSignature) -> Vec<Option<DefaultValue>> {
        self.defaults.get(signature).cloned().unwrap_or_default()
    }
}

/// The default of one value parameter as its library serialized it: the IR expression, and the
/// closed value it denotes when it is a constant.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KlibParameterDefault {
    pub(crate) expression: KlibIrExprId,
    pub(crate) constant: Option<DefaultValue>,
}

/// The default of value parameter `parameter` of `function`, a declaration of `arena`'s tree;
/// `None` when it declares none. Receivers and context parameters are not value parameters.
pub(crate) fn parameter_default(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    parameter: usize,
) -> Option<KlibParameterDefault> {
    let expression = function.regular_parameters.get(parameter)?.default_value?;
    let constant = match &arena.expr(expression).kind {
        KlibIrExprKind::Const(constant) => Some(constant_value(constant)),
        _ => None,
    };
    Some(KlibParameterDefault {
        expression,
        constant,
    })
}

/// One entry per value parameter: its constant default, if it has one.
fn value_parameter_defaults(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Vec<Option<DefaultValue>> {
    (0..function.regular_parameters.len())
        .map(|parameter| parameter_default(arena, function, parameter)?.constant)
        .collect()
}

fn constant_value(constant: &KlibIrConstant) -> DefaultValue {
    match constant {
        KlibIrConstant::Null => DefaultValue::Null,
        KlibIrConstant::Boolean(value) => DefaultValue::Bool(*value),
        KlibIrConstant::Char(value) => DefaultValue::Char(*value),
        KlibIrConstant::Byte(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Short(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Int(value) => DefaultValue::Int(i64::from(*value)),
        KlibIrConstant::Long(value) => DefaultValue::Long(*value),
        KlibIrConstant::Float(value) => DefaultValue::Float(*value),
        KlibIrConstant::Double(value) => DefaultValue::Double(*value),
        KlibIrConstant::String(value) => DefaultValue::Str(value.as_str().into()),
    }
}
