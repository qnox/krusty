//! `companion { … }` block members are static members of the classifier that declares the block:
//! kotlinc places their functions, accessors and backing fields on that class (initialized by the
//! class's `<clinit>`) and records them in the class's `@Metadata`, while a written
//! `companion fun C.f()` stays on the file facade.
use super::common;

const LANGUAGE: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n";

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(&format!("{LANGUAGE}{src}"), "Main")
}

/// Compile `src` with kotlinc (the feature enabled by flag) and krusty, and require each of
/// `classes` to carry exactly kotlinc's fields and methods, in class-file order with their access
/// flags, descriptors and generic signatures, and exactly kotlinc's `@Metadata`.
fn assert_members_and_metadata_match_kotlinc(stem: &str, src: &str, classes: &[&str]) {
    assert_members_and_metadata_match_kotlinc_on(stem, src, classes, &[common::stdlib_jar()]);
}

/// [`assert_members_and_metadata_match_kotlinc`] over `classpath`, also requiring each field's
/// `ConstantValue`. Returns the comparisons for further checks.
fn assert_members_and_metadata_match_kotlinc_on(
    stem: &str,
    src: &str,
    classes: &[&str],
    classpath: &[std::path::PathBuf],
) -> Vec<common::ReferenceComparison> {
    let source = format!("{LANGUAGE}{src}");
    let mut comparisons = Vec::new();
    for class in classes {
        let comparison = common::compare_with_kotlinc_plugin(
            stem,
            &source,
            class,
            classpath,
            "17",
            &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
        )
        .expect("reference kotlinc and javap are provisioned");
        assert_eq!(
            common::member_table(&comparison.krusty_bytes),
            common::member_table(&comparison.reference_bytes),
            "{class}: kotlinc's member table"
        );
        assert_eq!(
            common::raw_kotlin_metadata(&comparison.krusty_bytes),
            common::raw_kotlin_metadata(&comparison.reference_bytes),
            "{class}: kotlinc's @Metadata"
        );
        comparisons.push(comparison);
    }
    comparisons
}

#[test]
fn block_members_are_static_members_of_their_class() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       val v: String = \"O\"\n\
        \x20       fun f() = v + g()\n\
        \x20   }\n\
        }\n\
        companion fun A.g() = \"K\"\n\
        fun box() = A.f()\n";
    assert_members_and_metadata_match_kotlinc("BlockMembers", SRC, &["A", "BlockMembersKt"]);
    assert_eq!(run(SRC).expect("block members"), "OK");
}

#[test]
fn block_member_calls_block_member_and_companion_extension_unqualified() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(s: String) = g(s) + h()\n\
        \x20       fun h() = \"K\"\n\
        \x20   }\n\
        }\n\
        companion fun A.g(s: String) = s\n\
        fun box() = A.f(\"O\")\n";
    assert_eq!(run(SRC).expect("unqualified companion calls"), "OK");
}

/// Private block members are used from the class's own members and block, and from the classes
/// kotlinc reaches them through accessors from: a nested class, a lambda in it, and the carrier of
/// a reference written in a block body.
#[test]
fn instance_member_calls_inherited_and_private_block_members() {
    const SRC: &str = "open class Base {\n\
        \x20   companion { fun base() = \"O\" }\n\
        }\n\
        class C : Base() {\n\
        \x20   companion {\n\
        \x20       private val k = \"K\"\n\
        \x20       private fun own() = k\n\
        \x20       fun viaReferences() = (::own)() + (::k)()\n\
        \x20   }\n\
        \x20   fun ok() = base() + own()\n\
        \x20   class N {\n\
        \x20       fun viaNested() = own() + k\n\
        \x20       fun viaLambda(): String {\n\
        \x20           val l = { own() + k }\n\
        \x20           return l()\n\
        \x20       }\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val r = C().ok() + C.viaReferences() + C.N().viaNested() + C.N().viaLambda()\n\
        \x20   return if (r == \"OKKKKKKK\") \"OK\" else r\n\
        }\n";
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "PrivateBlockMembers");
}

/// A private block member used from another class — a nested class, a lambda there, an anonymous
/// object, and the carrier of a `::f`, `::p` or `::v` reference written in a block body or of a
/// `C::f` in a member — goes through its class's synthetic `access$…` accessors, which follow the
/// class's members in first-use order. The carriers are named after the declaring class.
#[test]
fn private_block_members_used_from_other_classes_go_through_class_accessors() {
    const SRC: &str = "abstract class Task {\n\
        \x20   abstract fun run(): Int\n\
        }\n\
        class C {\n\
        \x20   fun member(): Int {\n\
        \x20       val g = C::f\n\
        \x20       val h = C::p\n\
        \x20       return g(1) + h()\n\
        \x20   }\n\
        \x20   class N {\n\
        \x20       fun nested(): Int {\n\
        \x20           v = 4\n\
        \x20           val l = { y: Int -> f(y) + p }\n\
        \x20           return l(1) + v\n\
        \x20       }\n\
        \x20       fun anonymous(): Int {\n\
        \x20           val o = object : Task() {\n\
        \x20               override fun run(): Int = f(5) + v\n\
        \x20           }\n\
        \x20           return o.run()\n\
        \x20       }\n\
        \x20   }\n\
        \x20   companion {\n\
        \x20       private fun f(x: Int): Int = x\n\
        \x20       private val p: Int = 2\n\
        \x20       private var v: Int = 3\n\
        \x20       fun lam(): Int {\n\
        \x20           val g = { y: Int -> f(y) + p + v }\n\
        \x20           return g(1)\n\
        \x20       }\n\
        \x20       fun ref(): Int {\n\
        \x20           val g = ::f\n\
        \x20           return g(1)\n\
        \x20       }\n\
        \x20       fun pref(): Int {\n\
        \x20           val g = ::p\n\
        \x20           return g()\n\
        \x20       }\n\
        \x20       fun vref(): Int {\n\
        \x20           val g = ::v\n\
        \x20           g.set(6)\n\
        \x20           return g.get()\n\
        \x20       }\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val r = C.lam() + C.ref() + C.pref() + C().member() + C.N().nested() + C.vref() +\n\
        \x20       C.N().anonymous()\n\
        \x20   return if (r == 36) \"OK\" else \"fail \" + r\n\
        }\n";
    const USERS: [&str; 7] = [
        "C$N",
        "C$N$anonymous$o$1",
        "C$member$g$1",
        "C$member$h$1",
        "C$ref$g$1",
        "C$pref$g$1",
        "C$vref$g$1",
    ];
    let mut classes = vec!["C"];
    classes.extend(USERS);
    let comparisons = assert_members_and_metadata_match_kotlinc_on(
        "BlockAccessors",
        SRC,
        &classes,
        &[common::stdlib_jar()],
    );
    for header in [
        "public static final int access$f(int);",
        "public static final int access$getP$p();",
        "public static final void access$setV$p(int);",
        "public static final int access$getV$p();",
    ] {
        let accessor = common::method_block(&comparisons[0].reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares C.{header}");
        assert_eq!(
            common::method_block(&comparisons[0].krusty, header),
            accessor,
            "C.{header}"
        );
    }
    for (class, comparison) in USERS.iter().zip(&comparisons[1..]) {
        assert_eq!(
            common::member_blocks(&comparison.krusty),
            common::member_blocks(&comparison.reference),
            "{class}: kotlinc's members"
        );
    }
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "BlockAccessors");
}

/// A private block function of an interface is a private static of the interface. Every other
/// class reaches it through the interface's `access$…` accessor, which as an interface method is
/// not `final` and is named by `InterfaceMethodref`s: the carriers of references written in a
/// default member and in the block, and a nested class and its lambda.
#[test]
fn private_interface_block_functions_go_through_interface_accessors() {
    const SRC: &str = "interface Shape {\n\
        \x20   fun area(): Int {\n\
        \x20       val g = Shape::scale\n\
        \x20       return g(3) + twice()\n\
        \x20   }\n\
        \x20   class Nested {\n\
        \x20       fun viaNested(): Int {\n\
        \x20           val l = { y: Int -> scale(y) }\n\
        \x20           return l(1) + scale(2)\n\
        \x20       }\n\
        \x20   }\n\
        \x20   companion {\n\
        \x20       private fun scale(x: Int): Int = x * 2\n\
        \x20       private fun twice(): Int {\n\
        \x20           val h = ::scale\n\
        \x20           return h(4)\n\
        \x20       }\n\
        \x20   }\n\
        }\n\
        class Square : Shape\n\
        fun box(): String {\n\
        \x20   val r = Square().area() + Shape.Nested().viaNested()\n\
        \x20   return if (r == 20) \"OK\" else \"fail\"\n\
        }\n";
    const USERS: [&str; 3] = ["Shape$Nested", "Shape$area$g$1", "Shape$twice$h$1"];
    let mut classes = vec!["Shape"];
    classes.extend(USERS);
    let comparisons = assert_members_and_metadata_match_kotlinc_on(
        "InterfaceBlockAccessors",
        SRC,
        &classes,
        &[common::stdlib_jar()],
    );
    let header = "public static int access$scale(int);";
    let accessor = common::method_block(&comparisons[0].reference, header);
    assert!(!accessor.is_empty(), "kotlinc declares Shape.{header}");
    assert_eq!(
        common::method_block(&comparisons[0].krusty, header),
        accessor,
        "Shape.{header}"
    );
    for (class, comparison) in USERS.iter().zip(&comparisons[1..]) {
        assert_eq!(
            common::member_blocks(&comparison.krusty),
            common::member_blocks(&comparison.reference),
            "{class}: kotlinc's members"
        );
    }
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "InterfaceBlockAccessors");
}

/// kotlinc treats a block property of an interface as an interface property: without a getter
/// body it is abstract, initializer or not, and an abstract interface property cannot be private.
/// So no private interface storage exists to reach through accessors; a private property with a
/// getter body has no backing field and stays legal.
#[test]
fn private_interface_block_property_without_a_getter_is_rejected() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        interface Registry {\n\
        \x20   companion {\n\
        \x20       private val seed: Int = 1\n\
        \x20       private lateinit var count: String\n\
        \x20       private val derived: Int get() = 3\n\
        \x20       private fun next(): Int = derived\n\
        \x20   }\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn private_block_members_are_owned_by_their_class_not_their_file() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class Vault {\n\
        \x20   companion {\n\
        \x20       private fun hiddenCall(): String = \"O\"\n\
        \x20       private val hiddenValue: String = \"K\"\n\
        \x20   }\n\
        \x20   fun inside(): String = hiddenCall() + hiddenValue\n\
        }\n\
        private companion fun Vault.fileVisible(): String = \"OK\"\n\
        fun writtenControl(): String = Vault.fileVisible()\n\
        fun outside(): String {\n\
        \x20   return Vault.hiddenCall() + Vault.hiddenValue\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn private_block_member_access_uses_the_declaring_class_lexical_boundary() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class Host {\n\
        \x20   companion { private fun ownerOnly(): String = \"OK\" }\n\
        \x20   class Nested { fun legal(): String = ownerOnly() }\n\
        \x20   class NestedOwner {\n\
        \x20       companion { private val innerOnly: String = \"hidden\" }\n\
        \x20   }\n\
        \x20   fun illegalFromEnclosing(): String = NestedOwner.innerOnly\n\
        \x20   class Sibling { fun illegalFromSibling(): String = NestedOwner.innerOnly }\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn private_block_members_are_not_visible_from_another_file() {
    const DECLARATION: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class Secret { companion { private fun hidden(): String = \"hidden\" } }\n";
    const USE: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        fun illegal(): String = Secret.hidden()\n";
    common::assert_errors_match_kotlinc(
        &[("Secret.kt", DECLARATION), ("Use.kt", USE)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn kotlin_classifier_scope_does_not_inherit_companion_block_members() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        open class Parent {\n\
        \x20   companion { fun marker(): String = \"not inherited\" }\n\
        }\n\
        class Child : Parent()\n\
        fun invalid(): String { return Child.marker() }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn block_property_initializes_with_its_class_not_the_file() {
    const SRC: &str = "var initialized = false\n\
        fun initialize(): String {\n\
        \x20   initialized = true\n\
        \x20   return \"\"\n\
        }\n\
        class Foo {\n\
        \x20   companion { val p = initialize() }\n\
        }\n\
        companion val Foo.greeting: String = \"hi\"\n\
        fun box(): String {\n\
        \x20   if (Foo.greeting != \"hi\") return \"greeting\"\n\
        \x20   if (initialized) return \"a companion extension initialized its classifier\"\n\
        \x20   Foo.p\n\
        \x20   return if (initialized) \"OK\" else \"reading a block property did not\"\n\
        }\n";
    assert_eq!(run(SRC).expect("initialization order"), "OK");
}

#[test]
fn nested_class_block_members_belong_to_the_nested_class() {
    const SRC: &str = "class Outer {\n\
        \x20   class Nested {\n\
        \x20       companion { val v = \"OK\" }\n\
        \x20   }\n\
        }\n\
        fun box() = Outer.Nested.v\n";
    assert_members_and_metadata_match_kotlinc("NestedBlock", SRC, &["Outer$Nested"]);
    assert_eq!(run(SRC).expect("nested block"), "OK");
}

#[test]
fn block_property_beside_companion_object_property_keeps_its_field_name() {
    const SRC: &str = "class E {\n\
        \x20   companion { val value = \"O\" }\n\
        \x20   companion object { val value = \"K\" }\n\
        }\n\
        fun box() = E.value + E.Companion.value\n";
    assert_members_and_metadata_match_kotlinc("FieldNames", SRC, &["E"]);
    assert_eq!(run(SRC).expect("field names"), "OK");
}

#[test]
fn property_reference_to_block_property_reads_its_class() {
    const SRC: &str = "class C {\n\
        \x20   companion { var p = \"FAIL\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   C::p.set(\"OK\")\n\
        \x20   return (C::p)()\n\
        }\n";
    assert_eq!(run(SRC).expect("property reference"), "OK");
}

/// A block property with custom accessors is read and written through its class's static
/// accessors, unqualified from the class body and qualified from outside it.
#[test]
fn block_property_accessors_are_statics_of_their_class() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       val p: String get() = \"O\"\n\
        \x20       var backing: String = \"\"\n\
        \x20       var q: String\n\
        \x20           get() = backing\n\
        \x20           set(value) { backing = value }\n\
        \x20   }\n\
        \x20   fun g(): String { q = \"K\"; return p + q }\n\
        }\n\
        fun box(): String {\n\
        \x20   val r = C().g() + C.p + C.q\n\
        \x20   return if (r == \"OKOK\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("block property accessors"), "OK");
}

/// A block property with exactly one declared accessor keeps the compiler-default other one, and
/// other classes reach each side through its own accessor.
#[test]
fn block_property_with_one_declared_accessor_keeps_the_default_other() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       var log: Int = 0\n\
        \x20       var written: Int = 1\n\
        \x20           set(value) { log = value }\n\
        \x20       var read: Int = 2\n\
        \x20           get() = log + 10\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   C.written = 3\n\
        \x20   C.read = 4\n\
        \x20   val r = C.written + C.read\n\
        \x20   return if (r == 14) \"OK\" else r.toString()\n\
        }\n";
    assert_members_and_metadata_match_kotlinc(
        "OneDeclaredAccessor",
        SRC,
        &["C", "OneDeclaredAccessorKt"],
    );
    assert_eq!(run(SRC).expect("one declared block accessor"), "OK");
}

/// A stored block property's declared accessors read and write its static backing field through
/// `field`, like a top-level property's.
#[test]
fn block_property_accessors_use_their_backing_field() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       var written: Int = 0\n\
        \x20           set(value) { field = value + 1 }\n\
        \x20       var read: Int = 0\n\
        \x20           get() = field + 10\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   C.written = 1\n\
        \x20   C.read = 2\n\
        \x20   val r = C.written + C.read\n\
        \x20   return if (r == 14) \"OK\" else r.toString()\n\
        }\n";
    assert_members_and_metadata_match_kotlinc(
        "BlockBackingField",
        SRC,
        &["C", "BlockBackingFieldKt"],
    );
    assert_eq!(run(SRC).expect("block accessor backing field"), "OK");
}

#[test]
fn bare_classifier_call_invokes_block_and_extension_operators() {
    const SRC: &str = "class C(val s: String) {\n\
        \x20   companion { operator fun invoke(i: Int) = \"O\" }\n\
        \x20   companion object { operator fun invoke(c: Char) = \"FAIL\" }\n\
        }\n\
        class E\n\
        companion operator fun E.invoke(s: String) = s\n\
        fun box() = C(\"\").s + C(1) + E(\"K\")\n";
    assert_eq!(run(SRC).expect("implicit companion invoke"), "OK");
}

#[test]
fn companion_extensions_with_context_parameters_see_their_classifier_scope() {
    const SRC: &str = "class A { val k = \"K\" }\n\
        class C\n\
        context(a: A)\n\
        companion val C.o get() = \"O\"\n\
        context(a: A)\n\
        companion fun C.k() = a.k\n\
        context(_: A)\n\
        companion fun C.ok(): String = o + k()\n\
        fun <T, R> within(value: T, block: T.() -> R): R = value.block()\n\
        fun box() = within(A()) { C.ok() }\n";
    assert_eq!(run(SRC).expect("context companion extensions"), "OK");
}

#[test]
fn references_inside_a_block_name_block_members() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       lateinit var value: String\n\
        \x20       fun initialized() = ::value.isInitialized\n\
        \x20       fun k() = \"K\"\n\
        \x20       fun ref() = ::k\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (C.initialized()) return \"initialized\"\n\
        \x20   C.value = \"O\"\n\
        \x20   return C.value + C.ref()()\n\
        }\n";
    assert_eq!(run(SRC).expect("block member references"), "OK");
}

#[test]
fn static_scope_writes_and_increments_companion_properties() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       var n = 0\n\
        \x20       var s = \"\"\n\
        \x20       fun bump() { n++; ++n; n += 2; s += \"K\" }\n\
        \x20   }\n\
        }\n\
        companion var C.m = 1\n\
        companion fun C.more() { m++; m *= 3 }\n\
        fun box(): String {\n\
        \x20   C.bump()\n\
        \x20   C.more()\n\
        \x20   return if (\"${C.n}${C.s}${C.m}\" == \"4K6\") \"OK\" else \"${C.n}${C.s}${C.m}\"\n\
        }\n";
    assert_eq!(run(SRC).expect("static-scope writes"), "OK");
}

#[test]
fn static_scope_write_rung_precedes_an_outer_receiver() {
    const SRC: &str = "class Outer {\n\
        \x20   var assigned = \"outer\"\n\
        \x20   var added = 1\n\
        \x20   var incremented = 2\n\
        \x20   inner class Inner {\n\
        \x20       companion {\n\
        \x20           var assigned = \"inner\"\n\
        \x20           var added = 10\n\
        \x20           var incremented = 20\n\
        \x20       }\n\
        \x20       fun update(): String {\n\
        \x20           assigned = \"set\"\n\
        \x20           added += 5\n\
        \x20           incremented++\n\
        \x20           return this@Outer.assigned + \",\" + Inner.assigned + \",\" +\n\
        \x20               this@Outer.added + \",\" + Inner.added + \",\" +\n\
        \x20               this@Outer.incremented + \",\" + Inner.incremented\n\
        \x20       }\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val result = Outer().let { it.Inner().update() }\n\
        \x20   return if (result == \"outer,set,1,15,2,21\") \"OK\" else result\n\
        }\n";
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "OrderedStaticWrites");
}

#[test]
fn companion_modifier_and_context_clause_accept_both_prefix_orders() {
    const SRC: &str = "annotation class Mark\n\
        class Ambient\n\
        class Coordinate\n\
        context(_: Ambient) @Mark companion public fun Coordinate.first() = \"O\"\n\
        companion @Mark public context(_: Ambient) val Coordinate.second get() = \"K\"\n\
        fun <T, R> within(value: T, block: T.() -> R): R = value.block()\n\
        fun box() = within(Ambient()) { Coordinate.first() + Coordinate.second }\n";
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "CompanionContextPrefixes");
}

#[test]
fn companion_extension_has_no_value_receiver() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C { fun m() = 1 }\n\
        companion fun C.f() = this\n\
        companion fun C.g() = m()\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn kotlinc_resolves_block_members_from_krusty_metadata() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = \"\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   A.bar = \"K\"\n\
        \x20   return A.foo() + A.bar\n\
        }\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{LIB}"), "Lib");
    let dir = common::scratch_dir().expect("scratch directory");
    let lib = dir.join("lib");
    for (name, bytes) in &classes {
        let path = lib.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class directory")).expect("mkdir");
        std::fs::write(&path, bytes).expect("write class file");
    }
    let main = dir.join("Main.kt");
    std::fs::write(&main, MAIN).expect("write main");
    let args = [
        "-d".to_string(),
        dir.join("out").to_string_lossy().into_owned(),
        "-XXLanguage:+CompanionBlocksAndExtensions".to_string(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        main.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        return;
    };
    assert_eq!(
        code, 0,
        "kotlinc rejected krusty's class metadata: {stderr}"
    );
}

/// A block declared in another file of the module is reached through the module's declarations:
/// calls (with a default argument), reads, writes and property references all name the declaring
/// class, never a file facade.
#[test]
fn block_members_from_another_file_are_statics_of_their_class() {
    const DECLARING: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(suffix: String = \"K\"): String = \"O\" + suffix\n\
        \x20       var v: String = \"\"\n\
        \x20       val p: String get() = \"!\"\n\
        \x20   }\n\
        }\n";
    const USING: &str = "fun box(): String {\n\
        \x20   A.v = A.f()\n\
        \x20   val read = A::p\n\
        \x20   val written = A::v\n\
        \x20   val r = A.f(\"k\") + written() + read()\n\
        \x20   return if (r == \"OkOK!\") \"OK\" else r\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("A.kt", &format!("{LANGUAGE}{DECLARING}")),
            ("Main.kt", &format!("{LANGUAGE}{USING}")),
        ],
        "SeparateFileBlock",
    );
}

#[test]
fn library_block_members_are_statics_of_their_class() {
    const LIB: &str = "var initialized = false\n\
        fun initialize(): String { initialized = true; return \"\" }\n\
        open class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = initialize()\n\
        \x20       const val SUFFIX = \"!\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "class B : A() {\n\
        \x20   companion { fun own() = \"K\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (initialized) return \"a companion block initialized before its class\"\n\
        \x20   A.bar = B.own()\n\
        \x20   if (!initialized) return \"writing A.bar did not initialize A\"\n\
        \x20   return if (A.SUFFIX == \"!\") A.foo() + A.bar else \"A.SUFFIX is \" + A.SUFFIX\n\
        }\n";
    let result = common::expect_box_run_against(
        "companion-block-library",
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// A library block's `const val` is a compile-time constant where it is used: kotlinc folds `A.N`
/// into another `const val`'s `ConstantValue`, into an annotation argument and into an ordinary
/// expression, and none of them reads a field of `A`.
#[test]
fn library_block_const_is_a_compile_time_constant() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       const val N = 7\n\
        \x20   }\n\
        }\n\
        annotation class Tag(val n: Int)\n";
    const MAIN: &str = "const val M = A.N + 1\n\
        @Tag(A.N) fun tagged() {}\n\
        fun read(): Int = A.N\n\
        fun box(): String = if (M == 8 && read() == 7) \"OK\" else \"M is \" + M\n";
    let library = common::kotlinc_library(&format!("{LANGUAGE}{LIB}"))
        .expect("reference kotlinc is provisioned");
    let comparisons = assert_members_and_metadata_match_kotlinc_on(
        "LibraryConst",
        MAIN,
        &["LibraryConstKt"],
        &[library, common::stdlib_jar()],
    );
    let comparison = &comparisons[0];
    for method in ["read()", "tagged()"] {
        assert_eq!(
            common::method_instructions(&comparison.krusty, method),
            common::method_instructions(&comparison.reference, method),
            "LibraryConstKt.{method}: kotlinc's instructions"
        );
    }
    let annotations = |javap: &str| {
        javap
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("Tag("))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        annotations(&comparison.krusty),
        annotations(&comparison.reference),
        "tagged's annotation argument"
    );
}

/// The same uses as [`block_members_from_another_file_are_statics_of_their_class`] against a
/// compiled library: krusty builds the library, reads its block members back through its classpath
/// provider, and names their declaring class for calls with defaults, reads, writes and references.
#[test]
fn compiled_library_block_members_are_statics_of_their_class() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(suffix: String = \"K\"): String = \"O\" + suffix\n\
        \x20       var v: String = \"\"\n\
        \x20       val p: String get() = \"!\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   A.v = A.f()\n\
        \x20   val read = A::p\n\
        \x20   val written = A::v\n\
        \x20   val r = A.f(\"k\") + written() + read()\n\
        \x20   return if (r == \"OkOK!\") \"OK\" else r\n\
        }\n";
    let result = common::expect_box_run_against(
        "companion-block-library-uses",
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

#[test]
fn static_scope_selects_nearest_applicable_associated_function() {
    // Inside a class, its own block members precede its companion object's members and top-level
    // functions; a farther classifier's block member is reached when the nearer one does not
    // apply, and an instance member still precedes every block member.
    const SRC: &str = "fun f() = \"top\"\n\
        fun h(x: Int) = \"top-h\"\n\
        open class Base { companion { fun g(s: String) = \"base-g\"; fun k() = \"base-k\" } }\n\
        class C : Base() {\n\
        \x20   companion { fun f() = \"block\"; fun g(i: Int) = \"c-g\"; fun k() = \"c-k\" }\n\
        \x20   companion object { fun f() = \"obj\"; fun h(x: Int) = \"obj-h\" }\n\
        \x20   fun t1() = f()\n\
        \x20   fun t2() = g(\"s\")\n\
        \x20   fun t3() = k()\n\
        }\n\
        class D {\n\
        \x20   fun f(x: Int = 0) = \"member\"\n\
        \x20   companion { fun f() = \"block\" }\n\
        \x20   fun t() = f()\n\
        }\n\
        class E { companion { fun h(x: Int) = \"block-h\" } }\n\
        companion fun E.t() = h(1)\n\
        fun box(): String {\n\
        \x20   val r = C().t1() + \",\" + C().t2() + \",\" + C().t3() + \",\" + D().t() + \",\" + E.t()\n\
        \x20   return if (r == \"block,base-g,c-k,member,block-h\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("static scope precedence"), "OK");
}

#[test]
fn associated_declarations_are_not_members_of_instances() {
    // A companion extension names its classifier, not a value of it: kotlinc rejects `C().f()`.
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C\n\
        companion fun C.f() = \"f\"\n\
        companion val C.q get() = 2\n\
        fun use() {\n\
        \x20   C().f()\n\
        \x20   C().q\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

/// A library block property with a context parameter is read through its class's static getter,
/// which takes the context argument and nothing else.
#[test]
fn library_block_property_takes_its_context_argument() {
    const LIB: &str = "class Ctx(val s: String)\n\
        class A {\n\
        \x20   companion {\n\
        \x20       context(c: Ctx) val greeting: String get() = c.s + \"K\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun <T, R> within(value: T, block: T.() -> R): R = value.block()\n\
        fun box(): String = within(Ctx(\"O\")) { A.greeting }\n";
    let result = common::expect_box_run_against_kotlinc(
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// An `internal` block property of a kotlinc-built library is invisible outside its module even
/// though its JVM accessor is public: both compilers reject the read with the same diagnostics,
/// and a friend module reads it.
#[test]
fn library_internal_block_property_is_invisible_outside_its_module() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       internal val hidden: Int get() = 1\n\
        \x20       val shown: Int get() = 2\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun read(): Int = A.hidden + A.shown\n";
    let library = common::kotlinc_library(&format!("{LANGUAGE}{LIB}"))
        .expect("reference kotlinc is provisioned");
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let main = format!("{LANGUAGE}{MAIN}");
    let result = common::compiler_diagnostics_with_reference_args(
        &[("Main.kt", &main)],
        &classpath,
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
    common::expect_identical_rejection(&result, "internal library block property");
    assert_eq!(
        common::front_end_diagnostics_with_friend_paths(
            &main,
            &classpath,
            std::slice::from_ref(&library),
            None,
        ),
        Vec::<String>::new()
    );
}

/// An overloaded `::f` naming a block function from its class's static scope is selected by the
/// expected function type, in an inferred signature as in a body.
#[test]
fn static_scope_reference_is_selected_by_the_expected_function_type() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"int\"\n\
        \x20       fun f(x: String): String = \"string\"\n\
        \x20   }\n\
        \x20   fun pick() = apply1(::f)\n\
        \x20   fun body(): String {\n\
        \x20       val g: (Int) -> String = ::f\n\
        \x20       return g(1)\n\
        \x20   }\n\
        }\n\
        fun apply1(g: (String) -> String) = g(\"s\")\n\
        fun box(): String {\n\
        \x20   val r = C().pick() + \",\" + C().body()\n\
        \x20   return if (r == \"string,int\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("expected reference type"), "OK");
}

/// Without an expected type the same overloaded `::f` is kotlinc's ambiguity, reported with both
/// candidates.
#[test]
fn static_scope_reference_without_an_expected_type_is_ambiguous() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"int\"\n\
        \x20       fun f(x: String): String = \"string\"\n\
        \x20   }\n\
        \x20   fun pick() = ::f\n\
        }\n";
    common::assert_error_blocks_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn qualified_block_function_reference_is_selected_in_compact_signatures() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"O\"\n\
        \x20       fun f(x: String): String = x\n\
        \x20   }\n\
        }\n\
        fun selected(): (Int) -> String = C::f\n\
        fun box() = selected()(1) + \"K\"\n";
    assert_eq!(run(SRC).expect("qualified block function reference"), "OK");
}

#[test]
fn qualified_overloaded_block_function_reference_has_one_ambiguity() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"int\"\n\
        \x20       fun f(x: String): String = \"string\"\n\
        \x20   }\n\
        }\n\
        fun ref() = C::f\n";
    common::assert_error_blocks_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

/// A class's static scope directly follows its own instance receiver in the unqualified tower, so
/// an inner class's block declaration shadows the outer class's member of the same name, for a
/// call, a read and a `::f` reference, in checked bodies and inferred signatures alike.
#[test]
fn inner_static_scope_precedes_an_outer_implicit_receiver() {
    const SRC: &str = "class Outer {\n\
        \x20   fun f(): String = \"outer\"\n\
        \x20   val p: String get() = \"outer\"\n\
        \x20   inner class Inner {\n\
        \x20       companion {\n\
        \x20           fun f(): String = \"inner\"\n\
        \x20           val p: String get() = \"inner\"\n\
        \x20       }\n\
        \x20       fun call(): String = f()\n\
        \x20       fun read(): String = p\n\
        \x20       fun ref(): String = (::f)()\n\
        \x20       fun inferredCall() = f()\n\
        \x20       fun inferredRead() = p\n\
        \x20       fun inferredRef() = ::f\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val i = Outer().Inner()\n\
        \x20   val r = i.call() + i.read() + i.ref() + i.inferredCall() + i.inferredRead() +\n\
        \x20       i.inferredRef()()\n\
        \x20   return if (r == \"innerinnerinnerinnerinnerinner\") \"OK\" else r\n\
        }\n";
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "InnerStaticScope");
}
