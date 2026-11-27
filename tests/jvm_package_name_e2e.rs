//! `@file:JvmPackageName("a.b")` moves a file facade (and a multifile facade and its parts) into the
//! JVM package `a.b` while its declarations stay in the Kotlin package of the file. kotlinc records
//! the Kotlin package in the facade's `@Metadata` (`pn`) and lists the facade under the Kotlin
//! package of the module's `.kotlin_module`, with the JVM package beside it.
//!
//! `@JvmPackageName` is `internal` to the standard library, so every module here is compiled as a
//! friend of it (kotlinc `-Xfriend-paths`), as the Kotlin test library's own build does.

use super::common;
use krusty::diag::DiagSink;
use krusty::source::SourceInput;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Every file the reference compiler and krusty write for one module, by path under the output
/// directory, `.kotlin_module` included.
struct Outputs {
    reference: BTreeMap<String, Vec<u8>>,
    krusty: BTreeMap<String, Vec<u8>>,
}

impl Outputs {
    fn assert_identical(&self) {
        assert_eq!(
            self.reference.keys().collect::<Vec<_>>(),
            self.krusty.keys().collect::<Vec<_>>(),
            "exact output inventory"
        );
        let different = self
            .reference
            .iter()
            .filter(|(path, bytes)| self.krusty[*path] != **bytes)
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>();
        assert!(
            different.is_empty(),
            "outputs differ from kotlinc: {different:?}"
        );
    }
}

fn collect(
    root: &std::path::Path,
    directory: &std::path::Path,
    into: &mut BTreeMap<String, Vec<u8>>,
) {
    for entry in std::fs::read_dir(directory).expect("read output directory") {
        let path = entry.expect("read output entry").path();
        if path.is_dir() {
            collect(root, &path, into);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("output under its root")
                .to_string_lossy()
                .into_owned();
            into.insert(relative, std::fs::read(&path).expect("read output"));
        }
    }
}

/// Compile `sources` as one module with both compilers on `classpath`, the standard library and the
/// friend paths.
fn compile_both(sources: &[(&str, &str)], classpath: &[PathBuf]) -> Outputs {
    let stdlib = common::stdlib_jar();
    let work = common::scratch_dir().expect("allocate module output directory");
    let reference_out = work.join("reference");
    std::fs::create_dir_all(&reference_out).expect("create reference output");
    let mut arguments = vec![
        "-d".to_owned(),
        reference_out.to_string_lossy().into_owned(),
        format!("-Xfriend-paths={}", stdlib.display()),
    ];
    let mut jars = classpath.to_vec();
    jars.push(stdlib.clone());
    arguments.push("-cp".to_owned());
    arguments.push(
        std::env::join_paths(&jars)
            .expect("reference classpath")
            .to_string_lossy()
            .into_owned(),
    );
    for (name, source) in sources {
        let path = work.join(name);
        std::fs::write(&path, source).expect("write fixture source");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let (code, diagnostics) =
        common::kotlinc_compile(&arguments).expect("reference compiler is provisioned");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    let mut reference = BTreeMap::new();
    collect(&reference_out, &reference_out, &mut reference);

    let stems = sources
        .iter()
        .map(|(name, _)| name.trim_end_matches(".kt").to_string())
        .collect::<Vec<_>>();
    let inputs = sources
        .iter()
        .zip(&stems)
        .map(|((_, source), stem)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let mut paths = jars;
    paths.push(common::jdk_modules());
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new_with_friend_paths(
        paths,
        vec![stdlib],
    ));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone())
            .expect("JVM provider initialization"),
    );
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(krusty::compilation_target::CompilationTarget::Jvm, platform),
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let outputs = krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &krusty_jvm::JvmBackend::new(cp),
        "main",
        &mut diags,
    );
    assert!(
        !diags.has_errors(),
        "krusty rejected the fixture: {}",
        diags.render_all(sources, krusty::diag::DiagnosticNames::Hidden)
    );
    let krusty = outputs.into_iter().collect();
    let _ = std::fs::remove_dir_all(work);
    Outputs { reference, krusty }
}

/// A relocated file with a function, a property, a constant and an extension.
const RELOCATED: &str = "@file:JvmPackageName(\"p.jvm\")\n\
    package p\n\
    \n\
    fun helper(x: Int = 1): String = \"K$x\"\n\
    val answer: Int = 42\n\
    fun String.shout(): String = this + \"!\"\n\
    const val LIMIT = 3\n";

/// A file of the same Kotlin package that is not relocated, calling into the relocated facade.
const CALLER: &str = "package p\n\
    \n\
    fun box(): String {\n\
    \x20   val reference = ::helper\n\
    \x20   val joined = helper() + answer + reference(2) + \"a\".shout()\n\
    \x20   return if (joined == \"K142K2a!\") \"OK\" else joined\n\
    }\n";

#[test]
fn a_relocated_facade_matches_kotlinc() {
    let outputs = compile_both(&[("relocated.kt", RELOCATED)], &[]);
    assert!(outputs.krusty.contains_key("p/jvm/RelocatedKt.class"));
    outputs.assert_identical();
}

#[test]
fn calls_into_a_relocated_facade_match_kotlinc() {
    let outputs = compile_both(&[("relocated.kt", RELOCATED), ("Caller.kt", CALLER)], &[]);
    outputs.assert_identical();
}

/// Write `files` under a fresh directory and return it, for use as a classpath entry.
fn classpath_directory(files: &BTreeMap<String, Vec<u8>>) -> PathBuf {
    let directory = common::scratch_dir()
        .expect("allocate classpath directory")
        .join("library");
    for (path, bytes) in files {
        let target = directory.join(path);
        std::fs::create_dir_all(target.parent().expect("an output has a directory"))
            .expect("create library directory");
        std::fs::write(target, bytes).expect("write library output");
    }
    directory
}

/// A module that calls the relocated facade across a module boundary: through the reference
/// compiler's output and through krusty's, from source that names the Kotlin package only.
const CONSUMER: &str = "package use\n\
    \n\
    import p.answer\n\
    import p.helper\n\
    import p.shout\n\
    \n\
    fun box(): String {\n\
    \x20   val joined = helper() + answer + \"a\".shout()\n\
    \x20   return if (joined == \"K142a!\") \"OK\" else joined\n\
    }\n";

#[test]
fn a_consumer_of_a_relocated_facade_matches_kotlinc_and_runs_against_either_library() {
    let library = compile_both(&[("relocated.kt", RELOCATED)], &[]);
    let reference_library = classpath_directory(&library.reference);
    let krusty_library = classpath_directory(&library.krusty);
    compile_both(
        &[("Use.kt", CONSUMER)],
        std::slice::from_ref(&reference_library),
    )
    .assert_identical();
    compile_both(
        &[("Use.kt", CONSUMER)],
        std::slice::from_ref(&krusty_library),
    )
    .assert_identical();
    let jdk = common::jdk_modules();
    for library in [reference_library, krusty_library] {
        assert_eq!(
            common::expect_box_run(
                CONSUMER,
                "Use",
                &[library, common::stdlib_jar()],
                Some(jdk.as_path())
            ),
            "OK"
        );
    }
}

/// A facade next to a plain one of the same Kotlin package: the module lists both under the Kotlin
/// package, the relocated one beside its JVM package.
#[test]
fn relocated_and_plain_facades_of_one_package_share_a_module_entry() {
    let plain = "package p\nfun plain(): Int = 1\n";
    let outputs = compile_both(&[("relocated.kt", RELOCATED), ("plain.kt", plain)], &[]);
    outputs.assert_identical();
}

/// A multifile facade relocated by `@JvmPackageName`: the facade and each part live in the JVM
/// package.
#[test]
fn a_relocated_multifile_facade_matches_kotlinc() {
    let first = "@file:JvmPackageName(\"p.jvm\")\n\
        @file:JvmName(\"Shared\")\n\
        @file:JvmMultifileClass\n\
        package p\n\
        fun one(): Int = 1\n";
    let second = "@file:JvmPackageName(\"p.jvm\")\n\
        @file:JvmName(\"Shared\")\n\
        @file:JvmMultifileClass\n\
        package p\n\
        fun two(): Int = 2\n";
    let outputs = compile_both(&[("first.kt", first), ("second.kt", second)], &[]);
    outputs.assert_identical();
}
