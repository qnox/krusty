use crate::fir::{
    CallableId, ControlTargetId, FirExprId, FirLocalCallableRef, FirStatementId, LocalValueId,
    OriginId, PropertyId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirLoweringFailure {
    MissingExpression(FirExprId),
    MissingStatement(FirStatementId),
    RecursiveExpression(FirExprId),
    RecursiveStatement(FirStatementId),
    MissingControlTarget {
        target: ControlTargetId,
        target_depth: u32,
    },
    MissingBodyLocalCallable(crate::fir::BodyOwnerId),
    MalformedDestructureLowering {
        origin: OriginId,
    },
    UnsupportedConversion {
        origin: OriginId,
    },
    InvalidIntegerConstant {
        origin: OriginId,
    },
    InvalidCatchType {
        origin: OriginId,
    },
    InvalidRangeCounter {
        origin: OriginId,
        ty: crate::types::Ty,
    },
    /// A reversed or stepped loop progression whose last bound has no inclusive form, which the
    /// checker only accepts from `until` and `..<`.
    IrreversibleProgression,
    MissingCallable(CallableId),
    UnsupportedCallableReference(CallableId),
    UnsupportedExternalCallableReference(crate::fir::ExternalCallableId),
    UnsupportedClassifierCallableReference(crate::types::TypeName),
    UnsupportedExternalCall(crate::fir::ExternalCallableId),
    UnsupportedExternalProperty(crate::fir::ExternalPropertyId),
    UnsupportedExternalConstructor(crate::fir::ExternalCallableId),
    UnsupportedModuleConstructor(CallableId),
    UnsupportedIntrinsicCall,
    MissingProperty(PropertyId),
    MissingLocalClass(crate::fir::DeclarationId),
    InvalidConstructorCapture {
        owner: crate::fir::DeclarationId,
        field: u32,
    },
    InvalidConstructorCaptureArgument(FirExprId),
    MissingEnumClassifier(crate::types::TypeName),
    UnsupportedPropertyReferenceTarget,
    MissingParameter {
        target: CallableId,
        parameter: u32,
    },
    MissingExternalParameter {
        parameter: u32,
    },
    MissingImplicitReceiver {
        origin: OriginId,
    },
    MissingWhenSubject {
        origin: OriginId,
    },
    MissingLocalCallable(FirLocalCallableRef),
    UnsupportedLocalCallableReference(FirLocalCallableRef),
    MissingLocalDefault {
        function: crate::ir::FunId,
        parameter: u32,
    },
    MissingCapture {
        enclosing_depth: u32,
        source: crate::fir::FirCaptureSource,
    },
    UnsharedCaptureWrite {
        origin: OriginId,
        enclosing_depth: u32,
        source: LocalValueId,
    },
    MissingBodyResult {
        origin: OriginId,
    },
    /// A lifted local function whose physical parameter list is SHORTER than the capture prefix
    /// `BodySlots` reports. That is an invalid checked shape, not a function with no logical
    /// parameters, so the tail-call frame refuses rather than counting zero of them.
    MalformedLocalFrame {
        function: crate::ir::FunId,
        parameters: usize,
        first_parameter: u32,
    },
    ValueIdentityOverflow,
}
