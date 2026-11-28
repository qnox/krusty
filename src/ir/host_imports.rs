//! Dependency functions a target imports from its host instead of defining.

/// The host function a bodyless dependency function is: its import module and name. Kotlin/Wasm
/// `@WasmImport` declarations become these, and a support unit declares them without a body.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IrHostImport {
    pub module: String,
    pub name: String,
}
