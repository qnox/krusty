//! The recursive expression passes (checker and IR-lowering `expr_inner`) are depth-bounded
//! (`expr_depth`, 500) and promise the bound is survivable: past it the expression degrades
//! (`Error` type / lowering bail), the file is skipped, never crashed. That promise must hold on a
//! default 2 MiB test-thread stack even in unoptimized builds. Before the large match arms were
//! extracted, a single `expr_inner` frame was ~120–150 KiB; after extraction, the smaller recursive
//! frames still exhaust 2 MiB before reaching the documented bound. Each pass now grows a temporary
//! stack segment on the same thread when its caller lacks the required reserve. This is a front-end
//! + lowering regression guard — no classpath needed, so it runs everywhere.

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

#[test]
fn deep_left_leaning_boolean_chain_compiles_on_default_stack() {
    // 400 operands → recursion depth ~400, inside the 500 depth guard: must compile cleanly
    // through the whole pipeline without exhausting the calling thread's stack.
    let chain = vec!["true"; 400].join(" && ");
    let src = format!("fun deep(): Boolean = {chain}\n");
    let (es, emitted) = compile(&src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400-deep chain is inside the depth guard and must emit"
    );
}

#[test]
fn deep_inferred_return_chain_preinfers_on_default_stack() {
    // No declared return type → the MODULE-LEVEL pre-inference pass checks the expression body
    // before any per-file check runs, so it recurses just as deep and must survive on a default
    // stack too (it used to run the checker recursion unwrapped on the calling thread).
    let chain = vec!["true"; 400].join(" && ");
    let src = format!("fun deep() = {chain}\n");
    let mut d = DiagSink::new();
    let toks = lex(&src, &mut d);
    let files = vec![parse(&src, &toks, &mut d)];
    let mut syms = collect_signatures(&files, &mut d);
    krusty::frontend::preinfer_module_returns(&files, &mut syms, &mut d);
    check_file(&files[0], &mut syms, &mut d);
    let es: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
}

#[test]
fn deep_paren_nesting_parses_on_default_stack() {
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
    let (es, emitted) = compile(&src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "400 nested parens are inside the parser depth guard and must emit"
    );
}

#[test]
fn beyond_depth_guard_paren_nesting_degrades_without_crash() {
    // 1500 nested parens → past the parser's 1000-entry depth guard: the parser must emit a
    // diagnostic and produce an error expression — degrade, never crash — on a default 2 MiB
    // test-thread stack in an unoptimized build.
    let src = format!(
        "fun deep(): Int = {}1{}\n",
        "(".repeat(1500),
        ")".repeat(1500)
    );
    let (es, _) = compile(&src);
    assert!(
        es.iter().any(|m| m.contains("expression nesting too deep")),
        "expected the nesting-depth diagnostic for 1500 nested parens, got: {} diagnostic(s): {:?}",
        es.len(),
        es.iter().take(3).collect::<Vec<_>>()
    );
}

#[test]
fn deep_nested_call_chain_inside_guard_degrades_or_compiles_without_crash() {
    // 450-deep genuine call nesting `f(f(f(…f(1)…)))` — inside every 500 depth guard. Unlike
    // parens (grouping-only, no AST node), each level IS an AST node, so the checker and lowering
    // recurse the full 450 levels over call handling — much larger unoptimized frames than the
    // `&&`-chain levels above. Must never crash on a default 2 MiB test-thread stack.
    let src = format!(
        "fun f(x: Int): Int = x\nfun deep(): Int = {}1{}\n",
        "f(".repeat(450),
        ")".repeat(450)
    );
    let (es, emitted) = compile(&src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(
        emitted,
        "450-deep call chain is inside every depth guard and must emit"
    );
}

#[test]
fn deep_label_chain_parses_on_default_stack() {
    // 100k stacked expression labels (`l@ l@ … 1`) — the label prefix is consumed iteratively,
    // so chain length must cost O(1) stack. Per-label recursion in `parse_prefix` was an
    // unguarded, un-grown path: deep enough chains overflowed even the grown segment.
    let src = format!("fun deep(): Int = {}1\n", "l@ ".repeat(100_000));
    let (es, emitted) = compile(&src);
    assert!(es.is_empty(), "expected no diagnostics, got: {es:?}");
    assert!(emitted, "label chain is a semantic no-op and must emit");
}

#[test]
fn beyond_depth_guard_chain_degrades_without_crash() {
    // 700 operands → past the 500 depth guard: the expression may type as `Error` (diagnostics are
    // allowed), but the compiler must return, not crash.
    let chain = vec!["true"; 700].join(" && ");
    let src = format!("fun deep(): Boolean = {chain}\n");
    let _ = compile(&src);
}
