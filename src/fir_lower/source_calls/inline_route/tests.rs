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

// A suspend lambda's body, and a lambda written in a suspend local function, may run in a state
// machine, so their call to `applied` stays expanded (`SuspendCaller`); the same call in an
// ordinary function's lambda is kept for the compiled-body inliner.
#[test]
fn calls_in_suspend_lambdas_and_lambdas_of_suspend_local_functions_stay_expanded() {
    let ir = lowered_for_compiled_bodies(SUSPEND_CONTEXT_CALLERS);
    let (compiled, expanded) = routed_bodies(&ir);
    assert_eq!(compiled, ["plain", "plain$fir_4_0_11"]);
    let lambda_of = |owner: &str| {
        expanded
            .iter()
            .filter(|name| name.starts_with(&format!("{owner}$")))
            .count()
    };
    // The `builder { result = applied { … } }` lambda and the `run { applied { … } }` lambda.
    assert_eq!(
        (lambda_of("box"), lambda_of("local")),
        (1, 1),
        "{expanded:?}"
    );
}
