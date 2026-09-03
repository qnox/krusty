use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::common;

const LIB: &str = "package lib\n\
                   internal class Hidden(val value: Int)\n\
                   internal val hiddenProperty: Int = 7\n\
                   internal fun hiddenFun(value: Int): Int = value\n\
                   private fun hiddenPrivate(value: Int): Int = value\n\
                   class Visible(val value: Int) {\n\
                       internal val hiddenValue: Int = value\n\
                       internal fun hiddenMember(other: Int): Int = value + other\n\
                   }\n\
                   open class Parent {\n\
                       protected class ProtectedBox(val value: Int)\n\
                   }\n";

#[derive(Debug, PartialEq, Eq)]
struct ObservedDiagnostic {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

fn reference_diagnostics(output: &str, severity: &str) -> Vec<ObservedDiagnostic> {
    let lines = output.lines().collect::<Vec<_>>();
    let (diagnostics, remainder) = lines.as_chunks::<3>();
    let marker = format!(": {severity}: ");
    let observed = diagnostics
        .iter()
        .map(|diagnostic| {
            let line = diagnostic[0];
            let (location, message) = line
                .split_once(&marker)
                .unwrap_or_else(|| panic!("unexpected kotlinc diagnostic: {line}"));
            let mut fields = location.rsplitn(3, ':');
            let column = fields
                .next()
                .expect("kotlinc diagnostic column")
                .parse()
                .expect("numeric kotlinc diagnostic column");
            let line = fields
                .next()
                .expect("kotlinc diagnostic line")
                .parse()
                .expect("numeric kotlinc diagnostic line");
            let path = fields.next().expect("kotlinc diagnostic path");
            ObservedDiagnostic {
                file: Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("UTF-8 kotlinc diagnostic filename")
                    .to_string(),
                line,
                column,
                message: message.to_string(),
            }
        })
        .collect();
    assert_eq!(remainder.len(), 0);
    observed
}

struct Fixture {
    _roots: Vec<FixtureRoot>,
    classpath: Vec<PathBuf>,
    friend_output: PathBuf,
    jdk: Option<PathBuf>,
}

struct FixtureRoot(PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let stdlib = common::stdlib_jar();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "krusty_internal_classpath_access_{epoch}_{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("create Kotlin fixture directory");
        let root = FixtureRoot(root);
        let output = root.0.join("classes");
        std::fs::create_dir(&output).expect("create Kotlin fixture output directory");
        let source = root.0.join("Library.kt");
        std::fs::write(&source, LIB).expect("write Kotlin fixture source");
        let args = vec![
            "-d".to_string(),
            output.to_string_lossy().into_owned(),
            "-cp".to_string(),
            stdlib.to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ];
        match common::kotlinc_compile(&args).expect("reference compiler unavailable") {
            (0, _) => {}
            (code, stderr) => panic!("kotlinc fixture failed ({code}): {stderr}"),
        }
        let java_sources = [(
            "PackageBox.java".to_string(),
            "package javafixture;\n\
             class PackageBox {\n\
                 PackageBox(int value) {}\n\
             }\n"
            .to_string(),
        )];
        let (java_output, _) =
            common::javac_compile(&java_sources, &[]).expect("javac fixture failed");
        let java_root = java_output
            .parent()
            .expect("javac output has no parent")
            .to_path_buf();
        let jdk = Some({
            let home = common::java_home();
            PathBuf::from(format!("{home}/lib/modules"))
        });
        Fixture {
            _roots: vec![root, FixtureRoot(java_root)],
            classpath: vec![stdlib, output.clone(), java_output],
            friend_output: output,
            jdk,
        }
    }

    fn diagnostics(&self, source: &str) -> Vec<String> {
        common::front_end_diagnostics(source, &self.classpath, self.jdk.as_deref())
    }

    fn diagnostics_files(&self, sources: &[&str]) -> Vec<String> {
        common::front_end_diagnostics_files(sources, &self.classpath, self.jdk.as_deref())
    }

    fn friend_diagnostics(&self, source: &str) -> Vec<String> {
        common::front_end_diagnostics_with_friend_paths(
            source,
            &self.classpath,
            std::slice::from_ref(&self.friend_output),
            self.jdk.as_deref(),
        )
    }

    fn run_box(&self, source: &str) -> String {
        common::compile_and_run_box(source, "Main", &self.classpath, self.jdk.as_deref())
            .unwrap_or_else(|| {
                let diagnostics = self.diagnostics(source);
                let backend = common::backend_outcome_in_process(
                    source,
                    "Main",
                    &self.classpath,
                    self.jdk.as_deref(),
                );
                panic!(
                    "compile/run failed for {source:?}; diagnostics: {diagnostics:?}; backend: {backend:?}"
                )
            })
    }
}

#[test]
fn friend_classpath_grants_internal_visibility_without_relaxing_dependencies() {
    let fixture = Fixture::new();
    let source = "import lib.Hidden\nfun use(): Int = Hidden(1).value\n";

    assert_eq!(
        fixture.diagnostics(source),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(fixture.friend_diagnostics(source), Vec::<String>::new());
}

#[test]
fn friend_classpath_grants_internal_top_level_property_visibility() {
    let fixture = Fixture::new();
    let source = "import lib.hiddenProperty\nfun use(): Int = hiddenProperty\n";

    assert_eq!(
        fixture.diagnostics(source),
        ["unresolved reference 'hiddenProperty'."]
    );
    assert_eq!(fixture.friend_diagnostics(source), Vec::<String>::new());
}

#[test]
fn classifier_access_diagnostics_follow_resolution_scope() {
    let fixture = Fixture::new();

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Int { Hidden(1); return 0 }\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Visible\n\
             fun use(): Int = Visible(1).value\n"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture.diagnostics("fun use(): Int { Hidden(1); return 0 }\n"),
        ["unresolved reference 'Hidden'."]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(Hidden: Int): Int { Hidden(); return 0 }\n"
        ),
        ["no value passed for parameter 'value'."]
    );
    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import lib.Hidden\n\
             val Hidden: Int = 1\n\
             fun use(): Int { Hidden(); return 0 }\n"
        ),
        ["no value passed for parameter 'value'."]
    );
    assert_eq!(
        fixture.diagnostics_files(&[
            "package lib\nclass Hidden(val value: Int)\n",
            "package lib\n\
             fun use(): Int = Hidden(1).value\n",
        ]),
        Vec::<String>::new()
    );
    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import lib.Parent\n\
             class Child : Parent() {\n\
                 fun use(): Int = ProtectedBox(1).value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    let alias_diagnostics = fixture.diagnostics_files(&[
        "package first\n\
         import lib.Hidden as FirstAlias\n\
         fun use(): Int { FirstAlias(1); return 0 }\n",
        "package second\n\
         import lib.Hidden as SecondAlias\n\
         fun use(): Int { SecondAlias(2); return 0 }\n",
    ]);
    assert_eq!(
        alias_diagnostics,
        [
            "cannot access 'FirstAlias': it is internal",
            "cannot access 'SecondAlias': it is internal",
        ]
    );

    let protected_alias_diagnostics = fixture.diagnostics(
        "package consumer\n\
         import lib.Parent.ProtectedBox as Guard\n\
         fun use(): Int { Guard(1); return 0 }\n",
    );
    assert_eq!(
        protected_alias_diagnostics,
        ["cannot access 'Guard': it is protected"]
    );

    assert_eq!(
        fixture.diagnostics(
            "package consumer\n\
             import javafixture.PackageBox\n\
             fun use(): Int { PackageBox(1); return 0 }\n",
        ),
        [
            "cannot access 'constructor(p0: Int): PackageBox': it is package-private in 'javafixture.PackageBox'.",
            "cannot access 'class PackageBox : Any': it is package-private in file.",
        ]
    );
}

#[test]
fn classifier_access_diagnostics_cover_type_positions() {
    let fixture = Fixture::new();

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(value: Hidden): Int = 0\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Hidden? = null\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
    assert_eq!(
        fixture.diagnostics(
            "import lib.Visible\n\
             fun use(value: Visible): Int = value.value\n"
        ),
        Vec::<String>::new()
    );

    let alias_diagnostics = fixture.diagnostics(
        "import lib.Hidden as Alias\n\
         fun use(value: Alias): Int = 0\n",
    );
    assert_eq!(alias_diagnostics, ["cannot access 'Alias': it is internal"]);

    let nested_alias_diagnostics = fixture.diagnostics(
        "import lib.Parent.ProtectedBox as Guard\n\
         fun use(value: Guard): Int = 0\n",
    );
    assert_eq!(
        nested_alias_diagnostics,
        ["cannot access 'Guard': it is protected"]
    );

    assert_eq!(
        fixture.diagnostics(
            "import lib.Parent\n\
             class Child : Parent() {\n\
                 private fun use(value: ProtectedBox): ProtectedBox = value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             class Owner {\n\
                 class Hidden\n\
                 fun use(value: Hidden): Hidden = value\n\
             }\n"
        ),
        Vec::<String>::new()
    );

    assert_eq!(
        fixture.diagnostics(
            "import lib.Hidden\n\
             fun use(): Int {\n\
                 val value: Hidden? = null\n\
                 return 0\n\
             }\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );
}

#[test]
fn invisible_reference_suppression_matches_kotlinc_exactly() {
    let fixture = Fixture::new();

    let sources = [
        (
            "AliasSuppressed.kt",
            "@file:S(\"INVISIBLE_REFERENCE\")\n\
                 package aliascase\n\
                 import kotlin.Suppress as S\n\
                 fun use(): Int = lib.Hidden(1).value\n",
        ),
        (
            "FileSuppressed.kt",
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
                 package filecase\n\
                 import lib.Hidden\n\
                 fun use(): Int = Hidden(1).value\n",
        ),
        (
            "FunctionSuppressed.kt",
            "package functioncase\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 private fun use(): Int = lib.Hidden(1).value\n",
        ),
        (
            "JavaPackagePrivateSuppressed.kt",
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
                 package javapackageprivatecase\n\
                 fun use(): Int { javafixture.PackageBox(1); return 0 }\n",
        ),
        (
            "ClassSuppressed.kt",
            "package classcase\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 class Use { fun read(): Int = lib.Hidden(1).value }\n",
        ),
        (
            "CompanionSuppressed.kt",
            "package companioncase\n\
                 class Use {\n\
                     @Suppress(\"INVISIBLE_REFERENCE\")\n\
                     companion object { fun read(): Int = lib.Hidden(1).value }\n\
                 }\n",
        ),
        (
            "PropertySuppressed.kt",
            "package propertycase\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 val value: Int = lib.Hidden(1).value\n",
        ),
        (
            "PrimaryConstructorSuppressed.kt",
            "package primaryconstructorcase\n\
                 class Use @Suppress(\"INVISIBLE_REFERENCE\") constructor(\n\
                 val value: Int = lib.Hidden(1).value\n\
                 )\n",
        ),
        (
            "PrivateCallSuppressed.kt",
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
                 package privatecallcase\n\
                 import lib.hiddenPrivate\n\
                 private fun use(): Int = hiddenPrivate(1)\n",
        ),
        (
            "SecondaryConstructorSuppressed.kt",
            "package secondaryconstructorcase\n\
                 class Use {\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 constructor() { lib.Hidden(1) }\n\
                 }\n",
        ),
        (
            "TypeSuppressed.kt",
            "package typecase\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 private fun use(value: lib.Hidden): Int = value.value\n",
        ),
        (
            "MemberPropertySuppressed.kt",
            "package memberpropertycase\n\
                 class Use {\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 val value: Int = lib.Hidden(1).value\n\
                 }\n",
        ),
        (
            "LocalFunctionSuppressed.kt",
            "package localfunctioncase\n\
                 fun outer(): Int {\n\
                 @Suppress(\"INVISIBLE_REFERENCE\")\n\
                 fun local(): Int = lib.Hidden(1).value\n\
                 return local()\n\
                 }\n",
        ),
    ];
    let source_texts = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics(&sources, &fixture.classpath);
    assert_eq!(
        fixture.diagnostics_files(&source_texts),
        Vec::<String>::new(),
        "{}",
        result.krusty_stderr,
    );
    assert_eq!(
        (
            result.krusty_code,
            result.krusty_stderr.as_str(),
            result.reference_code,
        ),
        (0, "", 0),
        "{}",
        result.reference_stderr,
    );
    let warning = "suppression of error 'INVISIBLE_REFERENCE' might compile and work, but the compiler behavior is UNSPECIFIED and WILL NOT BE PRESERVED. Please report your use case to the Kotlin issue tracker instead: https://kotl.in/issue";
    assert_eq!(
        reference_diagnostics(&result.reference_stderr, "warning"),
        [
            ObservedDiagnostic {
                file: "AliasSuppressed.kt".to_string(),
                line: 1,
                column: 9,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "ClassSuppressed.kt".to_string(),
                line: 2,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "CompanionSuppressed.kt".to_string(),
                line: 3,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "FileSuppressed.kt".to_string(),
                line: 1,
                column: 16,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "FunctionSuppressed.kt".to_string(),
                line: 2,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "JavaPackagePrivateSuppressed.kt".to_string(),
                line: 1,
                column: 16,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "LocalFunctionSuppressed.kt".to_string(),
                line: 3,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "MemberPropertySuppressed.kt".to_string(),
                line: 3,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "PrimaryConstructorSuppressed.kt".to_string(),
                line: 2,
                column: 21,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "PrivateCallSuppressed.kt".to_string(),
                line: 1,
                column: 16,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "PropertySuppressed.kt".to_string(),
                line: 2,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "SecondaryConstructorSuppressed.kt".to_string(),
                line: 3,
                column: 11,
                message: warning.to_string(),
            },
            ObservedDiagnostic {
                file: "TypeSuppressed.kt".to_string(),
                line: 2,
                column: 11,
                message: warning.to_string(),
            },
        ]
    );

    assert_eq!(
        fixture.run_box(
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
             import lib.Hidden\n\
             fun box(): String = if (Hidden(1).value == 1) \"OK\" else \"FAIL\"\n"
        ),
        "OK"
    );
    assert_eq!(
        fixture.run_box(
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
             import lib.hiddenFun\n\
             fun box(): String = if (hiddenFun(2) == 2) \"OK\" else \"FAIL\"\n"
        ),
        "OK"
    );
    assert_eq!(
        fixture.run_box(
            "@file:Suppress(\"INVISIBLE_REFERENCE\")\n\
             import lib.Visible\n\
             fun box(): String {\n\
                 val visible = Visible(2)\n\
                 val total = visible.hiddenValue + visible.hiddenMember(3)\n\
                 return if (total == 7) \"OK\" else \"FAIL: $total\"\n\
             }\n"
        ),
        "OK"
    );
    assert_eq!(
        fixture.diagnostics(
            "@file:Suppress(\"INVISIBLE_MEMBER\")\n\
             import lib.Hidden\n\
             fun use(): Int { Hidden(1); return 0 }\n"
        ),
        ["cannot access 'Hidden': it is internal"]
    );

    let custom_suppress = "@file:custom.Suppress(\"INVISIBLE_REFERENCE\")\n\
         package custom\n\
         @Target(AnnotationTarget.FILE)\n\
         annotation class Suppress(val value: String)\n\
         fun use(): Int = lib.Hidden(1).value\n";
    assert_eq!(
        fixture.diagnostics(custom_suppress),
        ["cannot access 'lib.Hidden': it is internal"]
    );
    let custom_result = common::compiler_diagnostics(
        &[("CustomSuppress.kt", custom_suppress)],
        &fixture.classpath,
    );
    assert_eq!(
        (custom_result.krusty_code, custom_result.reference_code),
        (1, 1)
    );
    assert_eq!(
        reference_diagnostics(&custom_result.reference_stderr, "error"),
        [
            ObservedDiagnostic {
                file: "CustomSuppress.kt".to_string(),
                line: 5,
                column: 22,
                message: "cannot access 'class Hidden : Any': it is internal in file.".to_string(),
            },
            ObservedDiagnostic {
                file: "CustomSuppress.kt".to_string(),
                line: 5,
                column: 32,
                message: "cannot access 'class Hidden : Any': it is internal in file.".to_string(),
            },
        ]
    );
}
