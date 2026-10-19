//! A value class passed to a Java generic constructor crosses erasure as its box.
//!
//! `Holder(T value)` is `<init>(Ljava/lang/Object;)V`, and `Holder(Money(7))` selects it with
//! `T = Money`. The checked argument is `Money`, but the declaration consumes bare `T`, so the
//! constructor receives a `Money` box, as kotlinc passes it. krusty consumed the specialized `Money`
//! parameter and passed the `int` carrier (or, for a `String` carrier, the bare string).

use super::common;

const HOLDER: &str = "package jlib;\n\
    public final class Holder<T> {\n\
        private final T value;\n\
        public Holder(T value) { this.value = value; }\n\
        public T get() { return value; }\n\
        public String kind() { return value.getClass().getName(); }\n\
    }\n";

/// The same erased `T` slot reached through super-constructor delegation instead of `new`.
const GENERIC_BASE: &str = "package jlib;\n\
    public class GenericBase<T> {\n\
        public final String kind;\n\
        public GenericBase(T value) { kind = value.getClass().getName(); }\n\
    }\n";

fn expect_box_against_holder(source: &str, stem: &str) {
    let java = vec![
        ("Holder.java".to_string(), HOLDER.to_string()),
        ("GenericBase.java".to_string(), GENERIC_BASE.to_string()),
    ];
    let (library, _) = common::javac_compile(&java, &[]).expect("javac compiles the Java fixture");
    let classpath = [library.clone()];
    let reference = common::kotlinc_box_result_with_classpath(source, &classpath);
    assert_eq!(reference, "OK", "{stem}: kotlinc fixture must succeed");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(source, stem, &[library, stdlib], Some(jdk.as_path())),
        reference,
        "{stem}: krusty and kotlinc box results differ",
    );
}

#[test]
fn a_primitive_carrier_value_class_reaches_a_java_generic_constructor_boxed() {
    const MAIN: &str = "import jlib.Holder\n\
        @JvmInline value class Money(val cents: Int)\n\
        fun box(): String {\n\
        \x20   val holder = Holder(Money(7))\n\
        \x20   if (holder.kind() != \"Money\") return holder.kind()\n\
        \x20   return if (holder.get().cents == 7) \"OK\" else \"value\"\n\
        }\n";
    expect_box_against_holder(MAIN, "JavaGenericCtorPrimitiveCarrier");
}

#[test]
fn a_reference_carrier_value_class_reaches_a_java_generic_constructor_boxed() {
    const MAIN: &str = "import jlib.Holder\n\
        @JvmInline value class Label(val text: String)\n\
        fun box(): String {\n\
        \x20   val holder = Holder(Label(\"hi\"))\n\
        \x20   if (holder.kind() != \"Label\") return holder.kind()\n\
        \x20   return if (holder.get().text == \"hi\") \"OK\" else \"value\"\n\
        }\n";
    expect_box_against_holder(MAIN, "JavaGenericCtorReferenceCarrier");
}

#[test]
fn a_primary_super_delegation_passes_a_primitive_carrier_value_class_boxed() {
    const MAIN: &str = "import jlib.GenericBase\n\
        @JvmInline value class Money(val cents: Int)\n\
        class Derived(value: Money) : GenericBase<Money>(value)\n\
        fun box(): String = Derived(Money(7)).kind.let { if (it == \"Money\") \"OK\" else it }\n";
    expect_box_against_holder(MAIN, "JavaGenericSuperPrimaryPrimitive");
}

#[test]
fn a_primary_super_delegation_passes_a_reference_carrier_value_class_boxed() {
    const MAIN: &str = "import jlib.GenericBase\n\
        @JvmInline value class Label(val text: String)\n\
        class Derived(value: Label) : GenericBase<Label>(value)\n\
        fun box(): String = Derived(Label(\"hi\")).kind.let { if (it == \"Label\") \"OK\" else it }\n";
    expect_box_against_holder(MAIN, "JavaGenericSuperPrimaryReference");
}

#[test]
fn a_secondary_super_delegation_passes_a_value_class_boxed() {
    const MAIN: &str = "import jlib.GenericBase\n\
        @JvmInline value class Money(val cents: Int)\n\
        class Derived : GenericBase<Money> {\n\
        \x20   constructor(value: Money) : super(value)\n\
        }\n\
        fun box(): String = Derived(Money(7)).kind.let { if (it == \"Money\") \"OK\" else it }\n";
    expect_box_against_holder(MAIN, "JavaGenericSuperSecondary");
}

/// The secondary-constructor delegation boundary is the same for a module generic base: `T`
/// consumes the box.
#[test]
fn a_secondary_super_delegation_to_a_kotlin_generic_base_passes_a_value_class_boxed() {
    const MAIN: &str = "@JvmInline value class Money(val cents: Int)\n\
        open class Base<T>(value: T) { val kind: String = (value as Any).javaClass.name }\n\
        class Derived : Base<Money> {\n\
        \x20   constructor(value: Money) : super(value)\n\
        }\n\
        fun box(): String = Derived(Money(7)).kind.let { if (it == \"Money\") \"OK\" else it }\n";
    common::expect_box_same_as_kotlinc(MAIN, "KotlinGenericSuperSecondary");
}
