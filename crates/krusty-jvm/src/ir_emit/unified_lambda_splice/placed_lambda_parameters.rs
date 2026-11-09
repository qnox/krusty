//! The semantic parameters of a lambda an inline call places in its host's body, one per argument
//! its `FunctionN.invoke` site leaves on the stack.
//!
//! A placed literal or callable reference counts its arguments by its implementation's physical
//! parameters after the captures. For a `suspend` function type that arity includes the trailing
//! `Continuation` the suspend `invoke` is passed, which the implementation takes after the function
//! type's own parameters and its body reads as the current continuation. Both inline routes — the
//! MethodInliner port and the unified byte splice — size and type the placed arguments by this one
//! rule.

use krusty::types::{FnSig, Ty};

/// The semantic type of each of the `arity` arguments a placed lambda of function type
/// `signature` is invoked with: the function type's parameters, then the continuation when it is
/// `suspend`. `None` when the function type does not account for exactly `arity` arguments.
pub(in crate::ir_emit) fn placed_lambda_parameters(
    signature: &FnSig,
    arity: usize,
) -> Option<Vec<Ty>> {
    let mut parameters = signature.params.clone();
    if signature.suspend {
        parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
    }
    (parameters.len() == arity).then_some(parameters)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(params: Vec<Ty>, suspend: bool) -> FnSig {
        FnSig {
            params,
            ret: Ty::obj("kotlin/String"),
            context_count: 0,
            has_receiver: false,
            suspend,
        }
    }

    #[test]
    fn an_ordinary_function_type_takes_its_own_parameters() {
        let ordinary = signature(vec![Ty::Int], false);
        assert_eq!(placed_lambda_parameters(&ordinary, 1), Some(vec![Ty::Int]));
        assert_eq!(placed_lambda_parameters(&ordinary, 2), None);
    }

    #[test]
    fn a_suspend_function_type_takes_the_continuation_last() {
        let suspend = signature(vec![Ty::Int], true);
        assert_eq!(
            placed_lambda_parameters(&suspend, 2),
            Some(vec![Ty::Int, Ty::obj("kotlin/coroutines/Continuation")])
        );
        assert_eq!(placed_lambda_parameters(&suspend, 1), None);
    }
}
