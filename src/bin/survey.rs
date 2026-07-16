use krusty::diag::DiagSink;
use krusty::ir::IrFile;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::jvm::value_classes::lower_value_classes;
use krusty::lexer::lex;
use krusty::resolve::{check_file, collect_signatures_with_cp};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

const COROUTINE_HELPERS: &str = r#"package helpers
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

fun <T> runBlocking(block: suspend () -> T): T {
    var res: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) {
        res = it
    })
    return res!!.getOrThrow()
}

fun <T> handleResultContinuation(x: (T) -> Unit): Continuation<T> = object: Continuation<T> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<T>) {
       x(result.getOrThrow())
    }
}

fun handleExceptionContinuation(x: (Throwable) -> Unit): Continuation<Any?> = object: Continuation<Any?> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<Any?>) {
       result.exceptionOrNull()?.let(x)
    }
}

open class EmptyContinuation(override val context: CoroutineContext = EmptyCoroutineContext) : Continuation<Any?> {
    companion object : EmptyContinuation()
    override fun resumeWith(result: Result<Any?>) {
       result.getOrThrow()
    }
}

class ResultContinuation : Continuation<Any?> {
    override val context = EmptyCoroutineContext
    override fun resumeWith(result: Result<Any?>) {
       this.result = result.getOrThrow()
    }

    var result: Any? = null
}
"#;

/// Run the FULL pipeline (lex→parse→sigs→check→lower→value-classes→emit) against the real
/// classpath (stdlib + JDK `lib/modules`), so skip reasons match the conformance harness — not a
/// stdlib-less front-end-only approximation. Returns the first error (with a stage prefix for the
/// silent lower/emit bailouts that carry no diagnostic).
fn first_error(src: &str, cp: &Rc<Classpath>, stem: &str) -> Option<String> {
    let mut d = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = lex(src, &mut d);
    let files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut d, &features,
    )];
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let info = check_file(&files[0], &mut syms, &mut d);
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let facade = file_class_name(stem, files[0].package.as_deref());
    let mut ir = match lower_file(&files[0], &info, &syms) {
        Some(ir) => ir,
        None => return Some(format!("lower: {}", krusty::ir_lower::lower_bail_reason())),
    };
    emit_checked_ir(&mut ir, &facade, cp)
}

fn emit_checked_ir(ir: &mut IrFile, facade: &str, cp: &Rc<Classpath>) -> Option<String> {
    let vc_lib = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    let vc_resolver = krusty::symbol_resolver::SymbolResolver::new_scoped(&vc_lib, &[]);
    if !lower_value_classes(ir, &vc_resolver) {
        return Some("lower: value-class shape not lowered".into());
    }
    if !krusty::jvm::suspend::lower_suspend(ir, facade) {
        return Some("lower: suspend-function shape not lowered".into());
    }
    // Mirror the real backend's post-transform passes (jvm/backend.rs).
    krusty::jvm::ir_emit::mark_must_inline_lambdas(ir);
    krusty::jvm::ir_emit::reparent_lambda_impls(ir);
    match emit_all(ir, facade, &**cp, None) {
        Some(o) if !o.is_empty() => None,
        _ => Some(
            krusty::jvm::ir_emit::inline_bail_reason()
                .map(|r| format!("emit: {r}"))
                .unwrap_or_else(|| "emit: emit_all bailed (unsupported codegen)".into()),
        ),
    }
}

fn first_error_with_coroutine_helpers(src: &str, cp: &Rc<Classpath>, stem: &str) -> Option<String> {
    let mut d = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let blocks = [
        (stem.to_string(), src.to_string()),
        ("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()),
    ];
    let files: Vec<_> = blocks
        .iter()
        .map(|(_, content)| {
            let toks = lex(content, &mut d);
            krusty::parser::parse_with_features(content, &toks, &mut d, &features)
        })
        .collect();
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }

    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }

    for (i, file) in files.iter().enumerate() {
        let facade = file_class_name(&blocks[i].0, file.package.as_deref());
        for &decl in &file.decls {
            match file.decl(decl) {
                krusty::ast::Decl::Fun(f) if f.receiver.is_none() && !f.is_inline => {
                    syms.fn_facades.insert(f.name.clone(), facade.clone());
                }
                krusty::ast::Decl::Property(p) if p.receiver.is_none() => {
                    if let Some(&(ty, is_var, is_const)) = syms.props.get(&p.name) {
                        syms.prop_facades
                            .insert(p.name.clone(), (facade.clone(), ty, is_var, is_const));
                    }
                }
                _ => {}
            }
        }
    }

    for (i, file) in files.iter().enumerate() {
        let info = check_file(file, &mut syms, &mut d);
        if d.has_errors() {
            return Some(d.diags[0].msg.clone());
        }
        let facade = file_class_name(&blocks[i].0, file.package.as_deref());
        let mut ir = match lower_file(file, &info, &syms) {
            Some(ir) => ir,
            None => return Some(format!("lower: {}", krusty::ir_lower::lower_bail_reason())),
        };
        if let Some(err) = emit_checked_ir(&mut ir, &facade, cp) {
            return Some(err);
        }
    }
    None
}

fn categorize(err: &str) -> String {
    if err.contains("class bodies support") {
        return "nested decl in class body".into();
    }
    if err.contains("interface default") {
        return "interface default method".into();
    }
    if err.contains("mutable local variable") {
        return "mutable lambda capture".into();
    }
    if err.contains("bridge") {
        return "bridge method".into();
    }
    if err.contains("nullable primitive") || err.ends_with("? is not supported") {
        return "nullable primitive".into();
    }
    if err.contains("value/inline") || err.contains("inline class") {
        return "value/inline class".into();
    }
    if err.contains("secondary constructor") {
        return "secondary constructor".into();
    }
    if err.contains("conflicting declarations") {
        return "conflicting declarations".into();
    }
    if err.starts_with("lower:") || err.starts_with("emit:") {
        return err[..err.len().min(70)].to_string();
    }
    if err.contains("krusty: ") {
        let m = err.trim_start_matches("krusty: ");
        return format!("krusty: {}", &m[..m.len().min(60)]);
    }
    if err.contains("expected") {
        return format!("parse: {}", &err[..err.len().min(60)]);
    }
    format!("other: {}", &err[..err.len().min(60)])
}

fn collect_kt(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut es: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() {
                collect_kt(&p, out);
            } else if p.extension().is_some_and(|e| e == "kt") {
                out.push(p);
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let box_dir = args
        .next()
        .expect("usage: survey <box_dir> [--samples <category>]");
    let samples_cat = if args.next().as_deref() == Some("--samples") {
        args.next()
    } else {
        None
    };

    // Classpath: built PER FILE from its directives via the shared `krusty::toolchain` — the SAME
    // code path (and thus the same kotlin-stdlib family + Maven fallback) the conformance gate and the
    // e2e tests use. Reusing it means the survey can't drift from the gate by reimplementing jar
    // location (the drift that once dropped the core `kotlin-stdlib.jar`, turning `mutableListOf`/
    // `listOf`/`assertEquals` into false "unresolved" blockers). The JDK `lib/modules` bootclasspath is
    // appended so `java.*` resolves. Each distinct jar-set gets one cached `Classpath` (warm indexes).
    let jdk_modules = krusty::toolchain::jdk_modules();
    let mut cp_cache: HashMap<Vec<PathBuf>, Rc<Classpath>> = HashMap::new();

    let mut errors: HashMap<String, Vec<String>> = HashMap::new();
    let mut scanned = 0u32;
    let mut compiled = 0u32;
    let mut files = Vec::new();
    collect_kt(std::path::Path::new(&box_dir), &mut files);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let src = src.replace("OPTIONAL_JVM_INLINE_ANNOTATION", "@JvmInline");
        if src.contains("// FILE:") || src.contains("// MODULE:") {
            continue;
        }
        if !src.contains("fun box()") {
            continue;
        }
        // INDY-lambda mode isn't modeled by this front-end-only survey; otherwise defer ALL backend
        // applicability to the shared `conformance` directive logic (same as the gate — no drift).
        if src.contains("// LAMBDAS: INDY") || !krusty::conformance::applies(&src) {
            continue;
        }
        scanned += 1;
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
        let mut cp_paths = krusty::toolchain::classpath_jars_for(&src);
        if let Some(j) = &jdk_modules {
            cp_paths.push(j.clone());
        }
        let cp = cp_cache
            .entry(cp_paths.clone())
            .or_insert_with(|| Rc::new(Classpath::new(cp_paths.clone())))
            .clone();
        let err = if src.contains("// WITH_COROUTINES") {
            first_error_with_coroutine_helpers(&src, &cp, stem)
        } else {
            first_error(&src, &cp, stem)
        };
        match err {
            None => compiled += 1,
            Some(e) => {
                let cat = categorize(&e);
                errors
                    .entry(cat)
                    .or_default()
                    .push(f.to_string_lossy().to_string());
            }
        }
    }
    let total_skip: u32 = errors.values().map(|v| v.len() as u32).sum();
    println!("Scanned: {scanned}  Compiled: {compiled}  Skip-errors: {total_skip}");
    let mut sorted: Vec<_> = errors.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    if let Some(cat) = &samples_cat {
        for (k, files) in &sorted {
            if k.contains(cat.as_str()) {
                println!("Category: {k} ({} files)", files.len());
                for f in files.iter() {
                    println!("{f}");
                }
            }
        }
    } else {
        for (k, v) in &sorted {
            println!("  {:4}  {k}", v.len());
        }
    }
}
