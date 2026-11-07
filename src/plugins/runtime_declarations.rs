//! The runtime declarations a native plugin's generated code calls.
//!
//! A plugin names each one by its declaration: the package or classifier that declares it, its
//! name, and its parameter and result types. At the frontend/backend handoff the compilation's
//! symbol source answers that query like any other, the declared signature selects exactly one
//! candidate, and the selected identity is frozen with the file's other dependency declarations.
//! Generated code then calls the identity, and the target realizes the call as it realizes every
//! checked call to a dependency: no owner, physical name or descriptor crosses the boundary.

use crate::ir::ExternalCallableId;
use crate::types::{ClassifierDeclarationKind, ResolvedAnnotation, Ty, TypeName};

/// A runtime function a plugin's generated code calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFunction {
    pub declaration: RuntimeDeclaration,
    pub name: &'static str,
    /// The declared value parameters, without the dispatch receiver.
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// The parameters the plugin's own transform appends to the declaration when it compiles it,
    /// which every call passes after the declared ones: Compose's `$composer` and `$changed` for a
    /// composable runtime function. The target checks them against the compiled declaration.
    pub appended: Vec<Ty>,
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

    /// The declaration of `classifier` the compilation sees, as its provider normalized it. A plugin
    /// detects the runtime version it generates code for by whether a runtime classifier exists.
    fn classifier(&self, classifier: TypeName) -> Option<PluginClassifier>;

    /// Record that the plugin's own transform appended `appended` to the dependency declaration
    /// `target` when its library was compiled, so the calls the plugin rewrote pass them after the
    /// declared arguments. The target checks them against the compiled declaration.
    fn appended_parameters(&mut self, target: ExternalCallableId, appended: Vec<Ty>);
}

/// The facts a plugin reads about a classifier the file does not declare, normalized at the
/// provider boundary from source, Kotlin metadata or a Java class file alike.
#[derive(Clone, Debug)]
pub struct PluginClassifier {
    pub kind: ClassifierDeclarationKind,
    /// A Kotlin declaration, including a Kotlin builtin its provider realizes by a platform class
    /// (`kotlin/collections/List`); `false` for a Java classifier.
    pub is_kotlin: bool,
    /// Neither `open`, `abstract` nor `sealed`.
    pub is_final: bool,
    /// A value class's declaration: its underlying type over its own type parameters.
    pub value_class: Option<crate::types::DeclaredValueClass>,
    /// Declared by another file of the current compilation module.
    pub in_module: bool,
    /// An `inner` class, which captures an instance of its outer class.
    pub is_inner: bool,
    /// How many type parameters the classifier itself declares.
    pub type_parameters: usize,
    /// Its direct supertypes.
    pub supertypes: Vec<TypeName>,
    /// The annotations applied to it, with their arguments.
    pub annotations: Vec<ResolvedAnnotation>,
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
