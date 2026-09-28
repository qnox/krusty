//! Synthetic accessors for `private` members reached from a SEPARATE class in the same compilation.
//! A callable reference (`::priv`), a bound qualified-this reference inside a lambda, and an inner
//! class calling the outer's private method all require the private member to be reachable without an
//! illegal `invokespecial`/private access — kotlinc synthesizes `access$m`; krusty forwards through a
//! public `access$m` instance method. These compile to `ACC_PRIVATE` members plus accessors and RUN.

use super::common;

#[test]
fn callable_reference_to_private_member() {
    // `::priv` inside the class → a func-ref class must reach the private method via the accessor.
    common::expect_box_ok_with_stdlib(
        "class A {\n  private fun foo(): String = \"OK\"\n  fun r(): () -> String = ::foo\n}\n\
         fun box(): String = A().r()()\n",
        "Cref",
    );
}

#[test]
fn bound_reference_to_private_in_lambda() {
    // A bound `this@A::priv` captured inside a lambda body (kotlinc KT-63258 shape).
    common::expect_box_ok_with_stdlib(
        "class A {\n  private val ref: () -> String = run { this@A::foo }\n  \
         private fun foo(): String = \"OK\"\n  fun r(): String = ref()\n}\n\
         fun box(): String = A().r()\n",
        "Bound",
    );
}

#[test]
fn inner_class_calls_outer_private() {
    // An inner class directly calling the outer's private method → routed through the accessor.
    common::expect_box_ok_with_stdlib(
        "class Outer {\n  private fun secret(): String = \"OK\"\n  \
         inner class Inner {\n    fun get(): String = secret()\n  }\n}\n\
         fun box(): String = Outer().Inner().get()\n",
        "Inner",
    );
}

/// Private top-level declarations used from a class — directly, from a lambda there, and from the
/// carriers of `::tf`, `::tp` and `::tv` — go through the facade's synthetic accessors, which
/// follow its members in first-use order with kotlinc's bodies and debug tables.
#[test]
fn private_top_level_declarations_used_from_classes_go_through_facade_accessors() {
    const SRC: &str = "private fun tf(x: Int): Int = x\n\
        private val tp: Int = 2\n\
        private var tv: Int = 3\n\
        class D {\n\
        \x20   fun m(): Int {\n\
        \x20       val g = ::tf\n\
        \x20       val h = ::tp\n\
        \x20       tv = 5\n\
        \x20       return g(1) + h() + tv\n\
        \x20   }\n\
        \x20   fun lambda(): Int {\n\
        \x20       val l = { y: Int -> tf(y) + tp }\n\
        \x20       return l(1)\n\
        \x20   }\n\
        \x20   fun write(): Int {\n\
        \x20       val k = ::tv\n\
        \x20       k.set(7)\n\
        \x20       return k.get()\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val r = D().m() + D().lambda() + D().write()\n\
        \x20   return if (r == 18) \"OK\" else \"fail \" + r\n\
        }\n";
    let compare = |class: &str| {
        let comparison = common::compare_with_kotlinc_plugin(
            "FacadeAccessors",
            SRC,
            class,
            &[common::stdlib_jar()],
            "17",
            &[],
        )
        .expect("reference kotlinc and javap are provisioned");
        assert_eq!(
            common::member_table(&comparison.krusty_bytes),
            common::member_table(&comparison.reference_bytes),
            "{class}: kotlinc's member table"
        );
        comparison
    };
    let facade = compare("FacadeAccessorsKt");
    for header in [
        "public static final int access$tf(int);",
        "public static final int access$getTp$p();",
        "public static final void access$setTv$p(int);",
        "public static final int access$getTv$p();",
    ] {
        let accessor = common::method_block(&facade.reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares {header}");
        assert_eq!(
            common::method_block(&facade.krusty, header),
            accessor,
            "{header}"
        );
    }
    for class in ["D$m$g$1", "D$m$h$1", "D$write$k$1"] {
        let carrier = compare(class);
        assert_eq!(
            common::member_blocks(&carrier.krusty),
            common::member_blocks(&carrier.reference),
            "{class}: kotlinc's members"
        );
    }
    common::expect_box_same_as_kotlinc(SRC, "FacadeAccessors");
}

/// Private member properties read and written from an inner class and an anonymous object go
/// through the owner's `access$get<X>$p`/`access$set<X>$p`: `public static final synthetic`, after
/// the owner's members in first-use order, one per accessor a use needs, with kotlinc's bodies and
/// debug tables.
#[test]
fn private_member_properties_used_from_other_classes_go_through_field_accessors() {
    const SRC: &str = "class Holder {\n\
        \x20   private var mark: String = \"O\"\n\
        \x20   private var only: Int = 1\n\
        \x20   private val fixed: Long = 2L\n\
        \x20   fun viaObject(): String {\n\
        \x20       val probe = object { fun look() = fixed + only }\n\
        \x20       return mark + probe.look()\n\
        \x20   }\n\
        \x20   inner class Inner {\n\
        \x20       fun read(): String = mark\n\
        \x20       fun write() { mark = \"K\" }\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val holder = Holder()\n\
        \x20   holder.Inner().write()\n\
        \x20   val r = holder.Inner().read() + holder.viaObject()\n\
        \x20   return if (r == \"KK3\") \"OK\" else \"fail \" + r\n\
        }\n";
    let owner = common::compare_with_kotlinc_plugin(
        "MemberFieldAccessors",
        SRC,
        "Holder",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        common::member_table(&owner.krusty_bytes),
        common::member_table(&owner.reference_bytes),
        "Holder: kotlinc's member table"
    );
    for header in [
        "public static final long access$getFixed$p(Holder);",
        "public static final int access$getOnly$p(Holder);",
        "public static final java.lang.String access$getMark$p(Holder);",
        "public static final void access$setMark$p(Holder, java.lang.String);",
    ] {
        let accessor = common::method_block(&owner.reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares {header}");
        assert_eq!(
            common::method_block(&owner.krusty, header),
            accessor,
            "{header}"
        );
    }
    common::expect_box_same_as_kotlinc(SRC, "MemberFieldAccessors");
}

/// A named object's private backing fields are JVM statics. A nested object reaches them through
/// receiverless `access$get<X>$p()` / `access$set<X>$p(value)` bridges that `getstatic` / `putstatic`
/// the field. An instance bridge would `getfield` a static field and throw
/// `IncompatibleClassChangeError`.
#[test]
fn private_object_fields_used_from_a_nested_object_use_static_bridges() {
    const SRC: &str = "object Holder {\n\
        \x20   private val mark = \"O\"\n\
        \x20   private var tail = \"x\"\n\
        \x20   object Nested {\n\
        \x20       val read = mark\n\
        \x20       fun write(value: String) { tail = value }\n\
        \x20       fun current() = tail\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   Holder.Nested.write(\"K\")\n\
        \x20   val r = Holder.Nested.read + Holder.Nested.current()\n\
        \x20   return if (r == \"OK\") \"OK\" else \"fail \" + r\n\
        }\n";
    let owner = common::compare_with_kotlinc_plugin(
        "ObjectFieldAccessors",
        SRC,
        "Holder",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    for header in [
        "public static final java.lang.String access$getMark$p();",
        "public static final void access$setTail$p(java.lang.String);",
        "public static final java.lang.String access$getTail$p();",
    ] {
        let accessor = common::method_block(&owner.reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares {header}");
        assert_eq!(
            common::method_block(&owner.krusty, header),
            accessor,
            "{header}"
        );
    }
    common::expect_box_same_as_kotlinc(SRC, "ObjectFieldAccessors");
}

/// A default argument in an interface default implementation can read private companion storage
/// from another source file. The access still goes through the companion/owner's receiverless
/// static field bridge; an instance-shaped bridge would fail before the default body returns.
#[test]
fn private_companion_field_used_from_an_interface_default_argument_uses_a_static_bridge() {
    let sources = [
        (
            "Use.kt",
            "class Derived : Contract<String>\n\
             fun box(): String = Derived().value() ?: \"fail\"\n",
        ),
        (
            "Contract.kt",
            "interface Contract<T> {\n\
             \x20   fun value(input: String = RESULT): T? = input as T\n\
             \x20   companion object { private val RESULT = \"OK\" }\n\
             }\n",
        ),
    ];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "UseKt"),
        "OK",
        "reference fixture"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_class_reaches_a_private_top_level_declared_accessor() {
    // A private property's declared accessors are private facade methods, so a class in the same
    // file calls them through the facade's `access$` accessors.
    common::expect_box_ok_with_stdlib(
        "private val computed: String get() = \"O\"\n\
         private var stored = \"\"\n  set(value) { field = value + \"K\" }\n\
         class Reader {\n  fun read(): String {\n    stored = \"\"\n    return computed\n  }\n}\n\
         fun box(): String {\n  val first = Reader().read()\n  return first + stored\n}\n",
        "PrivateAccessor",
    );
}

#[test]
fn a_class_reaches_a_private_delegated_top_level_property() {
    // A private delegated property's accessors are private facade methods too.
    common::expect_box_ok_with_stdlib(
        "import kotlin.reflect.KProperty\n\
         class D(var v: String) {\n  operator fun getValue(t: Any?, p: KProperty<*>): String = v\n  operator fun setValue(t: Any?, p: KProperty<*>, x: String) { v = x }\n}\n\
         private var hidden: String by D(\"\")\n\
         class Writer {\n  fun write(): String {\n    hidden = \"O\"\n    return hidden + \"K\"\n  }\n}\n\
         fun box(): String = Writer().write()\n",
        "PrivateDelegated",
    );
}

#[test]
fn a_private_delegated_top_level_property_is_reached_through_facade_accessors() {
    // Its accessors are private facade methods, so `Writer.write` calls kotlinc's
    // `access$setHidden` / `access$getHidden`, which forward to them.
    const LIB: &str = "package lib\n\
        \n\
        import kotlin.reflect.KProperty\n\
        \n\
        class D(var v: String) {\n\
        \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = v\n\
        \x20   operator fun setValue(t: Any?, p: KProperty<*>, x: String) { v = x }\n\
        }\n";
    const SRC: &str = "import lib.D\n\
        \n\
        private var hidden: String by D(\"\")\n\
        \n\
        class Writer {\n\
        \x20   fun write(): String {\n\
        \x20       hidden = \"O\"\n\
        \x20       return hidden + \"K\"\n\
        \x20   }\n\
        }\n";
    let classes = common::classes_against_kotlinc_lib("PrivateDelegated", &[("Lib.kt", LIB)], SRC)
        .expect("reference kotlinc is provisioned");
    let (kotlinc, krusty) = classes
        .method_declarations("PrivateDelegatedKt")
        .expect("both compilers write the facade");
    assert_eq!(krusty, kotlinc);
    for (class, declaration) in [
        ("Writer", "public final java.lang.String write();"),
        (
            "PrivateDelegatedKt",
            "public static final void access$setHidden(java.lang.String);",
        ),
        (
            "PrivateDelegatedKt",
            "public static final java.lang.String access$getHidden();",
        ),
    ] {
        let (kotlinc, krusty) = classes.method_listing(class, declaration);
        assert_eq!(krusty, kotlinc, "{class}: {declaration}");
    }
}
