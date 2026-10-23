//! The IR krusty hands kotlinc compiler plugins, as plain data: a module's own declarations in
//! kotlinc's IR shape ([`ModuleIr`]), the identities of the dependency declarations they use
//! ([`CallableIdentity`]), and the declarations a KSP processor reads in a round ([`KspRound`]).
//! krusty's compiler builds it from checked code; `krusty-plugin-host` frames it for the JVM host that
//! runs the plugins. It has no dependencies, so the compiler takes none from the host.

// Cargo otherwise accepts a sibling checkout's older source mtimes as proof that its typed model
// is unchanged. Track the workspace's existing identity in dep-info; standalone consumers need
// not define it, and repeated builds of the same checkout remain cached.
const _: Option<&str> = option_env!("KRUSTY_BUILD_CHECKOUT");

mod identity;
mod ksp;
mod module;

pub use identity::{
    CallableIdentity, CallableKind, ClassName, Classifier, TypeArgument, TypeIdentity,
    TypeParameterOwner,
};
pub use ksp::{
    KspAnnotation, KspClass, KspClassKind, KspDeclaration, KspFile, KspFunction, KspFunctionKind,
    KspModifier, KspParameter, KspProperty, KspRound, KspType, KspTypeAlias, KspTypeArgument,
    KspTypeDeclaration, KspTypeParameter, KspTypeSlot, KspVariance,
};
pub use module::{
    BodyIr, CallIr, Callee, Constant, ExpressionIr, ExpressionKind, FileIr, FunctionIr, ModuleIr,
    ParameterIr, Span, StatementIr, ValueId, VariableIr, Visibility,
};
