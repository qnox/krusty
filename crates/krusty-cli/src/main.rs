//! krusty CLI driver — a kotlinc-compatible front end over the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. Output goes to a directory or a `.jar` (kotlinc `-d`).

use std::path::Path;

use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty_cli::staged_output::StagedOutput;
use krusty_cli::{cli, kotlinc_arguments, verify};

/// The compiler's allocation pattern (many short-lived vectors and strings) runs markedly faster on
/// mimalloc than on the platform allocator; see the dependency's note in `Cargo.toml`.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // `krusty cinterop …` is Kotlin/Native's `cinterop` tool: a `.def` file to an interop library.
    if argv.first().is_some_and(|command| command == "cinterop") {
        run_cinterop(&argv[1..]);
        return;
    }
    // `krusty compilation-fingerprint <kotlinc arguments>` prints the digest of everything that
    // compilation reads; a build tool keeps the previous output while it is unchanged.
    if argv
        .first()
        .is_some_and(|command| command == "compilation-fingerprint")
    {
        match krusty_cli::compilation_fingerprint::fingerprint(argv[1..].to_vec()) {
            Ok(fingerprint) => println!("{fingerprint}"),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    // `krusty refusals <kotlinc arguments>` says whether krusty compiles a compilation with those
    // arguments itself; a build tool hands a refused one to kotlinc instead.
    if argv.first().is_some_and(|command| command == "refusals") {
        run_refusals(argv[1..].to_vec());
        return;
    }
    // `krusty verify-compile …` compiles a compilation kotlinc compiled too and records how krusty
    // did; `krusty verify-report …` turns a build's records into its verify report.
    if argv
        .first()
        .is_some_and(|command| command == "verify-compile")
    {
        run_verify_compile(&argv[1..]);
        return;
    }
    if argv
        .first()
        .is_some_and(|command| command == "verify-report")
    {
        run_verify_report(&argv[1..]);
        return;
    }
    // Bazel starts a persistent worker by appending `--persistent_worker` to the tool's command
    // line; every actual compilation then arrives as a work request on stdin.
    if argv
        .iter()
        .any(|argument| argument == "--persistent_worker")
    {
        run_persistent_worker();
        return;
    }
    let opts = cli::parse(argv);

    // kotlinc rejects a malformed command line before anything else, in these words.
    if let Some(report) = argument_error_report(&opts) {
        eprint!("{report}");
        std::process::exit(1);
    }
    if opts.print_version {
        println!("{}", cli::version_line());
        return;
    }
    if opts.print_help {
        println!("{}", cli::HELP);
        return;
    }
    if let Some(report) = error_report(&opts) {
        eprint!("{report}");
        std::process::exit(2);
    }
    eprint!("{}", parse_report(&opts));
    if opts.sources.is_empty() {
        eprintln!("krusty: no source files. Use -help for usage.");
        std::process::exit(2);
    }

    match compile(&opts) {
        Ok(emitted) => println!(
            "ok: emitted {emitted} class file(s) to {}",
            opts.dest.display()
        ),
        Err(error) => {
            eprint!("{error}");
            std::process::exit(1);
        }
    }
}

/// Build the interop library the `cinterop` arguments describe, or say why not and exit 1.
fn run_cinterop(arguments: &[String]) {
    match krusty_cli::cinterop::parse(arguments) {
        Ok(krusty_cli::cinterop::Invocation::Help) => println!("{}", krusty_cli::cinterop::HELP),
        Ok(krusty_cli::cinterop::Invocation::Build(options)) => {
            if let Err(error) = krusty::native::cinterop::build_interop_library(&options) {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("info: use krusty cinterop -help for more information");
            std::process::exit(1);
        }
    }
}

/// Print each part of the command line krusty refuses, one per line, and exit 1 when there is one;
/// exit 0 when krusty compiles the whole request. See [`refusals`]; a command line that fails for
/// any other reason exits 2 with the report the compilation fails with, and neither compiler runs.
fn run_refusals(argv: Vec<String>) {
    let opts = cli::parse(argv);
    let refused = refusals(&opts).unwrap_or_else(|report| {
        eprint!("{report}");
        std::process::exit(2);
    });
    for refusal in &refused {
        println!("{refusal}");
    }
    if !refused.is_empty() {
        std::process::exit(1);
    }
}

/// The parts of the command line krusty refuses: the refusals a compilation with the same
/// arguments reports as errors before it reads a source, a kotlinc argument, output shape or
/// language feature krusty does not implement, or a compiler plugin it neither reimplements nor
/// hosts. A command line that fails for any other reason fails kotlinc too, so it is no refusal:
/// it is the error, the report the compilation fails with.
fn refusals(opts: &cli::Options) -> Result<Vec<String>, String> {
    let invalid = argument_error_report(opts)
        .or_else(|| {
            opts.errors
                .iter()
                .any(|error| !opts.refusals.contains(error))
                .then(|| error_report(opts))
                .flatten()
        })
        .or_else(|| {
            configuration_report(opts)
                .err()
                .map(|report| parse_report(opts) + &report)
        });
    if let Some(report) = invalid {
        return Err(report);
    }
    let plugins = opts.resolve_plugins(&[]);
    Ok(opts
        .refusals
        .iter()
        .cloned()
        .chain(plugins.errors)
        .chain(plugins.partial)
        .collect())
}

/// Compile one compilation kotlinc has compiled too, and record whether krusty supports it, how
/// long it took, its peak memory and how its output differs from kotlinc's. The record is the
/// result: a refused or failed compilation still exits 0, since the build ships kotlinc's output.
fn run_verify_compile(argv: &[String]) {
    let started = std::time::Instant::now();
    let arguments = verify::VerifyCompileArguments::parse(argv).unwrap_or_else(|error| {
        eprintln!("error: {error}\n{}", verify::VERIFY_COMPILE_USAGE);
        std::process::exit(2);
    });
    let mut record = verify::CompilationRecord {
        compilation: arguments.compilation.clone(),
        source_set: arguments.source_set.clone(),
        kotlin_version: arguments.kotlin_version.clone(),
        outcome: verify::Outcome::Unsupported {
            reasons: arguments.unsupported.clone(),
        },
        kotlinc_millis: arguments.kotlinc_millis,
        krusty_millis: None,
        krusty_peak_rss_bytes: None,
    };
    let opts = cli::parse(arguments.kotlinc_arguments.clone());
    // Only a request that compiles has a destination, and only the `-d` it names: a request
    // without kotlinc arguments must not reach the parser's default output.
    let destination = (!arguments.kotlinc_arguments.is_empty()).then_some(opts.dest.as_path());
    // Nothing is written, and the previous record is not removed, until the layout is proven safe.
    if let Err(error) = verify::check_layout(&arguments.reference, destination, &arguments.record) {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
    // A record left by an earlier invocation never stands in for this one, even if this one fails.
    let _ = std::fs::remove_file(&arguments.record);
    // The destination is verify's own, proven apart from kotlinc's output: an earlier run's classes
    // must not appear as krusty's.
    if let Some(destination) = destination.filter(|destination| destination.is_dir()) {
        if let Err(error) = std::fs::remove_dir_all(destination) {
            eprintln!("error: cannot clear {}: {error}", destination.display());
            std::process::exit(2);
        }
    }
    // A request without kotlinc arguments is one the build tool already knows krusty does not
    // support; one with arguments but no Kotlin source compiles nothing.
    if !arguments.kotlinc_arguments.is_empty()
        && !opts.sources.iter().any(|source| source.ends_with(".kt"))
    {
        record.outcome = verify::Outcome::NoSources;
        // Neither compiler compiled anything, so there is no time to compare.
        record.kotlinc_millis = None;
    } else if arguments.unsupported.is_empty() {
        record.outcome = match refusals(&opts) {
            Err(report) => verify::Outcome::Failed {
                errors: error_lines(&report),
            },
            Ok(refused) if !refused.is_empty() => verify::Outcome::Unsupported { reasons: refused },
            Ok(_) => {
                eprint!("{}", parse_report(&opts));
                // A panic is one more way krusty fails this compilation, not a reason to lose the
                // record of the build's other compilations.
                let compiled =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compile(&opts)))
                        .unwrap_or_else(|payload| {
                            Err(format!(
                                "error: internal compiler panic: {}\n",
                                panic_text(&*payload)
                            ))
                        });
                record.krusty_millis = Some(started.elapsed().as_millis() as u64);
                record.krusty_peak_rss_bytes = verify::peak_rss_bytes();
                match compiled {
                    Err(report) => verify::Outcome::Failed {
                        errors: error_lines(&report),
                    },
                    Ok(_) => match verify::compare_outputs(
                        &arguments.reference,
                        &opts.dest,
                        if arguments.kotlin_outputs_only {
                            verify::ReferenceOutputs::KotlinOnly
                        } else {
                            verify::ReferenceOutputs::All
                        },
                    ) {
                        Ok(comparison) => verify::Outcome::Compiled { comparison },
                        Err(error) => {
                            eprintln!("error: cannot compare the outputs: {error}");
                            std::process::exit(2);
                        }
                    },
                }
            }
        };
    }
    let json = serde_json::to_string_pretty(&record).expect("a record serializes");
    if let Some(parent) = arguments.record.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&arguments.record, json + "\n") {
        eprintln!(
            "error: cannot write {}: {error}",
            arguments.record.display()
        );
        std::process::exit(2);
    }
    println!("{}", verify::record_line(&record));
}

/// The error lines of a failed compilation's report, or the whole report when none is marked.
fn error_lines(report: &str) -> Vec<String> {
    let lines = report.lines().filter(|line| !line.trim().is_empty());
    let errors = lines
        .clone()
        .filter(|line| line.contains("error: "))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if errors.is_empty() {
        lines.map(str::to_string).collect()
    } else {
        errors
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

/// Write the HTML, Markdown and issue reports of a build's verify records, and say where.
fn run_verify_report(argv: &[String]) {
    let arguments = verify::VerifyReportArguments::parse(argv).unwrap_or_else(|error| {
        eprintln!("error: {error}\n{}", verify::VERIFY_REPORT_USAGE);
        std::process::exit(2);
    });
    let fail = |error: String| -> ! {
        eprintln!("error: {error}");
        std::process::exit(2);
    };
    // Every input is known and kept apart from the reports before any report is removed.
    let files = arguments
        .record_files()
        .unwrap_or_else(|error| fail(format!("cannot list the records: {error}")));
    if let Err(error) = verify::check_report_layout(&arguments.out, &files) {
        fail(error);
    }
    // An earlier invocation's report never survives this one, even when this one fails.
    for name in verify::REPORT_FILES {
        let path = arguments.out.join(name);
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                fail(format!("cannot remove {}: {error}", path.display()));
            }
        }
    }
    let records = files
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(file)
                .unwrap_or_else(|error| fail(format!("cannot read {}: {error}", file.display())));
            serde_json::from_str::<verify::CompilationRecord>(&text).unwrap_or_else(|error| {
                fail(format!(
                    "{} is not a verify record: {error}",
                    file.display()
                ))
            })
        })
        .collect::<Vec<_>>();
    let environment = verify::Environment {
        krusty_version: cli::VERSION.to_string(),
        build_tool: arguments.build_tool.clone(),
        jdk: arguments.jdk.clone(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    };
    let issue_url = verify::issue_url(&records, &environment, arguments.share_names);
    let [html, markdown, issue] = verify::REPORT_FILES;
    let outputs = [
        (
            html,
            verify::render_html(&records, &environment, &issue_url),
        ),
        (
            markdown,
            verify::render_markdown(&records, &environment, &issue_url),
        ),
        (
            issue,
            verify::issue_body(&records, &environment, arguments.share_names, true),
        ),
    ];
    std::fs::create_dir_all(&arguments.out).unwrap_or_else(|error| {
        fail(format!(
            "cannot create {}: {error}",
            arguments.out.display()
        ))
    });
    for (name, text) in &outputs {
        let path = arguments.out.join(name);
        std::fs::write(&path, text)
            .unwrap_or_else(|error| fail(format!("cannot write {}: {error}", path.display())));
    }
    println!("{}", verify::summary_line(&verify::Summary::of(&records)));
    println!("report: {}", arguments.out.join(html).display());
    println!(
        "submit it to help prioritize krusty: open the report's issue button, or paste {} into {}",
        arguments.out.join(issue).display(),
        verify::NEW_ISSUE_URL,
    );
}

/// Serve Bazel work requests until stdin closes.
///
/// The process is REUSED across requests, which is the point of a worker: the classpath decoding a
/// compilation pays for is still warm for the next target that shares those jars.
fn run_persistent_worker() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    // A panic inside the compiler must fail ONE request, not the worker process: bazel would
    // otherwise lose the worker mid-build and report it as a crash rather than as this target's
    // failure. `compile` has `expect`s, and a compiler does panic in the field.
    let serve = |unit: krusty_cli::worker::WorkUnit| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compile_work_unit(unit)))
            .unwrap_or_else(|payload| {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string());
                Err(format!("krusty: internal compiler panic: {detail}\n"))
            })
    };
    if let Err(error) = krusty_cli::worker::run(&mut input, &mut output, &serve) {
        eprintln!("krusty: worker: {error}");
        std::process::exit(1);
    }
}

/// Run one translated work request: compile, then satisfy the outputs bazel declared for the action.
fn compile_work_unit(unit: krusty_cli::worker::WorkUnit) -> Result<(), String> {
    let argv = unit.compiler_arguments();

    // A module with no Kotlin sources (an export wrapper, or a jar that only carries resources)
    // still has a declared output. Running the compiler would also demand a Kotlin distribution
    // the action does not have.
    if unit.sources.is_empty() {
        StagedOutput::new(&unit.output_jar)
            .commit()
            .map_err(|error| {
                format!(
                    "krusty: cannot write output to {}: {error}\n",
                    unit.output_jar.display()
                )
            })?;
    } else {
        let opts = cli::parse(argv);
        let errors: Vec<&str> = opts
            .argument_errors
            .iter()
            .chain(&opts.errors)
            .map(String::as_str)
            .collect();
        if !errors.is_empty() {
            return Err(format!("krusty: {}\n", errors.join("; ")));
        }
        compile(&opts)?;
    }
    krusty_cli::worker::package_resources(&unit.output_jar, &unit.resources)
        .map_err(|error| format!("krusty: {error}\n"))?;

    // Bazel fails an action whose DECLARED outputs are missing, so both must exist even though
    // krusty has nothing distinct to put in them.
    if let Some(abi_jar) = &unit.abi_jar {
        std::fs::copy(&unit.output_jar, abi_jar)
            .map_err(|error| format!("krusty: cannot write {}: {error}\n", abi_jar.display()))?;
    }
    if let Some(cri_file) = &unit.cri_file {
        std::fs::write(cri_file, b"")
            .map_err(|error| format!("krusty: cannot write {}: {error}\n", cri_file.display()))?;
    }
    Ok(())
}

/// Compile one source set, as configured. Returns the number of emitted class files, or a rendered
/// error report.
///
/// kotlinc's syntax errors, which reject the command line before anything else, in its words.
fn argument_error_report(opts: &cli::Options) -> Option<String> {
    if opts.argument_errors.is_empty() {
        return None;
    }
    let mut report = String::new();
    for error in &opts.argument_errors {
        report.push_str(&format!("error: {}\n", kotlinc_arguments::render(error)));
    }
    report.push_str("info: use -help for more information\n");
    Some(report)
}

/// The options krusty reports and exits on before compiling: invalid ones, and ones it refuses.
fn error_report(opts: &cli::Options) -> Option<String> {
    if opts.errors.is_empty() {
        return None;
    }
    Some(
        opts.errors
            .iter()
            .map(|error| format!("krusty: error: {error}\n"))
            .collect(),
    )
}

/// What a compilation reports once the command line parses: kotlinc's syntax warnings and its
/// `-XXLanguage` problems, then the options krusty skips.
fn parse_report(opts: &cli::Options) -> String {
    let mut report = String::new();
    for warning in &opts.argument_warnings {
        report.push_str(&format!(
            "warning: {}\n",
            kotlinc_arguments::render(warning)
        ));
    }
    for problem in &opts.language_feature_problems {
        let severity = if problem.is_error { "error" } else { "warning" };
        report.push_str(&format!(
            "{severity}: {}\n",
            kotlinc_arguments::render(&problem.message)
        ));
    }
    for ig in &opts.ignored {
        report.push_str(&format!("krusty: ignoring unsupported option '{ig}'\n"));
    }
    report
}

/// kotlinc reports the configuration errors, then the configuration warnings, some of which
/// `-Xwarning-level` may promote. Any error, or an error among the `-XXLanguage` problems the
/// [`parse_report`] holds, fails the invocation once everything is reported: the report is then
/// the error.
fn configuration_report(opts: &cli::Options) -> Result<String, String> {
    let mut report = String::new();
    let mut failed = opts
        .language_feature_problems
        .iter()
        .any(|problem| problem.is_error);
    let mut unconfigured_warning_under_werror = false;
    for error in &opts.configuration_errors {
        report.push_str(&format!("error: {error}\n"));
        failed = true;
    }
    for warning in &opts.warnings {
        match opts.warning_policy.command_line_disposition(warning.name) {
            cli::WarningDisposition::Warning => {
                report.push_str(&format!("warning: {}\n", warning.message));
            }
            cli::WarningDisposition::Error => {
                report.push_str(&format!("error: {}\n", warning.message));
                failed = true;
            }
            cli::WarningDisposition::Disabled => {}
            cli::WarningDisposition::WarningAndFail => {
                report.push_str(&format!("warning: {}\n", warning.message));
                unconfigured_warning_under_werror = true;
            }
        }
    }
    if failed {
        return Err(report);
    }
    if unconfigured_warning_under_werror {
        report.push_str("error: warnings found and -Werror specified\n");
        return Err(report);
    }
    Ok(report)
}

/// Extracted from `main` so the Bazel persistent worker can run a compilation and REPORT its failure
/// instead of terminating: a worker that exits on a broken source takes the whole build's worker
/// process down with it.
pub fn compile(opts: &cli::Options) -> Result<usize, String> {
    let report = configuration_report(opts)?;
    eprint!("{report}");

    let version = match opts.kotlin_reference_version {
        Some(version) => version,
        None => krusty::kotlin_version::configured_target()
            .map_err(|error| format!("krusty: {error}\n"))?,
    };
    krusty::kotlin_version::set_target(version).map_err(|error| format!("krusty: {error}\n"))?;
    let mut diags = DiagSink::new();
    let mut sources = Vec::new();
    let mut stems = Vec::new();
    for path in &opts.sources {
        let src = std::fs::read_to_string(path)
            .map_err(|error| format!("krusty: cannot read {path}: {error}\n"))?;
        stems.push(file_stem(path));
        sources.push(src);
    }

    // Say so BEFORE an entry is silently dropped by the reader: a missing or unopenable jar
    // otherwise surfaces only as unresolved references with nothing pointing at the jar.
    for problem in cli::classpath_entry_problems(&opts.classpath) {
        eprintln!("{problem}");
    }
    opts.check_jdk_release()
        .map_err(|error| format!("krusty: error: {error}\n"))?;
    let effective_classpath = opts
        .effective_classpath()
        .map_err(|error| format!("krusty: {error}\n"))?;
    let plugins = opts.resolve_plugins(&effective_classpath);
    for note in &plugins.notes {
        eprintln!("info: {note}");
    }
    if !plugins.errors.is_empty() {
        return Err(plugins
            .errors
            .iter()
            .map(|error| format!("error: {error}\n"))
            .collect());
    }
    let cp = std::rc::Rc::new(Classpath::new_with_friend_paths(
        effective_classpath,
        opts.friend_paths.clone(),
    ));
    // Construction is fallible: a corrupt selected dependency remains a terminal frontend
    // diagnostic and never becomes a queryable provider with an empty symbol index.
    let libraries = JvmLibraries::new(cp.clone())
        .map(|libraries| libraries.with_api_version(opts.language_settings.api_version));
    let platform =
        krusty::frontend::PlatformProvider::jvm(libraries).with_native_plugins(plugins.native);
    let source_inputs = opts
        .sources
        .iter()
        .zip(&sources)
        .zip(&stems)
        .map(|((path, source), stem)| {
            let input = krusty::source::SourceInput::new(
                krusty::source::kind(Path::new(path))
                    .expect("CLI source collection must classify every source"),
                source,
            )
            .with_file_stem(stem);
            let input = match opts.source_fragments.get(path) {
                Some(fragment) => input.with_fragment(*fragment),
                None => input,
            };
            if opts.common_sources.contains(path) {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    // The module name reaches diagnostics that spell it (an `expect` with no `actual` names the
    // module it looked in), never resolution.
    let analysis = krusty::frontend::analyze_source_set_streaming_with_module(
        &source_inputs,
        platform,
        &opts.language_settings.features,
        &opts.module_name,
        &mut diags,
    );

    // A `-jvm-target` sets the emitted class-file version (kotlinc's `jvmToolchain(25)` ⇒ v69).
    // Absent, the backend keeps krusty's v52 default.
    let backend = opts.jvm_backend(cp);
    // Each file's classes are staged as soon as they are emitted rather than held for the whole
    // module; nothing reaches `-d` unless the compilation succeeds.
    let mut output = StagedOutput::new(&opts.dest);
    let mut emitted = 0;
    krusty::compiler::emit_analyzed_to(
        analysis,
        &stems,
        &backend,
        &opts.module_name,
        &mut diags,
        &mut |(rel, bytes)| {
            if rel.ends_with(".class") {
                emitted += 1;
            }
            output.write(&rel, &bytes);
        },
    );

    // Render each diagnostic against ITS OWN source file (by `Diagnostic::file`), once — not the
    // whole list against every file, which mis-attributed multi-file errors to the wrong source.
    let rendered: Vec<(&str, &str)> = opts
        .sources
        .iter()
        .zip(&sources)
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    // Apply named compiler-warning policy before deciding whether the frontend failed. A warning
    // promoted by `-Xwarning-level=<NAME>:error` is an ordinary source error; an explicit
    // `warning` level exempts that diagnostic from global `-Werror`, and `disabled` removes it.
    let mut compiler_warning_under_werror = false;
    diags.diags.retain_mut(|diagnostic| {
        if diagnostic.severity != krusty::diag::Severity::Warning {
            return true;
        }
        match opts
            .warning_policy
            .compiler_disposition(diagnostic.warning_name)
        {
            cli::WarningDisposition::Disabled => false,
            cli::WarningDisposition::Warning => true,
            cli::WarningDisposition::Error => {
                diagnostic.severity = krusty::diag::Severity::Error;
                true
            }
            cli::WarningDisposition::WarningAndFail => {
                compiler_warning_under_werror = true;
                true
            }
        }
    });
    match opts.warning_policy.compiler_disposition(None) {
        cli::WarningDisposition::Disabled => diags.module_warnings.clear(),
        cli::WarningDisposition::WarningAndFail if !diags.module_warnings.is_empty() => {
            compiler_warning_under_werror = true;
        }
        cli::WarningDisposition::Warning
        | cli::WarningDisposition::WarningAndFail
        | cli::WarningDisposition::Error => {}
    }

    if diags.has_errors() {
        // Like kotlinc, a failed compilation reports its errors only.
        diags
            .diags
            .retain(|diagnostic| diagnostic.severity == krusty::diag::Severity::Error);
        diags.module_warnings.clear();
        return Err(format!(
            "{}krusty: {} error(s)\n",
            diags.render_all(&rendered),
            diags.diags.len()
        ));
    }
    // The same typed policy owns ordinary compiler and module warnings. Unlike command-line
    // configuration warnings, these are suppressed by `-nowarn`; `-Werror` reports them and fails
    // before any output is written.
    if compiler_warning_under_werror {
        // kotlinc reports unlocated module warnings before its -Werror summary, then the located
        // source warnings. Keep those three ledger segments distinct instead of prepending the
        // summary to the generic renderer (which owns module-before-source order for an ordinary
        // successful compilation).
        let module_warnings = std::mem::take(&mut diags.module_warnings);
        let mut report = String::new();
        for warning in module_warnings {
            report.push_str("warning: ");
            report.push_str(&warning);
            report.push('\n');
        }
        report.push_str("error: warnings found and -Werror specified\n");
        report.push_str(&diags.render_all(&rendered));
        return Err(report);
    }
    eprint!("{}", diags.render_all(&rendered));

    output.commit().map_err(|error| {
        format!(
            "krusty: cannot write output to {}: {error}\n",
            opts.dest.display()
        )
    })?;
    Ok(emitted)
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("File")
        .to_string()
}
