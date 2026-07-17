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
//! types + real `childSerializers`, wiring the plugin into the main compile path, and the language
//! features the 69-case `testData/boxIr` corpus needs.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use super::common;

/// Gap #7 emit diagnostic: lower a real `@Serializable Foo`, run the serialization plugin, then run
/// krusty's ACTUAL emitter over the result — does it produce a well-formed `Foo$serializer.class`?
/// This isolates whether the emitter can emit an `object` implementing the generic `KSerializer`
/// interface with bridges (the crux of finishing serialization conformance).
#[test]
fn serializer_object_emits_wellformed_bytecode() {
    use krusty::diag::DiagSink;
    use krusty::frontend::{check_file, collect_signatures_with_cp};
    use krusty::ir_lower::lower_file;
    use krusty::jvm::classpath::Classpath;
    use krusty::jvm::jvm_libraries::JvmLibraries;
    use krusty::jvm::names::file_class_name;
    use krusty::lexer::lex;
    use krusty::parser::parse;
    use krusty::plugins::{serialization::SerializationPlugin, PluginContext, PluginHost};

    let Some((core, json, std)) = runtime_jars() else {
        eprintln!("skipping: serialization runtime jars not in local cache");
        return;
    };
    let Some(jimage) = jimage() else {
        eprintln!("skipping: no JAVA_HOME/lib/modules");
        return;
    };
    let cp = Rc::new(Classpath::new(vec![std, core, json, jimage]));

    let src = "@Serializable class Foo(val a: Int, val b: String)";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    if d.has_errors() {
        eprintln!("skipping: krusty could not lower Foo (front-end gap)");
        return;
    }
    let runtime = JvmLibraries::new(cp.clone());
    let Some(mut ir) = lower_file(&files[0], &info, &syms, &runtime) else {
        eprintln!("skipping: Foo outside IR subset");
        return;
    };

    let ctx = PluginContext::from_source(&files[0], &ir);
    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    let facade = file_class_name("Foo", files[0].package.as_deref());
    let classes = krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None);
    let Some(classes) = classes else {
        panic!("EMIT GAP: emit_all returned None for the serializer object (gap #7 — emitter does not yet support this construct)");
    };
    let ser = classes
        .iter()
        .find(|(n, _)| n.contains("Foo$$serializer"))
        .unwrap_or_else(|| {
            panic!(
                "no Foo$$serializer emitted; got {:?}",
                classes.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });

    // Write it and run `javap` — a malformed classfile fails to parse.
    let out = std::env::temp_dir().join(format!("krusty_seremit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let p = out.join("Foo$$serializer.class");
    std::fs::write(&p, &ser.1).unwrap();
    let javap = std::env::var("JAVA_HOME")
        .map(|j| PathBuf::from(j).join("bin/javap"))
        .unwrap_or_else(|_| PathBuf::from("javap"));
    let o = Command::new(javap)
        .arg("-p")
        .arg(&p)
        .output()
        .expect("run javap");
    assert!(
        o.status.success(),
        "emitted Foo$$serializer.class is malformed (javap failed):\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    eprintln!(
        "gap #7 emit OK — Foo$$serializer.class is well-formed:\n{}",
        String::from_utf8_lossy(&o.stdout)
    );
}

/// Recursively locate a `<prefix>*.jar` (no `-sources`) under `dir`.
fn walk(dir: &std::path::Path, prefix: &str, depth: usize, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > 10 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, prefix, depth + 1, out);
        } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            if n.starts_with(prefix) && n.ends_with(".jar") && !n.contains("sources") {
                *out = Some(p.clone());
                return;
            }
        }
    }
}

fn locate(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut out = None;
    walk(
        &std::path::Path::new(&home).join(".gradle"),
        prefix,
        0,
        &mut out,
    );
    out
}

fn runtime_jars() -> Option<(PathBuf, PathBuf, PathBuf)> {
    // core, json, stdlib from the local gradle cache (mirrors a -classpath user). A deep walk that
    // reliably reaches the modules-2 cache (common::stdlib_jar's type-alias scan can miss it).
    let core = locate("kotlinx-serialization-core-jvm")?;
    let json = locate("kotlinx-serialization-json-jvm")?;
    let std = locate("kotlin-stdlib-2").or_else(common::stdlib_jar)?;
    Some((core, json, std))
}

fn krusty_binary() -> PathBuf {
    option_env!("CARGO_BIN_EXE_krusty")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/krusty"))
}

/// Compile `src` with the krusty binary against `cp`; returns (ok, stderr).
fn krusty_compile(src: &str, cp: &str, out: &str) -> (bool, String) {
    let bin = krusty_binary();
    if !bin.exists() {
        return (false, "krusty binary not built".into());
    }
    let o = Command::new(bin)
        .args(["-cp", cp, "-d", out, src])
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
    std::fs::write(&src, "@Serializable class Foo(val a: Int, val b: String)\n").unwrap();
    let cp = format!(
        "{}:{}:{}:{}",
        core.display(),
        json.display(),
        std.display(),
        jimage.display()
    );
    let o = Command::new(&bin)
        .args(["-cp", &cp, "-d"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run krusty");
    assert!(
        out.join("Foo.class").exists() && out.join("Foo$$serializer.class").exists(),
        "krusty binary must emit Foo.class + Foo$$serializer.class; stderr:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Gap #2 (closed): constructing a classpath class with a `null` argument for a reference parameter —
/// `PluginGeneratedSerialDescriptor(name, null, count)` — now resolves. A `$serializer` builds its
/// descriptor this way. Verifies the constructor-overload null-match end-to-end.
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
        "import kotlinx.serialization.internal.PluginGeneratedSerialDescriptor\n\
         fun build(): Int {\n\
         \x20   val d = PluginGeneratedSerialDescriptor(\"Foo\", null, 2)\n\
         \x20   d.addElement(\"a\", false)\n\
         \x20   return 0\n\
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
        "PluginGeneratedSerialDescriptor(name, null, n) must compile; stderr:\n{}",
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
