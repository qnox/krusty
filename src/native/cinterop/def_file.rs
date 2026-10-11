//! A Kotlin/Native `.def` file: the properties that say which headers to read and how, and the C
//! code after the `---` line that is read as if it were one more header.
//!
//! The properties are `java.util.Properties` text: `key = value` lines, `#` and `!` comments, and a
//! trailing `\` continuing a value on the next line. A key may be specialized per target family
//! (`compilerOpts.linux`) and per target (`compilerOpts.linux_x64`); those values are appended to the
//! plain key's for that target, as Kotlin/Native does.

use std::collections::BTreeMap;

/// A parsed `.def` file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DefFile {
    /// Every property as written, keyed by its full name (`compilerOpts.linux_x64` included).
    properties: BTreeMap<String, String>,
    /// The C code after the `---` separator line; empty when there is none.
    pub(crate) code: String,
    /// The line the code starts on, for diagnostics in it.
    pub(crate) code_line: u32,
}

/// The C preprocessor's view of `compilerOpts`: the macros it defines and undefines, in order, and
/// the directories it searches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompilerOptions {
    /// `#define` (`Some(value)`) or `#undef` (`None`) lines, in command-line order.
    pub(crate) macros: Vec<(String, Option<String>)>,
    pub(crate) include_dirs: Vec<String>,
}

impl DefFile {
    /// Read a `.def` file. Every text is one: a line that is not a property is a key without a
    /// value, as `java.util.Properties` reads it.
    pub(crate) fn parse(text: &str) -> Self {
        let mut def = DefFile::default();
        let mut lines = text.split('\n').enumerate();
        let mut pending: Option<String> = None;
        while let Some((index, raw)) = lines.next() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if pending.is_none() && line.trim() == "---" {
                def.code_line = index as u32 + 2;
                def.code = lines.map(|(_, line)| line).collect::<Vec<_>>().join("\n");
                break;
            }
            let mut logical = match pending.take() {
                Some(text) => text + line.trim_start(),
                None => {
                    let trimmed = line.trim_start();
                    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                        continue;
                    }
                    trimmed.to_string()
                }
            };
            if continues(&logical) {
                logical.pop();
                pending = Some(logical);
                continue;
            }
            def.property(&logical);
        }
        if let Some(logical) = pending {
            def.property(&logical);
        }
        def
    }

    /// One logical line: the key ends at the first unescaped `=`, `:` or whitespace, and the value
    /// starts after that separator and the whitespace around it, as `java.util.Properties` reads.
    fn property(&mut self, text: &str) {
        let mut key_end = text.len();
        let mut escaped = false;
        for (at, c) in text.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '=' || c == ':' || c.is_whitespace() {
                key_end = at;
                break;
            }
        }
        let mut rest = text[key_end..].trim_start();
        if let Some(after) = rest.strip_prefix(['=', ':']) {
            rest = after.trim_start();
        }
        self.properties
            .insert(unescape(&text[..key_end]), unescape(rest.trim_end()));
    }

    /// Every property as written, in key order.
    pub(crate) fn properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.properties
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// The value of `key` for the target named `konan_target` of `family`: the plain key's, then
    /// the family's and the target's own, joined by spaces. `None` when none of them is set.
    pub(crate) fn value(&self, key: &str, family: &str, konan_target: &str) -> Option<String> {
        let values: Vec<&str> = [
            key.to_string(),
            format!("{key}.{family}"),
            format!("{key}.{konan_target}"),
        ]
        .iter()
        .filter_map(|key| self.properties.get(key).map(String::as_str))
        .collect();
        (!values.is_empty()).then(|| values.join(" "))
    }

    /// [`Self::value`] split into words, honoring single and double quotes as a shell does.
    pub(crate) fn list(
        &self,
        key: &str,
        family: &str,
        konan_target: &str,
    ) -> Result<Vec<String>, String> {
        match self.value(key, family, konan_target) {
            Some(value) => split_words(&value),
            None => Ok(Vec::new()),
        }
    }
}

impl CompilerOptions {
    /// Read the `-D`, `-U` and `-I` options of `compilerOpts`; every other option is refused, since
    /// krusty cannot honor a C compiler flag it does not implement.
    pub(crate) fn parse(options: &[String]) -> Result<Self, String> {
        let mut parsed = CompilerOptions::default();
        let mut words = options.iter();
        while let Some(word) = words.next() {
            let mut operand = |flag: &str| -> Result<String, String> {
                match word.strip_prefix(flag) {
                    Some("") => words
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("`{flag}` needs an operand")),
                    Some(rest) => Ok(rest.to_string()),
                    None => unreachable!("the flag was matched"),
                }
            };
            if word.starts_with("-D") {
                let definition = operand("-D")?;
                let (name, value) = match definition.split_once('=') {
                    Some((name, value)) => (name.to_string(), value.to_string()),
                    None => (definition, "1".to_string()),
                };
                parsed.macros.push((name, Some(value)));
            } else if word.starts_with("-U") {
                parsed.macros.push((operand("-U")?, None));
            } else if word.starts_with("-I") {
                parsed.include_dirs.push(operand("-I")?);
            } else {
                return Err(format!("unsupported compiler option `{word}`"));
            }
        }
        Ok(parsed)
    }

    /// The `#define` and `#undef` lines these options amount to.
    pub(crate) fn predefines(&self) -> String {
        let mut source = String::new();
        for (name, value) in &self.macros {
            match value {
                Some(value) => source.push_str(&format!("#define {name} {value}\n")),
                None => source.push_str(&format!("#undef {name}\n")),
            }
        }
        source
    }
}

/// Whether a logical line ends in an unescaped `\`, which continues it.
fn continues(line: &str) -> bool {
    line.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

/// `java.util.Properties` escapes in a value: `\=`, `\:`, `\\`, `\t`, `\n` and `\ `.
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Words separated by whitespace; `'…'` and `"…"` group words with spaces in them.
fn split_words(value: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in value.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(format!("unterminated quote in `{value}`"));
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_continue_across_lines_and_the_code_follows_the_separator() {
        let def = DefFile::parse(
            "# zlib\npackage = platform.zlib\nheaders = zconf.h \\\n    zlib.h\n\
             compilerOpts = -DByte=uByte '-DQUOTED=a b'\ncompilerOpts.linux_x64 = -I/opt/x64\n\
             compilerOpts.linux = -D_GNU_SOURCE\nlinkerOpts = -lz\n---\n\nstatic int f(void);\n",
        );
        assert_eq!(
            def.value("package", "linux", "linux_x64").as_deref(),
            Some("platform.zlib")
        );
        assert_eq!(
            def.list("headers", "linux", "linux_x64").expect("words"),
            ["zconf.h", "zlib.h"]
        );
        assert_eq!(
            def.list("compilerOpts", "linux", "linux_x64")
                .expect("words"),
            [
                "-DByte=uByte",
                "-DQUOTED=a b",
                "-D_GNU_SOURCE",
                "-I/opt/x64"
            ]
        );
        assert_eq!(
            def.list("compilerOpts", "linux", "linux_arm64")
                .expect("words"),
            ["-DByte=uByte", "-DQUOTED=a b", "-D_GNU_SOURCE"]
        );
        assert_eq!(def.value("headerFilter", "linux", "linux_x64"), None);
        assert_eq!(def.code, "\nstatic int f(void);\n");
        assert_eq!(def.code_line, 10);
    }

    #[test]
    fn compiler_options_define_undefine_and_search() {
        let words = ["-DA", "-D", "B=2", "-UC", "-I", "include", "-Isys"].map(String::from);
        let options = CompilerOptions::parse(&words).expect("parses");
        assert_eq!(options.predefines(), "#define A 1\n#define B 2\n#undef C\n");
        assert_eq!(options.include_dirs, ["include", "sys"]);
        assert_eq!(
            CompilerOptions::parse(&["-O2".to_string()]),
            Err("unsupported compiler option `-O2`".to_string())
        );
    }

    #[test]
    fn a_key_ends_at_its_first_separator_as_java_properties_read_it() {
        let def = DefFile::parse("headers zlib.h\nlinkerOpts:-lz\nkey\\=x = y\n");
        assert_eq!(
            def.value("headers", "linux", "linux_x64").as_deref(),
            Some("zlib.h")
        );
        assert_eq!(
            def.value("linkerOpts", "linux", "linux_x64").as_deref(),
            Some("-lz")
        );
        assert_eq!(
            def.value("key=x", "linux", "linux_x64").as_deref(),
            Some("y")
        );
    }
}
