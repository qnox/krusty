//! The platform a compilation is for, as far as Kotlin source semantics depend on it.
//!
//! Every analysis request names one, and the backend that consumes the checked program must be
//! for the same one. It is separate from the semantic platform that supplies library declarations:
//! a Native build can still resolve against JVM-shaped library declarations, but its source is
//! checked under Native rules.

/// The platform whose Kotlin rules a source set is checked under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompilationTarget {
    Jvm,
    Js,
    WasmJs,
    WasmWasi,
    Native,
}

impl CompilationTarget {
    /// Whether a `value class` is inline only with `@JvmInline`. Kotlin/JVM asks for the annotation;
    /// JS, both Wasm targets and Native read a single-field `value class` as inline from the keyword
    /// alone, as kotlinc-js, kotlinc-wasm and kotlinc-native do.
    pub const fn inline_value_classes_require_jvm_inline(self) -> bool {
        match self {
            Self::Jvm => true,
            Self::Js | Self::WasmJs | Self::WasmWasi | Self::Native => false,
        }
    }
}
