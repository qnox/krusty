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
    /// The modifier a source parameter wrote. The parser keeps the two keywords apart; a parameter
    /// that wrote both is `noinline`, as Kotlin metadata reads it.
    pub const fn written(noinline: bool, crossinline: bool) -> Self {
        if noinline {
            InlineParameterModifier::Noinline
        } else if crossinline {
            InlineParameterModifier::Crossinline
        } else {
            InlineParameterModifier::None
        }
    }

    /// Whether a lambda for this parameter of an inline function runs in the caller's own frame,
    /// so that a `return` in it may leave the caller: kotlinc's `InlineStatus.returnAllowed`. A
    /// `noinline` lambda is a function object and a `crossinline` one may run inside an object the
    /// body builds, both outside the caller's frame.
    pub const fn runs_in_caller_frame(self) -> bool {
        matches!(self, InlineParameterModifier::None)
    }

    /// Whether a lambda for `parameter` of an inline function with these per-parameter
    /// `modifiers` runs in the caller's frame. A parameter the modifiers do not cover has no
    /// published fact, so it is not assumed to inline.
    pub fn runs_parameter_in_caller_frame(modifiers: &[Self], parameter: usize) -> bool {
        modifiers
            .get(parameter)
            .is_some_and(|modifier| modifier.runs_in_caller_frame())
    }

    /// Whether a local that a literal lambda for this parameter changes is `Ref`-boxed, as kotlinc's
    /// IR does. A `noinline` lambda is a closure. A `crossinline` one may be captured by an object
    /// or lambda the body builds, which keeps the `Ref` in its `$x$inlined` field; where the body
    /// invokes it directly the inliner expands it and the captured-vars pass unboxes the `Ref`
    /// again. A lambda for a parameter with neither modifier is always inlined, so it never boxes.
    pub const fn boxes_captures(self) -> bool {
        !matches!(self, InlineParameterModifier::None)
    }
}

/// How an inline expansion treats one physical parameter.
///
/// The checker publishes this with the parameter identity. Call-site expansion only consumes it:
/// it does not re-read the declaration index or decide from the argument's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineExpansionMode {
    /// A dispatch or extension receiver. A lambda passed here is a value.
    Receiver,
    /// A non-null function parameter that did not write `noinline`. Its lambda is spliced.
    Splice,
    /// Every other parameter: `noinline`, a nullable or aliased-away non-function type, a type
    /// parameter, `Any`, or a capture.
    Materialize,
}

impl InlineExpansionMode {
    /// `is_non_null_function` is the resolved declaration type, after typealias expansion, and is
    /// false for `T?` even when `T` is a function type.
    pub const fn declared(
        is_receiver: bool,
        is_non_null_function: bool,
        modifier: InlineParameterModifier,
    ) -> Self {
        if is_receiver {
            Self::Receiver
        } else if matches!(modifier, InlineParameterModifier::Noinline) || !is_non_null_function {
            Self::Materialize
        } else {
            Self::Splice
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InlineExpansionMode, InlineParameterModifier};

    #[test]
    fn a_non_null_function_parameter_splices_unless_it_wrote_noinline() {
        assert_eq!(
            InlineExpansionMode::declared(false, true, InlineParameterModifier::None),
            InlineExpansionMode::Splice
        );
        assert_eq!(
            InlineExpansionMode::declared(false, true, InlineParameterModifier::Crossinline),
            InlineExpansionMode::Splice
        );
        assert_eq!(
            InlineExpansionMode::declared(false, true, InlineParameterModifier::Noinline),
            InlineExpansionMode::Materialize
        );
    }

    #[test]
    fn a_receiver_or_non_function_parameter_is_not_spliced() {
        assert_eq!(
            InlineExpansionMode::declared(true, true, InlineParameterModifier::None),
            InlineExpansionMode::Receiver
        );
        assert_eq!(
            InlineExpansionMode::declared(false, false, InlineParameterModifier::None),
            InlineExpansionMode::Materialize
        );
    }
}
