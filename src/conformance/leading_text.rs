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
/// first declared file, whatever its language, so a declaration written there is compiled as part
/// of that file. Text before the first `// MODULE:` joins the first module's first file the same
/// way. Which file carries the text does not decide what its directives configure: a unit's modes
/// are read from all of its sources, and the case-wide ones from [`case_directive_region`].
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

    /// Start a declared file's content with the leading text, if no earlier file took it.
    pub(super) fn open_file(&mut self, content: &mut String) {
        if !self.claimed {
            content.push_str(&std::mem::take(&mut self.text));
            self.claimed = true;
        }
    }

    /// A new `// MODULE:` header: the module's own leading text starts. The text before the first
    /// header carries over until a file takes it.
    pub(super) fn start_module(&mut self, first_module: bool) {
        if !first_module || self.claimed {
            *self = LeadingText::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{kotlin_source_blocks, split_files, split_modules, KotlinSourceBlock};
    use super::case_directive_region;
    use crate::conformance::UnitCodegenModes;

    /// `bytecodeText/coroutines/destructuringInLambda/inlineSeparateFiles.kt` declares the class
    /// and inline function its files use before its first marker; JetBrains' runner compiles them
    /// as part of the first file.
    #[test]
    fn split_files_leading_source_joins_the_first_declared_file() {
        let src = "\
// LAMBDAS: CLASS
data class A(val x: String)
// FILE: a.kt
fun a() = A(\"O\")
// FILE: J.java
public class J {}
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

    /// `ModuleStructureExtractorImpl` assigns the leading text to the first declared file whatever
    /// its language, so a Java first file carries it, declarations included, and no Kotlin file is
    /// reinterpreted with it.
    #[test]
    fn split_files_leading_text_joins_a_java_first_file() {
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
        assert_eq!(
            java,
            vec![(
                "J.java".into(),
                "// LAMBDAS: CLASS\ndata class A(val x: String)\npublic class J {}\n".into()
            )]
        );
        assert_eq!(
            kt,
            vec![
                ("a".to_string(), "fun a() = A(\"O\")\n".to_string()),
                (
                    "b".to_string(),
                    "fun box(): String = a().x + \"K\"\n".to_string()
                ),
            ]
        );
    }

    /// The parse gate sees the same physical files: leading text on a Java first file is not part
    /// of any Kotlin block.
    #[test]
    fn kotlin_source_blocks_leave_leading_text_on_a_java_first_file() {
        let src = "\
// LAMBDAS: CLASS
// FILE: J.java
public class J {}
// FILE: a.kt
fun box(): String = \"OK\"
";
        assert_eq!(
            kotlin_source_blocks(src, "case").unwrap(),
            vec![KotlinSourceBlock {
                module: None,
                name: "a".to_string(),
                source: "fun box(): String = \"OK\"\n".to_string(),
            }]
        );
    }

    /// The text before the first `// MODULE:` joins the first module's first declared file; each
    /// module's text between its header and its first `// FILE:` joins that module's, a Java file
    /// included.
    #[test]
    fn split_modules_leading_text_joins_each_modules_first_declared_file() {
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
            mods[0].java_files,
            vec![(
                "Lib.java".to_string(),
                "// WITH_STDLIB\n// JVM_DEFAULT_MODE: disable\npublic class Lib {}\n".to_string()
            )]
        );
        assert_eq!(
            mods[0].files,
            vec![
                ("lib".to_string(), "interface A\n".to_string()),
                ("lib2".to_string(), "interface B\n".to_string()),
            ]
        );
        // The directive the Java file carries still configures its module's compilation.
        let lib_sources = mods[0]
            .files
            .iter()
            .chain(&mods[0].java_files)
            .map(|(_, source)| source.as_str());
        assert_eq!(
            UnitCodegenModes::of_unit_in_case(case_directive_region(src), lib_sources)
                .kotlinc_args(),
            vec!["-jvm-default".to_string(), "disable".to_string()]
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
