//! `is`, `as` and `as?` against an ARRAY type, through krusty's own code generator and runtime.
//!
//! An array is not a class a program declares, so the generator has no class id to look a
//! descriptor up by — and without a descriptor there is nothing to ask the runtime. But the
//! descriptor exists: every array the generator allocates already wears one of the runtime's own
//! (`kt_type_int_array`, `kt_type_array`, …), because the collector has to be told the element
//! width and whether to look inside. Naming that same descriptor at a type check is the whole of
//! what these programs need.
//!
//! What an array's descriptor distinguishes is the element WIDTH, not the element type a program
//! wrote, which is exactly Kotlin's own erasure: `is Array<String>` is not something a program may
//! write, `is Array<*>` is, and `IntArray` and `Array<Int>` are different types on both sides.
//!
//! The same is true of every other type the runtime names rather than the program: a cast is
//! CHECKED whenever its target is held as a reference and there is a descriptor to check against.
//! A scalar target is the one exclusion, and not an oversight — `x as Int` is an unboxing, whose
//! realization is a representation change rather than a question about an object.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn a_string_is_asked_about_the_same_way_a_class_is() {
    // `String` is the runtime's type, not the program's, so it had the array's gap too.
    let src = "fun box(): String {\n\
               \x20   val text: Any = \"a\"\n\
               \x20   val number: Any = 1\n\
               \x20   if (text !is String) return \"fail: not a String\"\n\
               \x20   if (number is String) return \"fail: a number is a String\"\n\
               \x20   if ((number as? String) != null) return \"fail: became a String\"\n\
               \x20   val back = text as? String ?: return \"fail: lost its own type\"\n\
               \x20   return if (back == \"a\") \"OK\" else \"fail: $back\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "StringIsAskedLikeAClass");
    expect_native_box(src, "StringIsAskedLikeAClass", "OK");
}

#[test]
fn an_unboxing_cast_stays_a_representation_change() {
    // The exclusion, stated as a program: `as Int` reads the number out of the box, and routing it
    // through the object check would hand the box back where the site wants the value.
    let src = "fun box(): String {\n\
               \x20   val boxed: Any = 7\n\
               \x20   val n = boxed as Int\n\
               \x20   val doubled = n * 2\n\
               \x20   return if (doubled == 14) \"OK\" else \"fail: $doubled\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "UnboxingCastStaysACoercion");
    expect_native_box(src, "UnboxingCastStaysACoercion", "OK");
}

#[test]
fn nothing_is_a_type_no_value_is_an_instance_of() {
    // `Nothing` has no instances at all, so the check is settled by the type. Its NULLABLE form is
    // the one value it admits — `Nothing?` is the type of `null` and of nothing else.
    //
    // Asserted against this backend only, deliberately. krusty's JVM backend answers `true` here:
    // `ref_internal` has no arm for `Ty::Nothing` and falls through to `java/lang/Object`, so it
    // emits `instanceof java/lang/Object`, which every non-null value passes. That is a defect in
    // shared code rather than a difference of opinion between targets, and it is being fixed on
    // its own branch; cross-checking here would pin the wrong answer.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val present: Any? = \"a\"\n\
         \x20   val absent: Any? = null\n\
         \x20   if (present is Nothing) return \"fail: a value is Nothing\"\n\
         \x20   if (present is Nothing?) return \"fail: a value is Nothing?\"\n\
         \x20   if (absent is Nothing) return \"fail: null is Nothing\"\n\
         \x20   if (absent !is Nothing?) return \"fail: null is not Nothing?\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "NothingHasNoInstances",
        "OK",
    );
}

#[test]
fn any_is_the_question_of_whether_there_is_a_value_at_all() {
    let src = "fun box(): String {\n\
               \x20   val present: Any? = 1\n\
               \x20   val absent: Any? = null\n\
               \x20   if (present !is Any) return \"fail: a value is not Any\"\n\
               \x20   if (absent is Any) return \"fail: null is Any\"\n\
               \x20   if (present !is Any?) return \"fail: a value is not Any?\"\n\
               \x20   if (absent !is Any?) return \"fail: null is not Any?\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "AnyIsWhetherThereIsAValue");
    expect_native_box(src, "AnyIsWhetherThereIsAValue", "OK");
}

#[test]
fn a_settled_check_still_evaluates_its_receiver() {
    // The constant is the ANSWER, not the expression. A receiver with effects runs exactly once,
    // which is what separates folding the answer from dropping the question. Native-only for the
    // `Nothing` half, for the reason given above.
    expect_native_box(
        "var calls = 0\n\
         fun subject(): Any? { calls++; return \"a\" }\n\
         fun box(): String {\n\
         \x20   val never = subject() is Nothing\n\
         \x20   val always = subject() is Any\n\
         \x20   if (never) return \"fail: Nothing\"\n\
         \x20   if (!always) return \"fail: Any\"\n\
         \x20   return if (calls == 2) \"OK\" else \"fail: $calls\"\n\
         }\n",
        "SettledCheckEvaluatesReceiver",
        "OK",
    );
}

#[test]
fn unit_is_asked_about_as_the_object_it_is() {
    // `Unit` reaches a check spelled as the object rather than as the carrier the generator names,
    // and it is one type either way.
    //
    // Native-only for the same reason as `Nothing`: `ref_internal` has no `Ty::Unit` arm either, so
    // krusty's JVM backend answers `"a" is Unit` with `true`.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val value: Any = Unit\n\
         \x20   val other: Any = \"a\"\n\
         \x20   if (value !is Unit) return \"fail: Unit is not Unit\"\n\
         \x20   if (other is Unit) return \"fail: a String is Unit\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnitIsAskedAsAnObject",
        "OK",
    );
}
