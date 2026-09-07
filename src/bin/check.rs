use krusty::diag::DiagSink;
use krusty::frontend::{analyze_source_set_with_features, SourceInput};

fn main() {
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        let mut d = DiagSink::new();
        let inputs = [SourceInput::kotlin(&src)];
        let _ = analyze_source_set_with_features(
            &inputs,
            Box::new(krusty::libraries::EmptySymbolSource),
            &krusty::features::LangFeatures::default(),
            &mut d,
        );
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
