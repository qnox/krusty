//! The parameter identities of the members a suspend lambda's class declares.
//!
//! kotlinc's `SuspendLambdaLowering` gives the class a constructor over the captured values and the
//! completion, `create` for a lambda of at most one parameter, and the typed `invoke` that
//! overrides its `FunctionN`. None of them is a source declaration, so the realization records what
//! each of their parameters is, as backend-neutral identities and roles, when it makes the class.
//! Every class-file surface that names one of those parameters (the `LocalVariableTable` and the
//! `-java-parameters` `MethodParameters`) formats it from this record, and nothing else: not from a
//! descriptor position and not from another surface's rows.

use crate::ir::{
    IrCapturedReceiver, IrGeneratedParameterRole, IrLiftingRoot, IrParameterIdentity,
    IrParameterRole,
};

/// One generated member of a suspend lambda's class whose parameters are recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SuspendLambdaMember {
    Constructor,
    Create,
    /// The typed `invoke`, and the erased `FunctionN.invoke` bridge of the same arity.
    Invoke,
}

/// The parameter identities of a suspend lambda class's constructor, `create` and typed `invoke`,
/// each list exactly parallel to that member's physical parameters.
#[derive(Clone, Debug)]
pub(crate) struct SuspendLambdaParameters {
    /// `<init>`: each captured value, under the identity the lambda captured it with, then the
    /// completion.
    constructor: Vec<IrParameterIdentity>,
    /// `create`, declared for a lambda of at most one parameter: its value, then the completion.
    create: Option<Vec<IrParameterIdentity>>,
    /// The typed `invoke`: the parameters of the `FunctionN.invoke` it overrides, the lambda's own
    /// parameters followed by the continuation.
    invoke: Vec<IrParameterIdentity>,
    /// The origins of the implicit receivers the constructor's captured receivers index: the
    /// lambda's own record, which its `invokeSuspend` no longer has.
    pub(crate) captured_receivers: Vec<IrCapturedReceiver>,
    /// Where the lambda was lifted from, which a captured receiver of a value-class member is named
    /// after.
    pub(crate) lifting_root: Option<IrLiftingRoot>,
}

impl SuspendLambdaParameters {
    /// The members of a lambda capturing `captures` (each the lambda's own captured-value or
    /// captured-receiver identity, in constructor order) with `parameters` parameters of its own,
    /// its receiver included.
    pub(crate) fn new(
        captures: Vec<IrParameterIdentity>,
        parameters: usize,
        captured_receivers: Vec<IrCapturedReceiver>,
        lifting_root: Option<IrLiftingRoot>,
    ) -> Self {
        for capture in &captures {
            match capture.role {
                IrParameterRole::CapturedValue { .. } => assert!(
                    capture.source_name.is_some(),
                    "a suspend lambda's captured value keeps its source name"
                ),
                IrParameterRole::CapturedReceiver { ordinal } => assert!(
                    (ordinal as usize) < captured_receivers.len(),
                    "a suspend lambda's captured receiver has a recorded origin"
                ),
                role => panic!("a suspend lambda's constructor captures no {role:?}"),
            }
        }
        let completion =
            || IrParameterIdentity::generated(IrGeneratedParameterRole::Continuation, None);
        let constructor = captures
            .into_iter()
            .chain(std::iter::once(completion()))
            .collect();
        let create = (parameters <= 1).then(|| {
            (0..parameters)
                .map(|_| {
                    IrParameterIdentity::generated(
                        IrGeneratedParameterRole::SuspendLambdaCreateValue,
                        None,
                    )
                })
                .chain(std::iter::once(completion()))
                .collect()
        });
        let invoke = (0..=parameters)
            .map(|ordinal| {
                IrParameterIdentity::generated(
                    IrGeneratedParameterRole::FunctionInvokeValue {
                        ordinal: u32::try_from(ordinal)
                            .expect("a suspend lambda's arity fits its FunctionN"),
                    },
                    None,
                )
            })
            .collect();
        Self {
            constructor,
            create,
            invoke,
            captured_receivers,
            lifting_root,
        }
    }

    /// The recorded identities of `member`'s parameters.
    pub(crate) fn member(&self, member: SuspendLambdaMember) -> &[IrParameterIdentity] {
        match member {
            SuspendLambdaMember::Constructor => &self.constructor,
            SuspendLambdaMember::Create => self
                .create
                .as_deref()
                .expect("only a suspend lambda with a recorded create declares one"),
            SuspendLambdaMember::Invoke => &self.invoke,
        }
    }

    /// The recorded identities of `member`'s parameters, checked against the physical parameters
    /// the member's descriptor declares. A planner reads them only through here: a list that does
    /// not describe every physical parameter is an internal error, never a shorter attribute.
    pub(crate) fn physical(
        &self,
        member: SuspendLambdaMember,
        physical_parameters: usize,
    ) -> &[IrParameterIdentity] {
        let identities = self.member(member);
        assert_eq!(
            identities.len(),
            physical_parameters,
            "the recorded parameter identities of a suspend lambda's {member:?} must match its \
             physical descriptor arity"
        );
        identities
    }

    /// The captured values, in constructor order.
    pub(crate) fn captures(&self) -> &[IrParameterIdentity] {
        &self.constructor[..self.constructor.len() - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFile;
    use crate::jvm::method_parameters::suspend_lambda_member as method_parameters;
    use crate::jvm::parameter_names::suspend_lambda_member as local_variables;
    use crate::types::Ty;

    /// The typed `invoke` records its `FunctionN.invoke` roles by ordinal, the continuation
    /// included, and a lambda of two parameters records no `create`. The JVM spellings of those
    /// roles are tested beside their projection in `parameter_names`.
    #[test]
    fn invoke_records_function_invoke_roles_by_ordinal() {
        let lambda = SuspendLambdaParameters::new(Vec::new(), 2, Vec::new(), None);
        assert_eq!(
            lambda
                .member(SuspendLambdaMember::Invoke)
                .iter()
                .map(|identity| identity.role)
                .collect::<Vec<_>>(),
            (0..3)
                .map(|ordinal| IrParameterRole::Generated(
                    IrGeneratedParameterRole::FunctionInvokeValue { ordinal }
                ))
                .collect::<Vec<_>>()
        );
        assert!(lambda.create.is_none());
        let one = SuspendLambdaParameters::new(Vec::new(), 1, Vec::new(), None);
        assert_eq!(
            one.member(SuspendLambdaMember::Create)
                .iter()
                .map(|identity| identity.role)
                .collect::<Vec<_>>(),
            [
                IrParameterRole::Generated(IrGeneratedParameterRole::SuspendLambdaCreateValue),
                IrParameterRole::Generated(IrGeneratedParameterRole::Continuation),
            ]
        );
    }

    #[test]
    #[should_panic(
        expected = "the recorded parameter identities of a suspend lambda's Invoke must match its \
                    physical descriptor arity"
    )]
    fn method_parameters_reject_a_list_shorter_than_the_descriptor() {
        let lambda = SuspendLambdaParameters::new(Vec::new(), 1, Vec::new(), None);
        method_parameters(
            &IrFile::default(),
            &lambda,
            SuspendLambdaMember::Invoke,
            &[Ty::Int, Ty::Int, Ty::obj("Continuation")],
        );
    }

    #[test]
    #[should_panic(
        expected = "the recorded parameter identities of a suspend lambda's Constructor must match \
                    its physical descriptor arity"
    )]
    fn local_variables_reject_a_list_longer_than_the_descriptor() {
        let lambda = SuspendLambdaParameters::new(
            vec![IrParameterIdentity::captured_receiver(0)],
            0,
            vec![IrCapturedReceiver::Enclosing {
                classifier: crate::types::type_name("Outer"),
            }],
            None,
        );
        local_variables(
            &IrFile::default(),
            &lambda,
            SuspendLambdaMember::Constructor,
            &[Ty::obj("Continuation")],
        );
    }
}
