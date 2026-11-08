//! The text of a box test that precedes a module's first `// FILE:` marker.

/// The case-wide directive region: the text before a case's first `// MODULE:` header, whose
/// directives Kotlin's test infrastructure applies to every module. Empty for a case without
/// modules, whose leading text is its one module's ([`LeadingText`]).
pub fn case_directive_region(src: &str) -> &str {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        if line.trim_start().starts_with("// MODULE:") {
            return &src[..offset];
        }
        offset += line.len();
    }
    ""
}

/// The text of a module that precedes its first `// FILE:` marker: its directives, and any source
/// written before the first marker.
///
/// Kotlin's test infrastructure (`ModuleStructureExtractorImpl`) keeps these lines in the module's
/// first file, so a declaration written there is compiled and a directive written there is the
/// module's. Text before the first `// MODULE:` joins the first module's first file the same way,
/// and its directives apply to every module ([`case_directive_region`]). The text joins the first
/// Kotlin file rather than a Java one: it is Kotlin source and directives, which a Java file
/// would carry only as comments javac ignores and the Kotlin compilation would never see.
#[derive(Default)]
pub(super) struct LeadingText {
    text: String,
    claimed: bool,
}

impl LeadingText {
    pub(super) fn push(&mut self, line: &str) {
        if !self.claimed {
            self.text.push_str(line);
            self.text.push('\n');
        }
    }

    /// Start a Kotlin file's content with the leading text, if no earlier Kotlin file took it.
    pub(super) fn open_kotlin_file(&mut self, content: &mut String) {
        if !self.claimed {
            content.push_str(&std::mem::take(&mut self.text));
            self.claimed = true;
        }
    }

    /// A new `// MODULE:` header: the module's own leading text starts. The text before the first
    /// header carries over until a Kotlin file takes it.
    pub(super) fn start_module(&mut self, first_module: bool) {
        if !first_module || self.claimed {
            *self = LeadingText::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{split_files, split_modules};
    use super::case_directive_region;

    /// `bytecodeText/coroutines/destructuringInLambda/inlineSeparateFiles.kt` declares the class
    /// and inline function its files use before its first marker; JetBrains' runner compiles them
    /// as part of the first file. A directive there stays with Kotlin when a Java file comes first.
    #[test]
    fn split_files_leading_source_joins_the_first_kotlin_file() {
        let src = "\
// LAMBDAS: CLASS
data class A(val x: String)
// FILE: J.java
public class J {}
// FILE: a.kt
fun a() = A(\"O\")
// FILE: b.kt
fun box(): String = a().x + \"K\"
";
        let (kt, java) = split_files(src);
        assert_eq!(java, vec![("J.java".into(), "public class J {}\n".into())]);
        assert_eq!(
            kt,
            vec![
                (
                    "a".to_string(),
                    "// LAMBDAS: CLASS\ndata class A(val x: String)\nfun a() = A(\"O\")\n"
                        .to_string()
                ),
                (
                    "b".to_string(),
                    "fun box(): String = a().x + \"K\"\n".to_string()
                ),
            ]
        );
    }

    /// The text before the first `// MODULE:` joins the first module's first Kotlin file; each
    /// module's text between its header and its first `// FILE:` joins that module's.
    #[test]
    fn split_modules_leading_text_joins_each_modules_first_kotlin_file() {
        let src = "\
// WITH_STDLIB
// MODULE: lib
// JVM_DEFAULT_MODE: disable
// FILE: Lib.java
public class Lib {}
// FILE: lib.kt
interface A
// FILE: lib2.kt
interface B
// MODULE: main(lib)
// SAM_CONVERSIONS: CLASS
// FILE: main.kt
fun box(): String = \"OK\"
";
        let mods = split_modules(src).expect("two-module test should split");
        assert_eq!(
            mods[0].files,
            vec![
                (
                    "lib".to_string(),
                    "// WITH_STDLIB\n// JVM_DEFAULT_MODE: disable\ninterface A\n".to_string()
                ),
                ("lib2".to_string(), "interface B\n".to_string()),
            ]
        );
        assert_eq!(
            mods[1].files,
            vec![(
                "main".to_string(),
                "// SAM_CONVERSIONS: CLASS\nfun box(): String = \"OK\"\n".to_string()
            )]
        );
        assert_eq!(case_directive_region(src), "// WITH_STDLIB\n");
        assert_eq!(case_directive_region("// FILE: a.kt\nfun a() = 1\n"), "");
    }
}
