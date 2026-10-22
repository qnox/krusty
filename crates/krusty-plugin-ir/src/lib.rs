//! The IR krusty hands kotlinc compiler plugins, as plain data: a module's own declarations in
//! kotlinc's IR shape ([`ModuleIr`]), the identities of the dependency declarations they use
//! ([`CallableIdentity`]), and the declarations a KSP processor reads in a round ([`KspRound`]).
//! krusty's compiler builds it from checked code; `krusty-plugin-host` frames it for the JVM host that
//! runs the plugins. It has no dependencies, so the compiler takes none from the host.

mod identity;
mod ksp;
mod module;

pub use identity::{
    CallableIdentity, CallableKind, ClassName, Classifier, TypeArgument, TypeIdentity,
    TypeParameterOwner,
};
pub use ksp::{
    KspAccessor, KspAnnotation, KspAnnotationArguments, KspArgument, KspClass, KspClassKind,
    KspClasspath, KspDeclaration, KspFile, KspFunction, KspFunctionKind, KspLibraryClass,
    KspModifier, KspOrigin, KspParameter, KspProperty, KspRound, KspType, KspTypeAlias,
    KspTypeArgument, KspTypeDeclaration, KspTypeParameter, KspTypeSlot, KspUseSite, KspValue,
    KspVariance,
};
pub use module::{
    BodyIr, CallIr, Callee, Constant, ExpressionIr, ExpressionKind, FileIr, FunctionIr, ModuleIr,
    ParameterIr, Span, StatementIr, ValueId, VariableIr, Visibility,
};
