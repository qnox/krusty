use super::common;

const LIB: &str = "package lib\n\
     class Cfg(val pretty: Boolean)\n\
     open class Fmt(val configuration: Cfg, val tag: String) {\n\
     \x20 companion object Default : Fmt(Cfg(false), \"default\")\n\
     }\n\
     class FmtBuilder { var pretty: Boolean = false }\n\
     class Solo(val configuration: Cfg, val tag: String)\n\
     fun Fmt(from: Fmt = Fmt.Default, builderAction: FmtBuilder.() -> Unit): Fmt {\n\
     \x20 val b = FmtBuilder()\n\
     \x20 b.builderAction()\n\
     \x20 return Fmt(Cfg(b.pretty), from.tag)\n\
     }\n\
     class Engine(val name: String)\n\
     class Client(val engine: Engine, val configuration: Cfg)\n\
     fun Client(engine: Engine, builderAction: FmtBuilder.() -> Unit = {}): Client {\n\
     \x20 val b = FmtBuilder()\n\
     \x20 b.builderAction()\n\
     \x20 return Client(engine, Cfg(b.pretty))\n\
     }\n";

#[test]
fn trailing_lambda_call_picks_the_function_over_the_constructor() {
    let main = "import lib.Fmt\n\
        fun box(): String {\n\
        \x20 val f = Fmt { pretty = true }\n\
        \x20 if (f.tag != \"default\") return \"fail tag\"\n\
        \x20 if (!f.configuration.pretty) return \"fail cfg\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpctorfn", LIB, main);
}

#[test]
fn explicit_constructor_arguments_still_pick_the_constructor() {
    let main = "import lib.Cfg\n\
        import lib.Fmt\n\
        fun box(): String {\n\
        \x20 val f = Fmt(Cfg(true), \"x\")\n\
        \x20 if (f.tag != \"x\") return \"fail tag\"\n\
        \x20 if (!f.configuration.pretty) return \"fail cfg\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpctorfnexplicit", LIB, main);
}

#[test]
fn constructor_arity_error_is_still_reported_without_a_same_named_function() {
    let main = "import lib.Solo\n\
        fun box(): String {\n\
        \x20 val s = Solo { }\n\
        \x20 return s.tag\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpctorfnnone", LIB, main) {
        assert!(
            diags.iter().any(|d| d.contains("configuration")),
            "expected the constructor's own parameter diagnostic, got: {diags:#?}"
        );
    }
}

#[test]
fn a_call_matching_neither_candidate_is_still_reported() {
    let main = "import lib.Fmt\n\
        fun box(): String {\n\
        \x20 val f = Fmt(1, 2, 3)\n\
        \x20 return f.tag\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpctorfnneither", LIB, main) {
        assert!(
            !diags.is_empty(),
            "a call matching neither the constructor nor the function must be reported"
        );
    }
}

/// The constructor and the function take the SAME argument count, so the constructor is probed first and
/// fails. That probe must not re-type the trailing lambda: typed without an expected type it becomes
/// `() -> Unit`, and the function — whose parameter is `FmtBuilder.() -> Unit` — no longer accepts it,
/// leaving `Client(engine) { … }` reported unresolved with its lambda body unresolved too.
#[test]
fn a_failed_constructor_probe_keeps_the_trailing_lambda_shaped_for_the_function() {
    let main = "import lib.Client\n\
        import lib.Engine\n\
        fun box(): String {\n\
        \x20 val c = Client(Engine(\"e\")) { pretty = true }\n\
        \x20 if (c.engine.name != \"e\") return \"fail engine\"\n\
        \x20 if (!c.configuration.pretty) return \"fail cfg\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpctorfnsamearity", LIB, main);
}
