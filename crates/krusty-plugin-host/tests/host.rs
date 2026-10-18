//! The real host: kotlin-compiler-embeddable 2.4.20 from Maven Central in a JVM, loading the real
//! power-assert plugin jar. Ignored by default because it downloads; CI runs it.

use std::path::PathBuf;
use std::sync::OnceLock;

use krusty_plugin_host::{
    provision, Curl, Environment, Fetcher, Host, HostError, HostRuntime, LoadedPlugin, Pins,
    Request,
};
use sha2::{Digest, Sha256};

const POWER_ASSERT: &str = "kotlin-power-assert-compiler-plugin-embeddable-2.4.20.jar";
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

fn power_assert() -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugin-host");
    let jar = dir.join(POWER_ASSERT);
    let digest = |jar: &PathBuf| {
        std::fs::read(jar)
            .map(|bytes| {
                Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    if digest(&jar) != POWER_ASSERT_SHA256 {
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!(
            "https://repo1.maven.org/maven2/org/jetbrains/kotlin/\
             kotlin-power-assert-compiler-plugin-embeddable/2.4.20/{POWER_ASSERT}"
        );
        Curl.fetch(&url, &jar).unwrap();
        assert_eq!(digest(&jar), POWER_ASSERT_SHA256);
    }
    jar.to_string_lossy().into_owned()
}

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
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-adapter-{}", std::process::id()));
    env.cache_dir = cache.clone();
    let mut report = Vec::new();
    let first = Host::start(&runtime, &env, &mut report).map(drop);
    let mut again = Vec::new();
    let second = Host::start(&runtime, &env, &mut again).map(drop);
    let _ = std::fs::remove_dir_all(&cache);
    assert_eq!((first, second), (Ok(()), Ok(())));
    assert_eq!(
        String::from_utf8(report).unwrap(),
        "info: compiling krusty's plugin host for kotlinc 2.4.20, once; later builds reuse it\n"
    );
    assert_eq!(String::from_utf8(again).unwrap(), "");
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_host_on_another_kotlin_version_than_the_build_is_refused() {
    let (mut runtime, mut env) = runtime();
    // The 2.4.20 classpath, presented as if the build were 2.4.21. Its adapter goes to a cache of
    // its own, so no later build finds it under 2.4.21.
    runtime.kotlin_version = "2.4.21".into();
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-mislabeled-{}", std::process::id()));
    env.cache_dir = cache.clone();
    let error = Host::start(&runtime, &env, &mut Vec::new()).err();
    let _ = std::fs::remove_dir_all(&cache);
    assert_eq!(
        error,
        Some(HostError::KotlinVersion {
            host: "2.4.20".into(),
            build: "2.4.21".into()
        })
    );
}
