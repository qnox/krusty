//! The real host: kotlin-compiler-embeddable 2.4.20 from Maven Central in a JVM, loading the real
//! power-assert plugin jar. Ignored by default because it downloads; CI runs it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use krusty_plugin_host::{
    provision, provision_artifact, ArtifactPin, Curl, Environment, Host, HostError, HostRuntime,
    LoadedPlugin, Pins, Request,
};
use sha2::{Digest, Sha256};

const POWER_ASSERT_SHA256: &str =
    "ad1b0b93d2ebad6f02e1682cd3dd4f81c5f666902aa5a4d3e3069e1101ed0bfc";

/// Provisioned once per test process, so parallel tests do not download the same jars side by side.
fn runtime() -> (HostRuntime, Environment) {
    static RUNTIME: OnceLock<(HostRuntime, Environment)> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let env = Environment::from_process().expect("a cache directory");
            let request = Request {
                kotlin_version: "2.4.20",
                reason: "the plugin host test",
                min_java: 8,
            };
            let pins = Pins::builtin(&env.platform);
            let runtime = provision(&request, &env, &pins, &Curl, &mut std::io::stderr())
                .unwrap_or_else(|error| panic!("{error}"));
            (runtime, env)
        })
        .clone()
}

/// The power-assert jar, provisioned like any pinned jar (verified, cached and published by an
/// atomic rename) and at most once per test process.
fn power_assert() -> String {
    static JAR: OnceLock<String> = OnceLock::new();
    JAR.get_or_init(|| {
        let (_, env) = runtime();
        let pin = ArtifactPin {
            group: "org.jetbrains.kotlin".into(),
            artifact: "kotlin-power-assert-compiler-plugin-embeddable".into(),
            version: "2.4.20".into(),
            sha256: POWER_ASSERT_SHA256.into(),
            size: 420_766,
        };
        let jar = provision_artifact(
            &pin,
            "the plugin host test",
            &env,
            &Curl,
            &mut std::io::stderr(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        jar.to_string_lossy().into_owned()
    })
    .clone()
}

fn digest(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        })
        .unwrap_or_default()
}

/// A cache of its own under the test target directory, removed when dropped.
struct ScratchCache(PathBuf);

impl ScratchCache {
    fn new(name: &str) -> Self {
        ScratchCache(
            PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("plugin-host-{name}-{}", std::process::id())),
        )
    }
}

impl Drop for ScratchCache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn started(runtime: &HostRuntime, env: &Environment) -> (Result<(), HostError>, String) {
    let mut report = Vec::new();
    let result = Host::start(runtime, env, &mut report).map(drop);
    (result, String::from_utf8(report).unwrap())
}

const COMPILING: &str =
    "info: compiling krusty's plugin host for kotlinc 2.4.20, once; later builds reuse it\n";

fn start(runtime: &HostRuntime, env: &Environment) -> Host {
    Host::start(runtime, env, &mut std::io::stderr()).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20 and power-assert"]
fn power_assert_loads_with_its_options_and_registers_its_extensions() {
    let (runtime, env) = runtime();
    let mut host = start(&runtime, &env);
    let plugins = host
        .load(
            &[power_assert()],
            &["plugin:org.jetbrains.kotlin.powerassert:function=kotlin.require".into()],
        )
        .unwrap();
    assert_eq!(
        plugins,
        [LoadedPlugin {
            registrar: "org.jetbrains.kotlin.powerassert.PowerAssertCompilerPluginRegistrar".into(),
            plugin_id: "org.jetbrains.kotlin.powerassert".into(),
            supports_k2: true,
            extensions: vec![
                ("org.jetbrains.kotlin.irGenerationExtension".into(), 1),
                (
                    "org.jetbrains.kotlin.fir.extensions.firExtensionRegistrar".into(),
                    1
                ),
            ],
        }]
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20 and power-assert"]
fn an_option_the_plugin_does_not_declare_fails_with_kotlincs_message() {
    let (runtime, env) = runtime();
    let mut host = start(&runtime, &env);
    let error = host
        .load(
            &[power_assert()],
            &["plugin:org.jetbrains.kotlin.powerassert:nope=x".into()],
        )
        .unwrap_err();
    assert_eq!(
        error,
        HostError::Plugin {
            message: "Unsupported plugin option: org.jetbrains.kotlin.powerassert:nope=x".into()
        }
    );
    // The host survives a refused request.
    assert_eq!(host.load(&[], &[]).unwrap(), []);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_classpath_entry_that_declares_no_registrar_loads_no_plugin() {
    let (runtime, env) = runtime();
    let stdlib = runtime
        .compiler_classpath
        .iter()
        .find(|jar| jar.ends_with("kotlin-stdlib-2.4.20.jar"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut host = start(&runtime, &env);
    assert_eq!(host.load(&[stdlib], &[]).unwrap(), []);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_plugin_classpath_entry_that_does_not_exist_is_refused() {
    let (runtime, env) = runtime();
    let missing = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("plugin-host-missing.jar")
        .to_string_lossy()
        .into_owned();
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.load(std::slice::from_ref(&missing), &[]).unwrap_err(),
        HostError::Plugin {
            message: format!("plugin classpath entry {missing} does not exist")
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_plugin_classpath_entry_that_is_not_a_jar_is_refused() {
    let (runtime, env) = runtime();
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-not-a-jar-{}.jar", std::process::id()));
    std::fs::write(&path, "not a jar").unwrap();
    let entry = path.to_string_lossy().into_owned();
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.load(std::slice::from_ref(&entry), &[]).unwrap_err(),
        HostError::Plugin {
            message: format!(
                "plugin classpath entry {entry} is not a jar: zip END header not found"
            )
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_plugin_declaring_a_legacy_component_registrar_is_refused() {
    use std::io::Write;
    let (runtime, env) = runtime();
    let jar = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-legacy-{}.jar", std::process::id()));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
    zip.start_file(
        "META-INF/services/org.jetbrains.kotlin.compiler.plugin.ComponentRegistrar",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"# the K1 entry point\nexample.LegacyRegistrar\n")
        .unwrap();
    zip.finish().unwrap();
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.load(&[jar.to_string_lossy().into_owned()], &[])
            .unwrap_err(),
        HostError::Plugin {
            message: "the plugin classpath declares the legacy ComponentRegistrar \
                      example.LegacyRegistrar; the plugin host loads CompilerPluginRegistrar \
                      plugins only"
                .into()
        }
    );
    assert_eq!(host.load(&[], &[]).unwrap(), []);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn the_adapter_is_compiled_once_and_then_reused_silently() {
    let (runtime, mut env) = runtime();
    let cache = ScratchCache::new("adapter");
    env.cache_dir = cache.0.clone();
    assert_eq!(started(&runtime, &env), (Ok(()), COMPILING.to_string()));
    assert_eq!(started(&runtime, &env), (Ok(()), String::new()));
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn an_adapter_built_against_other_pins_for_the_same_kotlin_version_is_not_reused() {
    let (runtime, mut env) = runtime();
    let cache = ScratchCache::new("repinned");
    env.cache_dir = cache.0.clone();
    assert_eq!(started(&runtime, &env), (Ok(()), COMPILING.to_string()));
    // The same Kotlin version and jars, but a corrected pin: what was built against the old
    // identity does not stand for the new one.
    let mut repinned = runtime.clone();
    repinned.compiler_identity = "0".repeat(64);
    assert_eq!(started(&repinned, &env), (Ok(()), COMPILING.to_string()));
    assert_eq!(started(&repinned, &env), (Ok(()), String::new()));
    let builds = std::fs::read_dir(cache.0.join("adapter")).unwrap().count();
    assert_eq!(builds, 2);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn an_altered_adapter_jar_is_rebuilt_rather_than_run() {
    let (runtime, mut env) = runtime();
    let cache = ScratchCache::new("altered");
    env.cache_dir = cache.0.clone();
    assert_eq!(started(&runtime, &env), (Ok(()), COMPILING.to_string()));
    let build = std::fs::read_dir(cache.0.join("adapter"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(
        build.join("krusty-plugin-host.jar"),
        b"not the jar that was built",
    )
    .unwrap();
    assert_eq!(started(&runtime, &env), (Ok(()), COMPILING.to_string()));
    assert_eq!(started(&runtime, &env), (Ok(()), String::new()));
}

#[test]
#[ignore = "downloads power-assert"]
fn parallel_tests_share_one_fetch_of_the_plugin_jar() {
    let jars: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4).map(|_| scope.spawn(power_assert)).collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    assert!(jars.iter().all(|jar| *jar == jars[0]), "{jars:?}");
    assert_eq!(digest(Path::new(&jars[0])), POWER_ASSERT_SHA256);
    let leftovers: Vec<_> = std::fs::read_dir(Path::new(&jars[0]).parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().contains(".partial-"))
        .collect();
    assert_eq!(leftovers, Vec::<std::ffi::OsString>::new());
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_host_on_another_kotlin_version_than_the_build_is_refused() {
    let (mut runtime, mut env) = runtime();
    // The 2.4.20 classpath, presented as if the build were 2.4.21. Its adapter goes to a cache of
    // its own, so no later build finds it under 2.4.21.
    runtime.kotlin_version = "2.4.21".into();
    let cache = ScratchCache::new("mislabeled");
    env.cache_dir = cache.0.clone();
    let error = Host::start(&runtime, &env, &mut Vec::new()).err();
    assert_eq!(
        error,
        Some(HostError::KotlinVersion {
            host: "2.4.20".into(),
            build: "2.4.21".into()
        })
    );
}

/// The lifecycle fixture's classes (`tests/fixtures/Registrars.kt`), compiled once per test process
/// by the runtime's own compiler.
fn fixture_classes() -> PathBuf {
    static CLASSES: OnceLock<PathBuf> = OnceLock::new();
    CLASSES
        .get_or_init(|| {
            let (runtime, _) = runtime();
            let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("plugin-host-fixture-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&classes);
            let classpath = std::env::join_paths(&runtime.compiler_classpath).unwrap();
            let output = std::process::Command::new(&runtime.java)
                .arg("-cp")
                .arg(&classpath)
                .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
                .args([
                    "-no-stdlib",
                    "-no-reflect",
                    "-jvm-target",
                    "1.8",
                    "-classpath",
                ])
                .arg(&classpath)
                .arg("-d")
                .arg(&classes)
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/Registrars.kt"
                ))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            classes
        })
        .clone()
}

/// A plugin jar of the fixture classes that declares `registrars`, in order, and the fixture's
/// option processor.
fn fixture_jar(name: &str, registrars: &[&str]) -> String {
    use std::io::Write;
    let classes = fixture_classes();
    let jar = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-{name}-{}.jar", std::process::id()));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for class in std::fs::read_dir(classes.join("fixture")).unwrap() {
        let class = class.unwrap();
        zip.start_file(
            format!("fixture/{}", class.file_name().to_string_lossy()),
            options,
        )
        .unwrap();
        zip.write_all(&std::fs::read(class.path()).unwrap())
            .unwrap();
    }
    let services = [
        (
            "org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor",
            "fixture.Options\n".to_string(),
        ),
        (
            "org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar",
            registrars
                .iter()
                .map(|class| format!("{class}\n"))
                .collect(),
        ),
    ];
    for (service, text) in services {
        zip.start_file(format!("META-INF/services/{service}"), options)
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    jar.to_string_lossy().into_owned()
}

/// Where the fixture records disposals, and the `-P` value that tells it.
fn marker(name: &str) -> (PathBuf, String) {
    let marker = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "plugin-host-{name}-{}.disposed",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&marker);
    let option = format!("plugin:fixture:marker={}", marker.display());
    (marker, option)
}

fn disposed(marker: &Path) -> String {
    std::fs::read_to_string(marker).unwrap_or_default()
}

/// Whether the host process holds `jar` open.
#[cfg(target_os = "linux")]
fn holds_open(host: &Host, jar: &str) -> bool {
    std::fs::read_dir(format!("/proc/{}/fd", host.id()))
        .unwrap()
        .filter_map(|fd| std::fs::read_link(fd.unwrap().path()).ok())
        .any(|target| target == Path::new(jar))
}

fn ir_extensions(count: u32) -> Vec<(String, u32)> {
    vec![("org.jetbrains.kotlin.irGenerationExtension".into(), count)]
}

fn fixture_plugin(registrar: &str, plugin_id: &str, irs: u32) -> LoadedPlugin {
    LoadedPlugin {
        registrar: registrar.into(),
        plugin_id: plugin_id.into(),
        supports_k2: true,
        extensions: ir_extensions(irs),
    }
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn registrars_share_one_storage_and_each_reports_what_it_added() {
    let (runtime, env) = runtime();
    let jar = fixture_jar("shared", &["fixture.First", "fixture.Second"]);
    let (_marker, option) = marker("shared");
    let mut host = start(&runtime, &env);
    // Second registers one more IR extension than it finds, so 2 means it saw First's.
    assert_eq!(
        host.load(&[jar], &[option]).unwrap(),
        [
            fixture_plugin("fixture.First", "fixture.first", 1),
            fixture_plugin("fixture.Second", "fixture.second", 2),
        ]
    );
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn unloading_disposes_the_registrars_resources_and_closes_the_jars() {
    let (runtime, env) = runtime();
    let jar = fixture_jar("unload", &["fixture.First", "fixture.Second"]);
    let (marker, option) = marker("unload");
    let mut host = start(&runtime, &env);
    host.load(std::slice::from_ref(&jar), &[option]).unwrap();
    assert!(holds_open(&host, &jar));
    assert_eq!(disposed(&marker), "");
    host.unload().unwrap();
    // kotlinc's Disposer disposes the last registered first.
    assert_eq!(disposed(&marker), "second disposed\nfirst disposed\n");
    assert!(!holds_open(&host, &jar));
    host.unload().unwrap();
    assert_eq!(disposed(&marker), "second disposed\nfirst disposed\n");
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_failing_registrar_leaves_nothing_loaded_and_nothing_open() {
    let (runtime, env) = runtime();
    let jar = fixture_jar("failing", &["fixture.First", "fixture.Failing"]);
    let (marker, option) = marker("failing");
    let mut host = start(&runtime, &env);
    let error = host
        .load(std::slice::from_ref(&jar), &[option])
        .unwrap_err();
    assert_eq!(
        error,
        HostError::Plugin {
            message: "java.lang.IllegalStateException: the failing fixture registrar fails (at \
                      fixture.Failing.registerExtensions(Registrars.kt:69))"
                .into()
        }
    );
    assert_eq!(disposed(&marker), "failing disposed\nfirst disposed\n");
    assert!(!holds_open(&host, &jar));
    assert_eq!(host.load(&[], &[]).unwrap(), []);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn loading_again_disposes_what_was_loaded_before() {
    let (runtime, env) = runtime();
    let jar = fixture_jar("reload", &["fixture.First"]);
    let (marker, option) = marker("reload");
    let mut host = start(&runtime, &env);
    host.load(std::slice::from_ref(&jar), std::slice::from_ref(&option))
        .unwrap();
    assert_eq!(
        host.load(&[jar], &[option]).unwrap(),
        [fixture_plugin("fixture.First", "fixture.first", 1)]
    );
    assert_eq!(disposed(&marker), "first disposed\n");
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn stopping_the_host_disposes_what_is_loaded() {
    let (runtime, env) = runtime();
    let jar = fixture_jar("stop", &["fixture.First", "fixture.Second"]);
    let (marker, option) = marker("stop");
    let mut host = start(&runtime, &env);
    host.load(&[jar], &[option]).unwrap();
    drop(host);
    assert_eq!(disposed(&marker), "second disposed\nfirst disposed\n");
}

/// Every Kotlin version krusty supports provisions, compiles the adapter and starts a host.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable for every supported Kotlin version"]
fn every_supported_kotlin_version_starts_a_host() {
    let env = Environment::from_process().expect("a cache directory");
    let versions = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../kotlin-versions"
    ))
    .unwrap();
    let versions: Vec<&str> = versions
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert!(!versions.is_empty());
    let pins = Pins::builtin(&env.platform);
    for version in versions {
        let request = Request {
            kotlin_version: version,
            reason: "the plugin host test",
            min_java: 8,
        };
        let runtime = provision(&request, &env, &pins, &Curl, &mut std::io::stderr())
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        let mut host = Host::start(&runtime, &env, &mut std::io::stderr())
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        assert_eq!(host.load(&[], &[]), Ok(Vec::new()), "{version}");
    }
}
