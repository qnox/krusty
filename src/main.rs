//! krusty CLI driver — a kotlinc-compatible front end over the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. Output goes to a directory or a `.jar` (kotlinc `-d`).

use std::io::Write;
use std::path::Path;

use krusty::cli;
use krusty::diag::DiagSink;
use krusty::frontend::{collect_signatures_with_cp, parse_source};
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;

fn main() {
    let opts = cli::parse(std::env::args().skip(1));

    if opts.print_version {
        println!("{}", cli::version_line());
        return;
    }
    if opts.print_help {
        println!("{}", cli::HELP);
        return;
    }
    for ig in &opts.ignored {
        eprintln!("krusty: ignoring unsupported option '{ig}'");
    }
    if opts.sources.is_empty() {
        eprintln!("krusty: no source files. Use -help for usage.");
        std::process::exit(2);
    }

    let mut diags = DiagSink::new();
    let mut sources = Vec::new();
    let mut files = Vec::new();
    let mut stems = Vec::new();
    for path in &opts.sources {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("krusty: cannot read {path}: {e}");
            std::process::exit(1);
        });
        files.push(parse_source(&src, &opts.features, &mut diags));
        stems.push(file_stem(path));
        sources.push(src);
    }

    let cp = std::rc::Rc::new(Classpath::new(opts.effective_classpath()));
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    krusty::jvm::prepare_module_symbols(&files, &stems, &mut syms);

    // A `-jvm-target` sets the emitted class-file version (kotlinc's `jvmToolchain(25)` ⇒ v69).
    // Absent, the backend keeps krusty's v52 default.
    let backend = krusty::jvm::JvmBackend::new(cp).with_class_major(opts.jvm_target_major);
    let outputs = krusty::compiler::compile(
        &files,
        &stems,
        &mut syms,
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
        print!("{}", diags.render_all(&rendered));
        eprintln!("krusty: {} error(s)", diags.diags.len());
        std::process::exit(1);
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
    if let Err(e) = result {
        eprintln!(
            "krusty: cannot write output to {}: {e}",
            opts.dest.display()
        );
        std::process::exit(1);
    }
    println!(
        "ok: emitted {emitted} class file(s) to {}",
        opts.dest.display()
    );
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
