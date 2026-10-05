//! `x::class` and `String::class` through krusty's own code generator and runtime.
//!
//! Both forms answer one type descriptor, and which one is the whole of the difference: a literal
//! over a TYPE names it statically, while a literal over a VALUE reads the descriptor the object is
//! wearing. The programs below are written so the two would disagree if the wrong one were read.

use super::common::{expect_box_run_with_stdlib, expect_native_box};

#[test]
fn a_bound_literal_answers_the_runtime_class() {
    // `x` is typed `CharSequence` and holds a `String`. The answer is the object's, not the site's.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val x: CharSequence = \"\"\n\
         \x20   val klass = x::class\n\
         \x20   return if (klass == String::class) \"OK\" else \"fail: $klass\"\n\
         }\n",
        "BoundClassLiteral",
        "OK",
    );
}

#[test]
fn two_literals_of_one_type_are_equal_without_being_the_same_object() {
    // Kotlin's `KClass` is equal by the class it stands for, which is what lets a literal be an
    // ordinary allocation instead of a canonical instance the runtime keeps a table of.
    expect_native_box(
        "class Holder\n\
         fun box(): String {\n\
         \x20   val fromValue = Holder()::class\n\
         \x20   val fromType = Holder::class\n\
         \x20   if (fromValue != fromType) return \"fail equal\"\n\
         \x20   if (fromValue == String::class) return \"fail distinct\"\n\
         \x20   if (Holder::class != Holder::class) return \"fail reflexive\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ClassLiteralEquality",
        "OK",
    );
}

#[test]
fn a_bound_literal_evaluates_its_receiver_exactly_once() {
    // The receiver is an expression, not a type name: it runs, and a program can see it run.
    expect_native_box(
        "fun box(): String {\n\
         \x20   var x = 42\n\
         \x20   val first = (x++)::class\n\
         \x20   if (first != Int::class) return \"fail class: $first\"\n\
         \x20   if (x != 43) return \"fail effect: $x\"\n\
         \x20   val second = { x *= 2; x }()::class\n\
         \x20   if (second != Int::class) return \"fail boxed: $second\"\n\
         \x20   return if (x == 86) \"OK\" else \"fail second effect: $x\"\n\
         }\n",
        "ClassLiteralReceiverEffect",
        "OK",
    );
}

#[test]
fn a_literal_over_a_primitive_names_the_boxed_type() {
    // A scalar has no descriptor of its own, so the receiver is boxed and the box's descriptor is
    // the answer — which is `Int`, the same class the type literal names.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val n = 7\n\
         \x20   if (n::class != Int::class) return \"fail int\"\n\
         \x20   if (true::class != Boolean::class) return \"fail boolean\"\n\
         \x20   if ('c'::class != Char::class) return \"fail char\"\n\
         \x20   if (n::class == Long::class) return \"fail width\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ClassLiteralOfAPrimitive",
        "OK",
    );
}

#[test]
fn the_names_agree_with_the_jvm_backend() {
    // `simpleName` and `qualifiedName` are read off the descriptor's own Kotlin name, which every
    // type carries because `toString` already needed it. The other backend is the oracle for what
    // they should say.
    //
    // `toString` is deliberately NOT compared: on the JVM it appends "(Kotlin reflection is not
    // available)" unless `kotlin-reflect` is on the classpath, which is a fact about that
    // dependency rather than about either backend. Native prints the `class <qualified name>` that
    // a JVM WITH reflection prints.
    assert_eq!(
        expect_box_run_with_stdlib(
            "package demo\n\
             class Holder\n\
             fun box(): String {\n\
             \x20   val k = Holder::class\n\
             \x20   return \"${k.simpleName}/${k.qualifiedName}/${Int::class.simpleName}\"\n\
             }\n",
            "ClassLiteralNames",
        ),
        "Holder/demo.Holder/Int"
    );
}

#[test]
fn a_literals_own_to_string_names_the_class() {
    expect_native_box(
        "package demo\n\
         class Holder\n\
         fun box(): String {\n\
         \x20   val rendered = \"${Holder::class}\"\n\
         \x20   return if (rendered == \"class demo.Holder\") \"OK\" else \"fail: $rendered\"\n\
         }\n",
        "ClassLiteralToString",
        "OK",
    );
}
