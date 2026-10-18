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
