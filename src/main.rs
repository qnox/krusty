//! krusty CLI driver — a kotlinc-compatible front end over the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. Output goes to a directory or a `.jar` (kotlinc `-d`).

use std::io::Write;
use std::path::Path;

use krusty::cli;
use krusty::codegen::emit::{emit_class, emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn main() {
    let opts = cli::parse(std::env::args().skip(1));

    if opts.print_version {
        println!("{}", cli::VERSION_LINE);
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
        let toks = lex(&src, &mut diags);
        files.push(parse(&src, &toks, &mut diags));
        stems.push(file_stem(path));
        sources.push(src);
    }

    let mut syms = collect_signatures(&files, &mut diags);
    syms.classpath = krusty::jvm::classpath::Classpath::new(opts.classpath.clone());

    // Per-file: typecheck → emit → buffer output. Only one file's codegen state is live at a time;
    // emitted class bytes are small, so buffering them (to write a dir or jar at the end) is cheap.
    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new(); // (relative path, bytes)
    let mut module_packages: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (i, file) in files.iter().enumerate() {
        let info = check_file(file, &syms, &mut diags);
        if diags.has_errors() {
            continue; // collect all diagnostics before bailing
        }

        // Each top-level `class` becomes its own `.class` file.
        for &d in &file.decls {
            if let krusty::ast::Decl::Class(c) = file.decl(d) {
                let internal = match file.package.as_deref() {
                    Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), c.name),
                    _ => c.name.clone(),
                };
                let bytes = emit_class(c, file, &info, &internal, &syms, &mut diags);
                outputs.push((format!("{internal}.class"), bytes));
            }
        }

        // The file facade (`<File>Kt`) is emitted only if the file has top-level functions.
        let has_facade_members = file
            .decls
            .iter()
            .any(|&d| matches!(file.decl(d), krusty::ast::Decl::Fun(_) | krusty::ast::Decl::Property(_)));
        if has_facade_members {
            let internal = file_class_name(&stems[i], file.package.as_deref());
            let bytes = emit_file(file, &info, &syms, &internal, &mut diags);
            if !diags.has_errors() {
                let facade = internal.rsplit('/').next().unwrap_or(&internal).to_string();
                module_packages.entry(file.package.clone().unwrap_or_default()).or_default().push(facade);
                outputs.push((format!("{internal}.class"), bytes));
            }
        }
        // `info` (per-file typecheck state) drops here, before the next file.
    }

    if diags.has_errors() {
        for (path, src) in opts.sources.iter().zip(&sources) {
            print!("{}", diags.render(path, src));
        }
        eprintln!("krusty: {} error(s)", diags.diags.len());
        std::process::exit(1);
    }

    // META-INF/<module>.kotlin_module — maps packages to their file-facade classes so Kotlin
    // consumers can resolve top-level declarations from the compiled module.
    if !module_packages.is_empty() {
        let packages: Vec<(String, Vec<String>)> = module_packages.into_iter().collect();
        let module_bytes = krusty::metadata::module::build_kotlin_module(&packages);
        outputs.push((format!("META-INF/{}.kotlin_module", opts.module_name), module_bytes));
    }

    let emitted = outputs.iter().filter(|(p, _)| p.ends_with(".class")).count();
    let result = if opts.dest.extension().map_or(false, |e| e == "jar") {
        write_jar(&opts.dest, &outputs)
    } else {
        write_dir(&opts.dest, &outputs)
    };
    if let Err(e) = result {
        eprintln!("krusty: cannot write output to {}: {e}", opts.dest.display());
        std::process::exit(1);
    }
    println!("ok: emitted {emitted} class file(s) to {}", opts.dest.display());
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

    zw.start_file("META-INF/MANIFEST.MF", opts).map_err(zip_io)?;
    zw.write_all(b"Manifest-Version: 1.0\r\nCreated-By: krusty\r\n\r\n")?;
    for (rel, bytes) in outputs {
        zw.start_file(rel, opts).map_err(zip_io)?;
        zw.write_all(bytes)?;
    }
    zw.finish().map_err(zip_io)?;
    Ok(())
}

fn zip_io(e: zip::result::ZipError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("File").to_string()
}
