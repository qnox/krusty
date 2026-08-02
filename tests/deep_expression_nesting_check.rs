//! The recursive passes over source nesting — the checker and IR-lowering `expr_inner`, and the
//! parser's expression/type/statement/declaration funnels — are depth-bounded
//! (`expr_depth`, 500 semantic levels; 1000 parser entries) and promise the bound is survivable:
//! past it the construct degrades (`Error` type / lowering bail / parser error node with a
//! balanced skip), the file is skipped, never crashed. That promise must hold on a
//! 2 MiB thread stack even in unoptimized builds. Before the large match arms were extracted, a
//! single `expr_inner` frame was ~120–150 KiB; after extraction, the smaller recursive frames still
//! exhaust 2 MiB before reaching the documented bound. Each pass now grows temporary stack segments
//! on the same thread as needed. Every regression below executes the compiler in an EXPLICIT 2 MiB
//! child thread: the canonical suite raises `RUST_MIN_STACK` to 128 MiB for unrelated legacy tests,
//! so merely relying on libtest's ambient thread would make these tests green without exercising
//! stack growth. This is a front-end + lowering regression guard — no classpath needed, so it runs
//! everywhere.

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures};
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use krusty::parser::parse;

/// Returns (diagnostics, emitted-through-the-whole-pipeline). Lowering may legitimately bail
/// (`None`) past the depth guard — the promise under test is "degrade, never crash".
fn compile(src: &str) -> (Vec<String>, bool) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let mut syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    let mut emitted = false;
    if !d.has_errors() {
        // Drive the checked file through the rest of the in-memory pipeline (lowering → backend
        // passes → emit): each stage recurses over the same deep expression and must survive too.
        let runtime = krusty::libraries::EmptySymbolSource;
        if let Some(mut ir) = lower_file(&files[0], &info, &syms, &runtime) {
            let facade = file_class_name("Deep", None);
            krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], &facade, "main", &syms)
                .expect("backend passes should accept the deep chain");
            let cp = Classpath::new(vec![]);
            emitted = emit_all(&ir, &facade, &cp, None).is_some();
        }
    }
    (d.diags.iter().map(|x| x.msg.clone()).collect(), emitted)
}

const REGRESSION_STACK_BYTES: usize = 2 * 1024 * 1024;

/// Run one complete compiler scenario on the small stack whose survival contract this module
/// verifies. An explicit `Builder::stack_size` is essential: `run-tests.sh` deliberately exports a
/// much larger `RUST_MIN_STACK`, and CI would otherwise test only the depth diagnostics while
/// silently ceasing to test same-thread stack growth. Keep the closure on one OS thread so this
/// also exercises `stacker`'s required thread-affinity behavior.
fn on_regression_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let handle = std::thread::Builder::new()
        .name("deep-expression-2mib".to_string())
        .stack_size(REGRESSION_STACK_BYTES)
        .spawn(f)
        .expect("spawn the explicit 2 MiB expression-regression thread");
    match handle.join() {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn compile_on_regression_stack(src: String) -> (Vec<String>, bool) {
    on_regression_stack(move || compile(&src))
}

#[test]
fn deep_left_leaning_boolean_chain_compiles_on_two_mib_stack() {
    // 400 operands → recursion depth ~400, inside the 500 depth guard: must compile cleanly
    // through the whole pipeline without exhausting the calling thread's stack.
    let chain = vec!["true"; 400].join(" && ");
    let src = format!("fun deep(): Boolean = {chain}\n");
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400-deep chain is inside the depth guard and must emit"
    );
}

#[test]
fn deep_inferred_return_chain_preinfers_on_two_mib_stack() {
    // No declared return type → the MODULE-LEVEL pre-inference pass checks the expression body
    // before any per-file check runs, so it recurses just as deep and must survive on the explicit
    // 2 MiB regression stack too (it used to run the checker recursion unwrapped on that thread).
    let chain = vec!["true"; 400].join(" && ");
    let src = format!("fun deep() = {chain}\n");
    let es = on_regression_stack(move || {
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let files = vec![parse(&src, &toks, &mut d)];
        let mut syms = collect_signatures(&files, &mut d);
        krusty::frontend::preinfer_module_returns(&files, &mut syms, &mut d);
        check_file(&files[0], &mut syms, &mut d);
        d.diags.iter().map(|x| x.msg.clone()).collect::<Vec<_>>()
    });
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
}

#[test]
fn deep_paren_nesting_parses_on_two_mib_stack() {
    // 400 nested parens → PARSER recursion depth ~400 entries, inside the parser's 1000-entry
    // depth guard: must parse and compile cleanly. Parens are grouping-only (no AST node), so the
    // downstream passes see a depth-1 expression — the parser's own recursion is the pass under
    // test here. A left-leaning `&&` chain parses iteratively, so the chain tests above never
    // exercise it.
    let src = format!(
        "fun deep(): Int = {}1{}\n",
        "(".repeat(400),
        ")".repeat(400)
    );
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400 nested parens are inside the parser depth guard and must emit"
    );
}

#[test]
fn beyond_depth_guard_paren_nesting_degrades_on_two_mib_stack() {
    // 1500 nested parens → past the parser's 1000-entry depth guard: the parser must emit a
    // diagnostic and produce an error expression — degrade, never crash — on the explicit 2 MiB
    // regression stack in an unoptimized build.
    let src = format!(
        "fun deep(): Int = {}1{}\n",
        "(".repeat(1500),
        ")".repeat(1500)
    );
    let (es, _) = compile_on_regression_stack(src);
    let depth_diagnostics = es
        .iter()
        .filter(|m| m.contains("expression nesting too deep"))
        .count();
    assert_eq!(
        depth_diagnostics, 1,
        "the guard must diagnose one failed expression, not once per unwinding parser frame; got: {es:?}"
    );
    assert!(
        !es.iter().any(|m| m.contains("expected ')'")),
        "balanced recovery must consume the rejected interior while leaving one closer for each enclosing frame; got: {es:?}"
    );
}

#[test]
fn deep_nested_call_chain_inside_guard_compiles_on_two_mib_stack() {
    // 450-deep genuine call nesting `f(f(f(…f(1)…)))` — inside every 500 depth guard. Unlike
    // parens (grouping-only, no AST node), each level IS an AST node, so the checker and lowering
    // recurse the full 450 levels over call handling — much larger unoptimized frames than the
    // `&&`-chain levels above. Must compile on the explicit 2 MiB regression stack.
    let src = format!(
        "fun f(x: Int): Int = x\nfun deep(): Int = {}1{}\n",
        "f(".repeat(450),
        ")".repeat(450)
    );
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "450-deep call chain is inside every depth guard and must emit"
    );
}

#[test]
fn deep_label_chain_parses_on_two_mib_stack() {
    // 100k stacked expression labels (`l@ l@ … 1`) — the label prefix is consumed iteratively,
    // so chain length must cost O(1) stack. Per-label recursion in `parse_prefix` was an
    // unguarded, un-grown path: deep enough chains overflowed even the grown segment.
    let src = format!("fun deep(): Int = {}1\n", "l@ ".repeat(100_000));
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(emitted, "label chain is a semantic no-op and must emit");
}

#[test]
fn beyond_depth_guard_chain_degrades_on_two_mib_stack() {
    // 700 operands → past the 500 depth guard: the expression may type as `Error` (diagnostics are
    // allowed), but the compiler must return, not crash.
    let chain = vec!["true"; 700].join(" && ");
    let src = format!("fun deep(): Boolean = {chain}\n");
    let _ = compile_on_regression_stack(src);
}

/// Lex + parse only, on the regression stack — for shapes whose parser recursion is the pass under
/// test but whose parsed form (deep generic types, deep nested classes) the later passes do not yet
/// admit. Returns the parse diagnostics.
fn parse_only_on_regression_stack(src: String) -> Vec<String> {
    on_regression_stack(move || {
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let _ = parse(&src, &toks, &mut d);
        d.diags.iter().map(|x| x.msg.clone()).collect()
    })
}

#[test]
fn deep_paren_type_nesting_compiles_on_two_mib_stack() {
    // 400 nested type parens in DECLARATION position (`fun deep(): ((((Int)))) = 1`) — `parse_type`
    // recursion, not `parse_bp`, so the expression guard never sees it. Parens are grouping-only in
    // type position too (the TypeRef collapses to `Int`), so the parser's own recursion is the pass
    // under test; the rest of the pipeline sees a flat type and must emit.
    let src = format!(
        "fun deep(): {}Int{} = 1\n",
        "(".repeat(400),
        ")".repeat(400)
    );
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400 nested type parens are inside the type depth guard and must emit"
    );
}

#[test]
fn beyond_depth_guard_paren_type_nesting_degrades_on_two_mib_stack() {
    // 5000 nested type parens → far past the parser's type depth guard AND past what one grown
    // stack segment can absorb: the parser must emit ONE diagnostic and degrade to an error type —
    // never crash — on the explicit 2 MiB regression stack in an unoptimized build.
    let src = format!(
        "fun deep(): {}Int{} = 1\n",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    let es = parse_only_on_regression_stack(src);
    let depth_diagnostics = es
        .iter()
        .filter(|m| m.contains("type nesting too deep"))
        .count();
    assert_eq!(
        depth_diagnostics, 1,
        "the guard must diagnose one failed type, not once per unwinding parser frame; got: {es:?}"
    );
    assert!(
        !es.iter().any(|m| m.contains("expected ')'")),
        "balanced recovery must consume the rejected interior while leaving one closer for each enclosing frame; got: {es:?}"
    );
}

#[test]
fn deep_generic_type_nesting_parses_on_two_mib_stack() {
    // 400 nested generic arguments (`List<List<…<Int>…>>`) — `parse_type` → `parse_type_args` →
    // `parse_type` recursion in declaration position. Parse-only: the later passes recurse over the
    // genuinely deep TypeRef tree separately; the parser's stack survival is the contract here.
    let src = format!(
        "fun deep(): {}Int{} = TODO()\n",
        "List<".repeat(400),
        ">".repeat(400)
    );
    let es = parse_only_on_regression_stack(src);
    assert!(es.is_empty(), "expected no parse diagnostics, got: {es:?}");
}

#[test]
fn beyond_depth_guard_generic_type_nesting_degrades_on_two_mib_stack() {
    // 5000 nested generic arguments → past the type depth guard: one diagnostic, then a recovery
    // that treats `<`/`>` as brackets (in type position they always are) so each enclosing
    // `parse_type_args` frame finds exactly the one `>` it expects — no `expected '>'` cascade.
    let src = format!(
        "fun deep(): {}Int{} = TODO()\n",
        "List<".repeat(5000),
        ">".repeat(5000)
    );
    let es = parse_only_on_regression_stack(src);
    let depth_diagnostics = es
        .iter()
        .filter(|m| m.contains("type nesting too deep"))
        .count();
    assert_eq!(
        depth_diagnostics, 1,
        "the guard must diagnose one failed type, not once per unwinding parser frame; got: {es:?}"
    );
    assert!(
        !es.iter().any(|m| m.contains("expected '>'")),
        "angle-aware recovery must leave one '>' for each enclosing type-argument frame; got: {es:?}"
    );
}

#[test]
fn deep_block_nesting_compiles_on_two_mib_stack() {
    // 400 nested `while (true) { … }` statement blocks — `parse_stmt` → `parse_block_expr` →
    // `parse_stmt` recursion, which never passes through `parse_bp`. 400 is inside the checker's
    // and lowering's 500-level block guards too, so the whole pipeline must compile and emit on
    // the explicit 2 MiB regression stack.
    let src = format!(
        "fun deep() {{ {}val x = 0 {}}}\n",
        "while (true) { ".repeat(400),
        "} ".repeat(400)
    );
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400 nested statement blocks are inside every depth guard and must emit"
    );
}

#[test]
fn beyond_depth_guard_block_nesting_degrades_on_two_mib_stack() {
    // 5000 nested `while` blocks → far past the statement depth guard and past one grown stack
    // segment: one diagnostic, balanced recovery leaving one `}` per enclosing block frame.
    let src = format!(
        "fun deep() {{ {}val x = 0 {}}}\n",
        "while (true) { ".repeat(5000),
        "} ".repeat(5000)
    );
    let es = parse_only_on_regression_stack(src);
    let depth_diagnostics = es
        .iter()
        .filter(|m| m.contains("statement nesting too deep"))
        .count();
    assert_eq!(
        depth_diagnostics, 1,
        "the guard must diagnose one failed statement, not once per unwinding parser frame; got: {es:?}"
    );
    assert!(
        !es.iter().any(|m| m.contains("expected '}'")),
        "balanced recovery must leave one '}}' for each enclosing block frame; got: {es:?}"
    );
}

#[test]
fn beyond_depth_guard_block_nesting_never_emits_on_two_mib_stack() {
    // The same 5000-deep shape through the FULL pipeline entry: the trip diagnostic must gate
    // emission (`has_errors` short-circuits lowering/emit), pinning the property that makes the
    // parser's degraded error nodes safe downstream — nothing past the front end ever sees them.
    let src = format!(
        "fun deep() {{ {}val x = 0 {}}}\n",
        "while (true) { ".repeat(5000),
        "} ".repeat(5000)
    );
    let (es, emitted) = compile_on_regression_stack(src);
    assert!(
        es.iter().any(|m| m.contains("statement nesting too deep")),
        "the trip diagnostic must surface through the pipeline entry; got: {es:?}"
    );
    assert!(!emitted, "a tripped file must never emit");
}

#[test]
fn deep_nested_class_nesting_parses_on_two_mib_stack() {
    // 300 nested class declarations — `parse_class` → member loop → `parse_class` recursion.
    // Parse-only: nested classes hoist to flat top-level decls (`C.C.C…`), and the parser's stack
    // survival plus clean hoisting is the contract under test.
    let src = format!("{}{}", "class C { ".repeat(300), "} ".repeat(300));
    let es = parse_only_on_regression_stack(src);
    assert!(es.is_empty(), "expected no parse diagnostics, got: {es:?}");
}

#[test]
fn beyond_depth_guard_class_nesting_degrades_on_two_mib_stack() {
    // 5000 nested class declarations → past the declaration depth guard: one diagnostic, balanced
    // recovery leaving one `}` per enclosing class-body frame — degrade, never crash.
    let src = format!("{}{}", "class C { ".repeat(5000), "} ".repeat(5000));
    let es = parse_only_on_regression_stack(src);
    let depth_diagnostics = es
        .iter()
        .filter(|m| m.contains("declaration nesting too deep"))
        .count();
    assert_eq!(
        depth_diagnostics, 1,
        "the guard must diagnose one failed declaration, not once per unwinding parser frame; got: {es:?}"
    );
    assert!(
        !es.iter().any(|m| m.contains("expected '}'")),
        "balanced recovery must leave one '}}' for each enclosing class-body frame; got: {es:?}"
    );
}
