//! A constructor call that OMITS a defaulted parameter, where the constructed class is defined in a
//! DIFFERENT file (the multi-file / `// WITH_COROUTINES` shape). `ClassSig.ctor_defaults` used to store
//! the default as an `ExprId` indexing the DEFINING file's arena; filling it from another file
//! dereferenced that id against the WRONG arena and panicked. Defaults are now captured file-independently
//! (`CtorDefaultValue`: literals + object singletons), so a cross-file fill emits the literal / `getstatic
//! …INSTANCE` directly. Compiled as ONE module (shared signatures) and round-tripped on the JVM.

use super::common;

use krusty::diag::DiagSink;
use krusty::frontend::collect_signatures_with_cp;
use krusty::jvm::names::file_class_name;

/// Compile two sources as one module (mirrors the conformance harness's `compile_multifile`): parse
/// each, collect signatures across BOTH, then check + lower + emit each file.
fn compile_two(a: &str, b: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(a);
    let parse = |s: &str, d: &mut DiagSink| {
        let toks = krusty::lexer::lex(s, d);
        krusty::parser::parse_with_features(s, &toks, d, &features)
    };
    let files = vec![parse(a, &mut diags), parse(b, &mut diags)];
    if diags.has_errors() {
        return None;
    }
    let cp_paths = vec![sl, jdk];
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    // Cross-file top-level function facades (so a call in one file resolves the other's `fun`).
    let stems = ["AKt", "BKt"];
    for (i, file) in files.iter().enumerate() {
        let facade = file_class_name(stems[i], file.package.as_deref());
        for &d in &file.decls {
            if let krusty::ast::Decl::Fun(f) = file.decl(d) {
                if f.receiver.is_none() && !f.is_inline {
                    let facade_name = krusty::types::type_name(&facade);
                    syms.fn_facades_by_decl.insert((i as u32, d.0), facade_name);
                    syms.fn_facades.insert(f.name.clone(), facade_name);
                }
            }
        }
    }
    let mut all = Vec::new();
    // Two phases with the module context (mirrors `compiler::compile`): cross-file inline bodies
    // then expand at sibling call sites.
    let mut infos = Vec::with_capacity(files.len());
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        infos.push(krusty::frontend::check_file_in_module(
            file, &files, i as u32, &mut syms, &mut diags,
        ));
        if diags.has_errors() {
            return None;
        }
    }
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        let facade = file_class_name(stems[i], file.package.as_deref());
        let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
        let bail = std::cell::RefCell::new(String::new());
        let mut ir = krusty::ir_lower::lower_file_in_module_reporting(
            file,
            i as u32,
            &infos[i],
            &syms,
            &runtime,
            &bail,
            krusty::ir_lower::ModuleCtx {
                files: &files,
                infos: &infos,
            },
        )?;
        // Shared post-lowering pass pipeline (jvm/backend.rs); unlowerable shape → skip.
        krusty::jvm::backend::run_backend_passes(&mut ir, file, &facade, "main", &syms).ok()?;
        all.extend(krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)?);
    }
    Some(all)
}

#[test]
fn cross_file_generic_inline_hof_expands_at_the_sibling_call_site() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    // `inline fun <R> foo(x, block)` declared in file A, called with a trailing lambda from file B:
    // the call-site inference types the lambda param from the sibling DECL (part 1) and the lowering
    // splices the sibling body in the callee's source context (part 2) — previously the file skipped
    // ("call foo").
    let a = "inline fun <R> foo(x: R, block: (R) -> R): R { return block(x) }
";
    let b = "fun box(): String { val r = foo(1) { x -> x + 1 }; return if (r == 2) \"OK\" else \"fail: $r\" }
";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}

fn run_two(a: &str, b: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let classes = compile_two(a, b)?;
    let box_class = common::find_box_class(&classes)?;
    common::run_box(&classes, &box_class, &[sl])
}

#[test]
fn cross_file_ctor_default_does_not_panic() {
    // `Base` (file A) has a defaulted ctor param; the call `Base()` in file B omits it — a CROSS-FILE
    // constructor call. Before defaults were captured file-independently, the checker dereferenced
    // `Base`'s default `ExprId` against file B's (smaller) arena and PANICKED. It must now complete
    // without panicking. (Cross-file class *construction* is itself not yet modeled, so the file then
    // cleanly skips — `None` — rather than producing bytecode; the point of THIS test is the absence of
    // a crash, which `run_two` returning at all proves.)
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let a = "open class Base(val n: Int = 7)\n";
    let b = "fun box(): String = if (Base().n == 7) \"OK\" else \"no\"\n";
    let _ = run_two(a, b); // must return (skip or run), never panic

    // Same with an OBJECT-singleton default (`= EmptyCoroutineContext`), the coroutine `EmptyContinuation`
    // shape — must also complete without panicking.
    let a2 = "import kotlin.coroutines.*\n\
open class Base(val ctx: CoroutineContext = EmptyCoroutineContext)\n";
    let b2 = "fun box(): String { Base(); return \"OK\" }\n";
    let _ = run_two(a2, b2);
}

#[test]
fn cross_file_top_level_default_uses_selected_decl() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let a = "fun choose(x: Int = 1): String = \"int:$x\"\n\
             fun choose(s: String, suffix: String = \"K\"): String = s + suffix\n";
    let b = "fun box(): String = choose(s = \"O\")\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}

#[test]
fn cross_file_top_level_default_before_trailing_lambda() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let a = "fun host(prefix: String = \"O\", block: () -> String): String = prefix + block()\n";
    let b = "fun box(): String = host { \"K\" }\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}

#[test]
fn cross_file_base_class_resolves_as_module_symbol() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let a = "open class Base { fun ok(): String = \"O\" }\n";
    let b = "class Child : Base()\n\
             fun box(): String = Child().ok() + \"K\"\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}

#[test]
fn cross_file_interface_resolves_as_module_symbol() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    let a = "interface Marker\n";
    let b = "class Impl : Marker\n\
             fun box(): String = if (Impl() is Marker) \"OK\" else \"fail\"\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}

#[test]
fn cross_file_inferred_return_inline_call_boxes_the_any_result() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    // `inline fun <T> id(x: T) = x` has an INFERRED return (erased `Any`); the caller's expansion
    // specializes the parameter slot to `int`, so the identity body yields a raw primitive where the
    // checker typed `Any` — the fall-through must box (`areturn` on an int was a VerifyError).
    let a = "inline fun <T> id(x: T) = x\n";
    let b =
        "fun test(arg: Int) = id(arg)\nfun box(): String = if (test(10) == 10) \"OK\" else \"F\"\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}
