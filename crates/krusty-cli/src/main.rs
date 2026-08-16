//! krusty CLI driver — a kotlinc-compatible front end over the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. Output goes to a directory or a `.jar` (kotlinc `-d`).

use std::io::Write;
use std::path::Path;

use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty_cli::cli;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
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

    if opts.print_version {
        println!("{}", cli::version_line());
        return;
    }
    if opts.print_help {
        println!("{}", cli::HELP);
        return;
    }
    if !opts.errors.is_empty() {
        for error in &opts.errors {
            eprintln!("krusty: error: {error}");
        }
        std::process::exit(2);
    }
    for ig in &opts.ignored {
        eprintln!("krusty: ignoring unsupported option '{ig}'");
    }
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

/// Serve Bazel work requests until stdin closes.
///
/// The process is REUSED across requests, which is the point of a worker: the classpath decoding a
/// compilation pays for is still warm for the next target that shares those jars.
fn run_persistent_worker() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    if let Err(error) = krusty_cli::worker::run(&mut input, &mut output, &compile_work_unit) {
        eprintln!("krusty: worker: {error}");
        std::process::exit(1);
    }
}

/// Run one translated work request: compile, then satisfy the outputs bazel declared for the action.
fn compile_work_unit(unit: krusty_cli::worker::WorkUnit) -> Result<(), String> {
    let mut argv: Vec<String> = vec!["-d".to_string(), unit.output_jar.display().to_string()];
    if let Some(module_name) = &unit.module_name {
        argv.push("-module-name".to_string());
        argv.push(module_name.clone());
    }
    if !unit.classpath.is_empty() {
        argv.push("-classpath".to_string());
        argv.push(
            unit.classpath
                .iter()
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    argv.extend(unit.kotlinc_args.iter().cloned());
    argv.extend(unit.sources.iter().map(|path| path.display().to_string()));

    let mut opts = cli::parse(argv);
    // The two options the worker decides directly rather than through a flag string.
    opts.jvm_default = unit.jvm_default;
    opts.no_param_assertions = !unit.param_assertions;
    if !opts.errors.is_empty() {
        return Err(format!("krusty: {}\n", opts.errors.join("; ")));
    }
    compile(&opts)?;

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
/// Extracted from `main` so the Bazel persistent worker can run a compilation and REPORT its failure
/// instead of terminating: a worker that exits on a broken source takes the whole build's worker
/// process down with it.
pub fn compile(opts: &cli::Options) -> Result<usize, String> {
    let mut diags = DiagSink::new();
    let mut sources = Vec::new();
    let mut stems = Vec::new();
    for path in &opts.sources {
        let src = std::fs::read_to_string(path)
            .map_err(|error| format!("krusty: cannot read {path}: {error}\n"))?;
        stems.push(file_stem(path));
        sources.push(src);
    }

    let effective_classpath = opts
        .effective_classpath()
        .map_err(|error| format!("krusty: {error}\n"))?;
    let cp = std::rc::Rc::new(Classpath::new(effective_classpath));
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let source_inputs = opts
        .sources
        .iter()
        .zip(&sources)
        .zip(&stems)
        .map(|((path, source), stem)| {
            krusty::source::SourceInput::new(
                krusty::source::kind(Path::new(path))
                    .expect("CLI source collection must classify every source"),
                source,
            )
            .with_file_stem(stem)
        })
        .collect::<Vec<_>>();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &source_inputs,
        platform,
        &opts.features,
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );

    // A `-jvm-target` sets the emitted class-file version (kotlinc's `jvmToolchain(25)` ⇒ v69).
    // Absent, the backend keeps krusty's v52 default.
    let backend = krusty::jvm::JvmBackend::new(cp)
        .with_class_major(opts.jvm_target_major)
        .with_jvm_default(opts.jvm_default)
        .with_param_assertions(!opts.no_param_assertions);
    let outputs = krusty::compiler::emit_checked(
        &analysis.files,
        &stems,
        &analysis.types,
        &analysis.symbols,
        &backend,
        &opts.module_name,
        &mut diags,
    );

    if diags.has_errors() {
        // Render each diagnostic against ITS OWN source file (by `Diagnostic::file`), once — not the
        // whole list against every file, which mis-attributed multi-file errors to the wrong source.
        let rendered: Vec<(&str, &str)> = opts
            .sources
            .iter()
            .zip(&sources)
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        return Err(format!(
            "{}krusty: {} error(s)\n",
            diags.render_all(&rendered),
            diags.diags.len()
        ));
    }

    let emitted = outputs
        .iter()
        .filter(|(p, _)| p.ends_with(".class"))
        .count();
    let result = if opts.dest.extension().is_some_and(|e| e == "jar") {
        write_jar(&opts.dest, &outputs)
    } else {
        write_dir(&opts.dest, &outputs)
    };
    result.map_err(|error| {
        format!(
            "krusty: cannot write output to {}: {error}\n",
            opts.dest.display()
        )
    })?;
    Ok(emitted)
}

fn write_dir(dir: &Path, outputs: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    for (rel, bytes) in outputs {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(())
}

/// Write outputs into a `.jar` (a zip with a minimal manifest) — kotlinc `-d foo.jar`.
fn write_jar(path: &Path, outputs: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    use zip::write::SimpleFileOptions;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut zw = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zw.start_file("META-INF/MANIFEST.MF", opts)
        .map_err(zip_io)?;
    zw.write_all(b"Manifest-Version: 1.0\r\nCreated-By: krusty\r\n\r\n")?;
    for (rel, bytes) in outputs {
        zw.start_file(rel, opts).map_err(zip_io)?;
        zw.write_all(bytes)?;
    }
    zw.finish().map_err(zip_io)?;
    Ok(())
}

fn zip_io(e: zip::result::ZipError) -> std::io::Error {
    std::io::Error::other(e)
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("File")
        .to_string()
}
