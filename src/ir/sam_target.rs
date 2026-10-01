//! The functional interface a SAM-converted lambda implements.

use crate::types::{Ty, TypeName};

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
    pub method_target: crate::fir::FirSamMethod,
    pub parameters: Vec<Ty>,
    pub result: Ty,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub context_count: u32,
    pub has_receiver: bool,
    pub suspend: bool,
    /// The method's primitive result replaces a non-primitive result it overrides.
    pub overrides_non_primitive_result: bool,
    /// A fun-interface conversion of a callable reference delegates equality/hashCode through
    /// Kotlin's `FunctionAdapter` contract. Ordinary lambdas remain identity objects.
    pub function_adapter: bool,
    /// The conversion wraps an existing function value: the lambda's single capture is that value
    /// and its implementation forwards to the value's `invoke`. A lambda literal converted directly
    /// implements the method with its own body instead.
    pub wraps_function_value: bool,
}
