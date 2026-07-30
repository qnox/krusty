//! The recursive expression passes (checker and IR-lowering `expr_inner`) are depth-bounded
//! (`expr_depth`, 500) and promise the bound is survivable: past it the expression degrades
//! (`Error` type / lowering bail), the file is skipped, never crashed. That promise must hold on a
//! default 2 MiB test-thread stack even in unoptimized builds, where a single `expr_inner` frame
//! is ~120–150 KiB — a left-leaning `a && b && c && …` chain only a few dozen operands long used
//! to overflow the stack long before the depth guard fired (each pass now runs on a dedicated
//! wide-stack thread sized for its guard). A front-end + lowering regression guard — no classpath
//! needed, so it runs everywhere.

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
fn beyond_depth_guard_chain_degrades_without_crash() {
    // 700 operands → past the 500 depth guard: the expression may type as `Error` (diagnostics are
    // allowed), but the compiler must return, not crash.
    let chain = vec!["true"; 700].join(" && ");
    let src = format!("fun deep(): Boolean = {chain}\n");
    let _ = compile(&src);
}
