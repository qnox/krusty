//! Heap-payload accounting for a checked expression and its retained semantic payload.

use super::*;

impl FirExprKind {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        match self {
            FirExprKind::Constant(FirConstant::String(value)) => value.len_utf16() * 2,
            FirExprKind::Constant(
                FirConstant::Int(_)
                | FirConstant::Long(_)
                | FirConstant::UInt(_)
                | FirConstant::ULong(_)
                | FirConstant::Double(_)
                | FirConstant::Float(_)
                | FirConstant::Boolean(_)
                | FirConstant::Char(_)
                | FirConstant::Null,
            )
            | FirExprKind::ImplicitReceiver { .. }
            | FirExprKind::SingletonValue { .. }
            | FirExprKind::EnumEntry { .. }
            | FirExprKind::ClassifierPropertyRead { .. }
            | FirExprKind::ValueRead(_)
            | FirExprKind::ForwardedSuperArgument { .. }
            | FirExprKind::CapturedValueRead { .. }
            | FirExprKind::ClassStorageRead { .. }
            | FirExprKind::ConstructorCaptureRead { .. }
            | FirExprKind::ConstructorContextRead { .. }
            | FirExprKind::ClassStorageSharedRead { .. }
            | FirExprKind::ClassStorageSharedWrite { .. }
            | FirExprKind::ConstructorCaptureSharedWrite { .. }
            | FirExprKind::EnclosingClassStorageRead { .. }
            | FirExprKind::LateinitFieldRead { .. }
            | FirExprKind::CapturedValueWrite { .. }
            | FirExprKind::ValueWrite { .. }
            | FirExprKind::TypeOperation { .. }
            | FirExprKind::ImplicitConversion { .. }
            | FirExprKind::Unary { .. }
            | FirExprKind::Binary { .. }
            | FirExprKind::Equality { .. }
            | FirExprKind::NullablePrimitiveComparison { .. }
            | FirExprKind::NullableNumericComparison { .. }
            | FirExprKind::Range { .. }
            | FirExprKind::InRange { .. }
            | FirExprKind::IndicesContainment { .. }
            | FirExprKind::SafeCall { .. }
            | FirExprKind::Elvis { .. }
            | FirExprKind::Throw(_)
            | FirExprKind::Jump { .. }
            | FirExprKind::Conditional { .. } => 0,
            FirExprKind::LateinitRead { name, .. } => name.len(),
            FirExprKind::EnclosingReceiver { path }
            | FirExprKind::CapturedImplicitReceiver { path, .. }
            | FirExprKind::CapturedClassStorageRead { path, .. }
            | FirExprKind::CapturedClassStorageSharedWrite { path, .. } => {
                path.len() * std::mem::size_of::<DeclarationId>()
            }
            FirExprKind::ClassLiteral { .. } => 0,
            FirExprKind::LocalPropertyReference { name, .. } => name.len(),
            FirExprKind::LocalDelegateAccess { .. } => 0,
            FirExprKind::AnnotationArray(expressions)
            | FirExprKind::StringTemplate(expressions) => {
                expressions.len() * std::mem::size_of::<FirExprId>()
            }
            FirExprKind::ArrayLiteral { elements, .. } => {
                elements.len() * std::mem::size_of::<FirArrayElement>()
            }
            FirExprKind::ArrayConstruction { .. } => 0,
            FirExprKind::PropertyRead {
                target,
                context_arguments,
                substitutions,
                ..
            }
            | FirExprKind::PropertyWrite {
                target,
                context_arguments,
                substitutions,
                ..
            } => {
                target.storage_payload_bytes()
                    + context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
            }
            FirExprKind::BackingFieldRead { .. } | FirExprKind::BackingFieldWrite { .. } => 0,
            FirExprKind::PluginExpression {
                data,
                types,
                operands,
                ..
            } => {
                data.len() * std::mem::size_of::<TypeName>()
                    + types.len() * std::mem::size_of::<ResolvedTy>()
                    + operands.len() * std::mem::size_of::<FirPluginOperand>()
            }
            FirExprKind::FunctionInvokeReference {
                target_parameters,
                reference_parameters,
                ..
            } => {
                (target_parameters.len() + reference_parameters.len())
                    * std::mem::size_of::<ResolvedTy>()
            }
            FirExprKind::CallableReference {
                target,
                substitutions,
                adaptation,
                ..
            } => {
                target.storage_payload_bytes()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
                    + adaptation.as_deref().map_or(0, |adaptation| {
                        std::mem::size_of::<FirReferenceAdaptation>()
                            + adaptation.storage_payload_bytes()
                    })
            }
            FirExprKind::PropertyReference {
                target,
                context_arguments,
                substitutions,
                adaptation,
                ..
            } => {
                target.storage_payload_bytes()
                    + context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + substitutions.len() * std::mem::size_of::<FirTypeSubstitution>()
                    + adaptation.as_deref().map_or(0, |adaptation| {
                        std::mem::size_of::<FirReferenceAdaptation>()
                            + adaptation.storage_payload_bytes()
                    })
            }
            FirExprKind::LocalCallableReference {
                context_arguments,
                adaptation,
                ..
            } => {
                context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + adaptation.as_deref().map_or(0, |adaptation| {
                        std::mem::size_of::<FirReferenceAdaptation>()
                            + adaptation.storage_payload_bytes()
                    })
            }
            FirExprKind::Call(call) => call.storage_payload_bytes(),
            FirExprKind::ConstructorCall(call) => call.storage_payload_bytes(),
            FirExprKind::AnonymousObject(object) => {
                object.captures.len() * std::mem::size_of::<FirLocalClassCapture>()
                    + object
                        .captures
                        .iter()
                        .map(FirLocalClassCapture::storage_payload_bytes)
                        .sum::<usize>()
                    + std::mem::size_of_val(object.capture_order.as_ref())
                    + object.delegate_arguments.len()
                        * std::mem::size_of::<FirInterfaceDelegateArgument>()
                    + object.super_arguments.len()
                        * std::mem::size_of::<FirAnonymousSuperArgument>()
            }
            FirExprKind::LocalCall {
                arguments,
                parameter_types,
                ..
            } => {
                arguments.len() * std::mem::size_of::<FirCallArgument>()
                    + arguments
                        .iter()
                        .map(FirCallArgument::storage_payload_bytes)
                        .sum::<usize>()
                    + parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            }
            FirExprKind::FunctionInvoke {
                context_arguments,
                arguments,
                parameter_types,
                ..
            } => {
                context_arguments.len() * std::mem::size_of::<FirReceiver>()
                    + arguments.len() * std::mem::size_of::<FirCallArgument>()
                    + arguments
                        .iter()
                        .map(FirCallArgument::storage_payload_bytes)
                        .sum::<usize>()
                    + parameter_types.len() * std::mem::size_of::<ResolvedTy>()
            }
            FirExprKind::ExtensionFunctionBinding {
                target_parameters, ..
            } => target_parameters.len() * std::mem::size_of::<ResolvedTy>(),
            FirExprKind::ComparisonCall { call, .. }
            | FirExprKind::ContainmentCall { call, .. }
            | FirExprKind::ComparableRangeContainment { lower: call, .. } => {
                call.storage_payload_bytes()
            }
            FirExprKind::Lambda {
                type_parameters,
                body,
                ..
            } => {
                type_parameters.len() * std::mem::size_of::<TypeParameterId>()
                    + std::mem::size_of::<FirBody>()
                    + body.storage_payload_bytes()
            }
            FirExprKind::Try { catches, .. } => catches.len() * std::mem::size_of::<FirCatch>(),
            FirExprKind::When { branches, .. } => {
                branches.len() * std::mem::size_of::<FirWhenBranch>()
                    + branches
                        .iter()
                        .map(|branch| {
                            branch.conditions.len() * std::mem::size_of::<FirWhenCondition>()
                        })
                        .sum::<usize>()
            }
            FirExprKind::Block { statements, .. } => {
                statements.len() * std::mem::size_of::<FirStatementId>()
            }
        }
    }
}
