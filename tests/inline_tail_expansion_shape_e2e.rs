//! The control-flow predicate behind a tail-only inline expansion, pinned on the lowered IR.
//!
//! An expansion of a non-`Unit` inline function is lowered as `var result = zero; loop@ while (true)
//! { body; break loop@ }; result`, because a non-local `return` inside the body has to carry a value
//! out of the middle of it. When the only return IS the tail, none of that is needed and the value
//! is simply the body's — which is what kotlinc emits.
//!
//! The downstream continuation tests observe this through spill arrays, which cannot tell the two
//! shapes apart on their own: a later change could keep those arrays correct while losing the
//! control-flow invariant. These look at the IR.
use super::common;

/// The lowered file for a source that uses only the JVM platform's own types.
fn lowered(src: &str, stem: &str) -> krusty::ir::IrFile {
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
    let (files, diagnostics) = common::capture_common_ir(src, stem, platform);
    assert!(diagnostics.is_empty(), "frontend rejected: {diagnostics:?}");
    files.into_iter().next().expect("one lowered file")
}

/// Neither fixture writes a loop, so every `While` in the lowered file is an expansion's exit loop.
fn exit_loops(file: &krusty::ir::IrFile) -> usize {
    file.exprs
        .iter()
        .filter(|e| matches!(e, krusty::ir::IrExpr::While { .. }))
        .count()
}

/// The sole return is the body's last statement: the expansion produces a value directly.
#[test]
fn a_sole_tail_return_expands_without_an_exit_loop() {
    let file = lowered(
        "inline fun twice(x: Int, block: (Int) -> Int): Int {\n\
        \x20   val y = x + 1\n\
        \x20   return block(y)\n\
         }\n\
         fun run(n: Int): Int = twice(n) { it * 2 }\n",
        "TailOnly",
    );
    assert_eq!(
        exit_loops(&file),
        0,
        "a tail-only expansion needs no result local and no exit loop"
    );
}

/// An early return has to carry its value out of the middle of the body, so the result local and
/// the labelled exit loop stay. This is the half that a predicate which was too eager would break.
#[test]
fn an_early_return_keeps_its_exit_loop() {
    let file = lowered(
        "inline fun guard(x: Int, block: (Int) -> Int): Int {\n\
        \x20   if (x < 0) return 0\n\
        \x20   return block(x)\n\
         }\n\
         fun run(n: Int): Int = guard(n) { it * 2 }\n",
        "EarlyReturn",
    );
    assert_eq!(
        exit_loops(&file),
        1,
        "a non-tail return still needs the exit loop"
    );
}

/// A `Unit` expansion has no result local to begin with, so the promotion must not make its returned
/// expression run twice — or disappear.
#[test]
fn a_unit_tail_return_evaluates_its_expression_once() {
    let src = "class Sink {\n\
        \x20   var hits = 0\n\
        \x20   fun hit() { hits += 1 }\n\
        }\n\
        inline fun once(sink: Sink, block: (Sink) -> Unit) {\n\
        \x20   return block(sink)\n\
        }\n\
        fun box(): String {\n\
        \x20   val sink = Sink()\n\
        \x20   once(sink) { it.hit() }\n\
        \x20   return if (sink.hits == 1) \"OK\" else \"hits=${sink.hits}\"\n\
        }\n";
    let jdk = common::jdk_modules();
    let Some(out) = common::compile_and_run_box(
        src,
        "UnitTail",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    ) else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK");
}
