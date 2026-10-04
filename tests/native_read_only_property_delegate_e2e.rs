//! `val x by Delegate()` where `Delegate` implements `kotlin.properties.ReadOnlyProperty`.
//!
//! The read calls `getValue` through the INTERFACE, which is a dependency declaration. Two things
//! follow. A call through a dependency interface cannot go through a slot — an override of a
//! dependency member takes a slot of its own, so the interface names no number to dispatch on —
//! and the runtime builds no `ReadOnlyProperty` of its own, unlike `ReadWriteProperty` whose
//! `notNull()` and `observable(…)` it does build. So every object that can stand behind the type
//! is a class of this file, and testing the receiver against each of them is both possible and
//! exhaustive.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A delegate of this file, read through the interface.
#[test]
fn a_read_only_delegate_of_this_file_answers_the_read() {
    let source = "import kotlin.properties.ReadOnlyProperty\n\
         import kotlin.reflect.KProperty\n\
         class Delegate : ReadOnlyProperty<Test, String> {\n\
         \x20   override fun getValue(thisRef: Test, property: KProperty<*>) = \"OK\"\n\
         }\n\
         class Test {\n\
         \x20   val message by Delegate()\n\
         }\n\
         fun box(): String = Test().message\n";
    every_backend_agrees_with_kotlinc("ReadOnlyDelegate", source);
}

/// The delegate READS its receiver, so the `thisRef` operand has to be the object.
#[test]
fn a_read_only_delegate_reads_the_object_it_was_given() {
    let source = "import kotlin.properties.ReadOnlyProperty\n\
         import kotlin.reflect.KProperty\n\
         class Delegate : ReadOnlyProperty<Holder, Int> {\n\
         \x20   override fun getValue(thisRef: Holder, property: KProperty<*>): Int = thisRef.base\n\
         }\n\
         class Holder(val base: Int) {\n\
         \x20   val doubled by Delegate()\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = Holder(42).doubled\n\
         \x20   return if (answer == 42) \"OK\" else \"fail \" + answer\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReadOnlyDelegateReadsReceiver", source);
}

/// The delegate is named by a `provideDelegate` operator rather than written directly.
#[test]
fn a_provided_read_only_delegate_answers_the_read() {
    let source = "import kotlin.properties.ReadOnlyProperty\n\
         import kotlin.reflect.KProperty\n\
         class Delegate : ReadOnlyProperty<Test, String> {\n\
         \x20   override fun getValue(thisRef: Test, property: KProperty<*>) = \"OK\"\n\
         }\n\
         class Provider {\n\
         \x20   operator fun provideDelegate(thisRef: Test, property: KProperty<*>) = Delegate()\n\
         }\n\
         class Test {\n\
         \x20   val message by Provider()\n\
         }\n\
         fun box(): String = Test().message\n";
    every_backend_agrees_with_kotlinc("ProvidedReadOnlyDelegate", source);
}

/// TWO delegates of one file, so the dispatch has more than one arm to choose between.
#[test]
fn two_read_only_delegates_are_told_apart() {
    let source = "import kotlin.properties.ReadOnlyProperty\n\
         import kotlin.reflect.KProperty\n\
         class First : ReadOnlyProperty<Test, String> {\n\
         \x20   override fun getValue(thisRef: Test, property: KProperty<*>) = \"O\"\n\
         }\n\
         class Second : ReadOnlyProperty<Test, String> {\n\
         \x20   override fun getValue(thisRef: Test, property: KProperty<*>) = \"K\"\n\
         }\n\
         class Test {\n\
         \x20   val head by First()\n\
         \x20   val tail by Second()\n\
         }\n\
         fun box(): String {\n\
         \x20   val t = Test()\n\
         \x20   return t.head + t.tail\n\
         }\n";
    every_backend_agrees_with_kotlinc("TwoReadOnlyDelegates", source);
}
