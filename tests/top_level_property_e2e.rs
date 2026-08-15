//! Top-level `val`/`var` properties: static backing field + getter/setter on the file facade,
//! initialized in `<clinit>`. Compiled by krusty in-process; run on the pooled JVM; the metadata is
//! also consumed by the real kotlinc (a Kotlin importer of the properties).

use std::fs;

use super::common;

#[test]
fn top_level_properties_run_and_round_trip() {
    let root = common::scratch_dir().expect("scratch dir");
    let lib = root.join("lib");

    // Compile the property library in-process (warm classpath cache), not via a cold krusty CLI spawn.
    let lib_src = "package demo\nval greeting: String = \"hi\"\nvar counter: Int = 10\nfun bump(): Int { counter = counter + 1; return counter }\n";
    let jdk = common::jdk_modules();
    common::compile_to_dir(lib_src, "Lib", &[], Some(jdk.as_path()), &lib)
        .expect("krusty compiles top-level properties");

    // (1) Run via Java: getter + var mutation through the generated accessors — the IR backend emits
    // top-level `val`/`var` as Kotlin's `private static [final]` field + a `public static getX()`/
    // `setX()` accessor ABI, so a Java consumer compiles + links against the accessors (phase 398).
    // Driven through the persistent `javac_run` server (no cold `javac`/`java` per case).
    let main = "public class M { public static void main(String[] a) { System.out.println(demo.LibKt.getGreeting() + \":\" + demo.LibKt.bump() + \":\" + demo.LibKt.bump()); } }";
    let m_path = root.join("M.java");
    fs::write(&m_path, main).unwrap();
    let out = common::javac_run(
        m_path.to_str().unwrap(),
        lib.to_str().unwrap(),
        lib.to_str().unwrap(),
        "M",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(
        out.trim(),
        "hi:11:12",
        "javac/run against krusty's top-level property accessors"
    );
}

/// Reverse-direction interop: the REAL kotlinc imports krusty-compiled top-level properties via the
/// facade `@Metadata` `Package.property` records (name, return type, flags, and the
/// `JvmPropertySignature` naming the emitted accessors).
#[test]
fn kotlinc_consumes_krusty_top_level_property_metadata() {
    let root = common::scratch_dir().expect("scratch dir");
    let lib = root.join("lib");
    let lib_src = "package demo\nval greeting: String = \"hi\"\nvar counter: Int = 10\nfun bump(): Int { counter = counter + 1; return counter }\n";
    let jdk = common::jdk_modules();
    common::compile_to_dir(lib_src, "Lib", &[], Some(jdk.as_path()), &lib)
        .expect("krusty compiles top-level properties");
    fs::write(root.join("C.kt"), "import demo.greeting\nimport demo.counter\nfun main() {\n  counter = counter + 1\n  println(greeting + \":\" + counter)\n}\n").unwrap();
    let cout = root.join("cout");
    let args = vec![
        root.join("C.kt").to_string_lossy().into_owned(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        "-d".to_string(),
        cout.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect(
        "provisioned kotlinc server unavailable — run `just kotlinc \"$(just max-version)\"`",
    );
    assert_eq!(
        code, 0,
        "real kotlinc must consume krusty's top-level property @Metadata: {stderr}"
    );
    // Run the kotlinc-compiled consumer in the pooled JavaRunner (fresh statics per request, so the
    // `counter` mutation starts from 10 regardless of step 1).
    let driver = "public class M2 { public static void main(String[] a) { CKt.main(); } }";
    let m2 = root.join("M2.java");
    fs::write(&m2, driver).unwrap();
    let stdlib = common::stdlib_jar();
    let cp = format!(
        "{}:{}:{}",
        cout.to_string_lossy(),
        lib.to_string_lossy(),
        stdlib.to_string_lossy()
    );
    let out = common::javac_run(
        m2.to_str().unwrap(),
        &cp,
        root.join("m2out").to_string_lossy().as_ref(),
        "M2",
    )
    .expect("pooled JavaRunner unavailable");
    assert_eq!(out.trim(), "hi:11", "kotlinc-compiled consumer output");
}
