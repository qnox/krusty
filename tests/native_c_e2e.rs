//! Native's C ABI surface: Kotlin exports and provider-authenticated cinterop imports.

use super::common;

#[path = "native_c_e2e/abi.rs"]
mod abi;
#[path = "native_c_e2e/interop.rs"]
mod interop;
