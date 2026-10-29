//! krusty's KSP host against real KSP2: a processor krusty compiles runs in the host over krusty's
//! round view of `fixtures/ksp/sources`, round after round while it generates files, and must
//! generate, log and see exactly what it does under KSP2 2.3.12 (`fixtures/ksp/ksp2.expected`).
//! Ignored by default because it downloads KSP's API and a JDK; CI runs it.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use krusty::jvm::classpath::Classpath;
use krusty::ksp_round::KspLibraries;
use krusty_plugin_host::{
    provision_ksp, Curl, Environment, GeneratedKind, HostError, KspHost, KspLoad, KspOutputs,
    KspRequest, KspRuntime, Pins,
};

const KSP_VERSION: &str = "2.3.12";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ksp")
}

fn runtime() -> (KspRuntime, Environment) {
    let env = Environment::from_process().expect("a cache directory");
    let request = KspRequest {
        ksp_version: KSP_VERSION,
        reason: "the KSP host test",
        min_java: 8,
    };
    let pins = Pins::builtin(&env.platform);
    let runtime = provision_ksp(&request, &env, &pins, &Curl, &mut std::io::stderr())
        .unwrap_or_else(|error| panic!("{error}"));
    (runtime, env)
}

/// A directory of its own under the test target directory, emptied first.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What krusty analyzes against: KSP's API, the standard library, the runtime's JDK and
/// `dependencies`.
fn classpath(runtime: &KspRuntime, dependencies: &[PathBuf]) -> Rc<Classpath> {
    let jdk = runtime.java_home.join("lib/modules");
    assert!(
        jdk.is_file(),
        "{} is not a JDK's module image",
        jdk.display()
    );
    let mut paths = runtime.api_classpath.clone();
    paths.push(jdk);
    paths.extend(dependencies.iter().cloned());
    Rc::new(Classpath::new(paths))
}

fn platform(runtime: &KspRuntime, dependencies: &[PathBuf]) -> krusty::frontend::PlatformProvider {
    let libraries = krusty::jvm::jvm_libraries::JvmLibraries::new(classpath(runtime, dependencies))
        .expect("the JVM provider");
    krusty::frontend::PlatformProvider::jvm(Box::new(libraries))
}

/// Compiles the processor `fixtures/ksp/<stem>.kt` with krusty into a classpath directory that
/// registers `provider`.
fn processor(runtime: &KspRuntime, stem: &str, provider: &str) -> PathBuf {
    processor_in(runtime, stem, provider, &format!("ksp-processor-{stem}"))
}

/// [`processor`], built into the scratch directory `directory`, so tests that run at the same time
/// can each build the same processor.
fn processor_in(runtime: &KspRuntime, stem: &str, provider: &str, directory: &str) -> PathBuf {
    let out = scratch(directory);
    compile(runtime, &fixtures().join(format!("{stem}.kt")), &out);
    let services = out.join("META-INF/services");
    std::fs::create_dir_all(&services).unwrap();
    std::fs::write(
        services.join("com.google.devtools.ksp.processing.SymbolProcessorProvider"),
        format!("{provider}\n"),
    )
    .unwrap();
    out
}

/// Compiles the Kotlin file `source` with krusty into the classpath directory `out`.
fn compile(runtime: &KspRuntime, source: &Path, out: &Path) {
    let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(source).unwrap();
    let inputs = [krusty::source::SourceInput::kotlin(&text).with_file_stem(&stem)];
    let stems = [stem.clone()];
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        platform(runtime, &[]),
        &krusty::features::LangFeatures::from_source(&text),
        &mut diags,
    );
    let backend = krusty::jvm::JvmBackend::new(classpath(runtime, &[]));
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let messages: Vec<&str> = diags.diags.iter().map(|diag| diag.msg.as_str()).collect();
    assert!(
        !diags.has_errors(),
        "krusty refused {stem}.kt: {messages:?}"
    );
    for (path, bytes) in artifacts {
        let file = out.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
}

/// What the processors generated, logged and wrote, in `ksp2.expected`'s form.
struct Run {
    generated: Vec<String>,
    log: Vec<String>,
}

/// Runs the processor on `classpath` in a host over `sources`, as a build driver does: a round over
/// the sources, then another over them and every Kotlin source the processors generated, while a
/// round generates any file.
fn run(
    runtime: &KspRuntime,
    env: &Environment,
    processor: &Path,
    sources: &[PathBuf],
    dependencies: &[PathBuf],
    outputs: &KspOutputs,
) -> Result<Run, HostError> {
    run_in_module(
        runtime,
        env,
        processor,
        sources,
        dependencies,
        outputs,
        "main",
    )
}

/// [`run`] for the module `module_name`.
fn run_in_module(
    runtime: &KspRuntime,
    env: &Environment,
    processor: &Path,
    sources: &[PathBuf],
    dependencies: &[PathBuf],
    outputs: &KspOutputs,
    module_name: &str,
) -> Result<Run, HostError> {
    let mut host = KspHost::start(runtime, env, &mut std::io::stderr())?;
    let providers = host.load(&KspLoad {
        processor_classpath: &[processor.to_path_buf()],
        options: &[],
        module_name,
        outputs,
        language_version: "2.3",
        api_version: "2.3",
        compiler_version: "2.3.20",
        ksp_version: KSP_VERSION,
        jvm_target: "17",
        jvm_default_mode: "disable",
        log_level: 3,
        warnings_as_errors: false,
    })?;
    assert_eq!(providers.len(), 1);
    let mut sources = sources.to_vec();
    let mut run = Run {
        generated: Vec::new(),
        log: Vec::new(),
    };
    let analysis = loop {
        let texts: Vec<String> = sources
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        let inputs: Vec<_> = texts
            .iter()
            .map(|text| krusty::source::SourceInput::kotlin(text))
            .collect();
        let mut diags = krusty::diag::DiagSink::new();
        let analysis = krusty::frontend::analyze_source_set_prefix_with_features(
            &inputs,
            0,
            inputs.len(),
            platform(runtime, dependencies),
            &krusty::features::LangFeatures::new(),
            &mut diags,
        );
        let paths: Vec<String> = sources
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let round = krusty::ksp_round::ksp_round(&analysis, &paths, module_name)
            .unwrap_or_else(|refusal| panic!("{refusal}"));
        let outcome = host.round(&round, &KspLibraries::new(&analysis))?;
        run.log.extend(outcome.log);
        if outcome.generated.is_empty() {
            break analysis;
        }
        for generated in outcome.generated {
            run.generated
                .push(relative(generated.kind, &generated.path, outputs));
            if generated.kind == GeneratedKind::Kotlin {
                sources.push(generated.path);
            }
        }
    };
    let outcome = host.finish(&KspLibraries::new(&analysis))?;
    run.log.extend(outcome.log);
    for generated in outcome.generated {
        run.generated
            .push(relative(generated.kind, &generated.path, outputs));
    }
    Ok(run)
}

fn relative(kind: GeneratedKind, path: &Path, outputs: &KspOutputs) -> String {
    let (name, root) = match kind {
        GeneratedKind::Kotlin => ("kotlin", &outputs.kotlin),
        GeneratedKind::Java => ("java", &outputs.java),
        GeneratedKind::Class => ("class", &outputs.classes),
        GeneratedKind::Resource => ("resource", &outputs.resources),
    };
    let relative = path.strip_prefix(root).unwrap_or(path);
    format!("{name} {}", relative.to_string_lossy())
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn a_processor_sees_generates_and_logs_what_it_does_under_ksp2() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Dump", "dump.DumpProvider");
    let out = scratch("ksp-outputs");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let source_dir = fixtures().join("sources");
    let sources = [source_dir.join("Sample.kt"), source_dir.join("Other.kt")];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    let mut actual: Vec<String> = run.generated;
    actual.extend(run.log);
    actual.push(std::fs::read_to_string(outputs.resources.join("dump.txt")).unwrap());
    let actual = actual
        .join("\n")
        .replace(&*source_dir.to_string_lossy(), "$SOURCES")
        .replace(&*outputs.kotlin.to_string_lossy(), "$OUTPUT");
    let expected: String = std::fs::read_to_string(fixtures().join("ksp2.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        std::fs::read_to_string(outputs.kotlin.join("gen/Generated.kt")).unwrap(),
        "package gen\n\n@sample.Marker\nclass Generated {\n    class Nested\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(outputs.resources.join("META-INF/services/dump.Service")).unwrap(),
        "dump.Implementation\n"
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn a_processor_deferring_symbols_is_refused_rather_than_dropping_them() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Defer", "defer.DeferProvider");
    let out = scratch("ksp-outputs-defer");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [
        fixtures().join("sources/Sample.kt"),
        fixtures().join("sources/Other.kt"),
    ];
    let error = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .err()
        .expect("the host refuses a deferring processor");
    assert_eq!(
        error.to_string(),
        "krusty's KSP host does not implement SymbolProcessor.process deferring symbols to a later round yet, which a processor uses"
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn primary_constructors_and_their_parameters_are_what_ksp2_reports() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Ctors", "ctors.CtorsProvider");
    let out = scratch("ksp-outputs-ctors");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [fixtures().join("constructors/Constructors.kt")];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("ctors.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn mapped_names_are_what_ksp2_reports() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Names", "names.NamesProvider");
    let out = scratch("ksp-outputs-names");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [fixtures().join("names/Source.kt")];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("names.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn jvm_signatures_are_what_ksp2_reports() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Signatures", "signatures.SignaturesProvider");
    let out = scratch("ksp-outputs-signatures");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [fixtures().join("signatures/Signatures.kt")];
    // A module name with punctuation, which an internal member's suffix makes a Java identifier.
    let run = run_in_module(
        &runtime,
        &env,
        &processor,
        &sources,
        &[],
        &outputs,
        "sig-module.core",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("signatures.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn class_headers_are_what_ksp2_reports() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Headers", "headers.HeadersProvider");
    let out = scratch("ksp-outputs-headers");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [fixtures().join("headers/Headers.kt")];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("headers.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn a_kept_sequence_reads_again_and_through_two_iterators_as_under_ksp2() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "Traverse", "traverse.TraverseProvider");
    let out = scratch("ksp-outputs-traverse");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [
        fixtures().join("sources/Sample.kt"),
        fixtures().join("companions/Companions.kt"),
        fixtures().join("sources/Other.kt"),
    ];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("traverse.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

/// Runs the Members processor over `sources` and compares its log with `expected`.
fn members_log_is_what_ksp2_reports(sources: &[PathBuf], scratch_name: &str, expected: &str) {
    log_is_what_ksp2_reports(
        "Members",
        "members.MembersProvider",
        sources,
        scratch_name,
        expected,
    );
}

/// Runs the processor `stem` (provided by `provider`) over `sources` and compares its log with
/// `expected`.
fn log_is_what_ksp2_reports(
    stem: &str,
    provider: &str,
    sources: &[PathBuf],
    scratch_name: &str,
    expected: &str,
) {
    log_with_dependencies_is_what_ksp2_reports(
        stem,
        provider,
        sources,
        &[],
        scratch_name,
        expected,
    );
}

/// Compiles the Java file `source` into `out` with the runtime JDK's `javac`.
fn javac(runtime: &KspRuntime, source: &Path, out: &Path) {
    let javac = runtime.java_home.join("bin").join("javac");
    let output = std::process::Command::new(&javac)
        .arg("-d")
        .arg(out)
        .arg(source)
        .output()
        .unwrap_or_else(|error| panic!("could not run {}: {error}", javac.display()));
    assert!(
        output.status.success(),
        "javac failed on {}: {}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// [`log_is_what_ksp2_reports`], with `dependencies` compiled onto the classpath: a Kotlin file by
/// krusty, a Java file by the runtime JDK's `javac`.
fn log_with_dependencies_is_what_ksp2_reports(
    stem: &str,
    provider: &str,
    sources: &[PathBuf],
    dependencies: &[PathBuf],
    scratch_name: &str,
    expected: &str,
) {
    let (runtime, env) = runtime();
    let dependencies: Vec<PathBuf> = match dependencies.is_empty() {
        true => Vec::new(),
        false => {
            let out = scratch(&format!("{scratch_name}-dependencies"));
            for dependency in dependencies {
                match dependency
                    .extension()
                    .and_then(|extension| extension.to_str())
                {
                    Some("java") => javac(&runtime, dependency, &out),
                    _ => compile(&runtime, dependency, &out),
                }
            }
            vec![out]
        }
    };
    let dependencies = dependencies.as_slice();
    let processor = processor_in(
        &runtime,
        stem,
        provider,
        &format!("{scratch_name}-processor"),
    );
    let out = scratch(scratch_name);
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let run = run(&runtime, &env, &processor, sources, dependencies, &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join(expected))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn members_and_their_types_are_what_ksp2_reports() {
    members_log_is_what_ksp2_reports(
        &[fixtures().join("members/Members.kt")],
        "ksp-outputs-members",
        "members.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn declarations_are_in_the_order_ksp2_lists_them() {
    members_log_is_what_ksp2_reports(
        &[fixtures().join("order/Order.kt")],
        "ksp-outputs-order",
        "order.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn members_sharing_a_name_are_listed_together_as_ksp2_lists_them() {
    members_log_is_what_ksp2_reports(
        &[fixtures().join("grouping/Grouping.kt")],
        "ksp-outputs-grouping",
        "grouping.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn annotation_arguments_and_accessors_are_what_ksp2_reports() {
    log_is_what_ksp2_reports(
        "Annotations",
        "annotations.AnnotationsProvider",
        &[
            fixtures().join("annotations/Aliased.kt"),
            fixtures().join("annotations/Annotations.kt"),
        ],
        "ksp-outputs-annotations",
        "annotations.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn library_classes_are_what_ksp2_reports() {
    log_with_dependencies_is_what_ksp2_reports(
        "Library",
        "library.LibraryProvider",
        &[fixtures().join("library/Library.kt")],
        &[fixtures().join("dependency/Dependency.kt")],
        "ksp-outputs-library",
        "library.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn library_annotation_members_are_what_ksp2_reports() {
    log_with_dependencies_is_what_ksp2_reports(
        "LibraryMembers",
        "librarymembers.LibraryMembersProvider",
        &[fixtures().join("library-members-source/Source.kt")],
        &[
            fixtures().join("library-members/Dependency.kt"),
            fixtures().join("library-members/Constants.java"),
            fixtures().join("library-members/Aliased.java"),
        ],
        "ksp-outputs-library-members",
        "library-members.expected",
    );
}

/// A type that names a typealias, as a whole or in a type argument, annotated or not, is one the
/// round leaves undescribed: KSP2 resolves it to the alias, keeping the alias's arguments (its log
/// shows `List<members.KeptString>`) and answering no annotations for any of these, but the round
/// does not describe a source alias type yet, so it refuses rather than report the expansion.
#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn types_naming_aliases_are_what_ksp2_reports() {
    members_log_is_what_ksp2_reports(
        &[fixtures().join("aliases/Aliases.kt")],
        "ksp-outputs-aliases",
        "aliases.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn a_type_naming_an_alias_applied_to_arguments_is_refused() {
    let (runtime, env) = runtime();
    let processor = processor_in(
        &runtime,
        "Members",
        "members.MembersProvider",
        "ksp-processor-members-alias-refusals",
    );
    let out = scratch("ksp-outputs-alias-refusals");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let error = run(
        &runtime,
        &env,
        &processor,
        &[fixtures().join("members-refused/GenericAlias.kt")],
        &[],
        &outputs,
    )
    .err()
    .expect("the host refuses a type naming an alias applied to arguments");
    assert_eq!(
        error.to_string(),
        "krusty's KSP host does not implement KSPropertyDeclaration.type of a type it cannot describe yet, which a processor uses"
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn getter_return_types_are_what_ksp2_reports() {
    log_is_what_ksp2_reports(
        "Accessors",
        "accessors.AccessorsProvider",
        &[fixtures().join("accessors/Accessors.kt")],
        "ksp-outputs-accessors",
        "accessors.expected",
    );
}

/// A getter that writes its own return type (`get(): Long`) is refused: KSP2 serves that written
/// reference, which the round does not describe yet.
#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn the_return_type_a_getter_writes_is_refused() {
    let (runtime, env) = runtime();
    let processor = processor_in(
        &runtime,
        "Accessors",
        "accessors.AccessorsProvider",
        "ksp-processor-accessors-refusals",
    );
    let out = scratch("ksp-outputs-accessors-refused");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let error = run(
        &runtime,
        &env,
        &processor,
        &[fixtures().join("accessors-refused/TypedGetter.kt")],
        &[],
        &outputs,
    )
    .err()
    .expect("the host refuses a getter's written return type");
    assert_eq!(
        error.to_string(),
        "krusty's KSP host does not implement KSPropertyGetter.returnType yet, which a processor uses"
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn effective_java_modifiers_are_what_ksp2_reports() {
    log_is_what_ksp2_reports(
        "JavaModifiers",
        "javamodifiers.JavaModifiersProvider",
        &[fixtures().join("java-modifiers/JavaModifiers.kt")],
        "ksp-outputs-java-modifiers",
        "java-modifiers.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn overridees_are_what_ksp2_reports() {
    log_is_what_ksp2_reports(
        "Overridees",
        "overridees.OverrideesProvider",
        &[fixtures().join("overridees/Overridees.kt")],
        "ksp-outputs-overridees",
        "overridees.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn the_overridee_of_an_override_is_refused() {
    let (runtime, env) = runtime();
    let processor = processor_in(
        &runtime,
        "Overridees",
        "overridees.OverrideesProvider",
        "ksp-processor-overridees-refusals",
    );
    for (fixture, query) in [
        (
            "OverridingFunction.kt",
            "KSFunctionDeclaration.findOverridee",
        ),
        (
            "OverridingProperty.kt",
            "KSPropertyDeclaration.findOverridee",
        ),
        ("DataComponent.kt", "KSFunctionDeclaration.findOverridee"),
    ] {
        let out = scratch(&format!("ksp-outputs-overridees-refused-{fixture}"));
        let outputs = KspOutputs {
            kotlin: out.join("kotlin"),
            java: out.join("java"),
            classes: out.join("classes"),
            resources: out.join("resources"),
        };
        let error = run(
            &runtime,
            &env,
            &processor,
            &[fixtures().join("overridees-refused").join(fixture)],
            &[],
            &outputs,
        )
        .err()
        .unwrap_or_else(|| panic!("the host refuses an overridee in {fixture}"));
        assert_eq!(
            error.to_string(),
            format!("krusty's KSP host does not implement {query} yet, which a processor uses"),
            "{fixture}"
        );
    }
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn class_literal_arguments_are_what_ksp2_reports() {
    log_with_dependencies_is_what_ksp2_reports(
        "Annotations",
        "annotations.AnnotationsProvider",
        &[fixtures().join("class-literals/Use.kt")],
        &[
            fixtures().join("class-literals/dependency/ClassDefaults.java"),
            fixtures().join("class-literals/dependency/KotlinDefaults.kt"),
        ],
        "ksp-outputs-class-literals",
        "class-literals.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn java_annotation_array_elements_default_to_empty_arrays() {
    log_with_dependencies_is_what_ksp2_reports(
        "Annotations",
        "annotations.AnnotationsProvider",
        &[fixtures().join("java-array-defaults/Use.kt")],
        &[
            fixtures().join("java-array-defaults/dependency/ArrayElements.java"),
            fixtures().join("java-array-defaults/dependency/Valued.java"),
        ],
        "ksp-outputs-java-array-defaults",
        "java-array-defaults.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn all_functions_and_properties_of_source_classifiers_are_what_ksp2_lists() {
    let (runtime, env) = runtime();
    let processor = processor(&runtime, "AllMembers", "allmembers.AllMembersProvider");
    let out = scratch("ksp-outputs-all-members");
    let outputs = KspOutputs {
        kotlin: out.join("kotlin"),
        java: out.join("java"),
        classes: out.join("classes"),
        resources: out.join("resources"),
    };
    let sources = [fixtures().join("all-members/AllMembers.kt")];
    let run = run(&runtime, &env, &processor, &sources, &[], &outputs)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(run.generated, Vec::<String>::new());
    let expected: Vec<String> = std::fs::read_to_string(fixtures().join("all-members.expected"))
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    assert_eq!(run.log, expected);
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn all_members_sharing_a_name_are_listed_together_as_ksp2_lists_them() {
    log_is_what_ksp2_reports(
        "AllMembers",
        "allmembers.AllMembersProvider",
        &[fixtures().join("all-members-grouping/Grouping.kt")],
        "ksp-outputs-all-members-grouping",
        "all-members-grouping.expected",
    );
}

#[test]
#[ignore = "downloads symbol-processing-api 2.3.12 and a JDK"]
fn all_functions_whose_order_krusty_cannot_say_are_refused() {
    let (runtime, env) = runtime();
    let processor = processor_in(
        &runtime,
        "AllMembers",
        "allmembers.AllMembersProvider",
        "ksp-processor-all-members-refusals",
    );
    for case in [
        "Intersection",
        "UnrelatedInterfaces",
        "SiblingOverrides",
        "Substitution",
        "EnumClass",
        "LibrarySupertype",
    ] {
        let out = scratch(&format!("ksp-outputs-all-members-{case}"));
        let outputs = KspOutputs {
            kotlin: out.join("kotlin"),
            java: out.join("java"),
            classes: out.join("classes"),
            resources: out.join("resources"),
        };
        let sources = [fixtures().join(format!("all-members-refused/{case}.kt"))];
        let error = run(&runtime, &env, &processor, &sources, &[], &outputs)
            .err()
            .unwrap_or_else(|| panic!("{case}: the host lists members it cannot order"));
        assert_eq!(
            error.to_string(),
            "krusty's KSP host does not implement KSClassDeclaration.getAllFunctions of a classifier whose members it cannot order yet, which a processor uses",
            "{case}"
        );
    }
}
