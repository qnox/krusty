//! krust CLI driver. Demonstrates the linear pipeline shape: lex+parse all files, collect
//! signatures globally (cheap), then typecheck each file. Codegen (Phase 3+) plugs in after check,
//! per file, with the file's arenas dropped before the next.

use krust::diag::DiagSink;
use krust::lexer::lex;
use krust::parser::parse;
use krust::resolve::{check_file, collect_signatures};

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: krust <file.kt> [more.kt ...]");
        std::process::exit(2);
    }

    let mut diags = DiagSink::new();
    let mut sources = Vec::new();
    let mut files = Vec::new();
    for path in &paths {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("krust: cannot read {path}: {e}");
                std::process::exit(1);
            }
        };
        let toks = lex(&src, &mut diags);
        let file = parse(&src, &toks, &mut diags);
        files.push(file);
        sources.push(src);
    }

    // Stage C: global signatures (cheap, no bodies).
    let syms = collect_signatures(&files, &mut diags);

    // Stage D: per-file typecheck.
    let mut total_decls = 0;
    for (i, file) in files.iter().enumerate() {
        total_decls += file.decls.len();
        let _info = check_file(file, &syms, &mut diags);
        let _ = i; // codegen for files[i] will go here, then drop its arenas
    }

    if diags.has_errors() {
        for (path, src) in paths.iter().zip(&sources) {
            print!("{}", diags.render(path, src));
        }
        eprintln!("krust: {} error(s)", diags.diags.len());
        std::process::exit(1);
    }
    println!("ok: {} file(s), {} declaration(s) typechecked", files.len(), total_decls);
}
