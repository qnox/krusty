//! `@argfile` expansion, as kotlinc's `preprocessCommandLineArguments` does it before parsing.

use std::io::ErrorKind;
use std::path::Path;

/// Replace every `@path` argument by the arguments that file holds. A file that cannot be read
/// contributes nothing and adds kotlinc's warning text to `problems`.
pub fn expand(
    arguments: impl IntoIterator<Item = String>,
    problems: &mut Vec<String>,
) -> Vec<String> {
    let mut expanded = Vec::new();
    for argument in arguments {
        let Some(path) = argument.strip_prefix('@') else {
            expanded.push(argument);
            continue;
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => expanded.extend(split(&contents)),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                problems.push(format!("Argfile not found: {}", absolute(path)))
            }
            Err(error) => problems.push(format!("Error while reading argfile: {error}")),
        }
    }
    expanded
}

/// kotlinc's `File.absolutePath`: relative to the working directory, not normalized.
fn absolute(path: &str) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.display().to_string();
    }
    std::env::current_dir()
        .map(|directory| directory.join(path).display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Tokens separated by whitespace. A token that reaches a `'` or `"` takes everything up to the
/// matching quote, with `\` escaping the next character, and ends there.
pub fn split(contents: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = contents.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut token = String::new();
        let mut quoted = false;
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                break;
            }
            if c == '"' || c == '\'' {
                quoted = true;
                while let Some(inner) = chars.next() {
                    if inner == c {
                        break;
                    }
                    if inner == '\\' {
                        if let Some(escaped) = chars.next() {
                            token.push(escaped);
                        }
                    } else {
                        token.push(inner);
                    }
                }
                break;
            }
            token.push(c);
        }
        // kotlinc returns a quoted token even when it is empty, and stops at the first empty
        // unquoted one, which only happens at the end of the input.
        if token.is_empty() && !quoted {
            return tokens;
        }
        tokens.push(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_group_and_backslashes_escape_inside_them() {
        assert_eq!(
            split("-d \"out dir\"  'a\\'b' plain\n-x"),
            vec!["-d", "out dir", "a'b", "plain", "-x"]
        );
    }

    /// A quoted section ends the token, as in kotlinc: `a"b c"d` is `ab c` then `d`.
    #[test]
    fn a_quote_ends_the_token() {
        assert_eq!(split("a\"b c\"d"), vec!["ab c", "d"]);
    }

    #[test]
    fn a_missing_argfile_is_reported_and_contributes_nothing() {
        let mut problems = Vec::new();
        let expanded = expand(
            ["x.kt".to_string(), "@/nonexistent/krusty.args".to_string()],
            &mut problems,
        );
        assert_eq!(expanded, vec!["x.kt"]);
        assert_eq!(
            problems,
            vec!["Argfile not found: /nonexistent/krusty.args"]
        );
    }
}
