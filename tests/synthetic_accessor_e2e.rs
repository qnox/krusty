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
