//! The semantic parameters of a lambda an inline call places in its host's body, one per argument
//! its `FunctionN.invoke` site leaves on the stack.
//!
//! A placed literal or callable reference counts its arguments by its implementation's physical
//! parameters after the captures. For a `suspend` function type that arity includes the trailing
//! `Continuation` the suspend `invoke` is passed, which the implementation takes after the function
//! type's own parameters and its body reads as the current continuation. Both inline routes — the
//! MethodInliner port and the unified byte splice — size and type the placed arguments by this one
//! rule.
//!
//! A compiler plugin may append parameters to the implementation (Compose's composer and
//! `$changed` follow a composable lambda's own parameters). The function type does not mention
//! them, so each such trailing argument takes the type the implementation declares for it. The
//! implementation's recorded parameter identities say which ones a plugin added.

use krusty::ir::{FunId, IrFile, IrParameterProvenance};
use krusty::types::{FnSig, Ty};

/// The types of the trailing arguments of a placed lambda that the implementation `function`
/// records as compiler-generated parameters: `arguments` are its parameters after its `captures`.
pub(in crate::ir_emit) fn generated_trailing_arguments(
    ir: &IrFile,
    function: FunId,
    captures: usize,
    arguments: &[Ty],
) -> Vec<Ty> {
    let identities = ir
        .fn_params
        .get(&function)
        .map_or(&[][..], |parameters| parameters.identities.as_slice());
    let generated = (0..arguments.len())
        .rev()
        .take_while(|&argument| {
            identities.get(captures + argument).is_some_and(|identity| {
                identity.provenance == IrParameterProvenance::CompilerGenerated
            })
        })
        .count();
    arguments[arguments.len() - generated..].to_vec()
}

/// The semantic type of each of the `arity` arguments a placed lambda of function type
/// `signature` is invoked with: the function type's parameters, then the continuation when it is
/// `suspend`, then the `appended` types of the arguments a plugin added. `None` when that does not
/// account for exactly `arity` arguments.
pub(in crate::ir_emit) fn placed_lambda_parameters(
    signature: &FnSig,
    arity: usize,
    appended: &[Ty],
) -> Option<Vec<Ty>> {
    let mut parameters = signature.params.clone();
    if signature.suspend {
        parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
    }
    // The function type and the continuation account for the leading arguments; the plugin's are
    // the last ones the implementation records as generated, which can include that continuation.
    let missing = arity.checked_sub(parameters.len())?;
    parameters.extend_from_slice(&appended[appended.len().checked_sub(missing)?..]);
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
        assert_eq!(
            placed_lambda_parameters(&ordinary, 1, &[]),
            Some(vec![Ty::Int])
        );
        assert_eq!(placed_lambda_parameters(&ordinary, 2, &[]), None);
    }

    #[test]
    fn a_suspend_function_type_takes_the_continuation_last() {
        let suspend = signature(vec![Ty::Int], true);
        assert_eq!(
            placed_lambda_parameters(&suspend, 2, &[]),
            Some(vec![Ty::Int, Ty::obj("kotlin/coroutines/Continuation")])
        );
        assert_eq!(placed_lambda_parameters(&suspend, 1, &[]), None);
    }

    #[test]
    fn a_continuation_the_implementation_records_as_generated_is_not_counted_twice() {
        let suspend = signature(vec![Ty::Int], true);
        let continuation = Ty::obj("kotlin/coroutines/Continuation");
        assert_eq!(
            placed_lambda_parameters(&suspend, 2, &[continuation]),
            Some(vec![Ty::Int, continuation])
        );
    }

    #[test]
    fn a_plugin_appended_argument_takes_the_type_its_implementation_declares() {
        let ordinary = signature(vec![], false);
        let composer = Ty::obj("androidx/compose/runtime/Composer");
        assert_eq!(
            placed_lambda_parameters(&ordinary, 2, &[composer, Ty::Int]),
            Some(vec![composer, Ty::Int])
        );
        assert_eq!(
            placed_lambda_parameters(&ordinary, 3, &[composer, Ty::Int]),
            None
        );
    }
}
