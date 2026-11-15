//! The statement normalizations a common-IR state machine runs on a suspend body before splitting
//! it at its suspension points, as the JVM's coroutine lowering runs them on its own bodies.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrConst, IrFile};
use crate::types::Ty;

use super::{
    desugar_value_try, desugar_value_when, function_value_types, hoist_suspensions,
    normalize_statement_try_results, splice_return_blocks, CoroutineRepresentation,
    SuspensionTyping,
};

/// The semantic zero a fresh temporary starts from. A target that realizes coroutines on common
/// IR stores each value as its semantic type, so the zero is the type's own.
struct SemanticZero;

impl CoroutineRepresentation for SemanticZero {
    fn zero(&self, ty: &Ty) -> IrConst {
        IrConst::zero_for_value_type(*ty)
    }
}

/// Move every suspension of `function`'s `body` to a statement boundary: a value block's
/// statements into its consumer's statement list, a nested suspension into a preceding temporary,
/// and a suspending value `try` or `when` into statements of its own. `function_result` is the
/// function's declared (pre-CPS) result.
pub(super) fn normalize(
    ir: &mut IrFile,
    function: u32,
    body: ExprId,
    function_result: Ty,
    suspend_functions: &HashSet<u32>,
) {
    splice_return_blocks(ir, body);
    // Every function's declared result, before the continuation pass gave a suspend one `Any?`.
    let orig_rets = (0..ir.functions.len() as u32)
        .map(|callee| {
            ir.suspend_declared_sigs
                .get(&callee)
                .map_or(ir.functions[callee as usize].ret, |(_, result)| *result)
        })
        .collect::<Vec<_>>();
    let typing = SuspensionTyping {
        orig_rets: &orig_rets,
        representation: &SemanticZero,
    };
    let mut value_types = function_value_types(ir, function, body);
    hoist_suspensions(ir, body, suspend_functions, &typing, &mut value_types);
    desugar_value_try(ir, &SemanticZero, body, suspend_functions, &function_result);
    desugar_value_when(ir, &SemanticZero, body, suspend_functions, &function_result);
    normalize_statement_try_results(ir, body, true);
}
