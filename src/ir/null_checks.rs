//! Which not-null check an [`super::IrExpr::NotNullAssert`] is.

/// The origin of a not-null assertion, which decides the JVM intrinsic that checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NullCheck {
    /// The source's `operand!!`: `Intrinsics.checkNotNull(value)` over a duplicate of the value.
    Source,
    /// kotlinc's implicit not-null cast over a Java value committed to a declared non-null type,
    /// naming the callable that produced it (`getenv(...)`):
    /// `Intrinsics.checkNotNullExpressionValue(value, name)`.
    Named(String),
    /// The same implicit cast over a value it cannot name, such as a block's result. kotlinc keeps
    /// the value in a temporary and checks it with `Intrinsics.checkNotNull(value)`.
    Unnamed,
}

impl NullCheck {
    /// Whether kotlinc inserted the check rather than the source writing it; `-Xno-call-assertions`
    /// removes exactly these.
    pub fn is_implicit(&self) -> bool {
        !matches!(self, Self::Source)
    }
}
