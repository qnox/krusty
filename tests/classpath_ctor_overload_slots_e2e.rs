//! Constructor overloads must share one candidate/mapping channel.
//!
//! The old dependency path turned on a detached slot-mapping channel whenever any constructor
//! parameter list had trailing defaults — e.g. a library class whose internal primary is
//! `(names: List<String>, nullable: Boolean = …, tag: String = …)` (kotlinpoet's `ClassName`
//! shape). It picked the first fitting parameter list and lost sibling overloads. Constructors now
//! contribute their declaration-owned `CallSig` to the common candidate selector; selection records
//! the winning declaration and its source-to-parameter mapping once.
//!
//! The dependency is emitted by Krusty, then consumed by Krusty. The harness's ordinary kotlinc
//! comparison remains the differential oracle for the consumer program.
use super::common;

const LIB: &str = "package lib\n\
    class Handle internal constructor(\n\
    \x20   val names: List<String>,\n\
    \x20   val nullable: Boolean = false,\n\
    \x20   val tag: String = \"\",\n\
    \x20   val marks: List<String> = emptyList(),\n\
    ) {\n\
    \x20   constructor(packageName: String, vararg simpleNames: String) :\n\
    \x20       this(listOf(packageName) + simpleNames)\n\
    \x20   constructor(packageName: String, simpleNames: List<String>) :\n\
    \x20       this(listOf(packageName) + simpleNames)\n\
    \x20   val display: String get() = names.joinToString(\".\")\n\
    }\n\
    class Config(val name: String, val nullable: Boolean = false, val tag: String = \"t\")\n";

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib("cos1", LIB)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(jdk.as_path()))
}

#[test]
fn positional_vararg_overload_survives_slot_channel() {
    // Two String args fit only the vararg secondary; the 4-slot primary (with its trailing
    // defaults) must not swallow the call.
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(\"com.example\", \"Foo\")\n\
        \x20   return if (h.display == \"com.example.Foo\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(run(MAIN).expect("positional vararg ctor overload"), "OK");
}

#[test]
fn positional_list_overload_survives_slot_channel() {
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(\"com.example\", listOf(\"Foo\"))\n\
        \x20   return if (h.display == \"com.example.Foo\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(run(MAIN).expect("positional List ctor overload"), "OK");
}

#[test]
fn named_arguments_bind_the_matching_overload() {
    // Named args were mapped against the first fitting param list (the primary), producing
    // "no parameter with name 'packageName'" instead of binding the List secondary.
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(packageName = \"com.example\", simpleNames = listOf(\"Foo\"))\n\
        \x20   return if (h.display == \"com.example.Foo\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(run(MAIN).expect("named args bind ctor overload"), "OK");
}

#[test]
fn named_vararg_array_binds_as_one_parameter() {
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(packageName = \"com.example\", simpleNames = arrayOf(\"Outer\", \"Inner\"))\n\
        \x20   return if (h.display == \"com.example.Outer.Inner\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(run(MAIN).expect("named vararg array ctor argument"), "OK");
}

#[test]
fn positional_extra_args_reach_the_vararg_overload() {
    // Three arguments exceed the vararg secondary's declared parameter count, but its own mapper
    // assigns both trailing arguments to the vararg slot before the common selector compares it
    // with the defaulted primary — the exact kotlinpoet `ClassName("pkg", "Outer", "Inner")` shape.
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(\"com.example\", \"Outer\", \"Inner\")\n\
        \x20   return if (h.display == \"com.example.Outer.Inner\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("vararg ctor with extra positional args"),
        "OK"
    );
}

#[test]
fn qualified_spelling_uses_the_same_candidate_selection() {
    // `lib.Handle(...)` reaches the same declaration candidates and records the same argument map;
    // qualification does not introduce another constructor-selection path.
    const MAIN: &str = "fun box(): String {\n\
        \x20   val h = lib.Handle(\"com.example\", \"Outer\", \"Inner\")\n\
        \x20   return if (h.display == \"com.example.Outer.Inner\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("qualified vararg ctor with extra positional args"),
        "OK"
    );
}

#[test]
fn hidden_ctor_param_list_never_claims_the_slot_mapping() {
    // The full kotlinpoet `ClassName` shape: a defaulted-suffix internal primary, a
    // HIDDEN-deprecated three-name `(packageName, simpleName, vararg simpleNames)` ctor, and the
    // visible `(packageName, vararg simpleNames)` secondary. The hidden declaration's param list
    // does not contribute a normalized constructor declaration and therefore cannot contribute an
    // argument map that disagrees with the visible member selected by the checker.
    const HLIB: &str = r#"package lib
class Poet internal constructor(
    val names: List<String>,
    val nullable: Boolean = false,
    val tag: String = "",
    val marks: List<String> = emptyList(),
) {
    @Deprecated("gone", level = DeprecationLevel.HIDDEN)
    constructor(packageName: String, simpleName: String, vararg simpleNames: String) :
        this(listOf(packageName, simpleName) + simpleNames)
    constructor(packageName: String, vararg simpleNames: String) :
        this(listOf(packageName) + simpleNames)
    val display: String get() = names.joinToString(".")
}
"#;
    const MAIN: &str = r#"import lib.Poet
fun box(): String {
    val h = Poet("com.example", "Outer", "Inner")
    return if (h.display == "com.example.Outer.Inner") "OK" else "fail:" + h.display
}
"#;
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib("cos2", HLIB).expect("Krusty dependency");
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &[libout, sl], Some(jdk.as_path()))
            .expect("hidden ctor list must not claim the slot mapping"),
        "OK"
    );
}

#[test]
fn trailing_default_primary_still_accepts_named_call() {
    // Naming a public primary's parameters with the defaulted suffix omitted (and reordered)
    // selects that declaration and carries its attached default realization into lowering.
    const MAIN: &str = "import lib.Config\n\
        fun box(): String {\n\
        \x20   val c = Config(nullable = true, name = \"n\")\n\
        \x20   return if (c.name == \"n\" && c.nullable && c.tag == \"t\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("primary named call with defaults"), "OK");
}
