//! The functional interface a SAM-converted lambda implements.

use crate::types::{Ty, TypeName};

/// Stable declaration identity of the abstract method a SAM conversion implements.
///
/// FIR chooses the method; lowering translates that choice once into this IR boundary contract so
/// a backend never imports the frontend SAM node or reselects the method from its spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSamMethod {
    Module(crate::fir::CallableId),
    External(crate::fir::ExternalCallableId),
    /// The `invoke` inherited from a function-type supertype.
    FunctionTypeInvoke,
}

/// Checked functional-interface target attached to a lambda after SAM conversion.
///
/// Both the call-site-specialized shape and the declaration shape are retained: the former types
/// the implementation while the latter determines the platform method that the closure implements.
/// This is frontend semantic data; platform owner spellings and descriptors do not belong here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSamTarget {
    pub classifier: TypeName,
    /// The abstract method's name, for a target that implements it by name.
    pub method: String,
    /// The abstract method the conversion implements, as the checker selected it. A target that
    /// implements the interface by declaration identity reads this, never the name.
    pub method_target: IrSamMethod,
    pub parameters: Vec<Ty>,
    /// Parameters specialized through a contravariant use-site capture. A target backend may use
    /// this semantic provenance to realize an adapter; common IR retains the logical types above.
    pub contravariant_parameters: Vec<bool>,
    pub result: Ty,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// The converted value's own callable view is `suspend`. A non-suspend value adapted to a
    /// suspend method is stored as that value's `FunctionN`; the continuation stays on the method.
    pub source_suspend: bool,
    /// The selected declaration requires the object-method dispatch path, not an ordinary slot.
    pub requires_object_dispatch: bool,
    /// The distinct own results of the declarations the method overrides, unspecialized. A target
    /// whose method result or bridges depend on the overridden results realizes them from these.
    pub overridden_results: Vec<Ty>,
    /// Exact declarations in the implemented override family, including inherited aliases.
    pub dispatch_methods: Vec<IrSamMethodFrame>,
    /// Effective inherited default declarations; header evidence alone does not call their bodies.
    pub default_methods: Vec<IrSamMethodFrame>,
    /// A fun-interface conversion of a callable reference delegates equality/hashCode through
    /// Kotlin's `FunctionAdapter` contract. Ordinary lambdas remain identity objects.
    pub function_adapter: bool,
    /// The conversion wraps an existing function value (neither a lambda literal nor a callable
    /// reference): the lambda's single capture is that value and its implementation forwards to
    /// the value's `invoke`.
    pub wraps_function_value: bool,
    /// A nullable function value converts conditionally: `null` remains `null`, and only a
    /// non-null function is wrapped. A lambda literal is never null, so its target leaves this
    /// false; the flag belongs to the function-value adapter, whose single capture is that value.
    pub nullable: bool,
    /// The interface is a Kotlin declaration, not a Java one.
    pub kotlin_interface: bool,
    /// Semantic identities parallel to `declared_parameters`. Targets format source and generated
    /// roles for their own metadata/debug ABI; common IR never reconstructs them from spelling.
    pub parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    /// The interface's own type parameters in declaration order, which the declared shape names.
    /// A target that writes a class implementing the interface declares them on that class.
    pub type_parameters: Vec<IrSamTypeParameter>,
    /// The declaration whose code holds the conversion, where a target that writes a class for
    /// it places that class.
    pub placement: IrSamPlacement,
}

/// Where the code holding a SAM conversion stands after inlining: Kotlin places the class a
/// conversion compiles to in the class whose member holds it, or at its file's level. A target
/// that names that class (kotlinc-wasm's `pkg.Owner.sam$pkg_Interface$0`) formats it from this.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrSamPlacement {
    /// Code of the file being lowered, inlined copies included: the classifier owning the
    /// expression ([`super::IrFile::expression_owners`]), or else this file's package.
    ThisFile,
    /// A dependency's code at the level of a file declaring `package`, a package name-tree node
    /// ([`TypeName::ROOT`] for the root package).
    DependencyFile { package: TypeName },
    /// A dependency's code in a member of its classifier `owner`.
    DependencyMember { owner: TypeName },
    /// A dependency's code in a member of a class private to its file, which the dependency does
    /// not name to this compilation.
    UnnamedDependencyClass,
}

/// An exact checked declaration and the unspecialized frame its interface slot carries.
/// `None` retains unavailable declaration evidence rather than guessing a nominal member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSamMethodFrame {
    pub method: Option<IrSamMethod>,
    pub parameters: Vec<Ty>,
    pub result: Ty,
}

/// One type parameter a functional interface declares: its semantic identity, and each declared
/// upper bound with whether that bound is an interface type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrSamTypeParameter {
    pub name: String,
    pub bounds: Vec<(Ty, bool)>,
}

impl IrSamTypeParameter {
    /// The checked conversion's type parameter, carried into common IR.
    pub(crate) fn lowered(parameter: &crate::fir::FirSamTypeParameter) -> Self {
        Self {
            name: parameter.name.to_string(),
            bounds: parameter
                .bounds
                .iter()
                .map(|bound| (bound.ty.get(), bound.is_interface))
                .collect(),
        }
    }
}
