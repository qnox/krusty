//! Shrinking parser-era adapter for unguarded inline plans.
//!
//! Checked FIR consumes the complete contract directly. The legacy AST lowerer can represent only
//! the original lambda-only subset and must reject every generalized plan without retrying it as an
//! ordinary dependency call.

use crate::libraries::{InlineBodyPlan, InlineBodyValue};

pub(super) fn plain_invoke_lambda(
    plan: &InlineBodyPlan,
) -> Option<(usize, Vec<usize>, Option<usize>)> {
    let InlineBodyPlan::InvokeLambda {
        lambda_parameter,
        arguments,
        prologue,
        cleanup,
        cause,
        recovery,
        defaults,
        result,
    } = plan
    else {
        return None;
    };
    if !prologue.is_empty()
        || !cleanup.is_empty()
        || cause.is_some()
        || recovery.is_some()
        || !defaults.is_empty()
    {
        return None;
    }
    let parameter = |value: &InlineBodyValue| match value {
        InlineBodyValue::Parameter(parameter) => Some(*parameter),
        InlineBodyValue::Cause => None,
    };
    Some((
        *lambda_parameter,
        arguments
            .iter()
            .map(parameter)
            .collect::<Option<Vec<_>>>()?,
        match result {
            None => None,
            Some(result) => Some(parameter(result)?),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        InlineCollectionAppend, InlineCollectionLocalNames, InlineIterationTraversal, LibraryMember,
    };
    use crate::types::Ty;

    fn member() -> Box<LibraryMember> {
        Box::new(LibraryMember::new(
            "dependency".to_string(),
            Vec::new(),
            Ty::Unit,
            "()V".to_string(),
        ))
    }

    #[test]
    fn generalized_inline_plans_cannot_enter_the_parser_era_adapter() {
        let iteration = InlineBodyPlan::Iteration {
            lambda_parameter: 0,
            index: None,
            traversal: InlineIterationTraversal::Array,
        };
        let transform = InlineBodyPlan::CollectionTransform {
            lambda_parameter: 0,
            traversal: InlineIterationTraversal::Array,
            local_names: InlineCollectionLocalNames {
                outer_receiver: "outer".into(),
                inner_receiver: "inner".into(),
                destination: "destination".into(),
                element: "element".into(),
            },
            factory: member(),
            capacity: None,
            append: InlineCollectionAppend::Member(member()),
        };

        assert!(plain_invoke_lambda(&iteration).is_none());
        assert!(plain_invoke_lambda(&transform).is_none());
    }

    #[test]
    fn only_the_original_plain_invoke_contract_is_adapted() {
        let plain = InlineBodyPlan::InvokeLambda {
            lambda_parameter: 2,
            arguments: vec![InlineBodyValue::Parameter(0), InlineBodyValue::Parameter(1)],
            prologue: Vec::new(),
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result: Some(InlineBodyValue::Parameter(0)),
        };
        assert_eq!(plain_invoke_lambda(&plain), Some((2, vec![0, 1], Some(0))));

        let mut guarded = plain;
        let InlineBodyPlan::InvokeLambda { cause, .. } = &mut guarded else {
            unreachable!()
        };
        *cause = Some(Ty::nullable(Ty::obj("kotlin/Throwable")));
        assert!(plain_invoke_lambda(&guarded).is_none());
    }
}
