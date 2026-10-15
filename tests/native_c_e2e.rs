//! Native's C ABI surface: Kotlin exports and provider-authenticated cinterop imports.

use super::common;

#[path = "native_c_abi_e2e.rs"]
mod abi;
#[path = "native_cinterop_e2e.rs"]
mod interop;
