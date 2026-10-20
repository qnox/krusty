//! The inlining obligation a callable carries from its declaration to the backend.

/// How a callable relates to bytecode inlining — the single state that replaces the old
/// `inline` + `inline_only`/`must_inline` boolean pairs (one per layer: [`FnFlags`],
/// [`LibraryCallable`], and `ir::Callee::Static`). Ordered weakest→strongest; the splice obligation
/// strengthens as you go down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InlineKind {
    /// Not an `inline` function — emit an ordinary call (`invokestatic`/`invokevirtual`).
    #[default]
    None,
    /// A Kotlin `inline` function (per its `@Metadata`): the JVM backend MAY splice its compiled body
    /// at the call site, but a real call is a legal fallback (the callee is a public method).
    CanInline,
    /// No legal direct-call fallback. This includes a NON-PUBLIC `@InlineOnly` function
    /// (`require`/`check`/`error`/`let`/…) whose method is inaccessible and a reified source body
    /// whose erased method may exist only to publish inline code. The backend MUST splice the body;
    /// a failed splice skips the whole file rather than emitting an inaccessible or unspecialized
    /// `invokestatic`.
    MustInline,
}

impl InlineKind {
    /// Build from the legacy `(inline, must_inline)` boolean pair. `must_inline` is the stronger
    /// signal (no callable fallback), so it wins regardless of the `inline` bit — which the `@Metadata`
    /// `inline` flag can read back as `false` for a `@JvmName`-mangled private callee even though it
    /// must still be spliced.
    pub fn from_flags(inline: bool, must_inline: bool) -> InlineKind {
        if must_inline {
            InlineKind::MustInline
        } else if inline {
            InlineKind::CanInline
        } else {
            InlineKind::None
        }
    }
    /// True when the backend may attempt to splice the body (`inline` OR `@InlineOnly`).
    pub fn can_inline(self) -> bool {
        self != InlineKind::None
    }
    /// True when splicing is mandatory — the callee has no legal call site to fall back to.
    pub fn must_inline(self) -> bool {
        self == InlineKind::MustInline
    }
}
