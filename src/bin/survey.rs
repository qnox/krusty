use krusty::diag::{line_col, DiagSink, Severity};
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
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

// Declaration-compatible frontend twin of the Kotlin codegen test runner's
// `TailCallOptimizationChecker`. The real helper inspects generated stack frames at runtime; a
// frontend census needs only its exact callable surface and must not score the runner-provided name
// as an unresolved source declaration.
const TAIL_CALL_OPTIMIZATION_CHECKER_HEADERS: &str = r#"package helpers
import kotlin.coroutines.Continuation

class TailCallOptimizationCheckerClass {
    suspend fun saveStackTrace() {}
    fun saveStackTrace(c: Continuation<*>) {}
    fun checkNoStateMachineIn(method: String) {}
    fun checkStateMachineIn(method: String) {}
}

val TailCallOptimizationChecker = TailCallOptimizationCheckerClass()
"#;

fn add_frontend_directive_helpers(src: &str, blocks: &mut Vec<(String, String)>) {
    if krusty::conformance::directive(src, "CHECK_TAIL_CALL_OPTIMIZATION") {
        blocks.push((
            "TailCallOptimizationChecker".to_string(),
            TAIL_CALL_OPTIMIZATION_CHECKER_HEADERS.to_string(),
        ));
    }
}

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
    let features = krusty::features::LangFeatures::from_source(src);
    first_error_blocks(
        &[(stem.to_string(), src.to_string())],
        &[],
        0,
        cp,
        &features,
        frontend_only,
    )
    .err()
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
/// An individual file may emit nothing because another file compiled in the same source set owns
/// the module's runnable classes. Only the aggregate output can
/// decide whether there is anything for the gate to load. Keeping that decision here gives
/// single-file and multi-file inputs the same rule without file-, module-, or syntax-specific
/// branches in the emission path.
fn require_compilation_output(
    emitted: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if !emitted.iter().any(|(name, _)| name.ends_with(".class")) {
        Err(EMITTED_NO_CLASSES.into())
    } else {
        Ok(emitted)
    }
}

/// Frontend-only census over one already-split source set: run the PRODUCTION two-pass front end
/// (compact headers, lazy signature solving, AST-to-checked-FIR body construction) with no backend
/// attached, and report the first frontend refusal.
///
/// This is deliberately not the legacy `lex → parse → collect → check` path: production emits from
/// checked FIR, so a "frontend conformance" number that stops at the legacy checker measures a front
/// end that no longer ships. `krusty::compiler::check_frontend_only` never constructs `fir_lower` or
/// an emitter, so no backend gap can enter the result.
fn frontend_census_error(
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    common_file_count: usize,
    cp: &Rc<Classpath>,
    features: &krusty::features::LangFeatures,
) -> Option<String> {
    let mut diagnostics = DiagSink::new();
    let mut inputs = blocks
        .iter()
        .enumerate()
        .map(|(index, (stem, content))| {
            let input = krusty::source::SourceInput::kotlin(content).with_file_stem(stem);
            if index < common_file_count {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    inputs.extend(
        java_blocks
            .iter()
            .map(|(stem, content)| krusty::source::SourceInput::java(content).with_file_stem(stem)),
    );
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        platform,
        features,
        &mut diagnostics,
    );
    let census = krusty::compiler::check_frontend_only(analysis, &mut diagnostics);
    let sources = blocks
        .iter()
        .chain(java_blocks)
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let failure = census.failures.first()?;
    let stage = failure.stage.as_str();
    let location = failure
        .span
        .and_then(|span| {
            sources
                .get(failure.source as usize)
                .map(|(_, text)| (span, *text))
        })
        .map(|(span, text)| {
            let (line, column) = line_col(text, span.lo);
            format!(
                "{}:{line}:{column}: ",
                sources
                    .get(failure.source as usize)
                    .map(|(name, _)| *name)
                    .unwrap_or("<source>")
            )
        })
        .unwrap_or_default();
    Some(format!(
        "{stage}: {}: {location}{}",
        failure.kind, failure.detail
    ))
}

/// Stream checked FIR through production common lowering, then discard each IR unit before the
/// next source is reparsed. No target backend or metadata emitter participates.
fn common_lowering_error(
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    common_file_count: usize,
    cp: &Rc<Classpath>,
    features: &krusty::features::LangFeatures,
) -> Option<String> {
    let mut diagnostics = DiagSink::new();
    let mut inputs = blocks
        .iter()
        .enumerate()
        .map(|(index, (stem, content))| {
            let input = krusty::source::SourceInput::kotlin(content).with_file_stem(stem);
            if index < common_file_count {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    inputs.extend(
        java_blocks
            .iter()
            .map(|(stem, content)| krusty::source::SourceInput::java(content).with_file_stem(stem)),
    );
    let stems = blocks
        .iter()
        .chain(java_blocks)
        .map(|(stem, _)| stem.clone())
        .collect::<Vec<_>>();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(JvmLibraries::new(cp.clone())),
        features,
        &mut diagnostics,
    );
    krusty::compiler::lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);
    diagnostics.has_errors().then(|| {
        let sources = blocks
            .iter()
            .chain(java_blocks)
            .map(|(name, source)| (name.as_str(), source.as_str()))
            .collect::<Vec<_>>();
        first_diagnostic("lower", &diagnostics, &sources)
    })
}

/// Emit a dependency of a frontend-only module test through the same two-pass FIR pipeline as the
/// shipping CLI. The dependent module needs real classpath artifacts, but backend participation is
/// confined to this prerequisite: the frontend census itself remains backend-free.
fn emit_frontend_dependency(
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    common_file_count: usize,
    cp: &Rc<Classpath>,
    features: &krusty::features::LangFeatures,
    module_name: &str,
    retain_java_headers: bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if let Some(error) = frontend_census_error(blocks, java_blocks, common_file_count, cp, features)
    {
        return Err(error);
    }

    let mut diagnostics = DiagSink::new();
    let mut inputs = blocks
        .iter()
        .enumerate()
        .map(|(index, (stem, content))| {
            let input = krusty::source::SourceInput::kotlin(content).with_file_stem(stem);
            if index < common_file_count {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    inputs.extend(
        java_blocks
            .iter()
            .map(|(stem, content)| krusty::source::SourceInput::java(content).with_file_stem(stem)),
    );
    let stems = blocks
        .iter()
        .chain(java_blocks)
        .map(|(stem, _)| stem.clone())
        .collect::<Vec<_>>();
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        platform,
        features,
        &mut diagnostics,
    );
    let backend = krusty::jvm::JvmBackend::new(cp.clone());
    let mut outputs =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, module_name, &mut diagnostics);
    if diagnostics.has_errors() {
        let sources = blocks
            .iter()
            .chain(java_blocks)
            .map(|(name, source)| (name.as_str(), source.as_str()))
            .collect::<Vec<_>>();
        return Err(first_diagnostic("emit", &diagnostics, &sources));
    }
    // A frontend-only consumer of a separately analyzed Java source module needs the same
    // declaration headers that shaped this module. They are analysis artifacts only: the full JVM
    // pipeline still lets javac produce the real classes, and no stub is returned from that path.
    if retain_java_headers && !java_blocks.is_empty() {
        let emitted = outputs
            .iter()
            .filter_map(|(path, _)| path.strip_suffix(".class"))
            .collect::<std::collections::HashSet<_>>();
        let Some(headers) = krusty::jvm::java_stub::stub_classes(
            java_blocks,
            krusty::jvm::java_stub::StubMode::Strict,
            &|candidate| emitted.contains(candidate) || cp.class_exists(candidate),
        ) else {
            return Err(
                "check: cannot retain Java source declaration headers for a dependent frontend module"
                    .to_string(),
            );
        };
        outputs.extend(
            headers
                .into_iter()
                .map(|(internal, bytes)| (format!("{internal}.class"), bytes)),
        );
    }
    Ok(outputs)
}

/// The survey twin of the gate's `compile_blocks`: compile a set of already-split `(stem, content)`
/// source blocks as ONE module, reporting the FIRST error (the gate only knows pass/skip). Returns
/// the emitted classes so `// MODULE:` tests can chain them onto a dependent module's classpath.
fn first_error_blocks(
    blocks: &[(String, String)],
    java_blocks: &[(String, String)],
    common_file_count: usize,
    cp: &Rc<Classpath>,
    features: &krusty::features::LangFeatures,
    frontend_only: bool,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if frontend_only {
        return match frontend_census_error(blocks, java_blocks, common_file_count, cp, features) {
            Some(error) => Err(error),
            None => Ok(Vec::new()),
        };
    }
    emit_frontend_dependency(
        blocks,
        java_blocks,
        common_file_count,
        cp,
        features,
        "main",
        false,
    )
    .and_then(require_compilation_output)
}

/// Survey a `// MODULE:` test the way the gate's `compile_module_test` builds it: each build unit
/// (dependsOn chains folded in) compiles in declaration order against its dependency modules'
/// emitted classes on the classpath, reporting the first error anywhere in the chain.
fn first_error_module(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    frontend_only: bool,
    common_lowering_only: bool,
) -> Option<String> {
    static UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(mut modules) = krusty::conformance::split_modules(src) else {
        return Some("module: unsupported // MODULE: shape".into());
    };
    // kotlinc's `// WITH_COROUTINES` helpers live in an implicit `support` module every module sees.
    if krusty::conformance::directive(src, "WITH_COROUTINES") {
        krusty::conformance::inject_support_module(&mut modules, COROUTINE_HELPERS);
    }
    if frontend_only && krusty::conformance::directive(src, "CHECK_TAIL_CALL_OPTIMIZATION") {
        let Some(support) = modules.iter_mut().find(|module| module.name == "support") else {
            return Some(
                "module: CHECK_TAIL_CALL_OPTIMIZATION requires the coroutine support module".into(),
            );
        };
        add_frontend_directive_helpers(src, &mut support.files);
    }
    let features = krusty::features::LangFeatures::from_source(src);
    let uid = UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("krusty_survey_mod_{}_{uid}", std::process::id()));
    let mut dirmap: HashMap<String, PathBuf> = HashMap::new();
    let units = krusty::conformance::module_units(&modules);
    let result = (|| {
        for (module_index, m) in units.iter().enumerate() {
            if m.files.is_empty() && m.java_files.is_empty() {
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
            let mut friend_paths = Vec::new();
            for d in &m.deps {
                match dirmap.get(d) {
                    Some(p) => {
                        cp_paths.push(p.clone());
                        if m.friends.iter().any(|friend| friend == d) {
                            friend_paths.push(p.clone());
                        }
                    }
                    None => return Some("module: dependency declared out of order".into()),
                }
            }
            if let Some(j) = jdk_modules {
                cp_paths.push(j.to_path_buf());
            }
            // Dependency-class dirs are unique per test — a fresh Classpath, not the shared cache.
            let cp = Rc::new(Classpath::new_with_friend_paths_and_jdk_release(
                cp_paths,
                friend_paths,
                Some(8),
            ));
            // A later module resolves this unit through its emitted classpath. Only those dependency
            // units need backend output during a frontend survey; the terminal unit stops after
            // checking. If a dependency cannot be emitted, the frontend survey cannot inspect its
            // consumers faithfully, so leave that case to the full conformance gate instead of
            // misclassifying a backend limitation as a frontend error.
            let needed_by_later = units[module_index + 1..]
                .iter()
                .any(|later| later.deps.iter().any(|dependency| dependency == &m.name));
            let artifacts = match if (frontend_only || common_lowering_only) && needed_by_later {
                emit_frontend_dependency(
                    &m.files,
                    &m.java_files,
                    m.common_file_count,
                    &cp,
                    &features,
                    &m.name,
                    true,
                )
            } else if common_lowering_only {
                match common_lowering_error(
                    &m.files,
                    &m.java_files,
                    m.common_file_count,
                    &cp,
                    &features,
                ) {
                    Some(error) => Err(error),
                    None => Ok(Vec::new()),
                }
            } else {
                first_error_blocks(
                    &m.files,
                    &m.java_files,
                    m.common_file_count,
                    &cp,
                    &features,
                    frontend_only,
                )
            } {
                Ok(artifacts) => artifacts,
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
            for (name, bytes) in &artifacts {
                let path = moddir.join(name);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParseFailureStage {
    Harness,
    Lex,
    Parse,
    Integrity,
    Panic,
}

impl ParseFailureStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::Lex => "lex",
            Self::Parse => "parse",
            Self::Integrity => "integrity",
            Self::Panic => "panic",
        }
    }
}

#[derive(Debug)]
struct ParseFailure {
    block: String,
    stage: ParseFailureStage,
    line: usize,
    column: usize,
    message: String,
    source_line: String,
}

#[derive(Debug)]
struct ParseSurveyOutcome {
    kotlin_blocks: usize,
    failures: Vec<ParseFailure>,
}

fn parse_block_name(block: &krusty::conformance::KotlinSourceBlock) -> String {
    match &block.module {
        Some(module) => format!("{module}/{}.kt", block.name),
        None => format!("{}.kt", block.name),
    }
}

fn diagnostic_failures(
    block: &krusty::conformance::KotlinSourceBlock,
    diagnostics: &DiagSink,
    stage: ParseFailureStage,
) -> Vec<ParseFailure> {
    diagnostics
        .diags
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| {
            let (line, column) = line_col(&block.source, diagnostic.span.lo);
            ParseFailure {
                block: parse_block_name(block),
                stage,
                line,
                column,
                message: diagnostic.msg.clone(),
                source_line: block
                    .source
                    .lines()
                    .nth(line.saturating_sub(1))
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            }
        })
        .collect()
}

fn survey_parse_file(file: &Path) -> ParseSurveyOutcome {
    let raw = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            return ParseSurveyOutcome {
                kotlin_blocks: 0,
                failures: vec![ParseFailure {
                    block: "<case>".into(),
                    stage: ParseFailureStage::Harness,
                    line: 1,
                    column: 1,
                    message: format!("cannot read {}: {error}", file.display()),
                    source_line: String::new(),
                }],
            }
        }
    };
    let source = krusty::conformance::prepare_test_source(&raw);
    let fallback = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("File");
    let blocks = match krusty::conformance::kotlin_source_blocks(&source, fallback) {
        Ok(blocks) => blocks,
        Err(error) => {
            return ParseSurveyOutcome {
                kotlin_blocks: 0,
                failures: vec![ParseFailure {
                    block: "<case>".into(),
                    stage: ParseFailureStage::Harness,
                    line: 1,
                    column: 1,
                    message: error,
                    source_line: String::new(),
                }],
            }
        }
    };
    let features = krusty::features::LangFeatures::from_source(&source);
    let mut failures = Vec::new();
    for block in &blocks {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut diagnostics = DiagSink::new();
            let tokens = lex(&block.source, &mut diagnostics);
            if diagnostics.has_errors() {
                return diagnostic_failures(block, &diagnostics, ParseFailureStage::Lex);
            }
            let ast = krusty::parser::parse_with_features(
                &block.source,
                &tokens,
                &mut diagnostics,
                &features,
            );
            if diagnostics.has_errors() {
                let stage = if diagnostics.diags.iter().any(|diagnostic| {
                    diagnostic.severity == Severity::Error
                        && diagnostic.msg.starts_with("invalid parser AST:")
                }) {
                    ParseFailureStage::Integrity
                } else {
                    ParseFailureStage::Parse
                };
                return diagnostic_failures(block, &diagnostics, stage);
            }
            if let Err(error) = ast.validate_integrity(&block.source) {
                return vec![ParseFailure {
                    block: parse_block_name(block),
                    stage: ParseFailureStage::Integrity,
                    line: 1,
                    column: 1,
                    message: error,
                    source_line: block.source.lines().next().unwrap_or("").trim().to_string(),
                }];
            }
            Vec::new()
        }));
        match result {
            Ok(mut block_failures) => failures.append(&mut block_failures),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic payload");
                failures.push(ParseFailure {
                    block: parse_block_name(block),
                    stage: ParseFailureStage::Panic,
                    line: 1,
                    column: 1,
                    message: message.to_string(),
                    source_line: block.source.lines().next().unwrap_or("").trim().to_string(),
                });
            }
        }
    }
    ParseSurveyOutcome {
        kotlin_blocks: blocks.len(),
        failures,
    }
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
    common_lowering_only: bool,
    cp_cache: &mut HashMap<Vec<PathBuf>, Rc<Classpath>>,
) -> SurveyOutcome {
    krusty::trace_compiler!("survey", "checking {}", file.display());
    let src = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            return SurveyOutcome::Failed(format!(
                "harness: cannot read {}: {error}",
                file.display()
            ))
        }
    };
    let src = krusty::conformance::prepare_test_source(&src);
    // An `IGNORE_BACKEND` mute is a statement about EMISSION, so it cannot excuse a frontend
    // refusal: a frontend census scores those cases exactly as the parse-only gate does. A
    // `TARGET_BACKEND` and `METADATA_TARGET_PLATFORMS` delimit the platform source universe.
    // `DONT_TARGET_EXACT_BACKEND` alone is only a runtime/codegen mute and cannot excuse FIR/common
    // lowering. Red-code fixtures are different: they are negative diagnostic programs whose
    // ordinary bodies `-Xheader-mode` deliberately does not check, so they are routed out of this
    // positive-acceptance census. A source explicitly rejected by BOTH K1 and K2 likewise has no
    // positive frontend oracle. A lone backend/frontend mute still cannot excuse our checker.
    if !krusty::conformance::frontend_applicable(&src, krusty::conformance::BACKENDS) {
        return SurveyOutcome::NotApplicable;
    }
    if !frontend_only
        && !common_lowering_only
        && !krusty::conformance::backend_applicable(&src, krusty::conformance::BACKENDS)
    {
        return SurveyOutcome::NotApplicable;
    }
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
    let base_jars = krusty::toolchain::classpath_jars_for(&src);
    let compilation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if src.contains("// MODULE:") {
            first_error_module(
                &src,
                &base_jars,
                jdk_modules,
                frontend_only,
                common_lowering_only,
            )
        } else {
            let mut cp_paths = base_jars.clone();
            if let Some(jdk) = jdk_modules {
                cp_paths.push(jdk.to_path_buf());
            }
            let cp = cp_cache
                .entry(cp_paths.clone())
                .or_insert_with(|| Rc::new(Classpath::new_with_jdk_release(cp_paths, 8)))
                .clone();
            if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
                let (mut blocks, java_blocks) = krusty::conformance::split_files(&src);
                if blocks.is_empty() && java_blocks.is_empty() {
                    blocks.push((stem.to_string(), src.to_string()));
                }
                if src.contains("// WITH_COROUTINES") {
                    blocks.push(("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()));
                }
                if frontend_only {
                    add_frontend_directive_helpers(&src, &mut blocks);
                }
                let features = krusty::features::LangFeatures::from_source(&src);
                if common_lowering_only {
                    common_lowering_error(&blocks, &java_blocks, 0, &cp, &features)
                } else {
                    first_error_blocks(&blocks, &java_blocks, 0, &cp, &features, frontend_only)
                        .err()
                }
            } else {
                if common_lowering_only {
                    let features = krusty::features::LangFeatures::from_source(&src);
                    common_lowering_error(
                        &[(stem.to_string(), src.to_string())],
                        &[],
                        0,
                        &cp,
                        &features,
                    )
                } else {
                    first_error(&src, &cp, stem, frontend_only)
                }
            }
        }
    }));
    match compilation {
        Ok(None) => SurveyOutcome::Passed,
        Ok(Some(error))
            if krusty::conformance::dont_targets_exact_backend(
                &src,
                krusty::conformance::BACKENDS,
            ) =>
        {
            match krusty::conformance::reference_jvm_acceptance(
                &src,
                stem,
                &base_jars,
                COROUTINE_HELPERS,
            ) {
                krusty::conformance::ReferenceJvmAcceptance::Rejected => {
                    SurveyOutcome::NotApplicable
                }
                krusty::conformance::ReferenceJvmAcceptance::Accepted => {
                    SurveyOutcome::Failed(error)
                }
                krusty::conformance::ReferenceJvmAcceptance::Unavailable(oracle_error) => {
                    SurveyOutcome::Failed(format!(
                        "{error}\nharness: reference JVM acceptance unavailable: {oracle_error}"
                    ))
                }
            }
        }
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

fn tsv_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn run_parse_only(files: Vec<PathBuf>, print_failures: bool, report_path: Option<PathBuf>) {
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
    let results = Mutex::new(Vec::with_capacity(files.len()));
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(jobs);
        for worker in 0..jobs {
            let files = &files;
            let next = &next;
            let results = &results;
            workers.push(
                std::thread::Builder::new()
                    .name(format!("parse-survey-{worker}"))
                    .stack_size(64 * 1024 * 1024)
                    .spawn_scoped(scope, move || loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else { break };
                        let outcome = survey_parse_file(file);
                        results
                            .lock()
                            .expect("parse survey result lock poisoned")
                            .push((file.to_string_lossy().into_owned(), outcome));
                    })
                    .expect("spawn parse survey worker"),
            );
        }
        for worker in workers {
            worker.join().expect("parse survey worker panicked");
        }
    });

    let mut results = results
        .into_inner()
        .expect("parse survey result lock poisoned");
    results.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let discovered = results.len();
    let kotlin_blocks: usize = results
        .iter()
        .map(|(_, outcome)| outcome.kotlin_blocks)
        .sum();
    let parsed = results
        .iter()
        .filter(|(_, outcome)| outcome.failures.is_empty())
        .count();
    let count_cases = |stage| {
        results
            .iter()
            .filter(|(_, outcome)| {
                outcome
                    .failures
                    .iter()
                    .any(|failure| failure.stage == stage)
            })
            .count()
    };
    let lex_failures = count_cases(ParseFailureStage::Lex);
    let parse_failures = count_cases(ParseFailureStage::Parse);
    let integrity_failures = count_cases(ParseFailureStage::Integrity);
    let panics = count_cases(ParseFailureStage::Panic);
    let harness_failures = count_cases(ParseFailureStage::Harness);

    if print_failures {
        for (file, outcome) in &results {
            if outcome.failures.is_empty() {
                println!(
                    "File: {file}\nParse: OK ({} Kotlin blocks)",
                    outcome.kotlin_blocks
                );
            } else {
                for failure in &outcome.failures {
                    println!(
                        "File: {file}\nBlock: {}\n{}:{}:{}: {}: {}\n{}",
                        failure.block,
                        failure.block,
                        failure.line,
                        failure.column,
                        failure.stage.as_str(),
                        failure.message,
                        failure.source_line,
                    );
                }
            }
        }
    }

    println!("Discovered cases: {discovered}");
    println!("Kotlin blocks:    {kotlin_blocks}");
    println!("Parsed cases:     {parsed}");
    println!("Lex failures:     {lex_failures}");
    println!("Parse failures:   {parse_failures}");
    println!("AST failures:     {integrity_failures}");
    println!("Panics:           {panics}");
    if harness_failures != 0 {
        println!("Harness failures: {harness_failures}");
    }

    if let Some(path) = report_path {
        let mut report = String::from("file\tblock\tstage\tline\tcolumn\tdiagnostic\tsource\n");
        for (file, outcome) in &results {
            for failure in &outcome.failures {
                report.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    tsv_field(file),
                    tsv_field(&failure.block),
                    failure.stage.as_str(),
                    failure.line,
                    failure.column,
                    tsv_field(&failure.message),
                    tsv_field(&failure.source_line),
                ));
            }
        }
        std::fs::write(&path, report).unwrap_or_else(|error| {
            panic!(
                "failed to write parse survey report {}: {error}",
                path.display()
            )
        });
    }

    if parsed != discovered {
        std::process::exit(1);
    }
}

fn run() {
    let mut args = std::env::args().skip(1);
    let box_dir = args.next().expect(
        "usage: survey <box_dir> [--parse-only | --frontend-only | --common-lowering-only] [--file <path>] [--samples <category>] [--report <path>]",
    );
    let mut samples_cat = None;
    let mut report_path = None;
    let mut only_file = None;
    let mut frontend_only = false;
    let mut common_lowering_only = false;
    let mut parse_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frontend-only" => frontend_only = true,
            "--common-lowering-only" => common_lowering_only = true,
            "--parse-only" => parse_only = true,
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

    if usize::from(parse_only) + usize::from(frontend_only) + usize::from(common_lowering_only) > 1
    {
        panic!("--parse-only, --frontend-only, and --common-lowering-only are mutually exclusive");
    }

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
    if parse_only {
        run_parse_only(files, only_file.is_some(), report_path);
        return;
    }
    // Kotlin's codegen corpus is compiled against its Java 8 mock-JDK surface. Resolve the same
    // public API from the selected host JDK's `ct.sym`; reading `lib/modules` here makes results
    // host-version dependent (for example JDK 21's `List.getLast()` changes Kotlin member
    // precedence in old corpus sources). An explicit survey bootclasspath remains authoritative.
    let jdk_modules = krusty::toolchain::jdk_symbols().or_else(krusty::toolchain::jdk_modules);
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
                            let outcome = survey_file(
                                file,
                                jdk_modules,
                                frontend_only,
                                common_lowering_only,
                                &mut cp_cache,
                            );
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
        let success_stage = if common_lowering_only {
            "Common lowering"
        } else {
            "Frontend"
        };
        for (file, outcome) in &results {
            match outcome {
                SurveyOutcome::Passed => println!("File: {file}\n{success_stage}: OK"),
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
        let out = first_error_blocks(&blocks, &[], 0, &cp, &features, false);
        assert!(
            out.is_ok(),
            "an all-expect file must not bail the whole set: {out:?}"
        );
    }

    /// A typealias-only file still emits its metadata facade. This is required for a dependent
    /// compilation to recover the alias and matches kotlinc's artifact shape.
    #[test]
    fn typealias_only_set_emits_its_metadata_facade() {
        let Some(cp) = test_cp() else { return };
        let blocks = vec![(
            "Alias".to_string(),
            "typealias Greeting = String\n".to_string(),
        )];
        let features = krusty::features::LangFeatures::from_source("");
        let out = first_error_blocks(&blocks, &[], 0, &cp, &features, false);
        assert!(
            out.is_ok(),
            "typealias metadata facade was not emitted: {out:?}"
        );
    }

    /// Single-file and block-set entry points both use the production FIR pipeline and observe the
    /// same metadata-facade output.
    #[test]
    fn typealias_only_file_emits_its_metadata_facade() {
        let Some(cp) = test_cp() else { return };
        assert_eq!(
            first_error("typealias Greeting = String\n", &cp, "Alias", false),
            None
        );
    }

    #[test]
    fn optional_expectation_metadata_is_visible_only_in_common_sources() {
        let Some(cp) = test_cp() else { return };
        let blocks = vec![(
            "Common".to_string(),
            "class Holder { companion object { @kotlin.js.JsStatic fun value() = \"OK\" } }"
                .to_string(),
        )];
        let features =
            krusty::features::LangFeatures::from_source("// LANGUAGE: +MultiPlatformProjects");
        let libraries = JvmLibraries::new(cp.clone());
        assert!(
            krusty::libraries::SemanticPlatform::is_optional_expectation(
                &libraries,
                krusty::types::type_name("kotlin/js/JsStatic"),
            )
        );
        assert_eq!(frontend_census_error(&blocks, &[], 1, &cp, &features), None);
        let platform_error = frontend_census_error(&blocks, &[], 0, &cp, &features);
        assert!(
            platform_error
                .as_deref()
                .is_some_and(|error| {
                    error.contains("unresolved reference") && error.contains("JsStatic")
                }),
            "a target-less optional expectation must not leak into an ordinary JVM source: {platform_error:?}"
        );
    }

    #[test]
    fn platform_actual_shadows_common_expectation_header() {
        let Some(cp) = test_cp() else { return };
        let blocks = vec![(
            "Main".to_string(),
            "@JvmInline value class Token(val value: String)".to_string(),
        )];
        let features = krusty::features::LangFeatures::from_source("");
        let libraries = JvmLibraries::new(cp.clone());
        assert!(
            !krusty::libraries::SemanticPlatform::is_optional_expectation(
                &libraries,
                krusty::types::type_name("kotlin/jvm/JvmInline"),
            )
        );
        assert_eq!(frontend_census_error(&blocks, &[], 0, &cp, &features), None);
    }

    #[test]
    fn full_survey_uses_checked_fir_for_extension_access() {
        let Some(cp) = test_cp() else { return };
        let source = "class A\n\
                      val action: Any.() -> String = { \"OK\" }\n\
                      fun box(): String = A().(action)()\n";
        assert_eq!(first_error(source, &cp, "ExtensionAccess", false), None);
    }

    #[test]
    fn frontend_only_module_stops_after_checking() {
        let source = "// MODULE: lib\n// FILE: alias.kt\ntypealias Greeting = String\n\
                      // MODULE: main(lib)\n// FILE: main.kt\nfun box() = \"OK\"\n";
        assert_eq!(first_error_module(source, &[], None, true, false), None);
        assert_eq!(first_error_module(source, &[], None, false, false), None);
    }

    #[test]
    fn frontend_only_module_emits_dependency_through_production_fir() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter\n\
                      // MODULE: lib\n// FILE: lib.kt\n\
                      interface A\ninterface B\n\
                      inline fun <reified T> select(value: T): T where T : A, T : B = value\n\
                      // MODULE: main(lib)\n// FILE: main.kt\n\
                      fun use(value: Any) { if (value is A && value is B) select(value) }\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_carries_java_dependency_headers_to_its_consumer() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib\n// FILE: JavaApi.java\n\
                      public class JavaApi { public String value() { return \"OK\"; } }\n\
                      // MODULE: main(lib)\n// FILE: main.kt\n\
                      fun box(): String = JavaApi().value()\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_tail_call_directive_helper_has_the_runner_callable_surface() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let mut classpath = vec![stdlib];
        if let Some(jdk) = krusty::toolchain::jdk_modules() {
            classpath.push(jdk);
        }
        let cp = Rc::new(Classpath::new_with_jdk_release(classpath, 8));
        let source = "import helpers.*\n\
                      suspend fun recorded() = TailCallOptimizationChecker.saveStackTrace()\n\
                      fun box(): String {\n\
                          TailCallOptimizationChecker.checkStateMachineIn(\"recorded\")\n\
                          return \"OK\"\n\
                      }\n";
        let blocks = vec![
            ("main".to_string(), source.to_string()),
            ("CoroutineUtil".to_string(), COROUTINE_HELPERS.to_string()),
            (
                "TailCallOptimizationChecker".to_string(),
                TAIL_CALL_OPTIMIZATION_CHECKER_HEADERS.to_string(),
            ),
        ];
        assert_eq!(
            frontend_census_error(&blocks, &[], 0, &cp, &krusty::features::LangFeatures::new(),),
            None
        );
    }

    #[test]
    fn frontend_only_module_folds_nested_object_const_from_emitted_dependency() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib\n// FILE: Class.kt\n\
                      annotation class Ann(val p: String)\n\
                      class Class { object Obj { const val Const = \"const\" } }\n\
                      // MODULE: main(lib)\n// FILE: main.kt\n\
                      @Ann(\"${Class.Obj.Const}+\") fun value(): String = \"OK\"\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_folds_top_level_const_expression_from_emitted_dependency() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib\n// FILE: lib.kt\n\
                      const val four = 2 + 2\n\
                      // MODULE: main(lib)\n// FILE: main.kt\n\
                      fun box(): String = if (four == 4) \"OK\" else four.toString()\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_friend_module_infers_inline_result_from_internal_api() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib\n// FILE: lib.kt\n\
                      @PublishedApi internal fun published() = \"OK\"\n\
                      // MODULE: main()(lib)()\n// FILE: main.kt\n\
                      inline fun callTest() = published()\nfun box() = callTest()\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_preserves_dnn_value_property_in_metadata() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib\n// FILE: lib.kt\n\
                      @JvmInline value class A<T>(val x: T & Any)\n\
                      // MODULE: main(lib)\n// FILE: main.kt\n\
                      fun <F : Any> read(value: F): F = A<F?>(value).x\n";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_preserves_context_property_overloads_in_metadata() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// LANGUAGE: +ContextParameters
                      // MODULE: lib
// FILE: lib.kt
class Wrapper(private val value: Int) {
    context(prefix: String) val value: String get() = prefix
}
                      // MODULE: main(lib)
// FILE: main.kt
context(prefix: String) fun read(value: Wrapper): String = value.value
";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_keeps_inner_outer_receiver_out_of_constructor_arity() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// MODULE: lib
// FILE: lib.kt
open class A { open inner class Inner }
// MODULE: main(lib)
// FILE: main.kt
class B : A() { inner class Inner : A.Inner() }
fun box() = B().Inner()
";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn frontend_only_module_imports_nested_typealias_from_dependency_metadata() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let jdk = krusty::toolchain::jdk_modules();
        let source = "// LANGUAGE: +NestedTypeAliases
// MODULE: lib
// FILE: lib.kt
class C(val p: String)
class Foo { typealias TA = C }
// MODULE: main(lib)
// FILE: main.kt
import Foo.TA
fun box(): String { val c: TA = TA(\"OK\"); return c.p }
";
        assert_eq!(
            first_error_module(source, &[stdlib], jdk.as_deref(), true, false),
            None
        );
    }

    #[test]
    fn backend_categories_are_not_rebucketed_by_incidental_words() {
        assert_eq!(
            categorize("lower: gate:suspend-try-catch"),
            "lower: gate:suspend-try-catch"
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
