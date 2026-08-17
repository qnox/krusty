//! A NULLABLE PRIMITIVE parameter in a dependency declaration.
//!
//! `Int?` compiles to `java/lang/Integer` in the JVM descriptor while `@Metadata` still names
//! `kotlin/Int`. Comparing those two names by their erasure group — which relates mapped builtins,
//! not boxes — never matches, so the whole metadata alignment for the function is lost. Parameter
//! NAMES go with it, and every named argument at the call site is reported as "no parameter with
//! name 'x' found", however the call is written. The primitive → wrapper table is the one pairing
//! that answers it.
use super::common;

const LIB: &str = "package lib\n\
    class Options(val label: String)\n\
    fun buildSpec(\n\
        toolset: Options?,\n\
        version: String? = null,\n\
        sizeGb: Int? = null,\n\
    ): String = (toolset?.label ?: \"none\") + \"/\" + (version ?: \"-\") + \"/\" + (sizeGb ?: 0)\n\
    fun boxedDefault(a: String, b: Int? = null): String = a + (b ?: -1)\n\
    fun boxedValueDefault(a: String, b: Int? = 7): String = a + (b ?: -1)\n\
    fun doubleDefault(a: String, b: Double? = null): String = a + (b ?: 0.5)\n\
    fun primitiveDefault(a: String, b: Int = 1): String = a + b\n";

#[test]
fn a_nullable_primitive_parameter_keeps_its_name() {
    const MAIN: &str = "import lib.Options\n\
        import lib.buildSpec\n\
        import lib.boxedDefault\n\
        import lib.boxedValueDefault\n\
        import lib.doubleDefault\n\
        import lib.primitiveDefault\n\
        fun box(): String {\n\
            val all = buildSpec(toolset = Options(\"t\"), version = \"v\", sizeGb = 4)\n\
            val some = buildSpec(toolset = null, sizeGb = 7)\n\
            val defaulted = boxedDefault(a = \"a\")\n\
            val supplied = boxedDefault(a = \"a\", b = 2)\n\
            val valued = boxedValueDefault(a = \"v\")\n\
            val doubled = doubleDefault(a = \"d\")\n\
            val primitive = primitiveDefault(a = \"p\")\n\
            if (all != \"t/v/4\") return \"fail all: \" + all\n\
            if (some != \"none/-/7\") return \"fail some: \" + some\n\
            if (defaulted != \"a-1\") return \"fail defaulted: \" + defaulted\n\
            if (supplied != \"a2\") return \"fail supplied: \" + supplied\n\
            if (valued != \"v7\") return \"fail valued: \" + valued\n\
            if (doubled != \"d0.5\") return \"fail doubled: \" + doubled\n\
            if (primitive != \"p1\") return \"fail primitive: \" + primitive\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("nullprim", LIB, MAIN);
}

#[test]
fn a_nullable_primitive_overload_keeps_its_own_parameters() {
    // The alignment decides which metadata function owns a JVM descriptor, so losing it does not
    // only cost names: `h(Int?)` and `h(Any?)` differ by `(Integer,String)` vs `(Object,String)`,
    // and with the primitive unmatched the `Any?` candidate claimed the `Integer` descriptor. That
    // compiles and throws `ClassCastException`, or silently calls the wrong overload — so `box()`
    // has to RUN.
    const OVERLOADS: &str = "package lib\n\
        fun h(x: Int?, ti: String = \"i\"): String = \"I:\" + x + \":\" + ti\n\
        fun h(x: Any?, ta: String = \"a\"): String = \"A:\" + x + \":\" + ta\n";
    const MAIN: &str = "import lib.h\n\
        fun box(): String {\n\
            val str = h(x = \"s\", ta = \"TA\")\n\
            val nul = h(x = null, ta = \"TA\")\n\
            val int = h(x = 3, ti = \"TI\")\n\
            if (str != \"A:s:TA\") return \"fail str: \" + str\n\
            if (nul != \"A:null:TA\") return \"fail null: \" + nul\n\
            if (int != \"I:3:TI\") return \"fail int: \" + int\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("nullprimovl", OVERLOADS, MAIN);
}

#[test]
fn every_boxed_primitive_keeps_its_name() {
    // `kotlin/Char` → `java/lang/Character` is the one non-mechanical entry in the pairing table.
    const TYPES: &str = "package lib\n\
        fun c(a: Char? = null, tag: String = \"c\"): String = \"\" + (a ?: 'z') + tag\n\
        fun b(a: Boolean? = null, tag: String = \"b\"): String = \"\" + (a ?: true) + tag\n\
        fun l(a: Long? = null, tag: String = \"l\"): String = \"\" + (a ?: 1L) + tag\n\
        fun s(a: Short? = null, tag: String = \"s\"): String = \"\" + (a ?: 2) + tag\n\
        fun y(a: Byte? = null, tag: String = \"y\"): String = \"\" + (a ?: 3) + tag\n\
        fun f(a: Float? = null, tag: String = \"f\"): String = \"\" + (a ?: 4.5f) + tag\n";
    const MAIN: &str = "import lib.c\n\
        import lib.b\n\
        import lib.l\n\
        import lib.s\n\
        import lib.y\n\
        import lib.f\n\
        fun box(): String {\n\
            val all = c(tag = \"C\") + b(tag = \"B\") + l(tag = \"L\") + s(tag = \"S\") + y(tag = \"Y\") + f(tag = \"F\")\n\
            return if (all == \"zCtrueB1L2S3Y4.5F\") \"OK\" else \"fail: \" + all\n\
        }\n";
    common::expect_box_ok_against("nullprimall", TYPES, MAIN);
}
