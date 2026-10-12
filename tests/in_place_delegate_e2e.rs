//! A delegated property whose delegate is a singleton (`Unit`, an object, an enum entry) or a
//! constant needs no `x$delegate` field: kotlinc's `SingletonOrConstantDelegationLowering`
//! evaluates the delegate again in each accessor and adds a private static `get<X>$delegate`
//! method that returns it, named by the property's `JvmPropertySignature.delegate_method`.
//!
//! `val x: Int by Unit` used to fail with "cannot compute JVM frames": the delegate's storage was
//! typed `Unit`, which the JVM spells `void`, so the static field read pushed no receiver for
//! `getValue`. A `Unit`-receiver `getValue` also spelled its receiver `V` in its generic signature
//! and lacked its `@NotNull` parameter annotation.
use super::common;

/// Every storage shape side by side: package `val`/`var`, a member, a constant mixed with an
/// ordinary initializer, a companion property hoisted onto its outer class, and an object's.
const DELEGATES: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    object Token\n\
    enum class Mode { A }\n\
    \n\
    operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1\n\
    operator fun Unit.setValue(thisRef: Any?, property: KProperty<*>, value: Int) {}\n\
    operator fun Unit.getValue(thisRef: Holder, property: KProperty<*>): String = \"s\"\n\
    operator fun Token.getValue(thisRef: Any?, property: KProperty<*>): Int = 2\n\
    operator fun Mode.getValue(thisRef: Any?, property: KProperty<*>): Int = 3\n\
    operator fun Int.getValue(thisRef: Any?, property: KProperty<*>): Int = this\n\
    operator fun String.getValue(thisRef: Any?, property: KProperty<*>): Int = length\n\
    \n\
    val unit1: Int by Unit\n\
    var unitVar: Int by Unit\n\
    val byToken: Int by Token\n\
    val byMode: Int by Mode.A\n\
    val byInt: Int by 7\n\
    val byString: Int by \"four\"\n\
    \n\
    class Holder(val k: Int) {\n\
    \x20   val unit2: String by Unit\n\
    \x20   val token: Int by Token\n\
    \x20   val plain = k + 1\n\
    \x20   val constant: Int by 3\n\
    \x20   companion object {\n\
    \x20       val shared: Int by Unit\n\
    \x20   }\n\
    }\n\
    \n\
    class OnlyUnit {\n\
    \x20   val only: Int by Unit\n\
    }\n\
    \n\
    object Registry {\n\
    \x20   val entry: Int by Unit\n\
    }\n";

#[test]
fn in_place_delegates_are_byte_identical_to_kotlinc() {
    for class in [
        "store/InPlaceDelegatesKt",
        "store/Holder",
        "store/Holder$Companion",
        "store/OnlyUnit",
        "store/Registry",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "InPlaceDelegates",
            DELEGATES,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} byte-identical to kotlinc: {diff}"));
    }
}

/// A member extension's delegate method carries both of its receivers: dispatch first, extension
/// second. The exact class comparison also pins `JvmPropertySignature.delegateMethod` and the
/// absence of `extension$delegate` storage.
const MEMBER_EXTENSION_DELEGATE: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    operator fun Unit.getValue(thisRef: String, property: KProperty<*>): Int = 1\n\
    \n\
    class ExtensionHost {\n\
    \x20   val String.extension: Int by Unit\n\
    }\n";

#[test]
fn member_extension_delegate_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "MemberExtensionDelegate",
        MEMBER_EXTENSION_DELEGATE,
        "store/ExtensionHost",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| {
        panic!("member extension delegate is byte-identical to kotlinc: {diff}")
    });
}

/// The generated delegate method is not generic: a property type parameter is approximated to its
/// bound while its surrounding receiver shape remains in the Signature. Concrete type arguments,
/// a generic dispatch owner, and a nullable value-class receiver keep their own shapes.
const GENERIC_EXTENSION_DELEGATES: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1\n\
    \n\
    @JvmInline value class Value(val raw: Int)\n\
    \n\
    val <T> T.packageValue: Int by Unit\n\
    val <T> List<T>.nestedValue: Int by Unit\n\
    val List<String>.concreteValue: Int by Unit\n\
    val Value?.nullableValue: Int by Unit\n\
    \n\
    class GenericHost<A> {\n\
    \x20   val <T> T.memberValue: Int by Unit\n\
    \x20   val <T> List<T>.nestedMemberValue: Int by Unit\n\
    \x20   val A.enclosingValue: Int by Unit\n\
    }\n";

#[test]
fn generic_extension_delegate_methods_use_erased_receivers() {
    for class in ["store/GenericExtensionDelegatesKt", "store/GenericHost"] {
        common::byte_diff_against_kotlinc_cp(
            "GenericExtensionDelegates",
            GENERIC_EXTENSION_DELEGATES,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

/// A retained property annotation's synthetic marker follows the getter and precedes the delegate
/// method, matching kotlinc's declaration order.
const ANNOTATED_MEMBER_DELEGATE: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    @Target(AnnotationTarget.PROPERTY)\n\
    annotation class Marker\n\
    \n\
    operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1\n\
    \n\
    class AnnotatedHost {\n\
    \x20   @Marker val value: Int by Unit\n\
    }\n";

#[test]
fn annotated_member_marker_precedes_its_delegate_method() {
    common::byte_diff_against_kotlinc_cp(
        "AnnotatedMemberDelegate",
        ANNOTATED_MEMBER_DELEGATE,
        "store/AnnotatedHost",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("annotated member delegate differs from kotlinc: {diff}"));
}

/// The reported shape runs, and the JVM declares the `get<X>$delegate` methods in place of the
/// delegate fields.
const RUN: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    object Token\n\
    \n\
    operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Int = 1\n\
    operator fun Unit.getValue(thisRef: Holder, property: KProperty<*>): String = \"s\"\n\
    operator fun Token.getValue(thisRef: Any?, property: KProperty<*>): Int = 2\n\
    operator fun Int.getValue(thisRef: Any?, property: KProperty<*>): Int = this\n\
    \n\
    val unit1: Int by Unit\n\
    val byInt: Int by 7\n\
    \n\
    class Holder {\n\
    \x20   val unit2: String by Unit\n\
    \x20   val token: Int by Token\n\
    }\n\
    \n\
    fun delegateFields(owner: Class<*>): Int =\n\
    \x20   owner.declaredFields.count { it.name.endsWith(\"\\$delegate\") }\n\
    \n\
    fun delegate(owner: Class<*>, name: String, vararg parameters: Class<*>): Any? {\n\
    \x20   val method = owner.getDeclaredMethod(name, *parameters)\n\
    \x20   method.isAccessible = true\n\
    \x20   return if (parameters.isEmpty()) method.invoke(null) else method.invoke(null, null)\n\
    }\n\
    \n\
    fun box(): String {\n\
    \x20   if (unit1 != 1) return \"fail: unit1\"\n\
    \x20   if (byInt != 7) return \"fail: byInt\"\n\
    \x20   val holder = Holder()\n\
    \x20   if (holder.unit2 != \"s\") return \"fail: unit2\"\n\
    \x20   if (holder.token != 2) return \"fail: token\"\n\
    \x20   if (delegateFields(Holder::class.java) != 0) return \"fail: Holder fields\"\n\
    \x20   val facade = Class.forName(\"store.InPlaceDelegateRunKt\")\n\
    \x20   if (delegateFields(facade) != 0) return \"fail: facade fields\"\n\
    \x20   if (delegate(facade, \"getUnit1\\$delegate\") !== Unit) return \"fail: unit1 delegate\"\n\
    \x20   if (delegate(facade, \"getByInt\\$delegate\") != 7) return \"fail: byInt delegate\"\n\
    \x20   val holderClass = Holder::class.java\n\
    \x20   if (delegate(holderClass, \"getUnit2\\$delegate\", holderClass) !== Unit) return \"fail: unit2 delegate\"\n\
    \x20   if (delegate(holderClass, \"getToken\\$delegate\", holderClass) !== Token) return \"fail: token delegate\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn in_place_delegates_run_and_expose_their_delegate_methods() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(RUN, "InPlaceDelegateRun", &[stdlib], Some(jdk.as_path())),
        "OK"
    );
}

/// A value-class getter mangles to `get<X>-<hash>`, and its delegate method extends that JVM name:
/// a member's is `getResult-<hash>$delegate`, while a value-class extension receiver mangles the
/// method again with its own signature, `getDoubled-<hash>$delegate-<hash>`. The member inventory
/// is compared whole; the classes' bytes still differ from kotlinc in the `$$delegatedProperties`
/// reference signature of a mangled getter, which any value-class delegated property shares.
const VALUE_CLASS_DELEGATES: &str = "package store\n\
    \n\
    import kotlin.reflect.KProperty\n\
    \n\
    @JvmInline\n\
    value class Score(val value: Int)\n\
    \n\
    operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>): Score = Score(1)\n\
    operator fun Unit.getValue(thisRef: Score, property: KProperty<*>): Int = thisRef.value\n\
    \n\
    class Board {\n\
    \x20   val result: Score by Unit\n\
    \x20   internal val hidden: Score by Unit\n\
    }\n\
    \n\
    val Score.doubled: Int by Unit\n";

#[test]
fn value_class_delegate_methods_follow_their_mangled_getters() {
    for (class, method) in [
        (
            "store/Board",
            "method 0x000a getResult-0DMqHrU$delegate(Lstore/Board;)Ljava/lang/Object; None",
        ),
        (
            "store/Board",
            "method 0x000a getHidden-0DMqHrU$main$delegate(Lstore/Board;)Ljava/lang/Object; None",
        ),
        (
            "store/ValueClassDelegatesKt",
            "method 0x000a getDoubled-wWP1W04$delegate-wWP1W04(I)Ljava/lang/Object; None",
        ),
    ] {
        let pair = common::ModuleClassPair::compile(
            &[("ValueClassDelegates.kt", VALUE_CLASS_DELEGATES)],
            class,
        );
        let kotlinc = common::member_table(&pair.kotlinc);
        assert_eq!(
            kotlinc.iter().filter(|row| row.as_str() == method).count(),
            1,
            "{class}: kotlinc declares {method}: {kotlinc:#?}"
        );
        assert_eq!(
            common::member_table(&pair.krusty),
            kotlinc,
            "{class} members"
        );
    }
}
