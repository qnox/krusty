//! What the tests of the real host share: one provisioned kotlinc 2.4.20 runtime per test process.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use krusty_plugin_host::{provision, Curl, Environment, Host, HostRuntime, Pins, Request};

/// Provisioned once per test process, so parallel tests do not download the same jars side by side.
pub fn runtime() -> (HostRuntime, Environment) {
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

pub fn start(runtime: &HostRuntime, env: &Environment) -> Host {
    Host::start(runtime, env, &mut std::io::stderr()).unwrap_or_else(|error| panic!("{error}"))
}

/// Compiles `sources` against `classpath` into `out` with the runtime's own kotlinc, passing it
/// `arguments` too, such as `-Xplugin`.
pub fn kotlinc(
    runtime: &HostRuntime,
    classpath: &[PathBuf],
    out: &Path,
    sources: &[&Path],
    arguments: &[&std::ffi::OsStr],
) {
    let compiler = std::env::join_paths(&runtime.compiler_classpath).unwrap();
    let output = std::process::Command::new(&runtime.java)
        .arg("-cp")
        .arg(&compiler)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args([
            "-no-stdlib",
            "-no-reflect",
            "-jvm-target",
            "1.8",
            "-classpath",
        ])
        .arg(std::env::join_paths(classpath).unwrap())
        .arg("-d")
        .arg(out)
        .args(arguments)
        .args(sources)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
