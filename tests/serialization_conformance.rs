//! Serialization conformance — diagnostics + the executable spec of the remaining work.
//!
//! STATUS: the **encode** round-trip is GREEN — `tests/serialization_roundtrip_e2e.rs` compiles
//! `@Serializable Foo` with krusty (plugin emits a functional `$serializer`), a real-`kotlinc` driver
//! runs `Json.encodeToString(Foo.serializer(), Foo(1,"x"))` against the published runtime, and the
//! JSON is correct. The serializer currently implements: the descriptor (built in `<init>` via
//! `PluginGeneratedSerialDescriptor` + `addElement`), `getDescriptor`, and `serialize` (drives the
//! `CompositeEncoder`). `deserialize` is a default-construct stub and `childSerializers` a null stub
//! (both honestly scoped — encode-only; neither is consulted on the encode path).
//!
//! This file holds: the emit diagnostic (`serializer_object_emits_wellformed_bytecode`), the
//! ctor-null-arg test, and a guard that a HAND-WRITTEN serializer still fails to compile (the
//! source-path gaps — object self-ref FIXED, ctor-null FIXED; remaining: `Json` companion/reified
//! resolution + `run{}`), plus an `#[ignore]`d *pure-krusty* round-trip spec that needs those
//! source-path gaps closed. The real working round-trip is the split-compilation one in
//! `serialization_roundtrip_e2e`.
//!
//! Remaining for full conformance: real `deserialize` (decode state machine), nullable/nested/richer
//! types + real `childSerializers`, and the language features the 69-case `testData/boxIr` corpus
//! needs.

use std::path::PathBuf;
use std::process::Command;

use super::common;

/// Gap #7 emit diagnostic: lower a real `@Serializable Foo`, run the serialization plugin, then run
/// krusty's ACTUAL emitter over the result — does it produce a well-formed `Foo$serializer.class`?
/// This isolates whether the emitter can emit an `object` implementing the generic `KSerializer`
/// interface with bridges (the crux of finishing serialization conformance).
#[test]
fn serializer_object_emits_wellformed_bytecode() {
    let Some((core, json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in local cache");
        return;
    };
    let Some(jimage) = jimage() else {
        eprintln!("skipping: no JAVA_HOME/lib/modules");
        return;
    };
    let src = "import kotlinx.serialization.Serializable\n@Serializable class Foo(val a: Int, val b: String)";
    let classes =
        common::compile_in_process(src, "Foo", &[std, core, json], Some(jimage.as_path()))
            .expect("production compiler emits Foo + Foo$serializer");
    let ser = classes
        .iter()
        .find(|(n, _)| n.contains("Foo$$serializer"))
        .unwrap_or_else(|| {
            panic!(
                "no Foo$$serializer emitted; got {:?}",
                classes.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });

    if let Err(error) = krusty::jvm::classreader::parse_class(&ser.1) {
        let out = std::env::temp_dir().join(format!("krusty_seremit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let path = out.join("Foo$$serializer.class");
        std::fs::write(&path, &ser.1).unwrap();
        let listing = common::javap(&["-p", &path.to_string_lossy()])
            .unwrap_or_else(|| "(javap unavailable)".to_string());
        panic!("emitted Foo$$serializer.class is malformed: {error:?}\n{listing}");
    }
}

fn runtime_jars() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let jars = krusty::toolchain::serialization_core_jar()
        .zip(krusty::toolchain::serialization_json_jar())
        .map(|(core, json)| (core, json, common::stdlib_jar()));
    if jars.is_none() && std::env::var_os("KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE").is_some() {
        panic!(
            "required pinned serialization runtime {} could not be provisioned; check network access or KRUSTY_DEPS_CACHE",
            krusty::toolchain::SERIALIZATION_VERSION
        );
    }
    jars
}

fn krusty_binary() -> PathBuf {
    common::krusty_binary()
}

/// `-Xplugin=` naming the reference distribution's serialization plugin: krusty synthesizes
/// serializers only when a build requests the plugin, as kotlinc does.
fn serialization_plugin_switch() -> String {
    let jar = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join("kotlinx-serialization-compiler-plugin.jar");
    format!("-Xplugin={}", jar.display())
}

/// Compile `src` with the krusty binary against `cp`; returns (ok, stderr).
fn krusty_compile(src: &str, cp: &str, out: &str) -> (bool, String) {
    let bin = krusty_binary();
    if !bin.exists() {
        return (false, "krusty binary not built".into());
    }
    let o = Command::new(bin)
        .args([&serialization_plugin_switch(), "-cp", cp, "-d", out, src])
        .output()
        .expect("run krusty");
    (
        o.status.success() && PathBuf::from(out).join("Foo.class").exists(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// The JDK `lib/modules` jimage (JDK classpath), from `JAVA_HOME`. krusty resolves `java.*` via it.
fn jimage() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("JAVA_HOME").ok()?).join("lib/modules");
    p.exists().then_some(p)
}

/// Plugin wired into the real compile path: the krusty binary compiling `@Serializable Foo` emits
/// the `Foo$$serializer` class.
#[test]
fn binary_compiles_serializable_and_emits_serializer() {
    let Some((core, json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in cache");
        return;
    };
    let Some(jimage) = jimage() else {
        eprintln!("skipping: no JAVA_HOME/lib/modules");
        return;
    };
    let bin = krusty_binary();
    if !bin.exists() {
        eprintln!("skipping: krusty binary not built");
        return;
    }
    let out = std::env::temp_dir().join(format!("krusty_binser_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let src = out.join("Foo.kt");
    std::fs::write(
        &src,
        "import kotlinx.serialization.Serializable\n@Serializable class Foo(val a: Int, val b: String)\n",
    )
    .unwrap();
    let cp = format!(
        "{}:{}:{}:{}",
        core.display(),
        json.display(),
        std.display(),
        jimage.display()
    );
    let o = Command::new(&bin)
        .args([&serialization_plugin_switch(), "-cp", &cp, "-d"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run krusty");
    assert!(
        out.join("Foo.class").exists() && out.join("Foo$$serializer.class").exists(),
        "krusty binary must emit Foo.class + Foo$$serializer.class; classpath: {cp}; stderr:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A null argument selects the matching public classpath constructor overload.
#[test]
fn classpath_ctor_with_null_arg_resolves() {
    let Some((core, _json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in local cache");
        return;
    };
    let Some(jimage) = jimage() else {
        eprintln!("skipping: no JAVA_HOME/lib/modules");
        return;
    };
    let bin = krusty_binary();
    if !bin.exists() {
        eprintln!("skipping: krusty binary not built");
        return;
    }
    let out = std::env::temp_dir().join(format!("krusty_ctornull_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let src = out.join("Gap2.kt");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        &src,
        "import kotlinx.serialization.SerializationException\n\
         fun build(): Int {\n\
         \x20   val e = SerializationException(\"Foo\", null)\n\
         \x20   return e.message?.length ?: 0\n\
         }\n",
    )
    .unwrap();
    let cp = format!("{}:{}:{}", core.display(), std.display(), jimage.display());
    let o = Command::new(&bin)
        .args(["-cp", &cp, "-d"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run krusty");
    assert!(
        out.join("Gap2Kt.class").exists(),
        "SerializationException(message, null) must compile; classpath: {cp}; stderr:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The real conformance round-trip. IGNORED until the documented blockers close; remove `#[ignore]`
/// (or set `KRUSTY_SER_CONFORMANCE=1`) to run it. Kept compiling so it can't bit-rot.
#[test]
#[ignore = "blocked by 3 core compiler gaps + real serializer bodies — see module docs"]
fn serializable_class_round_trips_through_real_runtime() {
    let Some((core, json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in local cache");
        return;
    };
    let cp = format!("{}:{}:{}", core.display(), json.display(), std.display());
    let src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serialization/SerBox.kt");
    let out = std::env::temp_dir().join(format!("krusty_ser_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();

    let (ok, err) = krusty_compile(src.to_str().unwrap(), &cp, out.to_str().unwrap());
    assert!(
        ok,
        "krusty must compile the @Serializable round-trip; stderr:\n{err}"
    );

    // Run box() on the JVM with the real runtime; expect "OK".
    let java = std::env::var("KSP_E2E_JDK")
        .map(|j| PathBuf::from(j).join("bin/java"))
        .unwrap_or_else(|_| PathBuf::from("java"));
    let run = Command::new(java)
        .args(["-cp", &format!("{}:{}", out.display(), cp)])
        .arg("SerBoxKt")
        .output()
        .expect("run box");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("OK"), "box() must return OK; got: {stdout}");
}

/// Non-ignored guard: documents that krusty currently CANNOT compile a hand-written serializer, and
/// pins the exact blocker set so the gap is tracked (this test flips to a failure — prompting its own
/// removal — once the compiler gaps close and the manual serializer compiles).
#[test]
fn manual_serializer_blockers_are_still_present() {
    let Some((core, json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in local cache");
        return;
    };
    let cp = format!("{}:{}:{}", core.display(), json.display(), std.display());
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/serialization/ManualSerializer.kt");
    let bin = krusty_binary();
    if !bin.exists() {
        eprintln!("skipping: krusty binary not built");
        return;
    }
    let out = std::env::temp_dir().join(format!("krusty_manualser_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let o = Command::new(bin)
        .args(["-cp", &cp, "-d"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run krusty");
    let err = String::from_utf8_lossy(&o.stderr);
    // krusty exits 0 even on diagnostics, so success == "emitted the class files". Today it does NOT
    // (the documented blockers). When it DOES, this assertion flips → prompting its removal and
    // enabling the ignored conformance round-trip above.
    let compiled = out.join("FooSer.class").exists();
    assert!(
        !compiled,
        "manual serializer now COMPILES — the serialization blockers are closed; enable the \
         ignored conformance round-trip test."
    );
    assert!(
        err.contains("error"),
        "expected diagnostics naming the blockers; got:\n{err}"
    );
}
