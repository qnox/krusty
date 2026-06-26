//! Kotlin built-in class → JVM class mapping, plus the canonical JVM internal names / descriptors
//! the rest of the compiler must occasionally materialize. The front end speaks Kotlin types; every
//! `java/lang/…` name lives here (the JVM "part") rather than being spelled across the core.
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
    "Any",
    "String",
    "CharSequence",
    "Throwable",
    "Cloneable",
    "Number",
    "Comparable",
    "Enum",
    "Annotation",
    "Iterable",
    "MutableIterable",
    "Iterator",
    "MutableIterator",
    "Collection",
    "MutableCollection",
    "List",
    "MutableList",
    "Set",
    "MutableSet",
    "ListIterator",
    "MutableListIterator",
    "Map",
    "MutableMap",
    "Nothing",
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

/// Map a Kotlin built-in type's **simple name** to its FRONT-END Kotlin internal name. Differs from
/// [`kotlin_builtin_to_jvm`] only for the COLLECTION types: the front end keeps `List` vs `MutableList`
/// distinct (`kotlin/collections/List` vs `…/MutableList`) so the read-only/mutable distinction survives
/// until emit, where [`to_jvm_internal`] erases both to the single JVM interface (`java/util/List`). All
/// other built-ins (`String`, `Comparable`, …) have no such distinction and keep their JVM identity.
pub fn kotlin_builtin_to_internal(simple: &str) -> Option<&'static str> {
    Some(match simple {
        "Iterable" => "kotlin/collections/Iterable",
        "MutableIterable" => "kotlin/collections/MutableIterable",
        "Collection" => "kotlin/collections/Collection",
        "MutableCollection" => "kotlin/collections/MutableCollection",
        "List" => "kotlin/collections/List",
        "MutableList" => "kotlin/collections/MutableList",
        "Set" => "kotlin/collections/Set",
        "MutableSet" => "kotlin/collections/MutableSet",
        "Map" => "kotlin/collections/Map",
        "MutableMap" => "kotlin/collections/MutableMap",
        "Iterator" => "kotlin/collections/Iterator",
        "MutableIterator" => "kotlin/collections/MutableIterator",
        "ListIterator" => "kotlin/collections/ListIterator",
        "MutableListIterator" => "kotlin/collections/MutableListIterator",
        // Non-collection built-ins keep their JVM identity (no read-only/mutable distinction).
        other => return kotlin_builtin_to_jvm(other),
    })
}

/// Map a JVM-mapped built-in type back to the Kotlin built-in whose `.kotlin_builtins` metadata declares
/// the Kotlin-only members it carries (`java/lang/CharSequence` → `kotlin/CharSequence` for `get`/`length`,
/// `java/lang/Number` → `kotlin/Number` for `toInt`/…, `java/lang/Comparable` → `kotlin/Comparable` for
/// `compareTo`). These are the mapped types whose Kotlin API differs from the JVM class's own methods;
/// `String`/`Any`/`Throwable` members resolve on the JVM class directly, and the collection types keep
/// their `kotlin/collections/…` identity in the front end. `None` for anything else.
pub fn jvm_to_kotlin_builtin_with_members(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "java/lang/CharSequence" => "kotlin/CharSequence",
        "java/lang/Number" => "kotlin/Number",
        "java/lang/Comparable" => "kotlin/Comparable",
        _ => return None,
    })
}

/// Whether a JVM-mapped Kotlin built-in is a JVM **interface** (so a member dispatches via
/// `invokeinterface`, not `invokevirtual`). A reliable answer for the curated mapped types — matching
/// kotlinc's `JavaToKotlinClassMap` — for when the classpath `.class` reader can't be consulted (e.g. a
/// JDK whose jimage format krusty doesn't decode). `None` for any other type (consult the classpath).
pub fn jvm_mapped_builtin_is_interface(jvm_internal: &str) -> Option<bool> {
    Some(match jvm_internal {
        "java/lang/CharSequence" | "java/lang/Comparable" | "java/lang/Iterable" => true,
        "java/lang/Number" | "java/lang/Object" | "java/lang/String" | "java/lang/Enum" => false,
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

use crate::types::Ty;

/// Bidirectional Kotlin↔JVM internal-name mapping for built-in *type identities* — the subset of
/// `JavaToKotlinClassMap` whose two sides have different internal names. The front-end core speaks
/// the Kotlin name (`kotlin/Any`); the JVM name (`java/lang/Object`) is materialized only when a
/// type crosses into the backend (descriptor emission, constant-pool class references). Listing the
/// pairs once here is what keeps every `java/lang/…` literal out of the compiler core.
const TYPE_MAP: &[(&str, &str)] = &[
    ("kotlin/Any", "java/lang/Object"),
    ("kotlin/String", "java/lang/String"),
    // Further built-ins (CharSequence, Comparable, Number, Enum, the primitive wrappers) and the
    // curated JVM-ABI method tables are migrated off `java/lang/…` in later phases; adding a pair
    // here also requires normalizing that name everywhere the classpath surfaces it.
];

/// Map a Kotlin built-in type's internal name to its JVM name (`kotlin/Any` → `java/lang/Object`).
/// Any other name — a user class, a JDK class already named in JVM form, a Kotlin stdlib class with
/// no JVM-builtin counterpart — passes through unchanged. Applied at the Ty→bytecode boundary.
pub fn to_jvm_internal(internal: &str) -> &str {
    // Emit-only mappings: core-introduced Kotlin names with a JVM counterpart that the classpath
    // never surfaces (so they stay out of the bidirectional `TYPE_MAP` and don't affect
    // `to_kotlin_internal`). `kotlin/Throwable` is synthesized by the front end for the `throw`
    // checkcast; the classpath always reads `java/lang/Throwable` directly.
    if internal == "kotlin/Throwable" {
        return "java/lang/Throwable";
    }
    // Emit-only: a BOXED primitive used as a reference (the element of `Array<Int>` = `[Ljava/lang/
    // Integer;`). The front end carries it as the Kotlin primitive name (`kotlin/Int`); only here does
    // it erase to the JVM wrapper. ONE-WAY (boxed primitives are never read back from the classpath
    // under these names), so it stays out of the bidirectional `TYPE_MAP`.
    match internal {
        "kotlin/Int" => return "java/lang/Integer",
        "kotlin/Long" => return "java/lang/Long",
        "kotlin/Short" => return "java/lang/Short",
        "kotlin/Byte" => return "java/lang/Byte",
        "kotlin/Double" => return "java/lang/Double",
        "kotlin/Float" => return "java/lang/Float",
        "kotlin/Boolean" => return "java/lang/Boolean",
        "kotlin/Char" => return "java/lang/Character",
        _ => {}
    }
    // Emit-only erasure of the Kotlin collection types (read-only AND mutable) to their single JVM
    // interface — `kotlin/collections/MutableList` → `java/util/List`, `…/List` → `java/util/List`, etc.
    // The front end keeps the two distinct (read-only vs mutable); they collapse only here at the bytecode
    // boundary. ONE-WAY (not in the bidirectional `TYPE_MAP`), so `to_kotlin_internal` never has to pick
    // ambiguously between `List`/`MutableList` when it reads a raw `java/util/List` descriptor.
    if let Some(simple) = internal.strip_prefix("kotlin/collections/") {
        if let Some(j) = kotlin_builtin_to_jvm(simple) {
            return j;
        }
    }
    TYPE_MAP
        .iter()
        .find(|(k, _)| *k == internal)
        .map(|(_, j)| *j)
        .unwrap_or(internal)
}

/// Inverse of [`to_jvm_internal`]: normalize a JVM built-in name read from the classpath/descriptors
/// to its Kotlin identity (`java/lang/Object` → `kotlin/Any`), mirroring how the reference compiler
/// maps Java types into Kotlin ones at the front-end boundary. Passes other names through unchanged.
pub fn to_kotlin_internal(internal: &str) -> &str {
    TYPE_MAP
        .iter()
        .find(|(_, j)| *j == internal)
        .map(|(k, _)| *k)
        .unwrap_or(internal)
}

/// The JVM wrapper (box) class for a primitive `Ty` (`Int` → `java/lang/Integer`), or `None` for a
/// non-primitive. The single source of truth for boxing owners shared by codegen and the front end.
pub fn wrapper_internal(t: Ty) -> Option<&'static str> {
    Some(match t {
        Ty::Int => "java/lang/Integer",
        Ty::Long => "java/lang/Long",
        Ty::Short => "java/lang/Short",
        Ty::Byte => "java/lang/Byte",
        Ty::Double => "java/lang/Double",
        Ty::Float => "java/lang/Float",
        Ty::Boolean => "java/lang/Boolean",
        Ty::Char => "java/lang/Character",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::to_jvm_internal;

    #[test]
    fn collection_types_erase_to_jvm_at_emit() {
        // Read-only and mutable Kotlin collections both collapse to the single JVM interface here.
        assert_eq!(to_jvm_internal("kotlin/collections/List"), "java/util/List");
        assert_eq!(
            to_jvm_internal("kotlin/collections/MutableList"),
            "java/util/List"
        );
        assert_eq!(to_jvm_internal("kotlin/collections/Map"), "java/util/Map");
        assert_eq!(
            to_jvm_internal("kotlin/collections/MutableMap"),
            "java/util/Map"
        );
        assert_eq!(
            to_jvm_internal("kotlin/collections/MutableCollection"),
            "java/util/Collection"
        );
        // A user/JDK class passes through unchanged.
        assert_eq!(to_jvm_internal("demo/Foo"), "demo/Foo");
        assert_eq!(to_jvm_internal("java/util/List"), "java/util/List");
    }
}
