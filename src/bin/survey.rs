use krusty::diag::{line_col, DiagSink};
use krusty::frontend::{
    check_file, check_file_in_source_set, collect_signatures_with_cp, FrontendSymbols,
};
use krusty::ir::IrFile;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

fn first_diagnostic(stage: &str, diagnostics: &DiagSink, sources: &[(&str, &str)]) -> String {
    let diagnostic = &diagnostics.diags[0];
    let (name, source) = sources
        .get(diagnostic.file as usize)
        .copied()
        .unwrap_or(("<source>", ""));
    let (line, column) = line_col(source, diagnostic.span.lo);
    let source_line = source
        .lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .trim();
    format!(
        "{stage}: {}\n{name}:{line}:{column}: {source_line}",
        diagnostic.msg
    )
}

/// Run the full pipeline against the real classpath (stdlib + JDK `lib/modules`), so skip reasons
/// match the conformance harness instead of a stdlib-less approximation. Returns the first error
/// with a stage prefix for the
/// silent lower/emit bailouts that carry no diagnostic).
fn first_error(src: &str, cp: &Rc<Classpath>, stem: &str, frontend_only: bool) -> Option<String> {
    let mut d = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = lex(src, &mut d);
    if d.has_errors() {
        return Some(first_diagnostic("lex", &d, &[(stem, src)]));
    }
    let mut files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut d, &features,
    )];
    if d.has_errors() {
        return Some(first_diagnostic("parse", &d, &[(stem, src)]));
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` (mirrors the gate).
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return Some(first_diagnostic("signatures", &d, &[(stem, src)]));
    }
    let info = check_file(&files[0], &mut syms, &mut d);
    if d.has_errors() {
        return Some(first_diagnostic("check", &d, &[(stem, src)]));
    }
    if frontend_only {
        return None;
    }
    let facade = file_class_name(stem, files[0].package.as_deref());
    let runtime = JvmLibraries::new(cp.clone());
    let lower_bail = std::cell::RefCell::new(String::new());
    let mut ir = match krusty::ir_lower::lower_file_reporting(
        &files[0],
        &info,
        &syms,
        &runtime,
        &lower_bail,
    ) {
        Some(ir) => ir,
        None => return Some(format!("lower: {}", lower_bail.borrow())),
    };
    emit_checked_ir(&mut ir, &files[0], 0, stem, &facade, &syms, cp)
        .and_then(require_compilation_output)
        .err()
}

fn emit_checked_ir(
    ir: &mut IrFile,
    file: &krusty::ast::File,
    file_index: u32,
    stem: &str,
    facade: &str,
    syms: &FrontendSymbols,
    cp: &Rc<Classpath>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    // Shared post-lowering pass pipeline (jvm/backend.rs), so the survey's skip
    // reasons track exactly what the shipping backend declines.
    match krusty::jvm::backend::run_backend_passes(ir, file, facade, "main", syms) {
        Err(krusty::jvm::backend::SkipReason::ValueClasses) => {
            return Err("lower: value-class shape not lowered".into())
        }
        Err(krusty::jvm::backend::SkipReason::Suspend) => {
            return Err("lower: suspend-function shape not lowered".into())
        }
        Err(krusty::jvm::backend::SkipReason::Bridges) => {
            return Err("lower: bridge-method shape not lowered".into())
        }
        Ok(()) => {}
    }
    // Facade `@Metadata`, as the gate and CLI backend write — a later MODULE's compile reads this
    // module's output from the classpath and needs it to resolve cross-module extensions.
    let metadata = krusty::jvm::backend::facade_package_metadata(file, file_index, syms);
    let run = krusty::jvm::ir_emit::EmitRun::default();
    // Survey exactly the artifact shape users receive. A survey-local partial option literal used to
    // omit class metadata and `SourceFile`, masking failures that only occur when a later module reads
    // the emitted class. Filename normalization and metadata admission belong to the shared shipping
    // constructor, including for logical nested testdata paths.
    let opts = krusty::jvm::backend::shipping_emit_options(stem, "main", None, cp.clone());
    match krusty::jvm::ir_emit::emit_all_with_opts(
        ir,
        facade,
        &**cp,
        metadata.as_ref(),
        &opts,
        &run,
    ) {
        // `Some([])` is NOT a bail: an all-`expect` file (decls stripped) or a typealias-only file
        // legitimately emits zero classes (the gate's per-file acceptance, mirrored). Emptiness at
        // the whole-file-set level is reported by the callers.
        Some(o) => Ok(o),
        None => Err(run
            .inline_bail()
            .map(|r| format!("emit: {r}"))
            .unwrap_or_else(|| "emit: emit_all bailed (unsupported codegen)".into())),
    }
}

/// Skip reason for any compilation unit that lowers cleanly but has no loadable JVM output.
///
/// The examples are intentionally illustrative, not an exhaustive syntax classification: this
/// survey should classify the generic emission result instead of acquiring special cases for
/// individual source forms.
const EMITTED_NO_CLASSES: &str =
    "emit: compilation unit emitted no classes (for example expect-stripped or typealias-only sources)";

/// Enforce the conformance harness's output postcondition at the compilation-unit boundary.
///
/// `emit_checked_ir` accepts `Some([])` for an individual file because another file compiled in
/// the same source set may provide the module's runnable classes. Only the aggregate output can
/// decide whether there is anything for the gate to load. Keeping that decision here gives
/// single-file and multi-file inputs the same rule without file-, module-, or syntax-specific
/// branches in the emission path.
fn require_compilation_output(
    emitted: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if emitted.is_empty() {
        Err(EMITTED_NO_CLASSES.into())
    } else {
        Ok(emitted)
    }
}

/// The survey twin of the gate's `compile_blocks`: compile a set of already-split `(stem, content)`
/// source blocks as ONE module, reporting the FIRST error (the gate only knows pass/skip). Returns
/// the emitted classes so `// MODULE:` tests can chain them onto a dependent module's classpath.
fn first_error_blocks(
    blocks: &[(String, String)],
    cp: &Rc<Classpath>,
    features: &krusty::features::LangFeatures,
    frontend_only: bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut d = DiagSink::new();
    let mut files = Vec::with_capacity(blocks.len());
    for (index, (_, content)) in blocks.iter().enumerate() {
        d.set_file(index as u32);
        let toks = lex(content, &mut d);
        if d.has_errors() {
            let sources = blocks
                .iter()
                .map(|(name, source)| (name.as_str(), source.as_str()))
                .collect::<Vec<_>>();
            return Err(first_diagnostic("lex", &d, &sources));
        }
        files.push(krusty::parser::parse_with_features(
            content, &toks, &mut d, features,
        ));
        if d.has_errors() {
            let sources = blocks
                .iter()
                .map(|(name, source)| (name.as_str(), source.as_str()))
                .collect::<Vec<_>>();
            return Err(first_diagnostic("parse", &d, &sources));
        }
    }
    d.set_file(0);
    // Multiplatform: a matched `expect` header is replaced by its `actual` across the set.
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }

    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        let sources = blocks
            .iter()
            .map(|(name, source)| (name.as_str(), source.as_str()))
            .collect::<Vec<_>>();
        return Err(first_diagnostic("signatures", &d, &sources));
    }

    // Use the production registrar for both positive facade owners and explicit splice-only
    // outcomes. Keeping a survey-local copy previously let extension functions and backend
    // emittability policy drift from the CLI and conformance harness.
    let stems: Vec<String> = blocks.iter().map(|(stem, _)| stem.clone()).collect();
    krusty::jvm::prepare_module_symbols(&files, &stems, &mut syms);

    let mut all = Vec::new();
    for (i, file) in files.iter().enumerate() {
        d.set_file(i as u32);
        let info = check_file_in_source_set(&files, i as u32, &mut syms, &mut d);
        if d.has_errors() {
            let sources = blocks
                .iter()
                .map(|(name, source)| (name.as_str(), source.as_str()))
                .collect::<Vec<_>>();
            return Err(first_diagnostic("check", &d, &sources));
        }
        if frontend_only {
            continue;
        }
        let facade = file_class_name(&blocks[i].0, file.package.as_deref());
        let runtime = JvmLibraries::new(cp.clone());
        let lower_bail = std::cell::RefCell::new(String::new());
        let mut ir = match krusty::ir_lower::lower_file_at_reporting(
            file,
            i as u32,
            &info,
            &syms,
            &runtime,
            &lower_bail,
        ) {
            Some(ir) => ir,
            None => return Err(format!("lower: {}", lower_bail.borrow())),
        };
        all.extend(emit_checked_ir(
            &mut ir,
            file,
            i as u32,
            &blocks[i].0,
            &facade,
            &syms,
            cp,
        )?);
    }
    if frontend_only {
        Ok(all)
    } else {
        require_compilation_output(all)
    }
}

/// Survey a `// MODULE:` test the way the gate's `compile_module_test` builds it: each build unit
/// (dependsOn chains folded in) compiles in declaration order against its dependency modules'
/// emitted classes on the classpath, reporting the first error anywhere in the chain.
fn first_error_module(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    frontend_only: bool,
) -> Option<String> {
    static UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(mut modules) = krusty::conformance::split_modules(src) else {
        return Some("module: unsupported // MODULE: shape".into());
    };
    // kotlinc's `// WITH_COROUTINES` helpers live in an implicit `support` module every module sees.
    if krusty::conformance::directive(src, "WITH_COROUTINES") {
        krusty::conformance::inject_support_module(&mut modules, COROUTINE_HELPERS);
    }
    let features = krusty::features::LangFeatures::from_source(src);
    let uid = UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("krusty_survey_mod_{}_{uid}", std::process::id()));
    let mut dirmap: HashMap<String, PathBuf> = HashMap::new();
    let units = krusty::conformance::module_units(&modules);
    let result = (|| {
        for (module_index, m) in units.iter().enumerate() {
            if !m.java_files.is_empty() {
                return Some("module: .java sources (javac-dependent, gate-only)".into());
            }
            if m.files.is_empty() {
                // An empty hmpp intermediate built standalone: nothing to compile, but dependents
                // still resolve its (empty) classpath dir.
                let moddir = tmp.join(&m.name);
                if std::fs::create_dir_all(&moddir).is_err() {
                    return Some("module: failed writing dependency classes".into());
                }
                dirmap.insert(m.name.clone(), moddir);
                continue;
            }
            let mut cp_paths = cp_jars.to_vec();
            for d in &m.deps {
                match dirmap.get(d) {
                    Some(p) => cp_paths.push(p.clone()),
                    None => return Some("module: dependency declared out of order".into()),
                }
            }
            if let Some(j) = jdk_modules {
                cp_paths.push(j.to_path_buf());
            }
            // Dependency-class dirs are unique per test — a fresh Classpath, not the shared cache.
            let cp = Rc::new(Classpath::new(cp_paths));
            // A later module resolves this unit through its emitted classpath. Only those dependency
            // units need backend output during a frontend survey; the terminal unit stops after
            // checking. If a dependency cannot be emitted, the frontend survey cannot inspect its
            // consumers faithfully, so leave that case to the full conformance gate instead of
            // misclassifying a backend limitation as a frontend error.
            let needed_by_later = units[module_index + 1..]
                .iter()
                .any(|later| later.deps.iter().any(|dependency| dependency == &m.name));
            let check_only = frontend_only && !needed_by_later;
            let classes = match first_error_blocks(&m.files, &cp, &features, check_only) {
                Ok(c) => c,
                Err(e) if frontend_only && (e.starts_with("lower:") || e.starts_with("emit:")) => {
                    return None
                }
                Err(e) => return Some(e),
            };
            let moddir = tmp.join(&m.name);
            // Created even when the unit emits nothing (an empty hmpp intermediate), so a
            // dependent's classpath entry exists.
            if std::fs::create_dir_all(&moddir).is_err() {
                return Some("module: failed writing dependency classes".into());
            }
            for (name, bytes) in &classes {
                let path = moddir.join(format!("{name}.class"));
                if std::fs::create_dir_all(path.parent().unwrap_or(&moddir)).is_err()
                    || std::fs::write(&path, bytes).is_err()
                {
                    return Some("module: failed writing dependency classes".into());
                }
            }
            dirmap.insert(m.name.clone(), moddir);
        }
        None
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Copy at most `limit` Unicode scalar values from a diagnostic.
///
/// Survey input can contain source-defined Unicode identifiers. Byte slicing at an arbitrary display
/// limit can panic in the reporting path, so every fallback bucket uses this shared safe truncation.
fn truncate_chars(message: &str, limit: usize) -> String {
    message.chars().take(limit).collect()
}

fn categorize(err: &str) -> String {
    let err = err.lines().next().unwrap_or(err);
    // Backend (`lower:`/`emit:`) diagnostics are already curated, precise reasons — keep them
    // verbatim rather than re-bucketing on a substring coincidence (e.g. a `lower:` reason that
    // happens to contain "bridge").
    if err.starts_with("lower:") || err.starts_with("emit:") {
        return truncate_chars(err, 70);
    }
    for stage in ["lex:", "parse:", "signatures:", "check:"] {
        if err.starts_with(stage) {
            return truncate_chars(err, 90);
        }
    }
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
    if err.starts_with("compiler panic:") {
        return format!("compiler panic: {}", truncate_chars(err, 80));
    }
    if err.contains("krusty: ") {
        let m = err.trim_start_matches("krusty: ");
        return format!("krusty: {}", truncate_chars(m, 60));
    }
    if err.contains("expected") {
        return format!("parse: {}", truncate_chars(err, 60));
    }
    format!("other: {}", truncate_chars(err, 60))
}

fn main() {
    run();
}

#[derive(Debug)]
enum SurveyOutcome {
    Passed,
    Failed(String),
    NotApplicable,
}

fn survey_file(
    file: &Path,
    jdk_modules: Option<&Path>,
    frontend_only: bool,
    cp_cache: &mut HashMap<Vec<PathBuf>, Rc<Classpath>>,
) -> SurveyOutcome {
    krusty::trace_compiler!("survey", "checking {}", file.display());
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let src = krusty::conformance::prepare_test_source(&src);
    if !krusty::conformance::backend_applicable(&src, krusty::conformance::BACKENDS) {
        return SurveyOutcome::NotApplicable;
    }
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
    let base_jars = krusty::toolchain::classpath_jars_for(&src);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if src.contains("// MODULE:") {
            first_error_module(&src, &base_jars, jdk_modules, frontend_only)
        } else {
            let mut cp_paths = base_jars;
            if let Some(jdk) = jdk_modules {
                cp_paths.push(jdk.to_path_buf());
            }
            let cp = cp_cache
                .entry(cp_paths.clone())
                .or_insert_with(|| Rc::new(Classpath::new(cp_paths)))
                .clone();
            if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
                let (mut blocks, java_blocks) = krusty::conformance::split_files(&src);
                if blocks.is_empty() && java_blocks.is_empty() {
                    blocks.push((stem.to_string(), src.to_string()));
                }
                if src.contains("// WITH_COROUTINES") {
                    blocks.push(("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()));
                }
                if !java_blocks.is_empty() {
                    Some("multifile: .java sources (javac-dependent, gate-only)".into())
                } else {
                    let features = krusty::features::LangFeatures::from_source(&src);
                    first_error_blocks(&blocks, &cp, &features, frontend_only).err()
                }
            } else {
                first_error(&src, &cp, stem, frontend_only)
            }
        }
    })) {
        Ok(None) => SurveyOutcome::Passed,
        Ok(Some(error)) => SurveyOutcome::Failed(error),
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic payload");
            SurveyOutcome::Failed(format!("compiler panic: {message}"))
        }
    }
}

fn run() {
    let mut args = std::env::args().skip(1);
    let box_dir = args.next().expect(
        "usage: survey <box_dir> [--frontend-only] [--file <path>] [--samples <category>] [--report <path>]",
    );
    let mut samples_cat = None;
    let mut report_path = None;
    let mut only_file = None;
    let mut frontend_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frontend-only" => frontend_only = true,
            "--file" => {
                only_file = Some(PathBuf::from(args.next().expect("--file requires a path")));
            }
            "--samples" => {
                samples_cat = Some(args.next().expect("--samples requires a category"));
            }
            "--report" => {
                report_path = Some(PathBuf::from(
                    args.next().expect("--report requires a path"),
                ));
            }
            _ => panic!("unknown survey argument: {arg}"),
        }
    }

    let jdk_modules = krusty::toolchain::jdk_modules();
    let limit = std::env::var("KRUSTY_BOX_LIMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);
    let files = match only_file.as_ref() {
        Some(path) => vec![if path.is_absolute() {
            path.clone()
        } else {
            Path::new(&box_dir).join(path)
        }],
        None => krusty::conformance::evenly_sample(
            krusty::conformance::kotlin_files(std::path::Path::new(&box_dir)),
            limit,
        ),
    };
    let jobs = std::env::var("KRUSTY_SURVEY_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        })
        .max(1)
        .min(files.len().max(1));
    let next = AtomicUsize::new(0);
    let completed = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let active = Arc::new(Mutex::new(vec![None::<String>; jobs]));
    {
        let completed = Arc::clone(&completed);
        let finished = Arc::clone(&finished);
        let active = Arc::clone(&active);
        let total = files.len();
        std::thread::Builder::new()
            .name("survey-progress".to_string())
            .spawn(move || {
                while !finished.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(15));
                    if finished.load(Ordering::Relaxed) {
                        break;
                    }
                    let active = active.lock().expect("survey progress lock poisoned");
                    eprintln!(
                        "survey progress: {}/{} complete; active: {}",
                        completed.load(Ordering::Relaxed),
                        total,
                        active
                            .iter()
                            .enumerate()
                            .filter_map(|(worker, file)| {
                                file.as_ref().map(|file| format!("worker {worker}: {file}"))
                            })
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                }
            })
            .expect("spawn survey progress reporter");
    }
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(jobs);
        for worker in 0..jobs {
            let files = &files;
            let next = &next;
            let results = &results;
            let completed = Arc::clone(&completed);
            let active = Arc::clone(&active);
            let jdk_modules = jdk_modules.as_deref();
            workers.push(
                std::thread::Builder::new()
                    .name(format!("survey-{worker}"))
                    .stack_size(64 * 1024 * 1024)
                    .spawn_scoped(scope, move || {
                        // `Classpath` contains thread-local `Rc` state. Each worker reuses its own
                        // small set rather than sharing it or rebuilding it for every corpus file.
                        let mut cp_cache = HashMap::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(file) = files.get(index) else { break };
                            active.lock().expect("survey progress lock poisoned")[worker] =
                                Some(file.to_string_lossy().into_owned());
                            let outcome =
                                survey_file(file, jdk_modules, frontend_only, &mut cp_cache);
                            results
                                .lock()
                                .expect("survey result lock poisoned")
                                .push((file.to_string_lossy().to_string(), outcome));
                            completed.fetch_add(1, Ordering::Relaxed);
                        }
                        active.lock().expect("survey progress lock poisoned")[worker] = None;
                    })
                    .expect("spawn survey worker"),
            );
        }
        for worker in workers {
            worker.join().expect("survey worker panicked");
        }
    });
    finished.store(true, Ordering::Relaxed);

    let mut results = results.into_inner().expect("survey result lock poisoned");
    results.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if only_file.is_some() {
        for (file, outcome) in &results {
            match outcome {
                SurveyOutcome::Passed => println!("File: {file}\nFrontend: OK"),
                SurveyOutcome::Failed(error) => println!("File: {file}\nError: {error}"),
                SurveyOutcome::NotApplicable => {
                    println!("File: {file}\nNot applicable to JVM_IR/K2")
                }
            }
        }
    }
    let discovered = results.len() as u32;
    let mut attempted = 0u32;
    let mut passed = 0u32;
    let mut not_applicable = 0u32;
    let mut errors: HashMap<String, Vec<String>> = HashMap::new();
    for (file, outcome) in results {
        match outcome {
            SurveyOutcome::Passed => {
                attempted += 1;
                passed += 1;
            }
            SurveyOutcome::Failed(e) => {
                attempted += 1;
                let cat = categorize(&e);
                errors.entry(cat).or_default().push(file);
            }
            SurveyOutcome::NotApplicable => not_applicable += 1,
        }
    }
    let failed: u32 = errors.values().map(|v| v.len() as u32).sum();
    println!(
        "Discovered: {discovered}  Applicable: {attempted}  Passed: {passed}  Failed: {failed}  Not-applicable: {not_applicable}"
    );
    let mut sorted: Vec<_> = errors.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    if let Some(path) = report_path {
        let mut report = String::from("count\tcategory\tfile\n");
        for (category, files) in &sorted {
            let category = category.replace('\t', "\\t").replace('\n', "\\n");
            for file in *files {
                report.push_str(&format!("{}\t{category}\t{file}\n", files.len()));
            }
        }
        std::fs::write(&path, report).unwrap_or_else(|error| {
            panic!("failed to write survey report {}: {error}", path.display())
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stdlib + JDK `lib/modules` compile classpath, or `None` → skip (toolchain unprovisioned).
    fn test_cp() -> Option<Rc<Classpath>> {
        let stdlib = krusty::toolchain::stdlib_jar()?;
        let mut paths = vec![stdlib];
        if let Some(j) = krusty::toolchain::jdk_modules() {
            paths.push(j);
        }
        Some(Rc::new(Classpath::new(paths)))
    }

    /// An all-`expect` common FILE legitimately emits ZERO classes after `strip_matched_expects`
    /// (kotlinc's JVM-MPP model) — the block set must still compile, mirroring the gate's
    /// per-file-empty acceptance. Regression pin: the survey used to misreport this as
    /// `emit: emit_all bailed`, its top skip-bucket (~68 corpus files).
    #[test]
    fn all_expect_file_in_set_is_not_an_emit_bail() {
        let Some(cp) = test_cp() else { return };
        let blocks = vec![
            (
                "Common".to_string(),
                "expect class S\nexpect fun make(): S\n".to_string(),
            ),
            (
                "Main".to_string(),
                "actual typealias S = String\nactual fun make(): S = \"OK\"\nfun box(): String = make()\n"
                    .to_string(),
            ),
        ];
        let features =
            krusty::features::LangFeatures::from_source("// LANGUAGE: +MultiPlatformProjects");
        let out = first_error_blocks(&blocks, &cp, &features, false);
        assert!(
            out.is_ok(),
            "an all-expect file must not bail the whole set: {out:?}"
        );
    }

    /// A block set whose files emit NOTHING (a typealias-only file lowers to no classes) is a skip
    /// with its PRECISE reason — not the `emit_all bailed` catch-all (mirrors the gate's
    /// whole-module-empty skip).
    #[test]
    fn typealias_only_set_reports_precise_reason() {
        let Some(cp) = test_cp() else { return };
        let blocks = vec![(
            "Alias".to_string(),
            "typealias Greeting = String\n".to_string(),
        )];
        let features = krusty::features::LangFeatures::from_source("");
        let out = first_error_blocks(&blocks, &cp, &features, false);
        assert_eq!(
            out.err().as_deref(),
            Some(
                "emit: compilation unit emitted no classes (for example expect-stripped or typealias-only sources)"
            )
        );
    }

    /// A single-file survey goes through the same compilation-unit postcondition as a block set.
    /// This prevents the two entry points from drifting back to separate file/module rules while
    /// still requiring both to report a successful-but-empty emission precisely.
    #[test]
    fn typealias_only_file_uses_compilation_unit_empty_reason() {
        let Some(cp) = test_cp() else { return };
        assert_eq!(
            first_error("typealias Greeting = String\n", &cp, "Alias", false).as_deref(),
            Some(EMITTED_NO_CLASSES)
        );
    }

    #[test]
    fn frontend_only_module_stops_after_checking() {
        let source = "// MODULE: lib\n// FILE: alias.kt\ntypealias Greeting = String\n\
                      // MODULE: main(lib)\n// FILE: main.kt\nfun box() = \"OK\"\n";
        assert_eq!(first_error_module(source, &[], None, true), None);
        assert_eq!(
            first_error_module(source, &[], None, false).as_deref(),
            Some(EMITTED_NO_CLASSES)
        );
    }

    #[test]
    fn backend_categories_are_not_rebucketed_by_incidental_words() {
        assert_eq!(
            categorize("lower: gate:suspend-erasure-bridge"),
            "lower: gate:suspend-erasure-bridge"
        );
        assert_eq!(
            categorize("emit: inline splice failed"),
            "emit: inline splice failed"
        );
    }

    #[test]
    fn diagnostic_truncation_respects_unicode_boundaries() {
        let message = "é".repeat(80);
        let truncated = truncate_chars(&message, 60);
        assert_eq!(truncated.chars().count(), 60);
        assert!(message.starts_with(&truncated));
    }

    #[test]
    fn frontend_stage_is_not_guessed_from_diagnostic_words() {
        assert_eq!(
            categorize("check: argument type mismatch: expected String"),
            "check: argument type mismatch: expected String"
        );
        assert_eq!(
            categorize("signatures: unresolved reference 'Missing'."),
            "signatures: unresolved reference 'Missing'."
        );
    }
}
