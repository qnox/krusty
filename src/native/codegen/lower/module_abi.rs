//! The Native ABI of a declaration that one file of the module defines and another file uses.
//!
//! Each file is lowered to its own object without seeing any other file's lowering, so the file
//! that defines a declaration and the file that uses it must agree on its entry points from facts
//! both of them hold. That agreement is one plan per checked declaration identity, computed by one
//! function from the declaration's checked shape: the defining file builds that shape from its own
//! checked declaration, a using file from the module record common lowering copied for it, and
//! both read the same plan. The defining file defines exactly the entry points a supported plan
//! names, adapting its own layout to them; a using file calls them and adapts its use-site type.
//! A plan that is not supported is a decline in the using file and no export in the defining one,
//! for the same reason in both.

mod constructors;
mod properties;
