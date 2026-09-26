//! The `crossinline`/`noinline` modifier a value parameter of an inline function wrote.
//!
//! Cross-phase declaration semantics, like [`crate::context_parameters`]: the parser and checked
//! FIR record it for a source declaration, the classpath provider for a compiled one, and common
//! IR, the metadata writer and the JVM inliner read it.

/// The inline modifier a value parameter wrote, the same fact for a source declaration and for a
/// classpath one (Kotlin metadata's `ValueParameter.flags`).
///
/// A `crossinline` lambda is inlined like any other, also into the objects and lambdas the body
/// hands it to, which the inliner regenerates for the call; it cannot return from the caller. A
/// `noinline` lambda is a real function object the body receives as a value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlineParameterModifier {
    /// Neither modifier, which is also what a parameter that is not function-typed has.
    #[default]
    None,
    Noinline,
    Crossinline,
}

impl InlineParameterModifier {
    /// Whether a local that a literal lambda for this parameter changes is `Ref`-boxed, as kotlinc's
    /// IR does. A `noinline` lambda is a closure. A `crossinline` one may be captured by an object
    /// or lambda the body builds, which keeps the `Ref` in its `$x$inlined` field; where the body
    /// invokes it directly the inliner expands it and the captured-vars pass unboxes the `Ref`
    /// again. A lambda for a parameter with neither modifier is always inlined, so it never boxes.
    pub const fn boxes_captures(self) -> bool {
        !matches!(self, InlineParameterModifier::None)
    }
}
