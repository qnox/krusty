//! `typeOf<T>()` through krusty's own code generator and runtime.
//!
//! Every site names one fixed type, so each is a static object the generator emits and the
//! runtime reads: `equals` is structural, `toString` prints what Kotlin/Native's `KTypeImpl`
//! prints, and `classifier`/`isMarkedNullable` are read off the object. Each program is first run
//! on the JVM; the one whose text differs there says what each backend prints.

use super::common::{
    expect_box_ok_files_with_stdlib, expect_box_ok_with_stdlib,
    expect_box_run_with_stdlib_diverging, expect_native_box, expect_native_decline,
    expect_native_sources, expect_native_sources_decline, kotlin_native_box,
};

fn both(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

#[test]
fn a_type_prints_what_kotlin_native_prints() {
    // The native text is kotlinc-native's for the same program. The JVM's, without kotlin-reflect,
    // is the stdlib `TypeReference`'s, which spells Java classes and says reflection is missing.
    let source = "import kotlin.reflect.typeOf\n\
         class G<T>\n\
         class C\n\
         fun box(): String = listOf(\n\
         \x20   typeOf<List<String>>(),\n\
         \x20   typeOf<Map<in Number, out String?>?>(),\n\
         \x20   typeOf<Array<*>>(),\n\
         \x20   typeOf<IntArray>(),\n\
         \x20   typeOf<Int?>(),\n\
         \x20   typeOf<Any>(),\n\
         \x20   typeOf<Unit>(),\n\
         \x20   typeOf<G<C>>(),\n\
         \x20   typeOf<() -> Unit>(),\n\
         ).joinToString(\"|\")\n";
    let jvm = expect_box_run_with_stdlib_diverging(
        source,
        "TypeOfToString",
        "kotlin.collections.List<kotlin.String>|\
         kotlin.collections.Map<in kotlin.Number, out kotlin.String?>?|\
         kotlin.Array<*>|kotlin.IntArray|kotlin.Int?|kotlin.Any|kotlin.Unit|G<C>|\
         kotlin.Function0<kotlin.Unit>",
    );
    assert_eq!(
        jvm,
        "java.util.List<java.lang.String> (Kotlin reflection is not available)|\
         java.util.Map<in java.lang.Number, out java.lang.String?>? (Kotlin reflection is not available)|\
         kotlin.Array<*> (Kotlin reflection is not available)|\
         kotlin.IntArray (Kotlin reflection is not available)|\
         java.lang.Integer? (Kotlin reflection is not available)|\
         java.lang.Object (Kotlin reflection is not available)|\
         kotlin.Unit (Kotlin reflection is not available)|\
         G<C> (Kotlin reflection is not available)|\
         kotlin.jvm.functions.Function0<kotlin.Unit> (Kotlin reflection is not available)",
        "TypeOfToString on the JVM"
    );
}

#[test]
fn two_sites_naming_one_type_are_equal_and_hash_alike() {
    both(
        "import kotlin.reflect.KType\n\
         import kotlin.reflect.typeOf\n\
         class C\n\
         inline fun <reified T> reified(): KType = typeOf<T>()\n\
         fun box(): String {\n\
         \x20   val a = typeOf<List<C>>()\n\
         \x20   val b = reified<List<C>>()\n\
         \x20   if (a != b || b != a) return \"fail equal: $a $b\"\n\
         \x20   if (a.hashCode() != b.hashCode()) return \"fail hash\"\n\
         \x20   if (a == typeOf<List<C?>>()) return \"fail argument nullability\"\n\
         \x20   if (a == typeOf<List<C>?>()) return \"fail nullability\"\n\
         \x20   if (typeOf<Map<in Number, Int>>() == typeOf<Map<out Number, Int>>()) return \"fail variance\"\n\
         \x20   if (typeOf<Array<IntArray>>() == typeOf<Array<Array<Int>>>()) return \"fail argument\"\n\
         \x20   val any: Any = a\n\
         \x20   if (any !is KType) return \"fail is\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "TypeOfEquality",
    );
}

#[test]
fn classifier_and_nullability_are_read_off_the_type() {
    both(
        "import kotlin.reflect.typeOf\n\
         class C\n\
         fun box(): String {\n\
         \x20   val t = typeOf<List<C>?>()\n\
         \x20   if (t.classifier != List::class) return \"fail classifier: ${t.classifier}\"\n\
         \x20   if (!t.isMarkedNullable) return \"fail nullable\"\n\
         \x20   if (typeOf<C>().classifier != C::class) return \"fail class\"\n\
         \x20   if (typeOf<C>().isMarkedNullable) return \"fail not nullable\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "TypeOfMembers",
    );
}

#[test]
fn a_reified_intersection_describes_its_common_supertype() {
    // kotlinc reifies `A & B` to their common supertype, `Any`, on both backends.
    both(
        "// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter\n\
         import kotlin.reflect.KType\n\
         import kotlin.reflect.typeOf\n\
         interface A\n\
         interface B\n\
         class C : A, B\n\
         inline fun <reified K> select(x: K): KType where K : A, K : B = typeOf<K>()\n\
         fun test(a: Any): KType? = if (a is A && a is B) select(a) else null\n\
         fun box(): String {\n\
         \x20   val described = test(C())\n\
         \x20   return if (described == typeOf<Any>()) \"OK\" else \"fail: $described\"\n\
         }\n",
        "TypeOfIntersection",
    );
}

#[test]
fn files_naming_one_type_share_its_object_and_its_class() {
    // Portable: the files' types are equal, hash alike and name one classifier.
    let declarations = (
        "Declarations",
        "import kotlin.reflect.KType\n\
         import kotlin.reflect.typeOf\n\
         class C\n\
         fun here(): KType = typeOf<Map<String, List<C?>>>()\n\
         fun local(): KType { class L; return typeOf<List<L>>() }\n",
    );
    let portable = "import kotlin.reflect.typeOf\n\
         fun box(): String {\n\
         \x20   val there = typeOf<Map<String, List<C?>>>()\n\
         \x20   if (here() != there || here().hashCode() != there.hashCode()) return \"fail equal\"\n\
         \x20   if (here().classifier != there.classifier) return \"fail classifier\"\n\
         \x20   if (local() != local()) return \"fail local\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_files_with_stdlib(&[declarations, ("Use", portable)], "Declarations");
    // Native: one object and one `KClass` for the program, whichever file names them, where the
    // JVM builds a new `KType` per call. A local class's type is its file's own object.
    let shared = "import kotlin.reflect.typeOf\n\
         fun box(): String {\n\
         \x20   val there = typeOf<Map<String, List<C?>>>()\n\
         \x20   if (here() !== there) return \"fail object\"\n\
         \x20   if (here().classifier !== there.classifier) return \"fail class\"\n\
         \x20   if (typeOf<C>().classifier !== typeOf<C>().classifier) return \"fail class read\"\n\
         \x20   if (local() !== local()) return \"fail local\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_native_sources(&[declarations, ("Use", shared)], "OK");
}

#[test]
fn a_program_s_own_ktype_is_read_through_its_overrides() {
    // `KType` is an open interface: a read through it reaches a program's implementation, not the
    // runtime's layout.
    both(
        "import kotlin.reflect.KClassifier\n\
         import kotlin.reflect.KType\n\
         import kotlin.reflect.KTypeProjection\n\
         import kotlin.reflect.typeOf\n\
         class Flipped(val base: KType) : KType {\n\
         \x20   override val annotations: List<Annotation> get() = emptyList()\n\
         \x20   override val classifier: KClassifier? get() = Int::class\n\
         \x20   override val arguments: List<KTypeProjection> get() = emptyList()\n\
         \x20   override val isMarkedNullable: Boolean get() = !base.isMarkedNullable\n\
         }\n\
         fun read(type: KType): String = \"${type.classifier == Int::class}/${type.isMarkedNullable}\"\n\
         fun box(): String {\n\
         \x20   val mine = read(Flipped(typeOf<String>()))\n\
         \x20   if (mine != \"true/true\") return \"fail mine: $mine\"\n\
         \x20   val runtime = read(typeOf<Int>())\n\
         \x20   if (runtime != \"true/false\") return \"fail runtime: $runtime\"\n\
         \x20   if (typeOf<String?>().classifier != String::class) return \"fail classifier\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "TypeOfOwnKType",
    );
}

#[test]
fn a_program_s_own_anonymous_ktype_is_read_through_its_overrides() {
    both(
        "import kotlin.reflect.KClassifier\n\
         import kotlin.reflect.KType\n\
         import kotlin.reflect.KTypeProjection\n\
         import kotlin.reflect.typeOf\n\
         fun nullable(): KType = object : KType {\n\
         \x20   override val annotations: List<Annotation> get() = emptyList()\n\
         \x20   override val classifier: KClassifier? get() = Long::class\n\
         \x20   override val arguments: List<KTypeProjection> get() = emptyList()\n\
         \x20   override val isMarkedNullable: Boolean get() = true\n\
         }\n\
         fun read(type: KType): String = \"${type.classifier == Long::class}/${type.isMarkedNullable}\"\n\
         fun box(): String {\n\
         \x20   val mine = read(nullable())\n\
         \x20   if (mine != \"true/true\") return \"fail mine: $mine\"\n\
         \x20   val runtime = read(typeOf<Long>())\n\
         \x20   if (runtime != \"true/false\") return \"fail runtime: $runtime\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "TypeOfOwnAnonymousKType",
    );
}

#[test]
fn a_ktype_another_file_implements_declines_its_member_reads() {
    // The reading file cannot name another file's override, and the runtime reads only its own
    // objects, so the member read declines rather than reach the runtime with the program's
    // object. Both a named class and an object expression are found in the other file.
    let named = (
        "Def",
        "import kotlin.reflect.KClassifier\n\
         import kotlin.reflect.KType\n\
         import kotlin.reflect.KTypeProjection\n\
         class Flipped(val base: KType) : KType {\n\
         \x20   override val annotations: List<Annotation> get() = emptyList()\n\
         \x20   override val classifier: KClassifier? get() = Int::class\n\
         \x20   override val arguments: List<KTypeProjection> get() = emptyList()\n\
         \x20   override val isMarkedNullable: Boolean get() = !base.isMarkedNullable\n\
         }\n\
         fun mine(base: KType): KType = Flipped(base)\n",
    );
    let anonymous = (
        "Def",
        "import kotlin.reflect.KClassifier\n\
         import kotlin.reflect.KType\n\
         import kotlin.reflect.KTypeProjection\n\
         fun mine(base: KType): KType = object : KType {\n\
         \x20   override val annotations: List<Annotation> get() = emptyList()\n\
         \x20   override val classifier: KClassifier? get() = Int::class\n\
         \x20   override val arguments: List<KTypeProjection> get() = emptyList()\n\
         \x20   override val isMarkedNullable: Boolean get() = !base.isMarkedNullable\n\
         }\n",
    );
    let user = (
        "Use",
        "import kotlin.reflect.KType\n\
         import kotlin.reflect.typeOf\n\
         fun read(type: KType): String = \"${type.classifier == Int::class}/${type.isMarkedNullable}\"\n\
         fun box(): String {\n\
         \x20   val mine = read(mine(typeOf<String>()))\n\
         \x20   if (mine != \"true/true\") return \"fail mine: $mine\"\n\
         \x20   val runtime = read(typeOf<Int>())\n\
         \x20   if (runtime != \"true/false\") return \"fail runtime: $runtime\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
    for def in [named, anonymous] {
        expect_box_ok_files_with_stdlib(&[def, user], "Def");
        expect_native_sources_decline(&[def, user], "another file implements `KType`");
    }
}

#[test]
fn an_iterator_type_is_not_a_mutable_iterator_type() {
    // kotlinc-native tells the two classes apart. The runtime marks both with one `Iterator`
    // marker, so `MutableIterator` has no descriptor of its own here and declines rather than
    // pass for `Iterator`.
    let both_types = "import kotlin.reflect.typeOf\n\
         class X\n\
         fun box(): String {\n\
         \x20   val read = typeOf<Iterator<X>>()\n\
         \x20   val mutable = typeOf<MutableIterator<X>>()\n\
         \x20   return \"${read == mutable}|$read|$mutable\"\n\
         }\n";
    if let Some(answer) = kotlin_native_box(both_types) {
        assert_eq!(
            answer, "false|kotlin.collections.Iterator<X>|kotlin.collections.MutableIterator<X>",
            "kotlinc-native's answer"
        );
    }
    expect_native_decline(
        both_types,
        "TypeOfMutableIterator",
        "has no runtime descriptor",
    );
    let read_only = "import kotlin.reflect.typeOf\n\
         class X\n\
         fun box(): String = \"${typeOf<Iterator<X>>()}\"\n";
    let expected = "kotlin.collections.Iterator<X>";
    if let Some(answer) = kotlin_native_box(read_only) {
        assert_eq!(answer, expected, "kotlinc-native's answer");
    }
    expect_native_box(read_only, "TypeOfIterator", expected);
}
