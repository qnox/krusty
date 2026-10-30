//! The source-member rung of lambda shaping for an unqualified call.
//!
//! The implicit receivers are swept innermost first for a source member that can shape the call's
//! lambdas. The depth of the receiver that answered bounds the per-receiver sweep of classpath
//! members and extensions: only receivers strictly inside it may override this answer.

use crate::ast::ExprId;
use crate::types::Ty;

use super::lambda_expectation::{module_member_lambda_shape, MemberLambdaShape};
use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// The innermost implicit receiver whose source member shapes the lambdas of `name(args)`,
    /// with its depth in the receiver tower.
    pub(super) fn implicit_source_member_lambda_shape(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        args: &[ExprId],
        partial: &[Option<Ty>],
        expected: Option<Ty>,
    ) -> Option<(usize, MemberLambdaShape)> {
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        self.implicit_member_receiver_types(scope)
            .into_iter()
            .enumerate()
            .find_map(|(depth, receiver)| {
                let (member, generic_member) = self.module_member_lambda_plan(
                    scope, call, receiver, name, args, partial, expected,
                );
                let member = member?;
                let contextual = self.contextual_call_shape(
                    scope,
                    &member.params,
                    &member.call_sig,
                    member.context_count,
                    arg_names.as_deref(),
                )?;
                module_member_lambda_shape(
                    &self.fed_source(),
                    &member,
                    generic_member.as_ref(),
                    &contextual,
                    args,
                    arg_names.as_deref(),
                    self.file.call_has_trailing_lambda.contains(&call.0),
                )
                .map(|shape| (depth, shape))
            })
    }
}
