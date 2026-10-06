//! How a member property is declared, as `@Metadata` records it: modality (`Property.flags` bits
//! 4-5), `lateinit` (bit 12), delegation (bit 15), and whether each accessor is the compiler default
//! (`isNotDefault`, bit 6 of `getter_flags` / `setter_flags`).
//!
//! Measured against kotlinc 2.4.20: an `override` not marked `final` is OPEN even in a final class,
//! an interface property with a getter is OPEN and one without is ABSTRACT, and a getter source
//! declares, a setter body, a `private set`, or a delegated property's accessor is not the default
//! one, while a bodiless `set` is. An accessor word is written only when it
//! differs from the default word derived from the property. A non-default setter records its value
//! parameter: the written name, `value` for a bodiless `private set`, `<set-?>` for a delegated
//! `var`. A `val` whose declared initializer is a non-null literal of its constant type has a
//! constant (bit 13), even a zero value that leaves no field store in the constructor.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

/// Each of `classes` declares the same methods, in the same order, from both compilers.
fn assert_members_identical(stem: &str, src: &str, classes: &[&str]) {
    let compiled =
        common::classes_against_kotlinc_lib(stem, &[("Lib.kt", "package lib\n\nclass Lib\n")], src)
            .expect("reference kotlinc is provisioned");
    for class in classes {
        let (kotlinc, krusty) = compiled
            .method_declarations(class)
            .unwrap_or_else(|| panic!("both compilers write {class}"));
        assert_eq!(krusty, kotlinc, "{class} members");
    }
}

#[test]
fn an_open_property_and_its_non_final_override_record_open_modality() {
    const SRC: &str = "package app\n\
        \n\
        open class B {\n\
        \x20   open val p: Int get() = 1\n\
        \x20   open val q: Int = 1\n\
        }\n\
        \n\
        class C : B() {\n\
        \x20   override val p: Int get() = 2\n\
        \x20   override val q: Int = 2\n\
        }\n\
        \n\
        class D : B() {\n\
        \x20   final override val q: Int = 3\n\
        }\n";
    assert_identical("open_property", SRC, "app/B");
    assert_identical("open_property", SRC, "app/C");
    assert_identical("open_property", SRC, "app/D");
}

#[test]
fn an_abstract_class_property_records_abstract_modality() {
    const SRC: &str = "package app\n\
        \n\
        abstract class B {\n\
        \x20   abstract val a: Int\n\
        \x20   open var o: Int = 0\n\
        }\n";
    assert_identical("abstract_property", SRC, "app/B");
}

#[test]
fn an_interface_property_with_a_getter_is_open() {
    const SRC: &str = "package app\n\
        \n\
        interface I {\n\
        \x20   val x: Int get() = 1\n\
        \x20   val y: Int\n\
        }\n";
    assert_identical("interface_property", SRC, "app/I");
}

#[test]
fn a_declared_getter_is_not_the_default_accessor() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   val x: Int get() = 1\n\
        \x20   var f: Int = 0\n\
        \x20       get() = field\n\
        }\n";
    assert_identical("declared_getter", SRC, "app/A");
}

#[test]
fn a_lateinit_property_records_lateinit() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   lateinit var s: String\n\
        }\n";
    assert_identical("lateinit_property", SRC, "app/A");
}

#[test]
fn a_top_level_lateinit_property_records_lateinit() {
    const SRC: &str = "package app\n\
        \n\
        class Payload\n\
        lateinit var payload: Payload\n";
    assert_identical("top_level_lateinit", SRC, "app/Top_level_lateinitKt");
}

#[test]
fn a_delegated_var_records_its_delegate_field_and_setter_parameter() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.reflect.KProperty\n\
        \n\
        class D {\n\
        \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = \"\"\n\
        \x20   operator fun setValue(t: Any?, p: KProperty<*>, v: String) {}\n\
        }\n\
        \n\
        class A {\n\
        \x20   var x: String by D()\n\
        \x20   val y: String by D()\n\
        }\n";
    assert_identical("delegated_var", SRC, "app/A");
}

#[test]
fn a_delegated_top_level_property_records_its_delegate_field() {
    // Package properties follow the member rule: `isDelegated` (bit 15), and the signature names
    // the `x$delegate` storage with its descriptor. A private one's accessors are private methods.
    const SRC: &str = "package app\n\
        \n\
        import kotlin.reflect.KProperty\n\
        \n\
        class Payload(val code: Int)\n\
        class D(var v: Payload) {\n\
        \x20   operator fun getValue(t: Any?, p: KProperty<*>): Payload = v\n\
        \x20   operator fun setValue(t: Any?, p: KProperty<*>, x: Payload) { v = x }\n\
        }\n\
        \n\
        var stored: Payload by D(Payload(1))\n\
        val read: Payload by D(Payload(2))\n\
        private var hidden: Payload by D(Payload(4))\n\
        \n\
        fun touch(): Int {\n\
        \x20   hidden = stored\n\
        \x20   return hidden.code + read.code\n\
        }\n";
    assert_identical("DelegatedTopLevel", SRC, "app/DelegatedTopLevelKt");
}

#[test]
fn only_a_setter_body_makes_a_declared_setter_not_default() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   var y: Int = 0\n\
        \x20       set\n\
        \x20   var w: Int = 0\n\
        \x20       set(given) { field = given }\n\
        }\n";
    assert_identical("declared_setter", SRC, "app/A");
}

#[test]
fn a_zero_literal_initializer_is_a_constant() {
    const SRC: &str = "package app\n\
        \n\
        class K {\n\
        \x20   val zero: Int = 0\n\
        \x20   val no: Boolean = false\n\
        \x20   val none: Double = 0.0\n\
        \x20   val one: Long = 1L\n\
        \x20   var count: Int = 0\n\
        \x20   val sum: Int = 1 + 2\n\
        \x20   val boxed: Any = 0\n\
        \x20   val maybe: Int? = 0\n\
        }\n";
    assert_identical("ZeroConstants", SRC, "app/K");
}

#[test]
fn a_folded_initializer_is_not_a_constant() {
    const SRC: &str = "package app\n\
        \n\
        val top: Int = 1 + 2\n\
        val literal: Int = -3\n\
        class K {\n\
        \x20   companion object {\n\
        \x20       val hoisted: Int = 2 * 3\n\
        \x20       val kept: Int = 6\n\
        \x20   }\n\
        }\n\
        object O {\n\
        \x20   val shifted: Long = 1L shl 4\n\
        }\n";
    assert_identical("FoldedConstants", SRC, "app/FoldedConstantsKt");
    assert_identical("FoldedConstants", SRC, "app/K");
    assert_identical("FoldedConstants", SRC, "app/K$Companion");
    assert_identical("FoldedConstants", SRC, "app/O");
}

#[test]
fn a_private_top_level_property_with_default_accessors_names_none() {
    const SRC: &str = "package app\n\
        \n\
        private var log = \"\"\n\
        private val fixed = 1\n\
        var shared = 0\n\
        \n\
        fun touch(): Int {\n\
        \x20   log = log + \"x\"\n\
        \x20   shared = 2\n\
        \x20   return fixed + log.length\n\
        }\n";
    assert_identical("PrivateAccessors", SRC, "app/PrivateAccessorsKt");
}

#[test]
fn a_package_accessor_word_carries_the_property_visibility() {
    const SRC: &str = "package app\n\
        \n\
        private val computed: Int get() = 2\n\
        private var guarded = 0\n\
        \x20   set(value) { field = value + 1 }\n\
        private val Int.twice: Int get() = this * 2\n\
        internal val scoped: Int get() = 3\n\
        val open: Int get() = 4\n\
        \n\
        fun touch(): Int {\n\
        \x20   guarded = 1\n\
        \x20   return computed + 3.twice\n\
        }\n";
    assert_identical("AccessorWords", SRC, "app/AccessorWordsKt");
}

#[test]
fn a_package_property_with_one_declared_accessor_names_the_default_other() {
    const SRC: &str = "package app\n\
        \n\
        var written = 0\n\
        \x20   set(value) { field = value + 1 }\n\
        internal var read = 0\n\
        \x20   get() = field\n\
        var narrowed = 0\n\
        \x20   private set(value) { field = value }\n\
        \n\
        fun touch() {\n\
        \x20   narrowed = 1\n\
        }\n";
    assert_identical("DefaultOtherAccessor", SRC, "app/DefaultOtherAccessorKt");
    assert_members_identical("DefaultOtherAccessor", SRC, &["app/DefaultOtherAccessorKt"]);
}

#[test]
fn a_public_var_with_an_internal_set_writes_the_setter_visibility() {
    // The setter is narrowed but not private: `setX` still exists, and the property's setter
    // flags carry `internal` rather than the property's own `public`.
    const SRC: &str = "package app\n\
        \n\
        var narrowed = 0\n\
        \x20   internal set\n\
        var declared = 0\n\
        \x20   internal set(value) { field = value + 1 }\n\
        \n\
        fun touch() {\n\
        \x20   narrowed = 1\n\
        \x20   declared = 2\n\
        }\n";
    assert_identical("InternalSet", SRC, "app/InternalSetKt");
    assert_members_identical("InternalSet", SRC, &["app/InternalSetKt"]);
}

#[test]
fn a_public_var_with_a_bodiless_private_set_publishes_only_its_getter() {
    // The default setter of a bodiless `private set` is private, so kotlinc generates no `setX`
    // and names none in the property's signature; another class in the file writes through the
    // facade's `access$set<X>$p` bridge. `a_private_top_level_property_with_default_accessors_names_none`
    // and `a_package_property_with_one_declared_accessor_names_the_default_other` are the controls.
    const SRC: &str = "package app\n\
        \n\
        var counter = 1\n\
        \x20   private set\n\
        \n\
        fun bump(): Int {\n\
        \x20   counter = counter + 1\n\
        \x20   return counter\n\
        }\n\
        \n\
        class Resetter {\n\
        \x20   fun reset() {\n\
        \x20       counter = 0\n\
        \x20   }\n\
        }\n";
    assert_identical("PrivateSet", SRC, "app/PrivateSetKt");
    assert_members_identical("PrivateSet", SRC, &["app/PrivateSetKt", "app/Resetter"]);
}

#[test]
fn a_generic_extension_setter_parameter_refers_to_its_type_parameter_by_id() {
    const SRC: &str = "package app\n\
        \n\
        class Slot<S>(var content: S)\n\
        var <E> Slot<E>.stored: E\n\
        \x20   get() = content\n\
        \x20   set(value) { content = value }\n\
        var <F> Slot<F>.unnamed: F\n\
        \x20   get() = content\n\
        \x20   set(next) { content = next }\n";
    assert_identical(
        "GenericExtensionSetter",
        SRC,
        "app/GenericExtensionSetterKt",
    );
}

#[test]
fn a_class_lists_its_properties_in_declaration_order() {
    const SRC: &str = "package app\n\
        \n\
        class Slot<S>(var content: S)\n\
        class Host<H>(var plain: H) {\n\
        \x20   var <M> Slot<M>.member: M\n\
        \x20       get() = content\n\
        \x20       set(next) { content = next }\n\
        \x20   val later: H get() = plain\n\
        \x20   val Slot<H>.last: H get() = content\n\
        }\n";
    assert_identical("PropertyListOrder", SRC, "app/Host");
}

/// kotlinc writes `HAS_CONSTANT` only for a `val` whose DECLARED type could be a `const val`
/// type: a constant initializer of a wider declared type has none.
#[test]
fn only_a_const_capable_declared_type_records_a_constant() {
    const SRC: &str = "package app\n\
        \n\
        abstract class Base { abstract val x: Any }\n\
        class K : Base() {\n\
        \x20   override val x: Any = \"abc\"\n\
        \x20   val boxed: Any = 0\n\
        \x20   val text: Any = \"t\"\n\
        \x20   val seq: CharSequence = \"abcd\"\n\
        \x20   val str: String = \"s\"\n\
        \x20   val maybe: String? = \"m\"\n\
        \x20   val unsigned: UInt = 1u\n\
        }\n\
        val top: CharSequence = \"abcd\"\n\
        val topAny: Any = 1\n\
        val topText: String = \"t\"\n";
    assert_identical("ConstCapableTypes", SRC, "app/K");
    assert_identical("ConstCapableTypes", SRC, "app/ConstCapableTypesKt");
}

/// The constants kotlinc's serializer accepts beyond a literal: a number conversion or unary
/// minus applied to a constant, a string template over constants and a concatenation of string
/// literals. Any other operation over constants is not one. The conversions are what the rule
/// is about, so this test has to call them.
#[test]
fn a_conversion_template_or_literal_concatenation_is_a_constant() {
    const SRC: &str = "package app\n\
        \n\
        const val X = 5\n\
        const val S = \"s\"\n\
        class Q {\n\
        \x20   val negated: Int = -X\n\
        \x20   val widened: Long = X.toLong()\n\
        \x20   val signed: Long = (-3).toLong()\n\
        \x20   val folded: Long = (1 + 2).toLong()\n\
        \x20   val real: Double = 0.toDouble()\n\
        \x20   val truncated: Int = 1.0.toInt()\n\
        \x20   val letter: Char = 65.toChar()\n\
        \x20   val template: String = \"$X\"\n\
        \x20   val mixed: String = \"a${S}b\"\n\
        \x20   val joined: String = \"a\" + \"b\" + \"c\"\n\
        \x20   val number: String = \"a\" + 1\n\
        \x20   val inverted: Boolean = !true\n\
        \x20   val sum: Int = 1 + 2\n\
        \x20   val read: Int = X\n\
        }\n\
        val topReal: Double = 0.toDouble()\n\
        val topByte: Byte = 1.toByte()\n\
        val topTemplate: String = \"x$X\"\n\
        val topSum: Int = 1 + 2\n";
    assert_identical("ConstantOperations", SRC, "app/Q");
    assert_identical("ConstantOperations", SRC, "app/ConstantOperationsKt");
}

/// kotlinc's parser folds only literals: `+3` and `"a" + "b"` are constants, while `+X` and
/// `"a" + S` over `const val` reads stay `unaryPlus`/`plus` calls and record no constant.
/// `-X`, `"$S"` and the bare read `S` are constants. A cached literal payload on an ordinary
/// property does not make a later read constant; only a declared const property does.
#[test]
fn a_constant_read_is_not_folded_like_a_literal() {
    const SRC: &str = "package app\n\
        \n\
        const val X = 5\n\
        const val S = \"s\"\n\
        val ordinary = 5\n\
        class R {\n\
        \x20   val plusRead: Int = +X\n\
        \x20   val plusLiteral: Int = +3\n\
        \x20   val minusRead: Int = -X\n\
        \x20   val ordinaryRead: Int = ordinary\n\
        \x20   val joinedRead: String = \"a\" + S\n\
        \x20   val readJoined: String = S + \"a\"\n\
        \x20   val joinedLiteral: String = \"a\" + \"b\"\n\
        \x20   val template: String = \"$S\"\n\
        \x20   val read: String = S\n\
        }\n\
        val topPlusRead: Int = +X\n\
        val topOrdinaryRead: Int = ordinary\n\
        val topJoinedRead: String = \"a\" + S\n\
        val topRead: String = S\n";
    assert_identical("ConstantReads", SRC, "app/R");
    assert_identical("ConstantReads", SRC, "app/ConstantReadsKt");
}
