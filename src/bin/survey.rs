use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp, FrontendSymbols};
use krusty::ir::IrFile;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
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

/// Run the full pipeline against the real classpath (stdlib + JDK `lib/modules`), so skip reasons
/// match the conformance harness instead of a stdlib-less approximation. Returns the first error
/// with a stage prefix for the
/// silent lower/emit bailouts that carry no diagnostic).
fn first_error(src: &str, cp: &Rc<Classpath>, stem: &str) -> Option<String> {
    let mut d = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(src);
    let toks = lex(src, &mut d);
    let mut files = vec![krusty::parser::parse_with_features(
        src, &toks, &mut d, &features,
    )];
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` (mirrors the gate).
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
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
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut d = DiagSink::new();
    let mut files: Vec<_> = blocks
        .iter()
        .map(|(_, content)| {
            let toks = lex(content, &mut d);
            krusty::parser::parse_with_features(content, &toks, &mut d, features)
        })
        .collect();
    if d.has_errors() {
        return Err(d.diags[0].msg.clone());
    }
    // Multiplatform: a matched `expect` header is replaced by its `actual` across the set.
    if features.has("MultiPlatformProjects") {
        krusty::frontend::strip_matched_expects(&mut files);
    }

    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return Err(d.diags[0].msg.clone());
    }

    // Use the production registrar for both positive facade owners and explicit splice-only
    // outcomes. Keeping a survey-local copy previously let extension functions and backend
    // emittability policy drift from the CLI and conformance harness.
    let stems: Vec<String> = blocks.iter().map(|(stem, _)| stem.clone()).collect();
    krusty::jvm::prepare_module_symbols(&files, &stems, &mut syms);

    let mut all = Vec::new();
    for (i, file) in files.iter().enumerate() {
        d.set_file(i as u32);
        let info = check_file(file, &mut syms, &mut d);
        if d.has_errors() {
            return Err(d.diags[0].msg.clone());
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
    require_compilation_output(all)
}

/// Survey a `// MODULE:` test the way the gate's `compile_module_test` builds it: each build unit
/// (dependsOn chains folded in) compiles in declaration order against its dependency modules'
/// emitted classes on the classpath, reporting the first error anywhere in the chain.
fn first_error_module(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
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
    let result = (|| {
        for m in &krusty::conformance::module_units(&modules) {
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
            let classes = match first_error_blocks(&m.files, &cp, &features) {
                Ok(c) => c,
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
    // Backend (`lower:`/`emit:`) diagnostics are already curated, precise reasons — keep them
    // verbatim rather than re-bucketing on a substring coincidence (e.g. a `lower:` reason that
    // happens to contain "bridge").
    if err.starts_with("lower:") || err.starts_with("emit:") {
        return truncate_chars(err, 70);
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
    if err.contains("krusty: ") {
        let m = err.trim_start_matches("krusty: ");
        return format!("krusty: {}", truncate_chars(m, 60));
    }
    if err.contains("expected") {
        return format!("parse: {}", truncate_chars(err, 60));
    }
    format!("other: {}", truncate_chars(err, 60))
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
    // Deeply nested corpus sources overflow the default main stack; the gate compiles on 64 MiB
    // worker stacks — match it.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run)
        .expect("spawn survey thread")
        .join()
        .expect("survey thread panicked");
}

fn run() {
    let mut args = std::env::args().skip(1);
    let box_dir = args
        .next()
        .expect("usage: survey <box_dir> [--samples <category>]");
    let samples_cat = if args.next().as_deref() == Some("--samples") {
        args.next()
    } else {
        None
    };

    // Build each classpath from source directives through the shared `toolchain` path used by
    // conformance and e2e tests. The JDK `lib/modules` bootclasspath is appended so `java.*`
    // resolves, and each distinct jar-set gets one cached `Classpath`.
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
        if !src.contains("fun box()") {
            continue;
        }
        // INDY-lambda mode is outside this survey; otherwise defer backend
        // applicability to the shared `conformance` directive logic.
        if src.contains("// LAMBDAS: INDY") || !krusty::conformance::applies(&src) {
            continue;
        }
        scanned += 1;
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
        let base_jars = krusty::toolchain::classpath_jars_for(&src);
        let err = if src.contains("// MODULE:") {
            first_error_module(&src, &base_jars, jdk_modules.as_deref())
        } else {
            let mut cp_paths = base_jars;
            if let Some(j) = &jdk_modules {
                cp_paths.push(j.clone());
            }
            let cp = cp_cache
                .entry(cp_paths.clone())
                .or_insert_with(|| Rc::new(Classpath::new(cp_paths.clone())))
                .clone();
            if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
                // The gate's `compile_multifile` shape: `// FILE:` blocks as one module, plus the
                // generated coroutine helpers for `// WITH_COROUTINES` (even single-file).
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
                    first_error_blocks(&blocks, &cp, &features).err()
                }
            } else {
                first_error(&src, &cp, stem)
            }
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
        let out = first_error_blocks(&blocks, &cp, &features);
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
        let out = first_error_blocks(&blocks, &cp, &features);
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
            first_error("typealias Greeting = String\n", &cp, "Alias").as_deref(),
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
}
