//! A call passing a VALUE CLASS to a classpath TOP-LEVEL function was rejected — `unresolved
//! function`, or `argument type mismatch: actual type is 'lib.Tag', but 'String' was expected`.
//!
//! A `@JvmInline value class` erases to its underlying in the JVM descriptor (`Budget(val millis:
//! Long)` → `J`, `Tag(val v: String)` → `Ljava/lang/String;`) while `@Metadata` names the class. Two
//! defects, one cause — the erased form leaking into places that decide against the Kotlin type:
//!
//!  1. `jvm_libraries::top_level_overloads` published the DESCRIPTOR's parameter types, so overload
//!     selection compared a `Budget` argument against `Long`. The declared types are now restored from
//!     `@Metadata` (`MetadataCallFacts::value_class_params`) — last, after every metadata/bytecode
//!     alignment has matched the erased form the class file actually spells. The emit descriptor stays
//!     physical and the value-classes pass unboxes at the call, exactly as for a mangled MEMBER.
//!  2. `meta_param_compat`/`meta_param_exact` decided the value-class case in the FINAL arm of an
//!     `else if` chain, so a value class with a REFERENCE underlying was judged by the arm for its
//!     erasure (`Ty::String` asks only whether the metadata name IS `String`) and rejected first. Such
//!     a function lost its metadata alignment entirely — parameter names and defaults included, which
//!     is why even a call passing NO value-class argument failed. The check is now made up front.
//!
//! Members and constructors with value-class parameters were already covered
//! (`classpath_value_class_default_e2e`); this pins the top-level form, and both underlying kinds.
use super::common;

const LIB: &str = r#"
    package lib

    @JvmInline
    value class Tag(val v: String)

    @JvmInline
    value class Budget(val millis: Long)

    fun budgetOf(millis: Long): Budget = Budget(millis)

    fun taggedOnly(tag: Tag): String = tag.v

    fun tagged(label: String = "-", tag: Tag = Tag("d"), tail: Boolean = false): String =
        "$label/${tag.v}/$tail"

    fun spend(label: String = "-", budget: Budget = Budget(60), tail: Boolean = false): String =
        "$label/${budget.millis}/$tail"
"#;

#[test]
fn top_level_value_class_parameters_resolve_and_run() {
    let Some(libout) = common::compile_lib("value_class_top_level_param", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    // Covers a REFERENCE underlying (`Tag` → `String`, the one the `else if` chain rejected) and a
    // PRIMITIVE one (`Budget` → `J`, the `runTest(timeout: Duration)` shape); no defaults, all
    // defaults supplied, a trailing default omitted, the value class supplied BY NAME while an earlier
    // parameter is omitted, and — the regression guard for defect 2 — a call to a value-class-
    // parametered function that passes no value class at all, which needs its metadata names.
    let main = "import lib.Tag\n\
        import lib.budgetOf\n\
        import lib.spend\n\
        import lib.tagged\n\
        import lib.taggedOnly\n\
        fun box(): String {\n\
        \x20 if (taggedOnly(Tag(\"x\")) != \"x\") return \"only: ${taggedOnly(Tag(\"x\"))}\"\n\
        \x20 if (tagged(\"x\", Tag(\"y\"), true) != \"x/y/true\") return \"all: ${tagged(\"x\", Tag(\"y\"), true)}\"\n\
        \x20 if (tagged(\"x\", Tag(\"y\")) != \"x/y/false\") return \"omit tail: ${tagged(\"x\", Tag(\"y\"))}\"\n\
        \x20 if (tagged(tag = Tag(\"y\")) != \"-/y/false\") return \"named: ${tagged(tag = Tag(\"y\"))}\"\n\
        \x20 if (tagged(label = \"x\", tail = true) != \"x/d/true\") return \"no vc: ${tagged(label = \"x\", tail = true)}\"\n\
        \x20 if (spend(budget = budgetOf(5)) != \"-/5/false\") return \"primitive: ${spend(budget = budgetOf(5))}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}
