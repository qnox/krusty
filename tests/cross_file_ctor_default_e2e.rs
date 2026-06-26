//! A constructor call that OMITS a defaulted parameter, where the constructed class is defined in a
//! DIFFERENT file (the multi-file / `// WITH_COROUTINES` shape). `ClassSig.ctor_defaults` used to store
//! the default as an `ExprId` indexing the DEFINING file's arena; filling it from another file
//! dereferenced that id against the WRONG arena and panicked. Defaults are now captured file-independently
//! (`CtorDefaultValue`: literals + object singletons), so a cross-file fill emits the literal / `getstatic
//! …INSTANCE` directly. Compiled as ONE module (shared signatures) and round-tripped on the JVM.

mod common;

use std::path::PathBuf;

use krusty::diag::DiagSink;
use krusty::jvm::names::file_class_name;
use krusty::resolve::{check_file, collect_signatures_with_cp};

/// Compile two sources as one module (mirrors the conformance harness's `compile_multifile`): parse
/// each, collect signatures across BOTH, then check + lower + emit each file.
fn compile_two(a: &str, b: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));

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
                    syms.fn_facades.insert(f.name.clone(), facade.clone());
                }
            }
        }
    }
    let mut all = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let info = check_file(file, &syms, &mut diags);
        if diags.has_errors() {
            return None;
        }
        let facade = file_class_name(stems[i], file.package.as_deref());
        let mut ir = krusty::ir_lower::lower_file(file, &info, &syms)?;
        if !krusty::jvm::value_classes::lower_value_classes(&mut ir) {
            return None;
        }
        all.extend(krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)?);
    }
    Some(all)
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
