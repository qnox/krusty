//! `x::class` and `String::class` through krusty's own code generator and runtime.
//!
//! Both forms answer one type descriptor, and which one is the whole of the difference: a literal
//! over a TYPE names it statically, while a literal over a VALUE reads the descriptor the object is
//! wearing. The programs below are written so the two would disagree if the wrong one were read.

use super::common::expect_box_run_with_stdlib;

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
