//! A CLASSPATH function whose value parameter is a RECEIVER function type (`Cfg.() -> Unit`) was
//! rejected at a trailing-lambda call site ("unresolved function"): the `@Metadata` signature decoder
//! built the parameter's `Ty::Fun` from the type's `kotlin/FunctionN` classifier alone and dropped the
//! `@kotlin.ExtensionFunctionType` type annotation, so the declared parameter read as `(Cfg) -> Unit`
//! while the lambda argument was shaped as `Cfg.() -> Unit` and no overload matched. The receiver mark
//! now survives the decode. Verified end-to-end on a real JVM against a kotlinc-compiled dependency.
use super::common;

const LIB: &str = "package lib\n\
     class Cfg { var tag: String = \"\" }\n\
     open class Engine(val name: String)\n\
     class Basic : Engine(\"basic\")\n\
     fun host(engine: Engine, block: Cfg.() -> Unit = {}): String {\n\
     \x20 val cfg = Cfg()\n\
     \x20 cfg.block()\n\
     \x20 return engine.name + cfg.tag\n\
     }\n\
     fun host(block: Cfg.() -> Unit = {}): String = host(Engine(\"none\"), block)\n";

#[test]
fn trailing_receiver_lambda_selects_the_classpath_overload() {
    let main = "import lib.Basic\n\
        import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(Basic()) { tag = \"-x\" }\n\
        \x20 if (s != \"basic-x\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpreceiverlambda", LIB, main);
}

#[test]
fn receiver_lambda_default_is_still_omittable() {
    let main = "import lib.Basic\n\
        import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(Basic())\n\
        \x20 if (s != \"basic\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpreceiverlambdadefault", LIB, main);
}

#[test]
fn a_receiver_lambda_parameter_binds_its_receiver_members() {
    let main = "import lib.Basic\n\
        import lib.host\n\
        fun box(): String {\n\
        \x20 val s = host(Basic()) { this.tag = \"-t\" }\n\
        \x20 if (s != \"basic-t\") return \"fail: \" + s\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpreceiverlambdathis", LIB, main);
}
