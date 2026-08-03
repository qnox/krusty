//! A CLASSPATH EXTENSION call that OMITS a defaulted argument resolved fine on an explicit receiver but
//! skipped the whole file on an IMPLICIT one ("this construct is not yet supported by the IR backend").
//! Both resolve to the `$default` synthetic, whose emit needs the call's argument→parameter mapping, and
//! only the explicit-receiver path recorded one. An UNLABELLED call's mapping follows from its own shape
//! — positional arguments fill parameters left to right, a TRAILING LAMBDA binds the LAST parameter, so
//! an omitted default may sit BETWEEN them — so the emit derives it rather than treating its absence as
//! "unknown". Round-tripped on a JVM.
use super::common;

const LIB: &str = "package lib\n\
     class Host { var seen: String = \"\" }\n\
     fun Host.tag(name: String, port: Int = 9) { seen = name + port }\n\
     fun Host.mark(name: String) { seen = name }\n\
     fun Host.mid(name: String, port: Int = 9, block: () -> String) { seen = name + port + block() }\n\
     fun Host.lead(port: Int = 9, block: () -> String) { seen = \"\" + port + block() }\n\
     fun build(block: Host.() -> Unit): String {\n\
     \x20 val host = Host()\n\
     \x20 host.block()\n\
     \x20 return host.seen\n\
     }\n";

#[test]
fn an_implicit_receiver_extension_call_may_omit_a_default() {
    let main = "import lib.build\n\
        import lib.tag\n\
        fun box(): String {\n\
        \x20 val seen = build { tag(\"a\") }\n\
        \x20 if (seen != \"a9\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultimplicit", LIB, main);
}

#[test]
fn an_implicit_receiver_extension_call_still_takes_every_argument() {
    let main = "import lib.build\n\
        import lib.tag\n\
        fun box(): String {\n\
        \x20 val seen = build { tag(\"a\", 3) }\n\
        \x20 if (seen != \"a3\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultimplicitfull", LIB, main);
}

#[test]
fn an_extension_with_no_defaults_is_unaffected() {
    let main = "import lib.build\n\
        import lib.mark\n\
        fun box(): String {\n\
        \x20 val seen = build { mark(\"m\") }\n\
        \x20 if (seen != \"m\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextnodefaultimplicit", LIB, main);
}

#[test]
fn the_explicit_receiver_spelling_keeps_working() {
    let main = "import lib.Host\n\
        import lib.tag\n\
        fun box(): String {\n\
        \x20 val host = Host()\n\
        \x20 host.tag(\"a\")\n\
        \x20 if (host.seen != \"a9\") return \"fail: \" + host.seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultexplicit", LIB, main);
}

/// A TRAILING LAMBDA binds the LAST parameter, so an omitted default can sit BETWEEN the positional
/// arguments and the lambda. Filling parameters left to right would pass the lambda in the omitted
/// parameter's slot — which compiles clean and throws `ClassCastException` at the call.
#[test]
fn a_trailing_lambda_binds_the_last_parameter_past_an_omitted_default() {
    let main = "import lib.build\n\
        import lib.mid\n\
        fun box(): String {\n\
        \x20 val seen = build { mid(\"a\") { \"L\" } }\n\
        \x20 if (seen != \"a9L\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultmidlambda", LIB, main);
}

#[test]
fn a_trailing_lambda_binds_the_last_parameter_past_a_leading_omitted_default() {
    let main = "import lib.build\n\
        import lib.lead\n\
        fun box(): String {\n\
        \x20 val seen = build { lead { \"L\" } }\n\
        \x20 if (seen != \"9L\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultleadlambda", LIB, main);
}

#[test]
fn a_trailing_lambda_with_every_argument_spelled_out_is_unaffected() {
    let main = "import lib.build\n\
        import lib.mid\n\
        fun box(): String {\n\
        \x20 val seen = build { mid(\"a\", 3) { \"L\" } }\n\
        \x20 if (seen != \"a3L\") return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpextdefaultmidlambdafull", LIB, main);
}
