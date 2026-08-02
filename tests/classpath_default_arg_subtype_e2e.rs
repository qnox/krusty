//! Omitting a defaulted trailing argument of a CLASSPATH function rejected any argument that was a
//! SUBTYPE of its parameter: the defaulted path measured applicability with the platform-only "same
//! erased shape" check instead of assignability, so `host(sub)` was reported unresolved while the same
//! call with every argument spelled out (`host(sub, 5)`) resolved. Applicability is one question with
//! one answer. Verified end-to-end on a real JVM against a kotlinc-compiled dependency.
use super::common;

const LIB: &str = "package lib\n\
     open class Engine(val name: String)\n\
     class Basic : Engine(\"basic\")\n\
     interface Factory\n\
     object Fast : Factory\n\
     fun host(engine: Engine, port: Int = 3): String = engine.name + port\n\
     fun make(factory: Factory, port: Int = 4): String = \"made\" + port\n";

#[test]
fn a_subtype_argument_still_selects_the_defaulted_overload() {
    let main = "import lib.Basic\n\
        import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(Basic())\n\
        \x20 if (s != \"basic3\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpdefaultsubtype", LIB, main);
}

#[test]
fn an_object_argument_still_selects_the_defaulted_overload() {
    let main = "import lib.Fast\n\
        import lib.make\n\
        fun box(): String {\n\
        \x20 val s = make(Fast)\n\
        \x20 if (s != \"made4\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpdefaultsubtypeobject", LIB, main);
}

#[test]
fn a_spelled_out_call_keeps_resolving() {
    let main = "import lib.Basic\n\
        import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(Basic(), 5)\n\
        \x20 if (s != \"basic5\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpdefaultsubtypeexplicit", LIB, main);
}

/// Applicability is not selection: once a subtype argument fits BOTH the base and the subtype overload,
/// the defaulted path must still pick the most specific one — the same answer the all-arguments-spelled-out
/// path gives. Selecting whichever the classpath happened to list first silently calls the wrong function.
#[test]
fn the_most_specific_overload_wins_on_the_defaulted_path() {
    const OVERLOADS: &str = "package lib\n\
         open class Base(val n: String)\n\
         class Sub : Base(\"sub\")\n\
         fun pick(b: Base, port: Int = 3): String = \"base\" + port\n\
         fun pick(s: Sub, port: Int = 4): String = \"sub\" + port\n";
    let main = "import lib.Sub\n\
        import lib.pick\n\
        fun box(): String {\n\
        \x20 val s = pick(Sub())\n\
        \x20 if (s != \"sub4\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpdefaultspecific", OVERLOADS, main);
}

/// Two overloads that differ only in how many defaults they fill are ordered by that count, not by
/// declaration order: `manyFirst("x")` fills ONE default, so the two-parameter overload wins whichever
/// way the classpath lists them. Order-dependent selection here is silent wrong-code.
#[test]
fn the_overload_filling_fewer_defaults_wins() {
    const MANY_FIRST: &str = "package lib\n\
         fun host(a: String, b: Int = 1, c: Int = 2): String = \"many\"\n\
         fun host(a: String, b: Int = 1): String = \"few\"\n";
    const FEW_FIRST: &str = "package lib\n\
         fun host(a: String, b: Int = 1): String = \"few\"\n\
         fun host(a: String, b: Int = 1, c: Int = 2): String = \"many\"\n";
    let main = "import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(\"x\")\n\
        \x20 if (s != \"few\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpdefaultcountmany", MANY_FIRST, main);
    common::expect_box_ok_against("cpdefaultcountfew", FEW_FIRST, main);
}
