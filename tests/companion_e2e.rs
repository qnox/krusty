//! `companion object` methods — compiled (like kotlinc) to a `C$Companion` class holding the methods,
//! a `public static final Companion` field on the outer class built in its `<clinit>`, and
//! `C.foo()` → `getstatic C.Companion; invokevirtual`. Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn companion_methods_run() {
    let src = "class C {\n\
    companion object {\n\
        fun answer(): Int = 42\n\
        fun greet(s: String): String = \"hi \" + s\n\
    }\n\
}\n\
fun box(): String {\n\
if (C.answer() != 42) return \"f1\"\n\
if (C.greet(\"x\") != \"hi x\") return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "C");
}
