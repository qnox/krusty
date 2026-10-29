//! The runtime declarations a native plugin's generated code calls.
//!
//! A plugin names each one by its declaration: the package or classifier that declares it, its
//! name, and its parameter and result types. At the frontend/backend handoff the compilation's
//! symbol source answers that query like any other, the declared signature selects exactly one
//! candidate, and the selected identity is frozen with the file's other dependency declarations.
//! Generated code then calls the identity, and the target realizes the call as it realizes every
//! checked call to a dependency: no owner, physical name or descriptor crosses the boundary.

use crate::ir::ExternalCallableId;
use crate::types::{Ty, TypeName};

/// A runtime function a plugin's generated code calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub declaration: RuntimeDeclaration,
    pub name: &'static str,
    /// The declared value parameters, without the dispatch receiver.
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Where a [`RuntimeFunction`] is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeDeclaration {
    /// A top-level function of this package.
    Package(TypeName),
    /// A member function this classifier declares.
    Member(TypeName),
}

/// One [`RuntimeFunction`] the compilation selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundRuntimeFunction {
    pub target: ExternalCallableId,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// The handoff's selection of the runtime declarations one file's plugins call.
pub trait PluginRuntime {
    /// Select `function` among the declarations the compilation sees, or say why none is selected.
    fn function(&mut self, function: &RuntimeFunction) -> Result<BoundRuntimeFunction, String>;

    /// Whether the compilation sees a declaration of `classifier`, which is how a plugin detects
    /// the runtime version it generates code for.
    fn declares_classifier(&self, classifier: TypeName) -> bool;
}

impl RuntimeFunction {
    /// `owner.name(Parameter, …): Result`, for a diagnostic.
    pub fn described(&self) -> String {
        let owner = match self.declaration {
            RuntimeDeclaration::Package(package) => package,
            RuntimeDeclaration::Member(owner) => owner,
        };
        let params = self
            .params
            .iter()
            .map(|param| param.source_name())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}.{}({params}): {}",
            owner.render().replace('/', "."),
            self.name,
            self.ret.source_name()
        )
    }
}
