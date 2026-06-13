//! krust CLI driver (Phase 0): lex the given `.kt` files and report.
//! Later phases extend this into the full per-file streaming pipeline (see docs/SPEC.md §3).

use krust::diag::DiagSink;
use krust::lexer::lex;
use krust::parser::parse;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: krust <file.kt> [more.kt ...]");
        std::process::exit(2);
    }
    let mut had_error = false;
    for path in &args {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("krust: cannot read {path}: {e}");
                had_error = true;
                continue;
            }
        };
        let mut diags = DiagSink::new();
        let toks = lex(&src, &mut diags);
        let file = parse(&src, &toks, &mut diags);
        print!("{path}: {} decls", file.decls.len());
        if diags.has_errors() {
            had_error = true;
            print!("\n{}", diags.render(path, &src));
        } else {
            println!(" (parsed ok)");
        }
    }
    if had_error {
        std::process::exit(1);
    }
}
