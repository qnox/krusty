use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn main() {
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let files = vec![parse(&src, &toks, &mut d)];
        let syms = collect_signatures(&files, &mut d);
        check_file(&files[0], &syms, &mut d);
        if d.diags.is_empty() {
            println!("{path}: OK");
        } else {
            for diag in &d.diags {
                let lo = diag.span.lo as usize;
                let line_start = src[..lo.min(src.len())]
                    .rfind('\n')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let line_end = src[lo.min(src.len())..]
                    .find('\n')
                    .map(|i| lo + i)
                    .unwrap_or(src.len());
                let line_no = src[..lo.min(src.len())].matches('\n').count() + 1;
                let line = src.get(line_start..line_end).unwrap_or("").trim_end();
                println!("{path}:{line_no}: {} | {}", diag.msg, line.trim());
            }
        }
    }
}
