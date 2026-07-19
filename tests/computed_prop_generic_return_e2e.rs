use super::common;

const LIB: &str = "package lib\n\
    class Holder<T>(val v: T)\n\
    object Make { fun str(): Holder<String> = Holder(\"hi\") }\n";

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib("computed_generic", LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl], Some(&jdk))
}

#[test]
fn computed_property_keeps_classpath_generic_return_arg() {
    const MAIN: &str = "import lib.Make\n\
        class C { val h get() = Make.str() }\n\
        fun box(): String = if (C().h.v.length == 2) \"OK\" else \"F:\" + C().h.v.length\n";
    assert_eq!(run(MAIN).expect("computed property generic return"), "OK");
}
