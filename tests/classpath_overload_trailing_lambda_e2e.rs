//! Overloaded classpath member functions called with a bare trailing lambda.
//!
//! Shape: a classpath object exposes `make(f: () -> Unit): Handle` AND `make(name: String): Handle`;
//! the consumer calls `Factory.make {}` and lets a property infer its type from the result. The
//! overload set must resolve to the function-typed candidate from the lambda argument alone —
//! failing to pick one surfaces as "cannot infer the type of property", which then cascades into
//! "unresolved reference" at every use site of the property.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib(tag, lib)?;
    Some(common::expect_box_run(
        main,
        "Main",
        &[libout, sl],
        Some(jdk.as_path()),
    ))
}

const LIB: &str = "package lib\n\
    class Handle { fun ping(): String = \"OK\" }\n\
    object Factory {\n\
        fun make(f: () -> Unit): Handle { f(); return Handle() }\n\
        fun make(name: String): Handle = Handle()\n\
    }\n";

#[test]
fn top_level_val_infers_from_overloaded_trailing_lambda_call() {
    const MAIN: &str = "import lib.Factory\n\
        private val handle = Factory.make {}\n\
        fun box(): String = handle.ping()\n";
    assert_eq!(
        run("ol1", LIB, MAIN).expect("overloaded trailing-lambda call"),
        "OK"
    );
}

#[test]
fn top_level_val_infers_from_classpath_top_level_overloaded_lambda_call() {
    // The same shape through the TOP-LEVEL selection channel: the overload pair lives at file
    // scope on the classpath (a facade), not on an object.
    const TLLIB: &str = "package lib\n\
        class Handle { fun ping(): String = \"OK\" }\n\
        fun make(f: () -> Unit): Handle { f(); return Handle() }\n\
        fun make(name: String): Handle = Handle()\n";
    const MAIN: &str = "import lib.make\n\
        private val handle = make {}\n\
        fun box(): String = handle.ping()\n";
    assert_eq!(
        run("ol3", TLLIB, MAIN).expect("classpath top-level overloaded lambda call"),
        "OK"
    );
}

#[test]
fn top_level_val_infers_from_same_file_overloaded_lambda_call() {
    // And through the SAME-FILE channel: no classpath involved at all.
    const MAIN: &str = "class Handle { fun ping(): String = \"OK\" }\n\
        fun make(f: () -> Unit): Handle { f(); return Handle() }\n\
        fun make(name: String): Handle = Handle()\n\
        private val handle = make {}\n\
        fun box(): String = handle.ping()\n";
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &[sl], Some(jdk.as_path())),
        "OK"
    );
}

#[test]
fn top_level_val_infers_from_overloaded_string_call() {
    // The sibling overload stays resolvable — guards against fixing the lambda arm by breaking the
    // value arm.
    const MAIN: &str = "import lib.Factory\n\
        private val handle = Factory.make(\"h\")\n\
        fun box(): String = handle.ping()\n";
    assert_eq!(run("ol2", LIB, MAIN).expect("overloaded string call"), "OK");
}

#[test]
fn top_level_val_infers_a_generic_parameter_from_a_bare_lambda() {
    const SOURCE: &str = "class Handle { fun ping(): String = \"OK\" }\n\
        fun <T> make(value: T): Handle = Handle()\n\
        private val handle = make {}\n\
        fun box(): String = handle.ping()\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    assert_eq!(
        common::expect_box_run(SOURCE, "GenericBareLambda", &[stdlib], Some(jdk.as_path())),
        "OK"
    );
}

#[test]
fn string_vararg_does_not_wildcard_admit_a_bare_lambda() {
    const SOURCE: &str = "class Handle { fun ping(): String = \"OK\" }\n\
        fun make(vararg names: String): Handle = Handle()\n\
        fun make(block: () -> Unit): Handle { block(); return Handle() }\n\
        private val handle = make {}\n\
        fun box(): String = handle.ping()\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    assert_eq!(
        common::expect_box_run(SOURCE, "VarargBareLambda", &[stdlib], Some(jdk.as_path())),
        "OK"
    );
}
