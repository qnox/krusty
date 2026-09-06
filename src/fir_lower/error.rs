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
        source: LocalValueId,
    },
    UnsharedCaptureWrite {
        origin: OriginId,
        enclosing_depth: u32,
        source: LocalValueId,
    },
    MissingBodyResult {
        origin: OriginId,
    },
    ValueIdentityOverflow,
}
