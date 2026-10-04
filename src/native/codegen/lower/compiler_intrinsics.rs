//! Ordinary dependency-call forms of compiler-supplied operations.
//!
//! Source operators normally arrive as `IrIntrinsic`. A callable reference to the same selected
//! declaration arrives as an external call instead, retaining the provider's exact
//! compiler-intrinsic fact. This boundary consumes that identity; it never reconstructs one from
//! an owner or callable spelling.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Realize an exact provider intrinsic reached through an ordinary member call.
    ///
    /// Returns `None` only when the selected declaration has no operation handled here, allowing
    /// the caller to continue through ordinary dependency realization.
    pub(super) fn compiler_intrinsic_call(
        &mut self,
        intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
        receiver: Option<u32>,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        match intrinsic {
            Some(crate::backend::BackendCompilerIntrinsic::StringPlus) => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Some(Err("a malformed `String.plus`".to_string()));
                };
                Some(self.string_plus(receiver, *argument, ret))
            }
            Some(crate::backend::BackendCompilerIntrinsic::BooleanNot) => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Some(Err("a malformed `Boolean.not`".to_string()));
                };
                if self.type_of(receiver).map(Ty::non_null) != Some(Ty::Boolean) {
                    return Some(Err(
                        "a `Boolean.not` receiver with a non-Boolean type".to_string()
                    ));
                }
                Some(self.boolean_not(receiver, ret))
            }
            Some(crate::backend::BackendCompilerIntrinsic::StringGet) => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Some(Err("a malformed `String.get`".to_string()));
                };
                Some(self.string_get(receiver, *index, ret))
            }
            _ => None,
        }
    }
}

pub(super) fn runtime_member_role(
    compiler_intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
    semantic_role: Option<crate::backend::BackendSemanticCallRole>,
) -> Option<super::super::super::intrinsics::RuntimeMemberRole> {
    use super::super::super::intrinsics::RuntimeMemberRole;
    use crate::backend::{BackendCompilerIntrinsic, BackendSemanticCallRole};

    match (compiler_intrinsic, semantic_role) {
        (Some(BackendCompilerIntrinsic::NullableAnyToString), _)
        | (_, Some(BackendSemanticCallRole::KotlinAnyToString)) => {
            Some(RuntimeMemberRole::ToString)
        }
        (_, Some(BackendSemanticCallRole::KotlinAnyHashCode)) => Some(RuntimeMemberRole::HashCode),
        (
            _,
            Some(
                BackendSemanticCallRole::KotlinAnyEquals
                | BackendSemanticCallRole::KotlinComparableCompareTo
                | BackendSemanticCallRole::KotlinFunctionInvoke,
            ),
        ) => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_any_member_role_survives_a_roleless_physical_override() {
        assert_eq!(
            runtime_member_role(
                None,
                Some(crate::backend::BackendSemanticCallRole::KotlinAnyToString),
            ),
            Some(crate::native::intrinsics::RuntimeMemberRole::ToString)
        );
    }
}

pub(super) fn console_intrinsic(
    intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
) -> Option<super::super::super::intrinsics::ConsoleIntrinsic> {
    use super::super::super::intrinsics::ConsoleIntrinsic;
    use crate::backend::BackendCompilerIntrinsic;

    match intrinsic {
        Some(BackendCompilerIntrinsic::Print) => Some(ConsoleIntrinsic::Print),
        Some(BackendCompilerIntrinsic::Println) => Some(ConsoleIntrinsic::Println),
        _ => None,
    }
}
