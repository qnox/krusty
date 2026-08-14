//! A CLASSPATH MEMBER whose value parameter is a Kotlin function type (`configure: Cfg.() -> Unit`)
//! never shaped its lambda argument: the member's signature came from the JVM `Signature` attribute
//! (which cannot spell a receiver function type) and its `@Metadata` call facts dropped the
//! per-parameter `@ExtensionFunctionType` mark, so the lambda was typed bare and the call reported
//! "none of the following candidates is applicable". The member now carries the same lambda facts a
//! top-level function does, and a call types the lambda against the parameter — including the generic
//! form (`<B> install(plugin: Plugin<B>, configure: B.() -> Unit)`), where `B` binds from the argument.
//! Verified end-to-end on a real JVM against a kotlinc-compiled dependency.
use super::common;

const LIB: &str = "package lib\n\
     class Cfg { var millis: Long = 0 }\n\
     interface Plugin<B : Any>\n\
     private class TimeoutPlugin : Plugin<Cfg>\n\
     val timeout: Plugin<Cfg> = TimeoutPlugin()\n\
     class Host {\n\
     \x20 var seen: Long = -1\n\
     \x20 fun configure(configure: Cfg.() -> Unit) {\n\
     \x20   val cfg = Cfg()\n\
     \x20   cfg.configure()\n\
     \x20   seen = cfg.millis\n\
     \x20 }\n\
     \x20 fun <B : Any> install(plugin: Plugin<B>, sample: B, configure: B.() -> Unit = {}) {\n\
     \x20   sample.configure()\n\
     \x20   seen = (sample as Cfg).millis\n\
     \x20 }\n\
     }\n\
     fun build(block: Host.() -> Unit): Long {\n\
     \x20 val host = Host()\n\
     \x20 host.block()\n\
     \x20 return host.seen\n\
     }\n";

#[test]
fn a_member_receiver_lambda_parameter_binds_its_receiver() {
    let main = "import lib.Host\n\
        fun box(): String {\n\
        \x20 val host = Host()\n\
        \x20 host.configure { millis = 7L }\n\
        \x20 if (host.seen != 7L) return \"fail: \" + host.seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cpmemberrecvlambda", LIB, main);
}

#[test]
fn a_generic_member_binds_its_receiver_lambda_from_the_argument() {
    let main = "import lib.Cfg\n\
        import lib.Host\n\
        import lib.timeout\n\
        fun box(): String {\n\
        \x20 val host = Host()\n\
        \x20 host.install(timeout, Cfg()) { millis = 9L }\n\
        \x20 if (host.seen != 9L) return \"fail: \" + host.seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cpmemberrecvlambdageneric", LIB, main);
}

#[test]
fn a_member_receiver_lambda_binds_through_an_implicit_receiver() {
    let main = "import lib.build\n\
        fun box(): String {\n\
        \x20 val seen = build { configure { millis = 11L } }\n\
        \x20 if (seen != 11L) return \"fail: \" + seen\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against_ref("cpmemberrecvlambdaimplicit", LIB, main);
}

/// A `suspend` member's generic signature carries a trailing `Continuation` its source parameter list
/// does not, so aligning the two positionally — for the receiver marks and for lambda specialization —
/// has to drop it first. Without that the receiver-lambda parameter of every suspend member loses its
/// `this`, which is the dominant shape in builder-style suspend APIs.
#[test]
fn a_suspend_member_receiver_lambda_parameter_binds_its_receiver() {
    const SUSPEND_LIB: &str = "package lib\n\
         class Cfg { var millis: Long = 0 }\n\
         class Host {\n\
         \x20 var seen: Long = -1\n\
         \x20 suspend fun configure(configure: Cfg.() -> Unit) {\n\
         \x20   val cfg = Cfg()\n\
         \x20   cfg.configure()\n\
         \x20   seen = cfg.millis\n\
         \x20 }\n\
         }\n";
    // Checked, not run: driving a `suspend` call needs a coroutine runtime that is not on this
    // classpath, and the fact under test is resolution — the lambda's `this` binding.
    let main = "import lib.Host\n\
        suspend fun use(host: Host) {\n\
        \x20 host.configure { millis = 13L }\n\
        }\n";
    if let Some(diagnostics) =
        common::checker_diags_against_ref("cpmemberrecvlambdasuspend", SUSPEND_LIB, main)
    {
        assert!(
            diagnostics.is_empty(),
            "a suspend member's receiver lambda must resolve, got: {diagnostics:#?}"
        );
    }
}
