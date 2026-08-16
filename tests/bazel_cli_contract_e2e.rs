//! The command-line contract `bazel/defs.bzl` depends on.
//!
//! The Starlark rule is not executable from this test suite — it needs a bazel invocation — but
//! everything it assumes about the krusty CLI is: a `@file` param file in Bazel's `multiline`
//! format, one argument per line; `-d <name>.jar` producing a real jar; `-module-name` naming the
//! `kotlin_module` index; `-classpath` taking colon-joined entries; and a non-zero exit when the
//! sources do not compile. A change to any of those breaks the rule silently, which is exactly what
//! this file exists to prevent.

use std::path::{Path, PathBuf};

use super::common;

fn workspace(tag: &str) -> PathBuf {
    let dir = common::scratch_dir()
        .expect("scratch directory")
        .join(format!("bazel-contract-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workspace");
    dir
}

/// Run the krusty binary with a Bazel-shaped param file: `multiline` format (exactly one argument
/// per line, no quoting) referenced as `@path`.
fn run_with_param_file(dir: &Path, arguments: &[String]) -> std::process::Output {
    let param_file = dir.join("params.txt");
    std::fs::write(&param_file, format!("{}\n", arguments.join("\n"))).expect("write param file");
    std::process::Command::new(common::krusty_binary())
        .arg(format!("@{}", param_file.display()))
        .output()
        .expect("run krusty")
}

fn jar_entries(jar: &Path) -> Vec<String> {
    let file = std::fs::File::open(jar).expect("open jar");
    let mut archive = zip::ZipArchive::new(file).expect("read jar");
    (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .expect("jar entry")
                .name()
                .to_string()
        })
        .collect()
}

/// The shape `krusty_jvm_library` emits: `-d <jar>`, `-module-name`, then the sources.
#[test]
fn a_param_file_compiles_to_a_jar() {
    let dir = workspace("jar");
    let source = dir.join("A.kt");
    std::fs::write(&source, "package demo\nfun hello(): String = \"hi\"\n").expect("write source");
    let jar = dir.join("util.jar");

    let output = run_with_param_file(
        &dir,
        &[
            "-d".to_string(),
            jar.display().to_string(),
            "-module-name".to_string(),
            "intellij.platform.util".to_string(),
            source.display().to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "krusty failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entries = jar_entries(&jar);
    assert!(
        entries.iter().any(|entry| entry == "demo/AKt.class"),
        "the compiled class must be in the jar: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry == "META-INF/intellij.platform.util.kotlin_module"),
        "-module-name names the kotlin_module index, which is how a consumer discovers this \
         module's top-level declarations: {entries:?}"
    );
}

/// `args.add_joined("-classpath", …, join_with = ":")` produces ONE argument holding every entry.
#[test]
fn a_colon_joined_classpath_resolves_a_dependency() {
    let dir = workspace("classpath");
    let dependency = dir.join("dep.jar");
    let library = dir.join("Lib.kt");
    std::fs::write(&library, "package lib\nfun shared(): Int = 41\n").expect("write library");
    let built = run_with_param_file(
        &dir,
        &[
            "-d".to_string(),
            dependency.display().to_string(),
            "-module-name".to_string(),
            "lib".to_string(),
            library.display().to_string(),
        ],
    );
    assert!(built.status.success(), "dependency must build");

    let consumer = dir.join("App.kt");
    std::fs::write(
        &consumer,
        "package app\nimport lib.shared\nfun answer(): Int = shared() + 1\n",
    )
    .expect("write consumer");
    let jar = dir.join("app.jar");
    // Two entries joined with `:`, as the rule builds them — a second, unrelated path proves the
    // separator is parsed rather than the whole string being taken as one file name.
    let classpath = format!(
        "{}:{}",
        dependency.display(),
        dir.join("absent.jar").display()
    );
    let output = run_with_param_file(
        &dir,
        &[
            "-d".to_string(),
            jar.display().to_string(),
            "-module-name".to_string(),
            "app".to_string(),
            "-classpath".to_string(),
            classpath,
            consumer.display().to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "the consumer must resolve its dependency through the joined classpath: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(jar_entries(&jar).iter().any(|e| e == "app/AppKt.class"));
}

/// The rule passes the project's kotlinc flags through verbatim, so krusty must accept the ones
/// intellij-community actually sets without treating them as source paths.
#[test]
fn the_projects_kotlinc_flags_are_accepted() {
    let dir = workspace("flags");
    let source = dir.join("F.kt");
    std::fs::write(&source, "package demo\nfun value(): Int = 1\n").expect("write source");
    let jar = dir.join("flags.jar");
    let output = run_with_param_file(
        &dir,
        &[
            "-d".to_string(),
            jar.display().to_string(),
            "-Xjvm-default=all".to_string(),
            "-progressive".to_string(),
            "-XXLanguage:+AllowEagerSupertypeAccessibilityChecks".to_string(),
            "-api-version".to_string(),
            "2.4".to_string(),
            "-language-version".to_string(),
            "2.4".to_string(),
            "-jvm-target".to_string(),
            "21".to_string(),
            source.display().to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "krusty must accept the project's own flags: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(jar_entries(&jar).iter().any(|e| e == "demo/FKt.class"));
}

/// A failing compile must FAIL the action. A rule whose compiler exits 0 on a broken source
/// produces an empty jar and a green build.
#[test]
fn a_broken_source_fails_the_action() {
    let dir = workspace("broken");
    let source = dir.join("B.kt");
    std::fs::write(&source, "package demo\nfun broken(): Int = nope()\n").expect("write source");
    let output = run_with_param_file(
        &dir,
        &[
            "-d".to_string(),
            dir.join("broken.jar").display().to_string(),
            source.display().to_string(),
        ],
    );
    assert!(
        !output.status.success(),
        "a source that does not compile must exit non-zero, or bazel reports success"
    );
}

/// The PERSISTENT WORKER, driven exactly as bazel drives it: line-delimited JSON work requests on
/// stdin, one response per request on stdout, from a single reused process.
///
/// This is the end-to-end proof for `use_worker = True` — the translation unit tests in
/// `krusty_cli::worker` cover the argument surface, but only this exercises the real binary writing
/// the real declared outputs.
#[test]
fn the_persistent_worker_serves_intellijs_argument_surface() {
    let dir = workspace("worker");
    let source = dir.join("I.kt");
    std::fs::write(
        &source,
        "package demo\ninterface I { fun f(): String = \"f\" }\nclass C : I\nfun use(s: String): Int = s.length\n",
    )
    .expect("write source");
    let jar = dir.join("demo.jar");
    let abi = dir.join("demo.abi.jar");
    let cri = dir.join("demo.kotlinCriStorage");

    // Request 1: the options intellij-community actually builds with. Request 2: a target carrying
    // Java, which must be refused WITHOUT ending the worker — so a third request still gets served.
    let requests = format!(
        concat!(
            r#"{{"arguments":["--target_label","//demo:demo","--kotlin_module_name","intellij.demo","#,
            r#""--jvm_default","no-compatibility","--x_lambdas","indy","--x_sam_conversions","indy","#,
            r#""--x_no_param_assertions","--x_no_call_assertions","--progressive","--warn","off","#,
            r#""--srcs","{src}","--out","{jar}","--abi-out","{abi}","--kotlin-cri-out","{cri}","#,
            r#""--java-count","0"],"requestId":1}}"#,
            "\n",
            r#"{{"arguments":["--srcs","{src}","--out","{jar}","--java-count","4"],"requestId":2}}"#,
            "\n",
            r#"{{"arguments":["--srcs","{src}","--out","{jar}","--jvm_default","disable"],"requestId":3}}"#,
            "\n"
        ),
        src = source.display(),
        jar = jar.display(),
        abi = abi.display(),
        cri = cri.display()
    );

    let mut child = std::process::Command::new(common::krusty_binary())
        .arg("--persistent_worker")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start worker");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("worker stdin")
        .write_all(requests.as_bytes())
        .expect("send requests");
    let output = child.wait_with_output().expect("worker exit");
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 3, "one response per request: {text}");

    assert!(
        lines[0].contains("\"exitCode\":0") && lines[0].contains("\"requestId\":1"),
        "the real argument surface must compile: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("\"exitCode\":1") && lines[1].contains("Java front end"),
        "a Java-carrying target is refused: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("\"exitCode\":1") && lines[2].contains("--jvm_default disable"),
        "an unemittable shape is refused, and the worker was still alive to say so: {}",
        lines[2]
    );

    // Every DECLARED output exists, or bazel fails the action.
    for declared in [&jar, &abi, &cri] {
        assert!(declared.exists(), "{} must be written", declared.display());
    }
    // `--jvm_default no-compatibility` reached the emitter: no compatibility holder in the jar.
    let entries = jar_entries(&jar);
    assert!(
        entries.iter().any(|entry| entry == "demo/I.class"),
        "{entries:?}"
    );
    assert!(
        !entries.iter().any(|entry| entry.contains("DefaultImpls")),
        "no-compatibility emits no $DefaultImpls: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry == "META-INF/intellij.demo.kotlin_module"),
        "--kotlin_module_name reached the compiler: {entries:?}"
    );
}
