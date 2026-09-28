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
    pub method: String,
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
}
