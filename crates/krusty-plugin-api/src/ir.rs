//! A module's own declarations in kotlinc's IR shape ([`ModuleIr`]) and the identities of the
//! dependency declarations they use ([`CallableIdentity`]), as plain data. krusty builds it from
//! checked code for a plugin that transforms IR, and reads back the module the plugin leaves.

mod identity;
mod module;

pub use identity::{
    CallableIdentity, CallableKind, ClassName, Classifier, TypeArgument, TypeIdentity,
    TypeParameterOwner,
};
pub use module::{
    BodyIr, CallIr, Callee, Constant, ExpressionIr, ExpressionKind, FileIr, FunctionIr, ModuleIr,
    ParameterIr, Span, StatementIr, ValueId, VariableIr, Visibility,
};
