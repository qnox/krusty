//! krusty CLI driver — a kotlinc-compatible front end over the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. Output goes to a directory or a `.jar` (kotlinc `-d`).

use std::io::Write;
use std::path::Path;

use krusty::cli;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::lexer::lex;
use krusty::parser::parse_with_features;
use krusty::resolve::collect_signatures_with_cp;

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
        let toks = lex(&src, &mut diags);
        files.push(parse_with_features(&src, &toks, &mut diags, &opts.features));
        stems.push(file_stem(path));
        sources.push(src);
    }

    let cp = std::rc::Rc::new(Classpath::new(opts.classpath.clone()));
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);

    // Multi-file: map each top-level (non-extension, non-inline) function to the facade class of the
    // file that declares it, so lowering a call to a function in ANOTHER file emits a cross-facade
    // `invokestatic` instead of bailing. Only the driver knows each file's stem→facade.
    if files.len() > 1 {
        use krusty::ast::Decl;
        use krusty::jvm::names::file_class_name;
        // Collect (name, facade) first — inserting into syms.{fn,prop}_facades while reading syms.props
        // would conflict-borrow.
        let mut fns: Vec<(String, String)> = Vec::new();
        let mut props: Vec<(String, String)> = Vec::new();
        for (i, file) in files.iter().enumerate() {
            let facade = file_class_name(&stems[i], file.package.as_deref());
            for &d in &file.decls {
                match file.decl(d) {
                    Decl::Fun(f) if f.receiver.is_none() && !f.is_inline => {
                        fns.push((f.name.clone(), facade.clone()))
                    }
                    Decl::Property(p) if p.receiver.is_none() => {
                        props.push((p.name.clone(), facade.clone()))
                    }
                    _ => {}
                }
            }
        }
        for (name, facade) in fns {
            syms.fn_facades.insert(name, facade);
        }
        for (name, facade) in props {
            if let Some(&(ty, is_var, is_const)) = syms.props.get(&name) {
                syms.prop_facades
                    .insert(name, (facade, ty, is_var, is_const));
            }
        }
    }

    // Common pipeline: front-end type-check each file, then lower through the selected backend
    // (JVM today; see docs/ARCHITECTURE.md). `-target wasm|js` would select a different backend here.
    // The backend shares the same classpath instance (caches) as the library set, for the inliner.
    let backend = krusty::jvm::JvmBackend::new(cp);
    let outputs = krusty::backend::compile(
        &files,
        &stems,
        &syms,
        &backend,
        &opts.module_name,
        &mut diags,
    );

    if diags.has_errors() {
        for (path, src) in opts.sources.iter().zip(&sources) {
            print!("{}", diags.render(path, src));
        }
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
