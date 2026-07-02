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

/// The Kotlin (read-only) collection internal name for a JVM collection interface a generic signature
/// spells in Java terms (`java/util/List` → `kotlin/collections/List`). The inverse of the collection
/// half of [`kotlin_builtin_to_jvm`], mapping to the READ-ONLY form (a return type is read-only by
/// default, and every read-only extension also applies to the mutable subtype). Needed where a signature
/// carries the erased JVM name but the front end resolves members/extensions on the Kotlin type — e.g. a
/// `suspend fun`'s `Continuation<List<T>>` return, recovered so `.map { … }` resolves. `None` for a
/// non-collection JVM name.
pub fn jvm_collection_to_kotlin(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "java/lang/Iterable" => "kotlin/collections/Iterable",
        "java/util/Iterator" => "kotlin/collections/Iterator",
        "java/util/ListIterator" => "kotlin/collections/ListIterator",
        "java/util/Collection" => "kotlin/collections/Collection",
        "java/util/List" => "kotlin/collections/List",
        "java/util/Set" => "kotlin/collections/Set",
        "java/util/Map" => "kotlin/collections/Map",
        "java/util/Map$Entry" => "kotlin/collections/Map$Entry",
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

/// The JVM wrapper (box) class internal name for a Kotlin primitive's INTERNAL NAME
/// (`kotlin/Int` → `java/lang/Integer`), or `None` if `internal` is not a Kotlin primitive name.
/// The single source of truth for the boxed form, shared by the emit-only boxing in
/// [`to_jvm_internal`], the `Ty`-keyed [`wrapper_internal`], and descriptor callers in the backend
/// and plugins — so the primitive→wrapper table is listed exactly once.
pub fn kotlin_prim_to_wrapper(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "kotlin/Int" => "java/lang/Integer",
        "kotlin/Long" => "java/lang/Long",
        "kotlin/Short" => "java/lang/Short",
        "kotlin/Byte" => "java/lang/Byte",
        "kotlin/Double" => "java/lang/Double",
        "kotlin/Float" => "java/lang/Float",
        "kotlin/Boolean" => "java/lang/Boolean",
        "kotlin/Char" => "java/lang/Character",
        // An unsigned type's boxed form is its own inline-class wrapper (`kotlin/UInt`), not a `java/lang/*`.
        "kotlin/UInt" => "kotlin/UInt",
        "kotlin/ULong" => "kotlin/ULong",
        _ => return None,
    })
}

/// Inverse of [`kotlin_prim_to_wrapper`]: the Kotlin primitive internal name for a JVM box class
/// (`java/lang/Long` → `kotlin/Long`), or `None` if `internal` is not a boxed primitive. A generic type
/// argument (e.g. `Continuation<T>` in a `suspend fun`'s signature) always carries the BOXED form, so
/// recovering the source primitive return needs this inverse. Unsigned inline-class wrappers are
/// intentionally omitted — they are not `java/lang/*` boxes and keep their own identity.
pub fn wrapper_to_kotlin_prim(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "java/lang/Integer" => "kotlin/Int",
        "java/lang/Long" => "kotlin/Long",
        "java/lang/Short" => "kotlin/Short",
        "java/lang/Byte" => "kotlin/Byte",
        "java/lang/Double" => "kotlin/Double",
        "java/lang/Float" => "kotlin/Float",
        "java/lang/Boolean" => "kotlin/Boolean",
        "java/lang/Character" => "kotlin/Char",
        _ => return None,
    })
}

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
    if let Some(wrapper) = kotlin_prim_to_wrapper(internal) {
        return wrapper;
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
    // Route through the single primitive→wrapper table: `boxed_ref` carries a primitive as its Kotlin
    // internal name (`Ty::Int` → `Obj("kotlin/Int")`, `Ty::UInt` → `Obj("kotlin/UInt")`), which
    // `kotlin_prim_to_wrapper` boxes (`kotlin/Int` → `java/lang/Integer`, `kotlin/UInt` → `kotlin/UInt`).
    kotlin_prim_to_wrapper(t.boxed_ref()?.obj_internal()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn primitive_wrapper_table_is_single_source() {
        // The 8 Kotlin primitive internal names → their JVM wrappers.
        let pairs = [
            ("kotlin/Int", "java/lang/Integer", Ty::Int),
            ("kotlin/Long", "java/lang/Long", Ty::Long),
            ("kotlin/Short", "java/lang/Short", Ty::Short),
            ("kotlin/Byte", "java/lang/Byte", Ty::Byte),
            ("kotlin/Double", "java/lang/Double", Ty::Double),
            ("kotlin/Float", "java/lang/Float", Ty::Float),
            ("kotlin/Boolean", "java/lang/Boolean", Ty::Boolean),
            ("kotlin/Char", "java/lang/Character", Ty::Char),
        ];
        for (internal, wrapper, prim) in pairs {
            assert_eq!(kotlin_prim_to_wrapper(internal), Some(wrapper));
            // The emit-only boxing in `to_jvm_internal` and the `Ty`-keyed `wrapper_internal` agree.
            assert_eq!(to_jvm_internal(internal), wrapper);
            assert_eq!(wrapper_internal(prim), Some(wrapper));
        }
        // Unsigned boxes to its own inline-class wrapper (`kotlin/UInt`), not a `java/lang/*`.
        assert_eq!(kotlin_prim_to_wrapper("kotlin/UInt"), Some("kotlin/UInt"));
        assert_eq!(kotlin_prim_to_wrapper("kotlin/ULong"), Some("kotlin/ULong"));
        assert_eq!(wrapper_internal(Ty::UInt), Some("kotlin/UInt"));
        assert_eq!(wrapper_internal(Ty::ULong), Some("kotlin/ULong"));
        // Non-primitives have no wrapper.
        assert_eq!(kotlin_prim_to_wrapper("kotlin/String"), None);
        assert_eq!(kotlin_prim_to_wrapper("demo/Foo"), None);
        assert_eq!(wrapper_internal(Ty::String), None);
    }

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

    #[test]
    fn kotlin_builtin_to_jvm_maps_top_level_and_collections() {
        // addTopLevel(...) mapped types.
        assert_eq!(kotlin_builtin_to_jvm("Any"), Some("java/lang/Object"));
        assert_eq!(kotlin_builtin_to_jvm("String"), Some("java/lang/String"));
        assert_eq!(
            kotlin_builtin_to_jvm("CharSequence"),
            Some("java/lang/CharSequence")
        );
        assert_eq!(
            kotlin_builtin_to_jvm("Throwable"),
            Some("java/lang/Throwable")
        );
        assert_eq!(
            kotlin_builtin_to_jvm("Cloneable"),
            Some("java/lang/Cloneable")
        );
        assert_eq!(kotlin_builtin_to_jvm("Number"), Some("java/lang/Number"));
        assert_eq!(
            kotlin_builtin_to_jvm("Comparable"),
            Some("java/lang/Comparable")
        );
        assert_eq!(kotlin_builtin_to_jvm("Enum"), Some("java/lang/Enum"));
        assert_eq!(
            kotlin_builtin_to_jvm("Annotation"),
            Some("java/lang/annotation/Annotation")
        );
        assert_eq!(kotlin_builtin_to_jvm("Nothing"), Some("java/lang/Void"));
        // Read-only and mutable collections collapse to the same JVM interface.
        for (ro, mut_) in [
            ("Iterable", "MutableIterable"),
            ("Iterator", "MutableIterator"),
            ("Collection", "MutableCollection"),
            ("List", "MutableList"),
            ("Set", "MutableSet"),
            ("ListIterator", "MutableListIterator"),
            ("Map", "MutableMap"),
        ] {
            assert_eq!(kotlin_builtin_to_jvm(ro), kotlin_builtin_to_jvm(mut_));
            assert!(kotlin_builtin_to_jvm(ro).is_some());
        }
        assert_eq!(kotlin_builtin_to_jvm("List"), Some("java/util/List"));
        assert_eq!(kotlin_builtin_to_jvm("Map"), Some("java/util/Map"));
        assert_eq!(
            kotlin_builtin_to_jvm("Iterable"),
            Some("java/lang/Iterable")
        );
        // Unmapped names return None.
        assert_eq!(kotlin_builtin_to_jvm("Int"), None);
        assert_eq!(kotlin_builtin_to_jvm("Foo"), None);
        assert_eq!(kotlin_builtin_to_jvm(""), None);
    }

    #[test]
    fn every_builtin_mapped_name_maps() {
        // The seed list and the mapping function stay in lockstep.
        for name in BUILTIN_MAPPED_NAMES {
            assert!(
                kotlin_builtin_to_jvm(name).is_some(),
                "seed name {name} is unmapped"
            );
        }
    }

    #[test]
    fn jvm_collection_to_kotlin_maps_read_only_forms() {
        assert_eq!(
            jvm_collection_to_kotlin("java/lang/Iterable"),
            Some("kotlin/collections/Iterable")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/Iterator"),
            Some("kotlin/collections/Iterator")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/ListIterator"),
            Some("kotlin/collections/ListIterator")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/Collection"),
            Some("kotlin/collections/Collection")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/List"),
            Some("kotlin/collections/List")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/Set"),
            Some("kotlin/collections/Set")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/Map"),
            Some("kotlin/collections/Map")
        );
        assert_eq!(
            jvm_collection_to_kotlin("java/util/Map$Entry"),
            Some("kotlin/collections/Map$Entry")
        );
        // Non-collection JVM names are not mapped.
        assert_eq!(jvm_collection_to_kotlin("java/lang/String"), None);
        assert_eq!(jvm_collection_to_kotlin("java/lang/Object"), None);
    }

    #[test]
    fn kotlin_builtin_to_internal_keeps_collection_mutability() {
        // Collections keep the read-only/mutable distinction in the front end.
        assert_eq!(
            kotlin_builtin_to_internal("List"),
            Some("kotlin/collections/List")
        );
        assert_eq!(
            kotlin_builtin_to_internal("MutableList"),
            Some("kotlin/collections/MutableList")
        );
        assert_eq!(
            kotlin_builtin_to_internal("MutableMap"),
            Some("kotlin/collections/MutableMap")
        );
        assert_eq!(
            kotlin_builtin_to_internal("Iterator"),
            Some("kotlin/collections/Iterator")
        );
        // Non-collection built-ins keep their JVM identity (delegate to kotlin_builtin_to_jvm).
        assert_eq!(
            kotlin_builtin_to_internal("String"),
            Some("java/lang/String")
        );
        assert_eq!(kotlin_builtin_to_internal("Any"), Some("java/lang/Object"));
        assert_eq!(
            kotlin_builtin_to_internal("Comparable"),
            Some("java/lang/Comparable")
        );
        assert_eq!(kotlin_builtin_to_internal("Nope"), None);
    }

    #[test]
    fn jvm_to_kotlin_builtin_with_members_covers_kotlin_api_types() {
        assert_eq!(
            jvm_to_kotlin_builtin_with_members("java/lang/CharSequence"),
            Some("kotlin/CharSequence")
        );
        assert_eq!(
            jvm_to_kotlin_builtin_with_members("java/lang/Number"),
            Some("kotlin/Number")
        );
        assert_eq!(
            jvm_to_kotlin_builtin_with_members("java/lang/Comparable"),
            Some("kotlin/Comparable")
        );
        // String/Object/Throwable resolve members on the JVM class directly — not remapped here.
        assert_eq!(jvm_to_kotlin_builtin_with_members("java/lang/String"), None);
        assert_eq!(jvm_to_kotlin_builtin_with_members("java/lang/Object"), None);
        assert_eq!(jvm_to_kotlin_builtin_with_members("java/util/List"), None);
    }

    #[test]
    fn jvm_mapped_builtin_is_interface_classifies_curated_types() {
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/CharSequence"),
            Some(true)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/Comparable"),
            Some(true)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/Iterable"),
            Some(true)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/Number"),
            Some(false)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/Object"),
            Some(false)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/String"),
            Some(false)
        );
        assert_eq!(
            jvm_mapped_builtin_is_interface("java/lang/Enum"),
            Some(false)
        );
        // Anything else: consult the classpath (None).
        assert_eq!(jvm_mapped_builtin_is_interface("demo/Foo"), None);
        assert_eq!(jvm_mapped_builtin_is_interface("java/util/List"), None);
    }

    #[test]
    fn is_throwable_internal_recognises_jdk_naming() {
        assert!(is_throwable_internal("java/lang/Throwable"));
        assert!(is_throwable_internal("java/lang/RuntimeException"));
        assert!(is_throwable_internal("java/io/IOException"));
        assert!(is_throwable_internal("java/lang/AssertionError"));
        assert!(is_throwable_internal("demo/MyError"));
        // Not a throwable by the naming convention.
        assert!(!is_throwable_internal("java/lang/String"));
        assert!(!is_throwable_internal("java/lang/Object"));
        assert!(!is_throwable_internal(""));
    }

    #[test]
    fn wrapper_to_kotlin_prim_is_inverse_of_prim_to_wrapper() {
        let boxes = [
            ("java/lang/Integer", "kotlin/Int"),
            ("java/lang/Long", "kotlin/Long"),
            ("java/lang/Short", "kotlin/Short"),
            ("java/lang/Byte", "kotlin/Byte"),
            ("java/lang/Double", "kotlin/Double"),
            ("java/lang/Float", "kotlin/Float"),
            ("java/lang/Boolean", "kotlin/Boolean"),
            ("java/lang/Character", "kotlin/Char"),
        ];
        for (wrapper, prim) in boxes {
            assert_eq!(wrapper_to_kotlin_prim(wrapper), Some(prim));
            // Round-trips through the forward table.
            assert_eq!(kotlin_prim_to_wrapper(prim), Some(wrapper));
        }
        // Unsigned wrappers are intentionally NOT recognised as java/lang boxes.
        assert_eq!(wrapper_to_kotlin_prim("kotlin/UInt"), None);
        assert_eq!(wrapper_to_kotlin_prim("java/lang/String"), None);
        assert_eq!(wrapper_to_kotlin_prim("demo/Foo"), None);
    }

    #[test]
    fn to_kotlin_internal_inverts_the_bidirectional_type_map() {
        assert_eq!(to_kotlin_internal("java/lang/Object"), "kotlin/Any");
        assert_eq!(to_kotlin_internal("java/lang/String"), "kotlin/String");
        // Round-trip against the forward mapping.
        assert_eq!(
            to_jvm_internal(to_kotlin_internal("java/lang/Object")),
            "java/lang/Object"
        );
        assert_eq!(
            to_kotlin_internal(to_jvm_internal("kotlin/Any")),
            "kotlin/Any"
        );
        // Names outside the bidirectional map pass through unchanged.
        assert_eq!(to_kotlin_internal("demo/Foo"), "demo/Foo");
        assert_eq!(to_kotlin_internal("java/util/List"), "java/util/List");
    }

    #[test]
    fn to_jvm_internal_special_cases_throwable_and_passthrough() {
        // kotlin/Throwable is an emit-only synthetic name → java/lang/Throwable.
        assert_eq!(to_jvm_internal("kotlin/Throwable"), "java/lang/Throwable");
        assert_eq!(to_jvm_internal("kotlin/Any"), "java/lang/Object");
        assert_eq!(to_jvm_internal("kotlin/String"), "java/lang/String");
        // A boxed primitive under kotlin/collections/ that is NOT a collection passes through.
        assert_eq!(
            to_jvm_internal("kotlin/collections/ArrayDeque"),
            "kotlin/collections/ArrayDeque"
        );
        // Unmapped stdlib name passes through.
        assert_eq!(to_jvm_internal("kotlin/Unit"), "kotlin/Unit");
    }
}
