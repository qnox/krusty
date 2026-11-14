//! The two WebAssembly targets kotlinc offers, and what separates them.
//!
//! `wasm-js` and `wasm-wasi` compile Kotlin to the same module: one WasmGC object model, one
//! instruction selection, one calling convention. What differs is the HOST — what the module
//! imports to reach the outside world and what starts it. Under `wasm-js` a JavaScript loader
//! instantiates the module and supplies its imports; under `wasm-wasi` the module imports
//! `wasi_snapshot_preview1` and any WASI runtime starts it through `_start`. That difference is all
//! [`WasmTarget`] carries, so everything above the host boundary is written once.

/// A Kotlin/Wasm target, named as kotlinc's `-Xwasm-target` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WasmTarget {
    /// `wasm-js`: a module instantiated by a JavaScript host.
    Js,
    /// `wasm-wasi`: a module run by a WASI preview-1 host.
    Wasi,
}

impl WasmTarget {
    pub const ALL: [Self; 2] = [Self::Js, Self::Wasi];

    /// kotlinc's spelling, which is also the suffix of the target's standard library
    /// (`kotlin-stdlib-wasm-js.klib`) and the value a klib manifest's `wasm_targets` records.
    pub fn name(self) -> &'static str {
        match self {
            Self::Js => "wasm-js",
            Self::Wasi => "wasm-wasi",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }

    /// The token Kotlin's codegen test directives use for this target (`// IGNORE_BACKEND:
    /// WASM_JS`). `WASM` names both.
    pub fn backend_token(self) -> &'static str {
        match self {
            Self::Js => "WASM_JS",
            Self::Wasi => "WASM_WASI",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WasmTarget;

    #[test]
    fn targets_round_trip_through_kotlinc_spelling() {
        for target in WasmTarget::ALL {
            assert_eq!(WasmTarget::from_name(target.name()), Some(target));
        }
        assert_eq!(WasmTarget::from_name("wasm"), None);
    }
}
