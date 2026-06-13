//! krust CLI driver — the linear, per-file streaming pipeline:
//! lex+parse all files → collect signatures globally → for each file: typecheck → emit `.class` →
//! drop the file's arenas. `-d <dir>` sets the output directory (default `krust-out`).

use std::path::{Path, PathBuf};

use krust::codegen::emit::{emit_file, file_class_name};
use krust::diag::DiagSink;
use krust::lexer::lex;
use krust::parser::parse;
use krust::resolve::{check_file, collect_signatures};

fn main() {
    let mut out_dir = PathBuf::from("krust-out");
    let mut cp_dirs: Vec<PathBuf> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-d" => out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into())),
            "-cp" | "-classpath" => {
                if let Some(v) = args.next() {
                    cp_dirs.extend(v.split(':').map(PathBuf::from));
                }
            }
            _ => paths.push(a),
        }
    }
    if paths.is_empty() {
        eprintln!("usage: krust [-d <out>] <file.kt> ...");
        std::process::exit(2);
    }

    let mut diags = DiagSink::new();
    let mut sources = Vec::new();
    let mut files = Vec::new();
    let mut stems = Vec::new();
    for path in &paths {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("krust: cannot read {path}: {e}");
            std::process::exit(1);
        });
        let toks = lex(&src, &mut diags);
        files.push(parse(&src, &toks, &mut diags));
        stems.push(file_stem(path));
        sources.push(src);
    }

    let mut syms = collect_signatures(&files, &mut diags);
    syms.classpath = krust::jvm::classpath::Classpath::new(cp_dirs);

    // Per-file: typecheck → emit → write → drop. Only one file's codegen state is live at a time.
    let mut emitted = 0;
    let mut module_packages: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (i, file) in files.iter().enumerate() {
        let info = check_file(file, &syms, &mut diags);
        if diags.has_errors() {
            continue; // collect all diagnostics before bailing
        }
        let write_class = |internal: &str, bytes: &[u8]| -> std::io::Result<()> {
            let path = out_dir.join(format!("{internal}.class"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, bytes)
        };

        // Each top-level `class` becomes its own `.class` file.
        for &d in &file.decls {
            if let krust::ast::Decl::Class(c) = file.decl(d) {
                let internal = match file.package.as_deref() {
                    Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), c.name),
                    _ => c.name.clone(),
                };
                let bytes = krust::codegen::emit::emit_class(c, &internal, &syms);
                if let Err(e) = write_class(&internal, &bytes) {
                    eprintln!("krust: cannot write {internal}.class: {e}");
                    std::process::exit(1);
                }
                emitted += 1;
            }
        }

        // The file facade (`<File>Kt`) is emitted only if the file has top-level functions.
        let has_funs = file.decls.iter().any(|&d| matches!(file.decl(d), krust::ast::Decl::Fun(_)));
        if has_funs {
            let internal = file_class_name(&stems[i], file.package.as_deref());
            let bytes = emit_file(file, &info, &syms, &internal, &mut diags);
            if !diags.has_errors() {
                if let Err(e) = write_class(&internal, &bytes) {
                    eprintln!("krust: cannot write {internal}.class: {e}");
                    std::process::exit(1);
                }
                let facade = internal.rsplit('/').next().unwrap_or(&internal).to_string();
                module_packages.entry(file.package.clone().unwrap_or_default()).or_default().push(facade);
                emitted += 1;
            }
        }
        // `info` (per-file typecheck state) drops here, before the next file.
    }

    // META-INF/main.kotlin_module — maps packages to their file-facade classes so Kotlin
    // consumers can resolve top-level declarations from the compiled module.
    if !diags.has_errors() && !module_packages.is_empty() {
        let packages: Vec<(String, Vec<String>)> = module_packages.into_iter().collect();
        let module_bytes = krust::metadata::module::build_kotlin_module(&packages);
        let mpath = out_dir.join("META-INF/main.kotlin_module");
        let _ = std::fs::create_dir_all(mpath.parent().unwrap());
        let _ = std::fs::write(&mpath, &module_bytes);
    }

    if diags.has_errors() {
        for (path, src) in paths.iter().zip(&sources) {
            print!("{}", diags.render(path, src));
        }
        eprintln!("krust: {} error(s)", diags.diags.len());
        std::process::exit(1);
    }
    println!("ok: emitted {emitted} class file(s) to {}", out_dir.display());
}

fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("File").to_string()
}
