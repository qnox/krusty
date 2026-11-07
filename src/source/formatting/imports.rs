use super::{formatting_tokens, Kind};

/// ktlint `import-ordering` rule (`ktlint_official`): an import section is sorted in plain
/// ASCII order with aliased imports placed after all non-aliased ones; blank lines inside
/// the section are dropped. A comment line anywhere inside the section makes ktlint leave
/// it untouched. Runs as a token-accurate pre-pass — comments and string contents are
/// never mistaken for imports — so the layout engine and its token guard always see the
/// sorted stream.
pub(super) fn sort_imports(source: &str) -> Option<String> {
    let scan = formatting_tokens(source)?;
    let mut diagnostics = crate::diag::DiagSink::new();
    let imports = crate::lexer::file_header(source, &mut diagnostics).import_starts;
    let tokens = &scan.tokens;
    // Import line spans: (line_start, content_end, line_end, has_alias); line_end includes
    // the newline when the line has one.
    let mut lines: Vec<(usize, usize, usize, bool)> = Vec::new();
    let mut brace_depth = 0usize;
    let mut line_start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            Kind::Newline => line_start = token.hi as usize,
            Kind::LBrace => brace_depth += 1,
            Kind::RBrace => brace_depth = brace_depth.saturating_sub(1),
            Kind::Word if imports.binary_search(&token.lo).is_ok() && brace_depth == 0 => {
                let at_line_start = index == 0 || tokens[index - 1].kind == Kind::Newline;
                let at_column_zero = token.lo as usize == line_start;
                if !at_line_start || !at_column_zero {
                    continue;
                }
                let mut content_end = token.hi as usize;
                let mut end = content_end;
                let mut has_alias = false;
                for following in tokens[index + 1..].iter() {
                    if following.kind == Kind::Newline {
                        end = following.hi as usize;
                        break;
                    }
                    if !matches!(following.kind, Kind::LineComment | Kind::Semicolon) {
                        content_end = following.hi as usize;
                        if following.kind == Kind::Word && following.text(source) == "as" {
                            has_alias = true;
                        }
                    }
                    end = following.hi as usize;
                }
                lines.push((token.lo as usize, content_end, end, has_alias));
            }
            _ => {}
        }
    }
    // Sections: import lines separated only by blank lines belong to one section; anything
    // else (code, a comment line) splits sections.
    let mut sections: Vec<std::ops::Range<usize>> = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let section_start = index;
        while let Some(next) = lines.get(index + 1) {
            let gap = &source[lines[index].2..next.0];
            if gap
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            {
                index += 1;
            } else {
                break;
            }
        }
        sections.push(section_start..index + 1);
        index += 1;
    }
    // A comment line anywhere in the import region makes ktlint leave every section
    // untouched, not just the one containing the comment.
    if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
        let commented = tokens.iter().any(|token| {
            matches!(token.kind, Kind::LineComment | Kind::BlockComment)
                && token.lo as usize > first.0
                && (token.lo as usize) < last.2
        });
        if commented {
            return Some(source.to_string());
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for section in sections {
        let run = &lines[section];
        if run.len() == 1 {
            continue;
        }
        output.push_str(&source[cursor..run[0].0]);
        let mut sorted: Vec<&(usize, usize, usize, bool)> = run.iter().collect();
        sorted.sort_by_key(|line| (line.3, &source[line.0..line.1]));
        let last = sorted.len() - 1;
        for (position, line) in sorted.iter().enumerate() {
            output.push_str(&source[line.0..line.2]);
            if !source[line.0..line.2].ends_with('\n') && position != last {
                output.push('\n');
            }
        }
        cursor = run[run.len() - 1].2;
    }
    if cursor == 0 {
        return Some(source.to_string());
    }
    output.push_str(&source[cursor..]);
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_only_header_directives_and_preserves_script_calls() {
        let source =
            "import z.Z\nimport a.A\nfun import(value: Int) = value\nimport(z)\nimport(a)\n";
        assert_eq!(
            sort_imports(source).as_deref(),
            Some("import a.A\nimport z.Z\nfun import(value: Int) = value\nimport(z)\nimport(a)\n")
        );
        let script = "import(z)\nimport(a)\n";
        assert_eq!(sort_imports(script).as_deref(), Some(script));
    }
}
