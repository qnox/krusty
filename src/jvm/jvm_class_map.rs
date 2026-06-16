//! Kotlin built-in class → JVM class mapping.
//!
//! This is a faithful port of the reference Kotlin compiler's `JavaToKotlinClassMap`:
//!   <kotlin>/core/compiler.common.jvm/src/org/jetbrains/kotlin/builtins/jvm/JavaToKotlinClassMap.kt
//! (the `init {}` block: `addTopLevel(...)` for top-level mapped types and `mutabilityMappings`
//! for the collection read-only/mutable pairs). In the reference compiler the JVM side is obtained
//! via JDK reflection (`Comparable::class.java` etc.); the resulting `kotlin.X → java/...X` pairs
//! are reproduced here as data so krusty resolves them without a JDK class on the classpath.
//!
//! These are NOT stdlib typealiases (those — `Exception`, `RuntimeException`, … — live in
//! `*TypeAliasesKt` `@Metadata` and are read from the classpath by `classpath::scan_types`). They
//! are intrinsic to the compiler, so they are seeded unconditionally.

/// Every simple name handled by [`kotlin_builtin_to_jvm`], used to seed the resolver's class map.
pub const BUILTIN_MAPPED_NAMES: &[&str] = &[
    "Any", "String", "CharSequence", "Throwable", "Cloneable", "Number", "Comparable", "Enum",
    "Annotation", "Iterable", "MutableIterable", "Iterator", "MutableIterator", "Collection",
    "MutableCollection", "List", "MutableList", "Set", "MutableSet", "ListIterator",
    "MutableListIterator", "Map", "MutableMap", "Nothing",
];

/// Map a Kotlin built-in type's **simple name** to its JVM internal name, mirroring
/// `JavaToKotlinClassMap`'s `kotlinToJava` direction. `None` if the name is not a mapped built-in.
///
/// Mutable collection variants (`MutableList`, …) map to the same JVM interface as their read-only
/// counterpart, exactly as in the reference `addMapping`.
pub fn kotlin_builtin_to_jvm(simple: &str) -> Option<&'static str> {
    Some(match simple {
        // addTopLevel(...) — top-level mapped types (java class via `X::class.java`).
        "Any" => "java/lang/Object",
        "String" => "java/lang/String",
        "CharSequence" => "java/lang/CharSequence",
        "Throwable" => "java/lang/Throwable",
        "Cloneable" => "java/lang/Cloneable",
        "Number" => "java/lang/Number",
        "Comparable" => "java/lang/Comparable",
        "Enum" => "java/lang/Enum",
        "Annotation" => "java/lang/annotation/Annotation",
        // mutabilityMappings — read-only Kotlin collection → JVM interface.
        "Iterable" | "MutableIterable" => "java/lang/Iterable",
        "Iterator" | "MutableIterator" => "java/util/Iterator",
        "Collection" | "MutableCollection" => "java/util/Collection",
        "List" | "MutableList" => "java/util/List",
        "Set" | "MutableSet" => "java/util/Set",
        "ListIterator" | "MutableListIterator" => "java/util/ListIterator",
        "Map" | "MutableMap" => "java/util/Map",
        // addKotlinToJava(FqNames.nothing, Void)
        "Nothing" => "java/lang/Void",
        _ => return None,
    })
}

/// Whether a resolved JVM internal name denotes a `Throwable` subtype, recognised structurally by
/// the JDK naming convention (`…Exception`/`…Error`, or `java/lang/Throwable` itself). Used only to
/// admit the no-arg / single-`String` constructor shapes every JDK throwable provides — the type
/// itself is resolved from the classpath, not from a hardcoded enumeration.
pub fn is_throwable_internal(internal: &str) -> bool {
    internal == "java/lang/Throwable"
        || internal.ends_with("Exception")
        || internal.ends_with("Error")
}
