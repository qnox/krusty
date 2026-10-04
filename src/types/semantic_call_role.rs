/// Semantic role attached by a declaration provider after exact callable selection. A target may
/// consume it when the selected language member has no ordinary dispatch on a chosen
/// representation. The role belongs to the checked cross-phase type contract; it is not provider
/// or target metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticCallRole {
    KotlinAnyEquals,
    KotlinAnyHashCode,
    KotlinAnyToString,
    /// The single comparison operation declared by `kotlin.Comparable`.
    KotlinComparableCompareTo,
    /// The invocation operation declared by a compiler-provided Kotlin function classifier.
    KotlinFunctionInvoke,
    /// The `name` property declared by Kotlin's callable-reflection contract.
    KotlinCallableReferenceName,
    /// A property-reference read with this many unbound receivers.
    KotlinPropertyReferenceGet(u8),
    /// A mutable property-reference write with this many unbound receivers.
    KotlinPropertyReferenceSet(u8),
    /// The standard property-reference delegation read with this many forwarded receivers.
    KotlinPropertyReferenceDelegateGet(u8),
    /// The standard property-reference delegation write with this many forwarded receivers.
    KotlinPropertyReferenceDelegateSet(u8),
}
