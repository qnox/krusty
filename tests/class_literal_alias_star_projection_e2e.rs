//! A bare class literal of a generic typealias applies the alias to star projections of its own
//! type parameters and expands that: `Arr::class` for `typealias Arr<T> = Array<T>` is
//! `KClass<Arr<*>>`, which is `KClass<Array<*>>`, and `IntMap::class` for
//! `typealias IntMap<A> = Map<A, Int>` is `KClass<Map<*, Int>>`, not `KClass<Map<*, *>>`.

use super::common;

/// Declared types accept each literal only when it is typed exactly as kotlinc types it.
#[test]
fn a_bare_generic_alias_class_literal_is_star_projected() {
    common::expect_box_same_as_kotlinc(
        "\
import kotlin.reflect.KClass

typealias Arr<T> = Array<T>
typealias Lst<T> = List<T>
typealias NullableElementList<T> = List<T?>
typealias IntMap<A> = Map<A, Int>

val array: KClass<Array<*>> = Arr::class
val list: KClass<List<*>> = Lst::class
val nullableElementList: KClass<List<*>> = NullableElementList::class
val map: KClass<Map<*, Int>> = IntMap::class
val javaArray: Class<Array<*>> = Arr::class.java
val library: KClass<ArrayList<*>> = ArrayList::class

fun box(): String {
    if (array.java.name != \"[Ljava.lang.Object;\") return array.java.name
    if (list.java.name != \"java.util.List\") return list.java.name
    if (nullableElementList.java.name != \"java.util.List\") return nullableElementList.java.name
    if (map.java.name != \"java.util.Map\") return map.java.name
    if (javaArray.name != \"[Ljava.lang.Object;\") return javaArray.name
    if (library.java.name != \"java.util.ArrayList\") return library.java.name
    return \"OK\"
}
",
        "BareGenericAliasClassLiteral",
    );
}

/// With no declared type, signature solving types each property. Each inferred type is then
/// accepted only by the exact declared type kotlinc gives it.
///
/// kotlinc also records the alias abbreviation (`KClass<Arr<*>>`) in the properties' `@Metadata`.
/// krusty does not yet record an abbreviation inside a class-literal type, with or without written
/// type arguments (`Arr<Int>::class` too), so this compares behavior rather than class files.
#[test]
fn an_inferred_bare_generic_alias_class_literal_is_star_projected() {
    common::expect_box_same_as_kotlinc(
        "\
import kotlin.reflect.KClass

typealias Arr<T> = Array<T>
typealias Lst<T> = List<T>
typealias IntMap<A> = Map<A, Int>
class Box<T>
typealias InBox<T> = Box<in T>
typealias OutBox<T> = Box<out T>

val array = Arr::class
val list = Lst::class
val map = IntMap::class
val inBox = InBox::class
val outBox = OutBox::class
val javaArray = Arr::class.java
val library = ArrayList::class
val javaLibrary = HashMap::class.java

fun box(): String {
    val a: KClass<Array<*>> = array
    val l: KClass<List<*>> = list
    val m: KClass<Map<*, Int>> = map
    val i: KClass<Box<*>> = inBox
    val o: KClass<Box<*>> = outBox
    val j: Class<Array<*>> = javaArray
    val k: KClass<ArrayList<*>> = library
    val h: Class<HashMap<*, *>> = javaLibrary
    val names = listOf(
        a.java.name, l.java.name, m.java.name, i.java.name, o.java.name,
        j.name, k.java.name, h.name,
    )
    if (names != listOf(
            \"[Ljava.lang.Object;\", \"java.util.List\", \"java.util.Map\", \"Box\", \"Box\",
            \"[Ljava.lang.Object;\", \"java.util.ArrayList\", \"java.util.HashMap\",
        )
    ) return names.toString()
    return \"OK\"
}
",
        "InferredGenericAliasClassLiteral",
    );
}

/// Generated stars belong only to `::class`. An ordinary callable reference on an object alias
/// selects the normalized singleton target when the alias's unused formal is omitted; supplying
/// that formal still applies the same alias declaration before selecting the member.
#[test]
fn an_object_alias_callable_reference_does_not_apply_class_literal_stars() {
    common::expect_box_same_as_kotlinc(
        "\
object SomeObject {
    fun foo(): String = \"OK\"
}

typealias OnSomeObject<T> = SomeObject

fun box(): String {
    val withTypeArgument = OnSomeObject<Any>::foo
    val withoutTypeArgument = OnSomeObject::foo
    return if (withTypeArgument(SomeObject) == withoutTypeArgument()) {
        withoutTypeArgument()
    } else {
        \"fail\"
    }
}
",
        "CallableReferenceOnObjectTypealias",
    );
}

/// A qualified walk retains the alias declaration selected by the provider record as well as its
/// normalized target classifier. Both declared checking and compact signature inference therefore
/// apply the same generated stars as the equivalent unqualified spelling.
#[test]
fn a_package_qualified_alias_class_literal_is_star_projected() {
    let result = common::compiler_diagnostics(
        &[
            (
                "Alias.kt",
                "package demo\n\
                 typealias Arr<T> = Array<T>\n\
                 typealias NullableElementList<T> = List<T?>\n",
            ),
            (
                "Use.kt",
                "import kotlin.reflect.KClass\n\
                 val declared: KClass<Array<*>> = demo.Arr::class\n\
                 val nullableElements: KClass<List<*>> = demo.NullableElementList::class\n\
                 val inferred = demo.Arr::class\n\
                 val exact: KClass<Array<*>> = inferred\n",
            ),
        ],
        &[common::stdlib_jar()],
    );
    let krusty_errors = common::compiler_errors(&result.krusty_stderr);
    let reference_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, Vec::<common::CompilerError>::new());
    assert_eq!(reference_errors, Vec::<common::CompilerError>::new());
    let krusty_warnings = common::compiler_warnings(&result.krusty_stderr);
    let reference_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(krusty_warnings, Vec::<common::CompilerError>::new());
    assert_eq!(reference_warnings, Vec::<common::CompilerError>::new());
    assert_eq!(result.krusty_code, 0);
    assert_eq!(result.reference_code, 0);
}

/// A selected alias already owns the complete expanded inner type. A package qualifier is not an
/// enclosing classifier expression, so rebuilding the inner type from that syntactic walk would
/// lose the alias-applied outer capture.
#[test]
fn a_package_qualified_inner_alias_preserves_its_captured_outer_projection() {
    let result = common::compiler_diagnostics(
        &[
            (
                "Alias.kt",
                "package demo\n\
                 class Outer<T> { inner class Inner }\n\
                 typealias ItemAlias<T> = Outer<T>.Inner\n",
            ),
            (
                "Use.kt",
                "import kotlin.reflect.KClass\n\
                 import demo.Outer\n\
                 val item: KClass<Outer<*>.Inner> = demo.ItemAlias::class\n",
            ),
        ],
        &[common::stdlib_jar()],
    );
    let krusty_errors = common::compiler_errors(&result.krusty_stderr);
    let reference_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, Vec::<common::CompilerError>::new());
    assert_eq!(reference_errors, Vec::<common::CompilerError>::new());
    let krusty_warnings = common::compiler_warnings(&result.krusty_stderr);
    let reference_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(krusty_warnings, Vec::<common::CompilerError>::new());
    assert_eq!(reference_warnings, Vec::<common::CompilerError>::new());
    assert_eq!(result.krusty_code, 0);
    assert_eq!(result.reference_code, 0);
}

/// The generated star retains the expanded classifier parameter's readable bound. The class
/// literal is therefore assignable where `Box`'s projected argument must remain below `Node`;
/// treating the generated projection as an unbounded `Any?` loses that declaration-owned fact.
#[test]
fn a_bare_alias_class_literal_star_uses_the_target_classifier_bound() {
    common::expect_box_same_as_kotlinc(
        "\
import kotlin.reflect.KClass

open class Node(val text: String)
class Box<T : Node>
typealias Alias<T> = Box<T>

fun acceptsBound(type: KClass<out Box<out Node>>): String = \"OK\"

fun box(): String = acceptsBound(Alias::class)
",
        "BoundedBareAliasClassLiteral",
    );
}

/// A nullable alias expansion does not itself denote a classifier, even though the target beneath
/// the nullable wrapper does. The complete diagnostic must match kotlinc and remain singular.
#[test]
fn a_nullable_alias_expansion_is_not_a_class_literal_left_side() {
    let source = "typealias MaybeList<T> = List<T>?\n\
val rejected = MaybeList::class\n";
    let result = common::compiler_diagnostics_with_shared_args(
        &[("NullableAliasClassLiteral.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let krusty = common::compiler_errors(&result.krusty_stderr);
    let kotlinc = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty, kotlinc, "{}", result.krusty_stderr);
    assert_eq!(
        kotlinc,
        [common::CompilerError {
            file: "NullableAliasClassLiteral.kt".to_string(),
            line: 2,
            column: 16,
            message: "type in a class literal cannot be nullable.".to_string(),
        }]
    );
    assert_eq!(result.krusty_code, result.reference_code);
}
