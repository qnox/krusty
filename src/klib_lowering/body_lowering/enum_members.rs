//! Calls of an enum class's implicit `values()`, `valueOf(value)` and `entries` getter, which the
//! common enum realization of the materialized class answers, as it does for a source enum.

use super::*;
use crate::metadata::klib_ir::tree::KlibIrMemberAccess;

/// The implicit member of an enum class a call names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum ImplicitEnumMember {
    Values,
    ValueOf,
    Entries,
}

impl ImplicitEnumMember {
    /// The member whose semantic role the provider published, if it is one of these.
    pub(in super::super) fn of(role: Option<crate::types::SemanticCallRole>) -> Option<Self> {
        match role? {
            crate::types::SemanticCallRole::KotlinEnumValues => Some(Self::Values),
            crate::types::SemanticCallRole::KotlinEnumValueOf => Some(Self::ValueOf),
            crate::types::SemanticCallRole::KotlinEnumEntries => Some(Self::Entries),
            _ => None,
        }
    }
}

impl BodyLowering<'_> {
    /// A call of `member` of the materialized enum `classifier`: its common enum operation.
    pub(super) fn implicit_enum_member(
        &mut self,
        classifier: TypeName,
        member: ImplicitEnumMember,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if !access.type_arguments.is_empty() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an enum class's implicit member is called with type arguments".to_owned(),
            ));
        }
        let arguments = super::calls::arguments(access);
        let enum_ty = Ty::obj_name(classifier);
        let (expression, result) = match (member, arguments.as_slice()) {
            (ImplicitEnumMember::Values, []) => {
                (IrExpr::EnumValues { classifier }, Ty::array(enum_ty))
            }
            (ImplicitEnumMember::ValueOf, [Some(name)]) => {
                let arg = self.operand(*name, Ty::String)?;
                (
                    IrExpr::EnumValueOf {
                        classifier,
                        arg,
                        declaration: crate::ir::EnumValueOfDeclaration::Member,
                    },
                    enum_ty,
                )
            }
            (ImplicitEnumMember::Entries, []) => (
                IrExpr::EnumEntries { classifier },
                Ty::obj_args("kotlin/enums/EnumEntries", &[enum_ty]),
            ),
            _ => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an enum class's implicit member is called with another shape than its own"
                        .to_owned(),
                ))
            }
        };
        if result != ty {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an enum class's implicit member is read at another type than its own".to_owned(),
            ));
        }
        let expression = self.ir.add_expr(expression);
        Ok(self.typed(expression, ty))
    }
}
