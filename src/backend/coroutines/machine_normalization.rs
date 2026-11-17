//! The statement normalizations a common-IR state machine runs on a suspend body before splitting
//! it at its suspension points, as the JVM's coroutine lowering runs them on its own bodies.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ExprId, IrConst, IrFile, IrTypeParameter};
use crate::types::Ty;

use super::{
    desugar_value_try, desugar_value_when, function_value_types, hoist_suspensions,
    normalize_statement_try_results, splice_return_blocks, CoroutineRepresentation,
    SuspensionTyping,
};

/// A target that realizes coroutines on common IR stores each value as its semantic type: a
/// temporary starts from that type's own zero, and holds a generic callee's result at the
/// erasure of the callee's own formals, which the caller does not declare.
struct SemanticZero;

impl CoroutineRepresentation for SemanticZero {
    fn zero(&self, ty: &Ty) -> IrConst {
        IrConst::zero_for_value_type(*ty)
    }

    fn suspension_result(&self, declared: Ty, callee_formals: &[IrTypeParameter]) -> Ty {
        // Each formal reads as its representative upper bound.
        let erasures = callee_formals
            .iter()
            .map(|parameter| (parameter.semantic_name.clone(), parameter.upper_bound()))
            .collect::<HashMap<_, _>>();
        let formals = erasures.keys().cloned().collect::<Vec<_>>();
        crate::types::ty_subst_formals(declared, &formals, &erasures)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{wk, TypeVariance};

    fn formal(semantic_name: &str, bounds: Vec<(Ty, bool)>) -> IrTypeParameter {
        IrTypeParameter {
            name: "T".to_string(),
            semantic_name: semantic_name.to_string(),
            bounds,
            variance: TypeVariance::Invariant,
            reified: false,
            captured_from_enclosing: false,
        }
    }

    /// `suspend fun <T> f(): T` called from a caller that does not declare `T`: the caller's
    /// temporary holds the callee's result at `T`'s bound, nullable as the bound is.
    #[test]
    fn a_generic_callee_result_reads_as_its_formal_bounds_in_the_caller() {
        let any = Ty::nullable(Ty::obj_name(wk::any()));
        let unbounded = formal("callee:T", Vec::new());
        assert_eq!(
            SemanticZero.suspension_result(unbounded.ty(), std::slice::from_ref(&unbounded)),
            any
        );
        let bounded = formal("callee:N", vec![(Ty::obj_name(wk::char_sequence()), false)]);
        let list = Ty::obj_args_name(wk::iterable(), &[bounded.ty()]);
        assert_eq!(
            SemanticZero.suspension_result(list, std::slice::from_ref(&bounded)),
            Ty::obj_args_name(wk::iterable(), &[Ty::obj_name(wk::char_sequence())])
        );
        // A type parameter the callee does not declare stays as written.
        let caller = formal("caller:R", Vec::new());
        assert_eq!(
            SemanticZero.suspension_result(caller.ty(), std::slice::from_ref(&unbounded)),
            caller.ty()
        );
    }
}
