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
    pub result: Ty,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// The converted value's own callable view is `suspend`. A non-suspend value adapted to a
    /// suspend method is stored as that value's `FunctionN`; the continuation stays on the method.
    pub source_suspend: bool,
    /// The method's primitive result replaces a non-primitive result it overrides.
    pub overrides_non_primitive_result: bool,
    /// Specialized semantic result contracts whose target bridges reach that primitive method.
    pub overridden_non_primitive_results: Vec<Ty>,
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
}
