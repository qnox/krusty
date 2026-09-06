//! Functional-interface conversion through source and dependency callable shapes.
use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

/// Top-level Kotlin function with a `Runnable` parameter, trailing lambda; the stored runnable is
/// invoked and its side effect observed.
#[test]
fn top_level_runnable_param_trailing_lambda_runs() {
    const SRC: &str = "var ran = \"\"\n\
        fun runIt(runnable: Runnable) { runnable.run() }\n\
        fun box(): String {\n\
        \x20 runIt { ran = \"OK\" }\n\
        \x20 return ran.ifEmpty { \"not ran\" }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_top_level_trailing");
}

/// Abstract member function with a `Runnable` parameter, runtime-verified through an override.
#[test]
fn abstract_member_runnable_param_trailing_lambda_runs() {
    const SRC: &str = "abstract class Manager {\n\
        \x20 abstract fun perform(runnable: Runnable): String\n\
        }\n\
        class M : Manager() {\n\
        \x20 override fun perform(runnable: Runnable): String {\n\
        \x20\x20 runnable.run()\n\
        \x20\x20 return \"ran\"\n\
        \x20 }\n\
        }\n\
        var ran = \"\"\n\
        fun box(): String {\n\
        \x20 val m: Manager = M()\n\
        \x20 val r = m.perform { ran = \"x\" }\n\
        \x20 return if (r == \"ran\" && ran == \"x\") \"OK\" else \"fail:$r:$ran\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_abstract_member");
}

/// Parenthesized (non-trailing) lambda: `runIt({ "x" })`.
#[test]
fn runnable_param_parenthesized_lambda_runs() {
    const SRC: &str = "var ran = \"\"\n\
        fun runIt(runnable: Runnable) { runnable.run() }\n\
        fun box(): String {\n\
        \x20 runIt({ ran = \"OK\" })\n\
        \x20 return ran.ifEmpty { \"not ran\" }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_parenthesized");
}

/// A GENERIC Java SAM parameter (`Consumer<String>`): a declared lambda parameter binds the SAM
/// method's substituted parameter type (String), and an implicit `it` does too. Runtime-verified.
#[test]
fn consumer_param_lambda_binds_declared_and_implicit_parameters() {
    const SRC: &str = "import java.util.function.Consumer\n\
        var seen = \"\"\n\
        fun consume(c: Consumer<String>) { c.accept(\"hello\") }\n\
        fun box(): String {\n\
        \x20 consume { s -> seen = s + \"!\" }\n\
        \x20 if (seen != \"hello!\") return \"declared:$seen\"\n\
        \x20 consume { seen = it }\n\
        \x20 return if (seen == \"hello\") \"OK\" else \"implicit:$seen\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_consumer_binding");
}

/// A type argument may specialize a generic SAM's semantic inputs to primitives even though its JVM
/// method keeps reference-erased slots. The runtime assertion covers the representation boundary as
/// well as inference: accepting the call but advertising primitive instantiated slots to the generic
/// SAM would fail when LambdaMetafactory links the closure.
#[test]
fn projected_generic_sam_boxes_specialized_primitive_boundary() {
    const SRC: &str =
        "fun <T> compareWith(cmp: java.util.Comparator<in T>, left: T, right: T): Int =\n\
        \x20 cmp.compare(left, right)\n\
        fun box(): String {\n\
        \x20 val result = compareWith<Int>({ a, b -> a - b }, 4, 9)\n\
        \x20 return if (result < 0) \"OK\" else \"result:$result\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_projected_generic_boundary");
}

/// The same semantic call shapes across a compiled dependency boundary. This is deliberately a
/// checker assertion: the contract under review is provider-neutral candidate scoring, while emitting
/// a call to an independently compiled Kotlin facade is a separate backend capability. Source-backed
/// tests above and below execute the selected SAMs, so keeping this test at the semantic boundary makes
/// a lowering skip neither hide nor masquerade as a provider-resolution regression.
#[test]
fn dependency_callables_share_sam_shape_and_preference() {
    const LIBRARY: &str = r#"
        package semantic.samboundaryfixture

        fun execute(task: Runnable): String {
            task.run()
            return "executed"
        }

        fun choose(task: Runnable): String = "adapted"
        fun choose(value: Any): String = "plain"

        class Host {
            fun consume(action: java.util.function.Consumer<String>): String {
                action.accept("member")
                return "consumed"
            }
        }
    "#;
    const MAIN: &str = r#"
        import semantic.samboundaryfixture.Host
        import semantic.samboundaryfixture.choose
        import semantic.samboundaryfixture.execute

        var observed = ""

        fun box(): String {
            val top = execute { observed = "top" }
            if (top != "executed" || observed != "top") return "top:$top:$observed"

            val member = Host().consume { observed = it }
            if (member != "consumed" || observed != "member") return "member:$member:$observed"

            val preferred = choose { }
            return if (preferred == "plain") "OK" else "preference:$preferred"
        }
    "#;

    // The package and declarations are intentionally suite-specific: e2e tests compile concurrently,
    // so a generic facade name must not let another fixture's class files influence this assertion.
    let Some(diagnostics) =
        common::checker_diags_against("semantic_sam_dependency_boundary", LIBRARY, MAIN)
    else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "dependency SAM calls should resolve through the shared candidate model: {diagnostics:?}"
    );
}

/// Overload disambiguation (kotlinc-pinned): a lambda picks `f(Runnable)` over `f(String)`, but
/// `g(Any)` over `g(Runnable)` — an exact function-type-to-Any match beats the SAM conversion.
/// Both declaration orders of the Any/Runnable pair resolve the same way.
#[test]
fn overload_picks_runnable_over_string_but_any_over_runnable() {
    const SRC: &str = "fun pick(r: Runnable): String = \"runnable\"\n\
        fun pick(s: String): String = \"string\"\n\
        fun pickAny(r: Runnable): String = \"runnable\"\n\
        fun pickAny(a: Any): String = \"any\"\n\
        fun pickAnyRev(a: Any): String = \"any\"\n\
        fun pickAnyRev(r: Runnable): String = \"runnable\"\n\
        fun box(): String {\n\
        \x20 val a = pick { }\n\
        \x20 val b = pickAny { }\n\
        \x20 val c = pickAnyRev { }\n\
        \x20 return if (a == \"runnable\" && b == \"any\" && c == \"any\") \"OK\" else \"fail:$a:$b:$c\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_overload_choice");
}

/// The conversion applies in ANY argument position, not only the trailing one (kotlinc-pinned).
#[test]
fn runnable_param_lambda_in_first_position_runs() {
    const SRC: &str = "var ran = false\n\
        fun first(r: Runnable, n: Int) { r.run() }\n\
        fun box(): String {\n\
        \x20 first({ ran = true }, 1)\n\
        \x20 return if (ran) \"OK\" else \"not ran\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_first_position");
}

/// Negative pin: a lambda into a parameter of a NON-SAM Java interface (`java.util.List<String>`)
/// must still be an argument type mismatch.
#[test]
fn lambda_against_non_sam_java_interface_still_fails() {
    let diags = diagnostics(
        "fun takeList(l: java.util.List<String>) {}\n\
         fun box(): String {\n\
         \x20 takeList { }\n\
         \x20 return \"OK\"\n\
         }\n",
    );
    assert_eq!(
        diags,
        ["argument type mismatch: actual type is '() -> Unit', but 'List<String>' was expected."]
    );
}

#[test]
fn mapped_kotlin_interface_is_not_a_sam() {
    const SOURCE: &str = "fun take(value: CharSequence) {}\n\
        fun use() { take { } }\n";
    let (reference_code, _) =
        common::kotlinc_source_result("MappedKotlinInterfaceReference", SOURCE);
    assert_ne!(
        reference_code, 0,
        "kotlinc accepted a lambda as CharSequence"
    );
    assert_eq!(
        diagnostics(SOURCE),
        ["argument type mismatch: actual type is '() -> Unit', but 'CharSequence' was expected."]
    );
}

/// Negative pin (kotlinc-verified): a lambda into a KOTLIN plain (non-`fun`) interface parameter
/// converts through no mechanism — still an argument type mismatch.
#[test]
fn lambda_against_kotlin_plain_interface_still_fails() {
    let diags = diagnostics(
        "interface Plain { fun run() }\n\
         fun takePlain(p: Plain) {}\n\
         fun box(): String {\n\
         \x20 takePlain { }\n\
         \x20 return \"OK\"\n\
         }\n",
    );
    assert_eq!(
        diags,
        ["argument type mismatch: actual type is '() -> Unit', but 'Plain' was expected."]
    );
}

/// Negative pin (shape stays supported): a Kotlin `fun interface` parameter already converts a
/// lambda through the Kotlin SAM mechanism — this keeps working.
#[test]
fn kotlin_fun_interface_param_still_converts() {
    const SRC: &str = "fun interface KRunner { fun run() }\n\
        var ran = \"\"\n\
        fun takeK(r: KRunner) { r.run() }\n\
        fun box(): String {\n\
        \x20 takeK { ran = \"OK\" }\n\
        \x20 return ran.ifEmpty { \"not ran\" }\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_kotlin_fun_interface");
}

/// A function VALUE (not only a lambda literal) converts to a Java SAM parameter. The checker records
/// the exact selected SAM and lowering builds the forwarding wrapper from that handoff.
#[test]
fn function_value_into_java_sam_param_converts() {
    const SOURCE: &str = "fun runIt(r: Runnable) { r.run() }\n\
         fun box(): String {\n\
         \x20 var result = \"fail\"\n\
         \x20 val f: () -> Unit = { result = \"OK\" }\n\
         \x20 runIt(f)\n\
         \x20 return result\n\
         }\n";
    common::expect_box_ok_with_stdlib(SOURCE, "function_value_java_sam");
}

/// Two same-arity SAM overloads remain ambiguous and list both exact candidates.
#[test]
fn two_sam_overload_top_level_lambda_is_ambiguous() {
    let diags = diagnostics(
        "import java.util.function.Consumer\n\
         fun two(r: Runnable): String = \"runnable\"\n\
         fun two(c: Consumer<String>): String = \"consumer\"\n\
         fun box(): String = two { }\n",
    );
    assert_eq!(
        diags,
        ["overload resolution ambiguity between candidates:\nfun two(r: Runnable): String\nfun two(c: Consumer<String>): String"]
    );
}

/// Negative pin (kotlinc-pinned): the same two-SAM ambiguity for MEMBER overloads — and the
/// diagnostic lists the two candidates with their distinct parameter types.
#[test]
fn two_sam_overload_member_lambda_is_ambiguous() {
    const SOURCE: &str = "import java.util.function.Consumer\n\
         class M {\n\
         \x20 fun perform(c: Consumer<String>): String = \"consumer\"\n\
         \x20 fun perform(r: Runnable): String = \"runnable\"\n\
         }\n\
         fun box(): String = M().perform { }\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("member_sam_ambiguity_reference", SOURCE);
    assert_ne!(
        reference_code, 0,
        "kotlinc accepted fixture: {reference_stderr}"
    );
    let diags = diagnostics(SOURCE);
    assert_eq!(
        diags,
        ["overload resolution ambiguity between candidates:\nfun perform(c: Consumer<String>): String\nfun perform(r: Runnable): String"]
    );
}

/// Member overloads (kotlinc-pinned): an exact `Any` match beats the SAM conversion into
/// `Runnable`, in BOTH declaration orders. Runtime-verified.
#[test]
fn member_overload_any_and_runnable_picks_any_both_orders() {
    const SRC: &str = "class M {\n\
        \x20 fun perform(r: Runnable): String = \"runnable\"\n\
        \x20 fun perform(a: Any): String = \"any\"\n\
        \x20 fun performRev(a: Any): String = \"any\"\n\
        \x20 fun performRev(r: Runnable): String = \"runnable\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val m = M()\n\
        \x20 val a = m.perform { }\n\
        \x20 val b = m.performRev { }\n\
        \x20 return if (a == \"any\" && b == \"any\") \"OK\" else \"fail:$a:$b\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_member_any_runnable");
}

/// Member overloads (kotlinc-pinned): a 1-parameter lambda cannot fit the 0-parameter
/// `Runnable` SAM, so `Consumer<String>` wins — in BOTH declaration orders. Runtime-verified.
#[test]
fn member_overload_consumer_and_runnable_picks_consumer_both_orders() {
    const SRC: &str = "import java.util.function.Consumer\n\
        class M {\n\
        \x20 fun perform(c: Consumer<String>): String = \"consumer\"\n\
        \x20 fun perform(r: Runnable): String = \"runnable\"\n\
        \x20 fun performRev(r: Runnable): String = \"runnable\"\n\
        \x20 fun performRev(c: Consumer<String>): String = \"consumer\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val m = M()\n\
        \x20 val a = m.perform { s -> s.length; \"y\" }\n\
        \x20 val b = m.performRev { s -> s.length; \"y\" }\n\
        \x20 return if (a == \"consumer\" && b == \"consumer\") \"OK\" else \"fail:$a:$b\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "sam_member_consumer_runnable");
}

/// A destructured lambda declares ONE (synthetic) parameter, so it cannot fit a two-parameter
/// SAM — kotlinc-pinned rejection. The arity mismatch must surface at the argument boundary
/// instead of silently zipping the single parameter against both SAM slots.
#[test]
fn destructured_lambda_does_not_fit_a_two_parameter_sam() {
    const SOURCE: &str = "fun take(c: java.util.function.BiConsumer<String, Int>) {}\n\
        fun use() { take { (a, b) -> } }\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("sam_destructured_arity_reference", SOURCE);
    assert_ne!(
        reference_code, 0,
        "kotlinc accepted fixture: {reference_stderr}"
    );
    let diags = diagnostics(SOURCE);
    assert_eq!(
        diags,
        [
            "krusty: cannot destructure this type (no operator 'component1')",
            "krusty: cannot destructure this type (no operator 'component2')",
            "argument type mismatch: actual type is '(String!) -> Unit', but 'BiConsumer<String, Int>' was expected.",
        ]
    );
}

/// `UnaryOperator<T>` inherits `Function<T, T>.apply`; both parameter and result specialize to `T`.
#[test]
fn inherited_sam_method_substitutes_the_interface_type_argument() {
    const SRC: &str = "fun applyTwice(u: java.util.function.UnaryOperator<Int>, x: Int): Int = u.apply(u.apply(x))\n\
        fun box(): String {\n\
        \x20 val a = applyTwice({ it + 1 }, 40)\n\
        \x20 val b = applyTwice({ v -> v * 2 }, 3)\n\
        \x20 return if (a == 42 && b == 12) \"OK\" else \"fail:$a:$b\"\n\
        }\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("InheritedJdkSamReference", SRC);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected fixture: {reference_stderr}"
    );
    common::expect_box_ok_with_stdlib(SRC, "sam_inherited_substitution");
}

/// User Java hierarchies cover multi-hop, diamond, and reordered generic substitution.
#[test]
fn user_defined_sam_hierarchy_substitutes_through_the_supertype() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let java = [
        (
            "Fn.java".to_string(),
            "package fixtures;\npublic interface Fn<T, R> { R apply(T t); }\n".to_string(),
        ),
        (
            "Mid.java".to_string(),
            "package fixtures;\npublic interface Mid<T> extends Fn<T, T> { }\n".to_string(),
        ),
        (
            "Op.java".to_string(),
            "package fixtures;\npublic interface Op<T> extends Mid<T> { }\n".to_string(),
        ),
        (
            "Left.java".to_string(),
            "package fixtures;\npublic interface Left<T> extends Fn<T, T> { }\n".to_string(),
        ),
        (
            "Right.java".to_string(),
            "package fixtures;\npublic interface Right<T> extends Fn<T, T> { }\n".to_string(),
        ),
        (
            "Diamond.java".to_string(),
            "package fixtures;\npublic interface Diamond<T> extends Left<T>, Right<T> { }\n"
                .to_string(),
        ),
        (
            "Reverse.java".to_string(),
            "package fixtures;\npublic interface Reverse<A, B> extends Fn<B, A> { }\n".to_string(),
        ),
        (
            "First.java".to_string(),
            "package fixtures;\npublic interface First { void first(); }\n".to_string(),
        ),
        (
            "Second.java".to_string(),
            "package fixtures;\npublic interface Second { void second(); }\n".to_string(),
        ),
        (
            "Both.java".to_string(),
            "package fixtures;\npublic interface Both extends First, Second { }\n".to_string(),
        ),
        (
            "Base.java".to_string(),
            "package fixtures;\npublic interface Base { void run(); }\n".to_string(),
        ),
        (
            "NoSam.java".to_string(),
            "package fixtures;\npublic interface NoSam extends Base { default void run() {} }\n"
                .to_string(),
        ),
    ];
    let (classes, _) = common::javac_compile(&java, &[])
        .expect("javac is required for the inherited Java SAM regression");
    let root = classes.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![classes, stdlib];
    const REJECTED: &str = "fun takeBoth(value: fixtures.Both) {}\n\
         fun takeNoSam(value: fixtures.NoSam) {}\n\
         fun use() {\n\
         \x20 takeBoth { }\n\
         \x20 takeNoSam { }\n\
         }\n";
    const ACCEPTED: &str = "fun runOp(o: fixtures.Op<Int>, x: Int): Int = o.apply(x)\n\
         fun runDiamond(o: fixtures.Diamond<Int>, x: Int): Int = o.apply(x)\n\
         fun runReverse(o: fixtures.Reverse<String, Int>, x: Int): String = o.apply(x)\n\
         fun box(): String {\n\
         \x20 val r = runOp({ it * 3 }, 5)\n\
         \x20 val s = runOp({ v -> v + 10 }, 5)\n\
         \x20 val d = runDiamond({ it + 1 }, 4)\n\
         \x20 val reversed = runReverse({ it.toString() }, 7)\n\
         \x20 return if (r == 15 && s == 15 && d == 5 && reversed == \"7\") \"OK\" else \"fail:$r:$s:$d:$reversed\"\n\
         }\n";
    let kotlinc_args = vec!["-classpath".to_string(), classpath[0].display().to_string()];
    let (accepted_code, accepted_stderr) = common::kotlinc_source_result_with_args(
        "InheritedUserSamReference",
        ACCEPTED,
        &kotlinc_args,
    );
    assert_eq!(
        accepted_code, 0,
        "kotlinc rejected fixture: {accepted_stderr}"
    );
    let (rejected_code, _) = common::kotlinc_source_result_with_args(
        "NonSamHierarchyReference",
        REJECTED,
        &kotlinc_args,
    );
    assert_ne!(rejected_code, 0, "kotlinc accepted non-SAM fixtures");
    let rejected = common::front_end_diagnostics(REJECTED, &classpath, Some(&jdk));
    assert_eq!(
        rejected,
        [
            "argument type mismatch: actual type is '() -> Unit', but 'Both' was expected.",
            "argument type mismatch: actual type is '() -> Unit', but 'NoSam' was expected.",
        ]
    );
    let output = common::compile_and_run_box(ACCEPTED, "Main", &classpath, Some(&jdk));
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn source_fun_interface_inherits_its_generic_sam_shape() {
    const SOURCE: &str = "fun interface Fn<T, R> { fun apply(value: T): R }\n\
        fun interface Op<T> : Fn<T, T>\n\
        fun runOp(op: Op<Int>, value: Int): Int = op.apply(value)\n\
        fun box(): String {\n\
        \x20 val result = runOp({ it + 2 }, 40)\n\
        \x20 return if (result == 42) \"OK\" else \"result:$result\"\n\
        }\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("InheritedSourceSamReference", SOURCE);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected fixture: {reference_stderr}"
    );
    assert_eq!(diagnostics(SOURCE), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(SOURCE, "source_inherited_sam");
}
