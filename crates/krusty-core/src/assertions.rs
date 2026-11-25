//! Compile-time policy for Kotlin's `assert` operation, shared by the language-feature table
//! that selects it and the phases that realize it.

/// Compile-time policy for Kotlin's `assert` operation. Every backend must guard, retain, or elide
/// the checked operation while preserving lazy condition/message evaluation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AssertionMode {
    #[default]
    Runtime,
    AlwaysEnabled,
    AlwaysDisabled,
}

/// How `assert(...)` compiles: kotlinc's `-Xassertions` mode, or a platform's default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssertionCompilation {
    /// kotlinc's JVM default (`-Xassertions=legacy`): `assert` is no compiler operation. The call
    /// stays an ordinary call of the library's inline function, whose body evaluates the condition
    /// and then reads `kotlin._Assertions.ENABLED`.
    Legacy,
    /// The checked compiler operation, under this mode.
    Checked(AssertionMode),
}
