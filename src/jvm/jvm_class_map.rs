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

use crate::name_tree::FxHashMap;
use crate::types::TypeName;

/// JVM realization of a semantic intrinsic companion. Kotlinc's `CompanionObjectMapping` consists
/// of every primitive owner plus `String` and `Enum`; derive the primitive portion from the shared
/// Kotlin/JVM mapping instead of maintaining another classifier-name list.
pub fn intrinsic_companion_to_jvm(internal: &str) -> Option<String> {
    let companion = crate::types::existing_type_name(internal)?;
    if companion.nested_segment_ref() != "Companion" {
        return None;
    }
    let owner = companion.nested_owner()?;
    let ids = builtin_ids();
    let primitive = ids
        .jvm_builtin
        .get(&owner)
        .is_some_and(|(_, jvm)| ids.wrapper_prim.contains_key(jvm));
    if !primitive && !owner.matches("kotlin/String") && !owner.matches("kotlin/Enum") {
        return None;
    }
    Some(format!(
        "kotlin/jvm/internal/{}CompanionObject",
        owner.segment_ref()
    ))
}

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

/// `JavaToKotlinClassMap`'s `kotlinToJava` direction, keyed by the FULL Kotlin INTERNAL name (as kotlinc
/// keys it by `FqName`) — NEVER a simple name, so `kotlin/collections/List` → `java/util/List` and a bare
/// `kotlin/List` (not a real type) simply isn't a key. Mutable variants map to the same JVM interface as
/// their read-only counterpart (`addMapping`). `None` if `internal` is not a mapped built-in.
pub fn kotlin_builtin_to_jvm(internal: &str) -> Option<&'static str> {
    Some(match internal {
        // Top-level `kotlin` package builtins.
        "kotlin/Any" => "java/lang/Object",
        "kotlin/String" => "java/lang/String",
        "kotlin/CharSequence" => "java/lang/CharSequence",
        "kotlin/Throwable" => "java/lang/Throwable",
        "kotlin/Cloneable" => "java/lang/Cloneable",
        "kotlin/Number" => "java/lang/Number",
        "kotlin/Comparable" => "java/lang/Comparable",
        "kotlin/Enum" => "java/lang/Enum",
        "kotlin/Annotation" => "java/lang/annotation/Annotation",
        "kotlin/Nothing" => "java/lang/Void",
        // `kotlin.collections` — read-only AND mutable erase to the one JVM interface.
        "kotlin/collections/Iterable" | "kotlin/collections/MutableIterable" => {
            "java/lang/Iterable"
        }
        "kotlin/collections/Iterator" | "kotlin/collections/MutableIterator" => {
            "java/util/Iterator"
        }
        "kotlin/collections/Collection" | "kotlin/collections/MutableCollection" => {
            "java/util/Collection"
        }
        "kotlin/collections/List" | "kotlin/collections/MutableList" => "java/util/List",
        "kotlin/collections/Set" | "kotlin/collections/MutableSet" => "java/util/Set",
        "kotlin/collections/ListIterator" | "kotlin/collections/MutableListIterator" => {
            "java/util/ListIterator"
        }
        "kotlin/collections/Map" | "kotlin/collections/MutableMap" => "java/util/Map",
        "kotlin/collections/Map.Entry"
        | "kotlin/collections/Map$Entry"
        | "kotlin/collections/MutableMap.MutableEntry"
        | "kotlin/collections/MutableMap$MutableEntry" => "java/util/Map$Entry",
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
        // The front end spells the nested Kotlin type with a DOT (`kotlin/collections/Map.Entry`, see the
        // `kotlin_builtin_to_jvm` key above), not a `$` — so the reverse map must too, or a `Map.Entry`
        // extension (`component1`/`component2`) won't match a `java/util/Map$Entry` receiver.
        "java/util/Map$Entry" => "kotlin/collections/Map.Entry",
        _ => return None,
    })
}

/// The MUTABLE Kotlin collection interface a JVM collection type also realizes. A `java.util.*` collection
/// is a Kotlin PLATFORM (flexible) type — `(Mutable)List!` — so it is simultaneously the read-only face
/// ([`jvm_collection_to_kotlin`]) AND the mutable one. Adding the mutable face as a supertype lets a
/// `MutableCollection.plusAssign` / `MutableList.add` extension apply to a `java/util/ArrayList` receiver,
/// exactly as kotlinc resolves it. `None` for a non-collection JVM type.
pub fn jvm_collection_to_kotlin_mutable(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "java/lang/Iterable" => "kotlin/collections/MutableIterable",
        "java/util/Iterator" => "kotlin/collections/MutableIterator",
        "java/util/ListIterator" => "kotlin/collections/MutableListIterator",
        "java/util/Collection" => "kotlin/collections/MutableCollection",
        "java/util/List" => "kotlin/collections/MutableList",
        "java/util/Set" => "kotlin/collections/MutableSet",
        "java/util/Map" => "kotlin/collections/MutableMap",
        // The mutable sibling of the read-only `Map.Entry` map above — a concrete entry (`AbstractMap`'s
        // `SimpleEntry`) supports `setValue`; front-end DOT form, not a `$`, like the read-only key.
        "java/util/Map$Entry" => "kotlin/collections/MutableMap.MutableEntry",
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
        // Non-collection built-ins keep their JVM identity (no read-only/mutable distinction). The map is
        // FQN-keyed, so form the top-level Kotlin internal (`kotlin/<simple>`) before the lookup.
        other => return kotlin_builtin_to_jvm(&format!("kotlin/{other}")),
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

pub fn jvm_to_kotlin_builtin_with_members_name(internal: TypeName) -> Option<TypeName> {
    builtin_ids().with_members.get(&internal).copied()
}

/// The canonical Kotlin builtin declaration whose source metadata describes a mapped JVM type. Every
/// erasure group contributes one owner here; for read-only/mutable collection siblings the read-only
/// declaration is canonical. Consumers asking for `.kotlin_builtins` facts therefore never rebuild
/// partial collection-vs-class mapping branches or maintain a separate per-name capability table.
pub fn jvm_to_kotlin_builtin_metadata_name(internal: TypeName) -> Option<TypeName> {
    builtin_ids().metadata_owner.get(&internal).copied()
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
        "kotlin/UByte" => "kotlin/UByte",
        "kotlin/UShort" => "kotlin/UShort",
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

pub fn wrapper_to_kotlin_prim_name(internal: TypeName) -> Option<&'static str> {
    builtin_ids().wrapper_prim.get(&internal).copied()
}

/// Map a Kotlin built-in type's internal name to its JVM name (`kotlin/Any` → `java/lang/Object`).
/// Any other name — a user class, a JDK class already named in JVM form, a Kotlin stdlib class with
/// no JVM-builtin counterpart — passes through unchanged. Applied at the Ty→bytecode boundary.
pub fn to_jvm_internal(internal: &str) -> &str {
    // Emit-only: a BOXED primitive used as a reference (the element of `Array<Int>` = `[Ljava/lang/
    // Integer;`). The front end carries it as the Kotlin primitive name (`kotlin/Int`); only here does
    // it erase to the JVM wrapper. ONE-WAY (boxed primitives are never read back from the classpath
    // under these names), so it stays out of the classifier erasure groups.
    if let Some(wrapper) = kotlin_prim_to_wrapper(internal) {
        return wrapper;
    }
    // `JavaToKotlinClassMap` (`kotlinToJava`), keyed by full Kotlin internal name — the codegen erasure
    // kotlinc's `KotlinTypeMapper` performs: `kotlin/Number` → `java/lang/Number`,
    // `kotlin/collections/MutableList` → `java/util/List`, `kotlin/Throwable` → `java/lang/Throwable`, …
    // The FRONT END keeps the Kotlin identity (own hierarchy/members, read-only vs mutable); only here, at
    // the JVM boundary, does it erase. The inverse chooses each erasure group's canonical Kotlin
    // declaration; for collection groups that is the read-only declaration.
    // Covers the top-level Kotlin mapped built-ins too (`kotlin/CharSequence` → `java/lang/CharSequence`,
    // `kotlin/Number`, `kotlin/Enum`, …), since they are keyed by their full internal name here.
    if let Some(j) = kotlin_builtin_to_jvm(internal) {
        return j;
    }
    if crate::types::existing_type_name(internal)
        .is_some_and(super::jvm_libraries::is_fictitious_kfunction)
    {
        return crate::types::KFUNCTION_INTERNAL;
    }
    // `kotlin/Function{N}` is a METADATA-only name (what `@Metadata` calls a function type); the
    // runtime class is `kotlin/jvm/functions/Function{N}`. A consumer that materializes a
    // metadata-decoded function type into bytecode (a checkcast on an alias-expanded return) must
    // erase it here or reference a class that does not exist (corpus
    // `nestedFunctionTypeAliasExpansion.kt`: NoClassDefFoundError kotlin/Function1).
    if let Some(mapped) = internal
        .strip_prefix("kotlin/Function")
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|arity| crate::types::FUNCTION_N_INTERNAL.get(arity).copied())
    {
        return mapped;
    }
    internal
}

/// Which declaration supplies a mapped builtin's SOURCE member/supertype scope. This is part of the
/// Kotlin↔JVM mapping itself, not a classpath-loading heuristic: `KotlinDeclaration` means the JVM class
/// is only the physical realization and its Java API must not be joined into the Kotlin source scope.
/// The other mapped builtins remain joined until krusty implements kotlinc's JVM-visible-method whitelist.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BuiltinScopeProvenance {
    JoinedWithJvm,
    KotlinDeclaration,
}

/// One Kotlin-to-JVM erasure identity group.
///
/// Most JVM classifiers in this table are also imported as the corresponding Kotlin builtin
/// (`java.util.List` has Kotlin's mapped `List!` view). `kotlin.Nothing` is intentionally different:
/// it erases to `java.lang.Void`, but a Java declaration that explicitly writes `java.lang.Void` still
/// denotes that ordinary Java class, not Kotlin's bottom type. Keeping that direction on the group
/// prevents the classpath reader from inferring semantic identity from a shared descriptor.
struct ErasureGroup {
    kotlin_names: &'static [&'static str],
    jvm_name: &'static str,
    scope: BuiltinScopeProvenance,
    maps_java_source_to_kotlin: bool,
}

impl ErasureGroup {
    const fn mapped(
        kotlin_names: &'static [&'static str],
        jvm_name: &'static str,
        scope: BuiltinScopeProvenance,
    ) -> Self {
        Self {
            kotlin_names,
            jvm_name,
            scope,
            maps_java_source_to_kotlin: true,
        }
    }

    const fn erasure_only(
        kotlin_names: &'static [&'static str],
        jvm_name: &'static str,
        scope: BuiltinScopeProvenance,
    ) -> Self {
        Self {
            kotlin_names,
            jvm_name,
            scope,
            maps_java_source_to_kotlin: false,
        }
    }
}

/// The JVM-erasure identity groups: each group's Kotlin internal names, the single JVM internal name
/// they erase to, and the semantic source-scope policy. Keeping all three facts in the same table prevents
/// the classpath loader from reconstructing a parallel collection/String name list that can drift.
const ERASURE_GROUPS: &[ErasureGroup] = &[
    ErasureGroup::mapped(
        &["kotlin/Any"],
        "java/lang/Object",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/String"],
        "java/lang/String",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &["kotlin/CharSequence"],
        "java/lang/CharSequence",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Throwable"],
        "java/lang/Throwable",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Cloneable"],
        "java/lang/Cloneable",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Number"],
        "java/lang/Number",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Comparable"],
        "java/lang/Comparable",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Enum"],
        "java/lang/Enum",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &["kotlin/Annotation"],
        "java/lang/annotation/Annotation",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::erasure_only(
        &["kotlin/Nothing"],
        "java/lang/Void",
        BuiltinScopeProvenance::JoinedWithJvm,
    ),
    ErasureGroup::mapped(
        &[
            "kotlin/collections/Iterable",
            "kotlin/collections/MutableIterable",
        ],
        "java/lang/Iterable",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &[
            "kotlin/collections/Iterator",
            "kotlin/collections/MutableIterator",
        ],
        "java/util/Iterator",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &[
            "kotlin/collections/ListIterator",
            "kotlin/collections/MutableListIterator",
        ],
        "java/util/ListIterator",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &[
            "kotlin/collections/Collection",
            "kotlin/collections/MutableCollection",
        ],
        "java/util/Collection",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &["kotlin/collections/List", "kotlin/collections/MutableList"],
        "java/util/List",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &["kotlin/collections/Set", "kotlin/collections/MutableSet"],
        "java/util/Set",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &["kotlin/collections/Map", "kotlin/collections/MutableMap"],
        "java/util/Map",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
    ErasureGroup::mapped(
        &[
            "kotlin/collections/Map.Entry",
            "kotlin/collections/Map$Entry",
            "kotlin/collections/MutableMap.MutableEntry",
            "kotlin/collections/MutableMap$MutableEntry",
        ],
        "java/util/Map$Entry",
        BuiltinScopeProvenance::KotlinDeclaration,
    ),
];

/// Id-keyed views of the curated builtin tables, interned once. Hot resolution paths compare
/// `TypeName` ids through these maps instead of locking the global name tree per string compare.
struct BuiltinIds {
    erasure_group: FxHashMap<TypeName, u8>,
    /// Any builtin (Kotlin or JVM spelling, primitives included) → its canonical JVM internal.
    jvm_builtin: FxHashMap<TypeName, (&'static str, TypeName)>,
    /// Groups whose JVM face is a `java/util/*` collection interface, as a bitmask by group index.
    collection_groups: u32,
    /// Groups whose Kotlin metadata declaration replaces, rather than joins, the JVM source scope.
    authoritative_scope_groups: u32,
    coll_to_kotlin: FxHashMap<TypeName, TypeName>,
    coll_to_kotlin_mutable: FxHashMap<TypeName, TypeName>,
    wrapper_prim: FxHashMap<TypeName, &'static str>,
    with_members: FxHashMap<TypeName, TypeName>,
    /// Canonical Kotlin declaration whose `.kotlin_builtins` record describes a JVM-mapped owner.
    /// Read-only collection faces win for erasure groups with mutable siblings, matching the frontend
    /// identity already used for a raw JVM collection return.
    metadata_owner: FxHashMap<TypeName, TypeName>,
}

fn builtin_ids() -> &'static BuiltinIds {
    static IDS: std::sync::OnceLock<BuiltinIds> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let tn = crate::types::type_name;
        let mut erasure_group = FxHashMap::default();
        let mut jvm_builtin = FxHashMap::default();
        let mut collection_groups = 0u32;
        let mut authoritative_scope_groups = 0u32;
        let mut coll_to_kotlin = FxHashMap::default();
        let mut coll_to_kotlin_mutable = FxHashMap::default();
        let mut with_members = FxHashMap::default();
        let mut metadata_owner = FxHashMap::default();
        for (group, mapping) in ERASURE_GROUPS.iter().enumerate() {
            let ErasureGroup {
                kotlin_names,
                jvm_name,
                scope,
                ..
            } = mapping;
            let g = u8::try_from(group).expect("erasure group count fits u8");
            let jvm_id = tn(jvm_name);
            // Every erasure group is declared by a Kotlin builtin. Keeping the inverse beside the
            // forward identity table prevents consumers from reconstructing only the collection or
            // Kotlin-only-member subsets and silently missing mapped types such as Cloneable/String.
            if let Some(kotlin_name) = kotlin_names.first() {
                metadata_owner.insert(jvm_id, tn(kotlin_name));
            }
            for kotlin_name in *kotlin_names {
                let id = tn(kotlin_name);
                erasure_group.insert(id, g);
                jvm_builtin.insert(id, (*jvm_name, jvm_id));
            }
            erasure_group.insert(jvm_id, g);
            jvm_builtin.insert(jvm_id, (*jvm_name, jvm_id));
            if jvm_name.starts_with("java/util/") {
                collection_groups |= 1 << group;
            }
            if *scope == BuiltinScopeProvenance::KotlinDeclaration {
                authoritative_scope_groups |= 1 << group;
            }
            if let Some(kotlin) = jvm_collection_to_kotlin(jvm_name) {
                coll_to_kotlin.insert(jvm_id, tn(kotlin));
            }
            if let Some(kotlin) = jvm_collection_to_kotlin_mutable(jvm_name) {
                coll_to_kotlin_mutable.insert(jvm_id, tn(kotlin));
            }
            if let Some(kotlin) = jvm_to_kotlin_builtin_with_members(jvm_name) {
                with_members.insert(jvm_id, tn(kotlin));
            }
        }
        let mut wrapper_prim = FxHashMap::default();
        for wrapper in [
            "java/lang/Integer",
            "java/lang/Long",
            "java/lang/Short",
            "java/lang/Byte",
            "java/lang/Double",
            "java/lang/Float",
            "java/lang/Boolean",
            "java/lang/Character",
        ] {
            let prim = wrapper_to_kotlin_prim(wrapper).expect("wrapper table covers JVM boxes");
            let wrapper_id = tn(wrapper);
            wrapper_prim.insert(wrapper_id, prim);
            jvm_builtin.insert(wrapper_id, (wrapper, wrapper_id));
            jvm_builtin.insert(tn(prim), (wrapper, wrapper_id));
        }
        BuiltinIds {
            erasure_group,
            jvm_builtin,
            collection_groups,
            authoritative_scope_groups,
            coll_to_kotlin,
            coll_to_kotlin_mutable,
            wrapper_prim,
            with_members,
            metadata_owner,
        }
    })
}

/// Compare a semantic Kotlin/internal class name with a JVM-erased internal name without rendering the
/// semantic name first.
pub fn type_name_maps_to_jvm_internal(internal: TypeName, jvm_internal: &str) -> bool {
    match ERASURE_GROUPS
        .iter()
        .position(|mapping| mapping.jvm_name == jvm_internal)
    {
        Some(group) => {
            jvm_erasure_group(internal) == Some(u8::try_from(group).expect("group fits u8"))
        }
        None => internal.matches(jvm_internal),
    }
}

/// Whether a mapped builtin's decoded Kotlin declaration is authoritative for its source-level
/// members and supertypes. This answers only the mapping-policy question; callers must separately
/// verify that the declaration was actually decoded before replacing classfile data.
pub fn mapped_builtin_has_authoritative_kotlin_scope(internal: TypeName) -> bool {
    let ids = builtin_ids();
    jvm_erasure_group(internal)
        .is_some_and(|group| ids.authoritative_scope_groups & (1 << group) != 0)
}

fn jvm_erasure_group(internal: TypeName) -> Option<u8> {
    builtin_ids().erasure_group.get(&internal).copied()
}

/// Compare two id-backed class names by their JVM erasure identity without materializing either name.
pub fn type_names_map_to_same_jvm_internal(left: TypeName, right: TypeName) -> bool {
    left == right
        || jvm_erasure_group(left)
            .zip(jvm_erasure_group(right))
            .is_some_and(|(left, right)| left == right)
}

pub fn type_name_maps_to_jvm_collection_interface(internal: TypeName) -> bool {
    let ids = builtin_ids();
    jvm_erasure_group(internal).is_some_and(|g| ids.collection_groups & (1 << g) != 0)
        || internal.starts_with("java/util/")
}

/// Whether `internal` is a KOTLIN semantic face of a collection erasure group, as opposed to the
/// group's JVM `java/util/*` face. Derive this from the shared builtin mapping table rather than a
/// package-prefix test: adding or renaming a mapped collection then updates every consumer through the
/// one erasure policy, and metadata cannot replace a semantic Kotlin name with an emit-only JVM name.
pub fn is_kotlin_collection_type_name(internal: TypeName) -> bool {
    type_name_maps_to_jvm_collection_interface(internal)
        && builtin_ids()
            .jvm_builtin
            .get(&internal)
            .is_some_and(|(_, jvm_name)| *jvm_name != internal)
}

pub fn jvm_collection_to_kotlin_type_name(internal: TypeName) -> Option<TypeName> {
    builtin_ids().coll_to_kotlin.get(&internal).copied()
}

pub fn jvm_collection_to_kotlin_mutable_type_name(internal: TypeName) -> Option<TypeName> {
    builtin_ids().coll_to_kotlin_mutable.get(&internal).copied()
}

pub fn type_name_to_jvm_builtin_internal(internal: TypeName) -> Option<&'static str> {
    builtin_ids().jvm_builtin.get(&internal).map(|(s, _)| *s)
}

/// One JDK member kotlinc re-admits over the `.kotlin_builtins` scope of a mapped class
/// (`JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`): a mapped Kotlin collection's source scope is
/// its Kotlin declaration, but these Java default methods are part of the Kotlin API surface too.
/// Entries are keyed by the JVM owner the signature is declared on, matched by physical name and
/// erased descriptor — the same key kotlinc's table uses.
struct MappedVisibleMethod {
    jvm_owner: &'static str,
    name: &'static str,
    descriptor: &'static str,
    /// `Some(face)` — a kotlinc `MUTABLE_METHOD_SIGNATURES` entry, visible only on that mutable
    /// Kotlin face. `None` — visible on the group's read-only face (the first of its
    /// [`ErasureGroup::kotlin_names`]); the mutable face inherits it through the Kotlin hierarchy,
    /// mirroring `JvmMappedScope`, which attaches non-mutating signatures to the read-only
    /// container only.
    mutable_face: Option<&'static str>,
}

const fn visible(
    jvm_owner: &'static str,
    name: &'static str,
    descriptor: &'static str,
) -> MappedVisibleMethod {
    MappedVisibleMethod {
        jvm_owner,
        name,
        descriptor,
        mutable_face: None,
    }
}

const fn mutable(
    jvm_owner: &'static str,
    face: &'static str,
    name: &'static str,
    descriptor: &'static str,
) -> MappedVisibleMethod {
    MappedVisibleMethod {
        jvm_owner,
        name,
        descriptor,
        mutable_face: Some(face),
    }
}

/// kotlinc 2.4's whitelist for the mapped collection interfaces (the KotlinDeclaration erasure
/// groups). `kotlin/Throwable`, `kotlin/CharSequence` & co. stay JoinedWithJvm and need no entries;
/// `java/util/Map.remove(Object,Object)` is omitted because Kotlin's `MutableMap` itself declares
/// `remove(key, value)` — re-admitting the Java signature would publish a duplicate candidate.
const MAPPED_VISIBLE_METHODS: &[MappedVisibleMethod] = &[
    visible(
        "java/util/Iterator",
        "forEachRemaining",
        "(Ljava/util/function/Consumer;)V",
    ),
    visible(
        "java/lang/Iterable",
        "forEach",
        "(Ljava/util/function/Consumer;)V",
    ),
    visible(
        "java/lang/Iterable",
        "spliterator",
        "()Ljava/util/Spliterator;",
    ),
    visible(
        "java/util/Collection",
        "spliterator",
        "()Ljava/util/Spliterator;",
    ),
    visible(
        "java/util/Collection",
        "parallelStream",
        "()Ljava/util/stream/Stream;",
    ),
    visible(
        "java/util/Collection",
        "stream",
        "()Ljava/util/stream/Stream;",
    ),
    mutable(
        "java/util/Collection",
        "kotlin/collections/MutableCollection",
        "removeIf",
        "(Ljava/util/function/Predicate;)Z",
    ),
    mutable(
        "java/util/List",
        "kotlin/collections/MutableList",
        "replaceAll",
        "(Ljava/util/function/UnaryOperator;)V",
    ),
    mutable(
        "java/util/List",
        "kotlin/collections/MutableList",
        "addFirst",
        "(Ljava/lang/Object;)V",
    ),
    mutable(
        "java/util/List",
        "kotlin/collections/MutableList",
        "addLast",
        "(Ljava/lang/Object;)V",
    ),
    mutable(
        "java/util/List",
        "kotlin/collections/MutableList",
        "removeFirst",
        "()Ljava/lang/Object;",
    ),
    mutable(
        "java/util/List",
        "kotlin/collections/MutableList",
        "removeLast",
        "()Ljava/lang/Object;",
    ),
    visible(
        "java/util/Map",
        "getOrDefault",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
    ),
    visible(
        "java/util/Map",
        "forEach",
        "(Ljava/util/function/BiConsumer;)V",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "computeIfAbsent",
        "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "computeIfPresent",
        "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "compute",
        "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "merge",
        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "putIfAbsent",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "replaceAll",
        "(Ljava/util/function/BiFunction;)V",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "replace",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
    ),
    mutable(
        "java/util/Map",
        "kotlin/collections/MutableMap",
        "replace",
        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Z",
    ),
];

/// Whether a Java member read from the mapped JVM class is part of the SOURCE scope of the mapped
/// Kotlin face `face` (`kotlin/collections/MutableMap`, …) under kotlinc's
/// `VISIBLE_METHOD_SIGNATURES` whitelist. Consulted only where the Kotlin declaration is otherwise
/// authoritative; the JVM face (`java/util/Map` itself) keeps its full classfile scope untouched.
pub fn mapped_scope_keeps_jvm_method(face: TypeName, name: &str, descriptor: &str) -> bool {
    MAPPED_VISIBLE_METHODS.iter().any(|method| {
        if method.name != name || method.descriptor != descriptor {
            return false;
        }
        match method.mutable_face {
            Some(mutable) => face.matches(mutable),
            None => ERASURE_GROUPS.iter().any(|group| {
                group.jvm_name == method.jvm_owner
                    && group
                        .kotlin_names
                        .first()
                        .is_some_and(|read_only| face.matches(read_only))
            }),
        }
    })
}

pub fn to_jvm_type_name(internal: TypeName) -> TypeName {
    builtin_ids()
        .jvm_builtin
        .get(&internal)
        .map_or(internal, |(_, id)| *id)
}

/// Inverse of [`to_jvm_internal`]: normalize a JVM built-in name read from a classpath signature to
/// the canonical Kotlin declaration in its erasure group. Read-only collection declarations are the
/// canonical source identity; mutable siblings remain ordinary Kotlin subtypes.
pub fn to_kotlin_internal(internal: &str) -> &str {
    ERASURE_GROUPS
        .iter()
        .find(|mapping| mapping.jvm_name == internal && mapping.maps_java_source_to_kotlin)
        .and_then(|mapping| mapping.kotlin_names.first().copied())
        .unwrap_or(internal)
}

/// The JVM wrapper (box) class for a primitive `Ty` (`Int` → `java/lang/Integer`), or `None` for a
/// non-primitive. The single source of truth for boxing owners shared by codegen and the front end.
pub fn wrapper_internal(t: Ty) -> Option<&'static str> {
    let scalar = t.scalar_value_repr()?;
    let internal = t.obj_internal()?;
    // The semantic type is the Kotlin classifier. JVM boxing is chosen here, at the backend
    // boundary, from that classifier identity; core never manufactures a wrapper type.
    if scalar == t || t.is_unsigned() {
        kotlin_prim_to_wrapper(&internal.render())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_kotlin_collection_type_name, jvm_collection_to_kotlin_type_name,
        jvm_to_kotlin_builtin_metadata_name, kotlin_prim_to_wrapper,
        mapped_builtin_has_authoritative_kotlin_scope, to_jvm_internal, to_jvm_type_name,
        to_kotlin_internal, wrapper_internal, wrapper_to_kotlin_prim_name,
    };
    use crate::types::{type_name, Ty};

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
            assert_eq!(
                wrapper_to_kotlin_prim_name(type_name(wrapper)),
                Some(internal)
            );
            // The emit-only boxing in `to_jvm_internal` and the `Ty`-keyed `wrapper_internal` agree.
            assert_eq!(to_jvm_internal(internal), wrapper);
            assert_eq!(to_jvm_type_name(type_name(internal)), type_name(wrapper));
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
        assert_eq!(
            to_kotlin_internal("java/util/List"),
            "kotlin/collections/List"
        );
        assert_eq!(
            to_kotlin_internal("java/lang/Comparable"),
            "kotlin/Comparable"
        );
        assert_eq!(
            to_kotlin_internal("java/lang/Void"),
            "java/lang/Void",
            "a Java Void declaration is not Kotlin's bottom type"
        );
        assert_eq!(to_jvm_internal("kotlin/Nothing"), "java/lang/Void");
        assert_eq!(
            jvm_collection_to_kotlin_type_name(type_name("java/util/List")),
            Some(type_name("kotlin/collections/List"))
        );
        assert_eq!(
            jvm_collection_to_kotlin_type_name(type_name("java/util/Map$Entry")),
            Some(type_name("kotlin/collections/Map.Entry"))
        );
        assert_eq!(
            jvm_collection_to_kotlin_type_name(type_name("demo/Foo")),
            None
        );
        assert!(is_kotlin_collection_type_name(type_name(
            "kotlin/collections/List"
        )));
        assert!(is_kotlin_collection_type_name(type_name(
            "kotlin/collections/MutableMap"
        )));
        assert!(is_kotlin_collection_type_name(type_name(
            "kotlin/collections/Map.Entry"
        )));
        assert!(!is_kotlin_collection_type_name(type_name("java/util/List")));
        assert!(!is_kotlin_collection_type_name(type_name("kotlin/String")));
        assert!(!is_kotlin_collection_type_name(type_name("demo/Foo")));
    }

    #[test]
    fn builtin_metadata_owner_unifies_collection_and_mapped_class_lookups() {
        assert_eq!(
            jvm_to_kotlin_builtin_metadata_name(type_name("java/util/List")),
            Some(type_name("kotlin/collections/List"))
        );
        assert_eq!(
            jvm_to_kotlin_builtin_metadata_name(type_name("java/lang/CharSequence")),
            Some(type_name("kotlin/CharSequence"))
        );
        assert_eq!(
            jvm_to_kotlin_builtin_metadata_name(type_name("java/lang/String")),
            Some(type_name("kotlin/String")),
            "the canonical inverse covers mapped classes without Kotlin-only member tables too"
        );
        assert_eq!(
            jvm_to_kotlin_builtin_metadata_name(type_name("java/lang/Cloneable")),
            Some(type_name("kotlin/Cloneable")),
            "interface classification must come from metadata, not a curated name subset"
        );
        assert_eq!(
            jvm_to_kotlin_builtin_metadata_name(type_name("demo/Foo")),
            None
        );
    }

    #[test]
    fn builtin_scope_provenance_is_part_of_the_erasure_mapping() {
        // Collections and String take their source API from the Kotlin declaration. The remaining
        // mapped classes deliberately keep a joined JVM scope until the JVM-visible-method whitelist
        // is implemented. Pinning both sides here prevents a caller from inferring policy from package
        // names or growing another per-class exception list.
        assert!(mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "kotlin/String"
        )));
        assert!(mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "kotlin/collections/List"
        )));
        assert!(mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "kotlin/collections/MutableMap"
        )));
        assert!(!mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "kotlin/CharSequence"
        )));
        assert!(!mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "kotlin/Throwable"
        )));
        assert!(!mapped_builtin_has_authoritative_kotlin_scope(type_name(
            "example/UserType"
        )));
    }
}
