use crate::fir_lower::tests::lower_with_jvm_stdlib_into;
use crate::ir::{ExprId, IrFile, SameModuleInlining};

const SUSPEND_CONTEXT_CALLERS: &str = r#"import kotlin.coroutines.*

inline fun applied(block: () -> String): String = block()

suspend fun yielded(value: String): String = suspendCoroutine { it.resume(value) }

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

suspend fun viaLocal(): String {
    suspend fun local(): String = run { applied { yielded("K") } }
    return local()
}

fun plain(): String {
    val lambda = { applied { "P" } }
    return lambda()
}

fun box(): String {
    var result = ""
    builder { result = applied { yielded("O") } }
    builder { result += viaLocal() }
    return result + plain()
}
"#;

fn lowered_for_compiled_bodies(source: &str) -> IrFile {
    let mut ir = IrFile::default();
    ir.set_same_module_inlining(SameModuleInlining::CompiledBodies);
    lower_with_jvm_stdlib_into(source, "Route", ir)
}

fn reaches(ir: &IrFile, root: ExprId, found: &dyn Fn(ExprId) -> bool) -> bool {
    let mut pending = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if found(expression) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

/// The bodies (by IR function name) that reach a call kept for the backend's compiled-body inliner,
/// and those that reach an expansion of a same-module inline call.
fn routed_bodies(ir: &IrFile) -> (Vec<&str>, Vec<&str>) {
    let mut compiled = Vec::new();
    let mut expanded = Vec::new();
    for function in &ir.functions {
        let Some(body) = function.body else { continue };
        if reaches(ir, body, &|e| ir.compiled_inline_call(e).is_some()) {
            compiled.push(function.name.as_str());
        }
        if reaches(ir, body, &|e| ir.inline_regions.contains(&e)) {
            expanded.push(function.name.as_str());
        }
    }
    (compiled, expanded)
}

// A suspend lambda's body, a suspend local function and a lambda written in one keep their call
// to `applied` for the compiled-body inliner, as an ordinary function's lambda does: the coroutine
// transformer builds their machines over the inlined bytecode, as kotlinc's does.
#[test]
fn calls_in_suspend_callers_are_kept_for_the_compiled_body_inliner() {
    let ir = lowered_for_compiled_bodies(SUSPEND_CONTEXT_CALLERS);
    let (compiled, expanded) = routed_bodies(&ir);
    // Only the library's inline calls are expanded: `suspendCoroutine`, `startCoroutine`, the
    // `Continuation { … }` builder and `run`, with the lambdas they are handed.
    assert_eq!(
        expanded,
        [
            "yielded",
            "builder",
            "yielded$fir_1_0_6",
            "builder$fir_2_0_7",
            "local$fir_3_0_8"
        ]
    );
    // `plain` and its lambda, the `builder { result = applied { … } }` lambda of `box` (which
    // `box` reaches through the literal it passes) and the `run { applied { … } }` lambda of `local`.
    assert_eq!(
        compiled,
        [
            "plain",
            "box",
            "local$fir_3_0_8",
            "plain$fir_4_0_11",
            "box$fir_5_0_13"
        ]
    );
}

const SUSPEND_INLINE_CALLER: &str = r#"inline fun applied(block: () -> String): String = block()

suspend inline fun outer(prefix: String): String = applied { prefix }

suspend fun run(seed: String): String = outer(seed)
"#;

// A suspend inline function's template is expanded at its call sites in IR, so the call to
// `applied` it holds is expanded with it rather than kept for the compiled-body inliner.
#[test]
fn calls_in_a_suspend_inline_function_stay_expanded() {
    let ir = lowered_for_compiled_bodies(SUSPEND_INLINE_CALLER);
    let (compiled, expanded) = routed_bodies(&ir);
    assert_eq!((compiled, expanded), (vec![], vec!["outer", "run"]));
}

const INLINE_PARAMETER_LITERALS: &str = r#"inline fun inner(g: () -> Unit) { g() }

inline fun forwards(f: () -> Unit) { inner { f() } }

inline fun forwardsDeeper(f: () -> Unit) { inner { inner { f() } } }

context(_: String)
inline fun forwardsInContext(f: () -> Unit) { inner { f() } }

inline fun ownLiteral(f: () -> Unit) {
    inner { println("own") }
    f()
}

inline fun notInlined(noinline f: () -> Unit) { inner { f() } }

fun box(): String {
    forwards {}
    forwardsDeeper {}
    with("context") { forwardsInContext {} }
    ownLiteral {}
    notInlined {}
    return "OK"
}
"#;

// A caller expanding an inline function's template splices its literal for each inline parameter,
// which it reaches only through expanded bodies: a literal reading one, at any depth, keeps its
// call expanded (`InlineParameterInLiteral`). A literal of the function's own, or one reading a
// `noinline` parameter (a value the template keeps), leaves the call to the compiled-body inliner.
// An anonymous context parameter binds no value, and the parameters after it are still recognized.
#[test]
fn literals_reading_an_inline_parameter_keep_their_call_expanded_in_the_template() {
    let ir = lowered_for_compiled_bodies(INLINE_PARAMETER_LITERALS);
    let (compiled, expanded) = routed_bodies(&ir);
    let named = |names: &[&str], wanted: &str| names.contains(&wanted);
    assert!(named(&expanded, "forwards"), "{expanded:?}");
    assert!(named(&expanded, "forwardsDeeper"), "{expanded:?}");
    assert!(named(&expanded, "forwardsInContext"), "{expanded:?}");
    assert!(!named(&compiled, "forwards"), "{compiled:?}");
    assert!(!named(&compiled, "forwardsDeeper"), "{compiled:?}");
    assert!(!named(&compiled, "forwardsInContext"), "{compiled:?}");
    assert!(named(&compiled, "ownLiteral"), "{compiled:?}");
    assert!(named(&compiled, "notInlined"), "{compiled:?}");
}

const NESTED_TEMPLATES: &str = r#"import kotlin.enums.enumEntries

enum class Color { RED, GREEN }

inline fun <reified T> named(): String = T::class.java.simpleName

inline fun <reified T> forwardsReified(): String = named<T>()

inline fun <reified T : Enum<T>> parsed(s: String): T = enumValueOf<T>(s)

@OptIn(ExperimentalStdlibApi::class)
inline fun <reified T : Enum<T>> listed(): String = enumEntries<T>().toString()

inline fun concrete(): String = enumValueOf<Color>("RED").name

fun box(): String {
    forwardsReified<Color>()
    parsed<Color>("GREEN")
    listed<Color>()
    concrete()
    return "OK"
}
"#;

// A template that expands another inline call is inlined from its compiled body: a forwarded
// reified parameter, a reified enum operation and a concrete one alike.
#[test]
fn templates_with_nested_expansions_are_inlined_from_compiled_bodies() {
    let ir = lowered_for_compiled_bodies(NESTED_TEMPLATES);
    let (compiled, _) = routed_bodies(&ir);
    let box_calls = |name: &str| {
        let function = ir
            .functions
            .iter()
            .find(|function| function.name == "box")
            .expect("box");
        reaches(&ir, function.body.expect("box body"), &|e| {
            ir.compiled_inline_call(e)
                .is_some_and(|function| ir.functions[function as usize].name == name)
        })
    };
    assert!(compiled.contains(&"box"), "{compiled:?}");
    assert!(box_calls("forwardsReified"));
    assert!(box_calls("concrete"));
    assert!(box_calls("parsed"));
    assert!(box_calls("listed"));
}
