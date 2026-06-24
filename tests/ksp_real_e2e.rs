//! Real KSP **from a JAR**, end-to-end: provision the KSP2 toolchain, compile a real Kotlin
//! `SymbolProcessor` into a JAR, then run KSP2 (`KotlinSymbolProcessing.execute`) over Kotlin source
//! with that processor discovered via `ServiceLoader` — and assert it inspected the class and
//! generated code. This is the genuine article the codegen-host design (`docs/PLUGIN_API.md`)
//! orchestrates, exercising the KSP capability matrix:
//!   - from-jar discovery (ServiceLoader of `SymbolProcessorProvider`)
//!   - annotation query (`Resolver.getSymbolsWithAnnotation`)
//!   - declaration + property inspection (`KSClassDeclaration.getAllProperties`)
//!   - code generation (`CodeGenerator.createNewFile`)
//!
//! Heavy + network + JDK-sensitive (Kotlin 2.0.21's compiler rejects JDK >= 24), so it is OPT-IN:
//! set `KRUSTY_KSP_E2E=1`. It reuses `<repo>/target/cache/ksp-toolchain` (gitignored) across runs: the KSP jars
//! (provisioned via `krusty::plugins::deps`) and a JDK 21 (`KSP_E2E_JDK`, or an already-extracted
//! `jdk-21*`, else downloaded). Self-skips if prerequisites are missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::plugins::deps;

const KSP_VER: &str = "2.0.21-1.0.28";
const KOTLIN_VER: &str = "2.0.21";

fn run(cmd: &mut Command, what: &str) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("{what} failed:\n{}", String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            eprintln!("{what} could not start: {e}");
            false
        }
    }
}

/// A JDK whose `java` Kotlin 2.0.21 accepts (<= 23). Order: `KSP_E2E_JDK`, an extracted `jdk-21*`,
/// else download Temurin 21 into the toolchain dir.
fn jdk_home(tool: &Path) -> Option<PathBuf> {
    if let Ok(j) = std::env::var("KSP_E2E_JDK") {
        let p = PathBuf::from(j);
        if p.join("bin/java").exists() {
            return Some(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(tool) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("jdk-21"))
                && p.join("bin/java").exists()
            {
                return Some(p);
            }
        }
    }
    // Download Temurin 21.
    let tgz = tool.join("jdk21.tar.gz");
    let url = "https://api.adoptium.net/v3/binary/latest/21/ga/linux/x64/jdk/hotspot/normal/eclipse?project=jdk";
    if !run(
        Command::new("curl").args(["-sL", url, "-o"]).arg(&tgz),
        "download jdk21",
    ) {
        return None;
    }
    if !run(
        Command::new("tar").arg("xzf").arg(&tgz).current_dir(tool),
        "extract jdk21",
    ) {
        return None;
    }
    jdk_home(tool) // re-scan
}

fn classpath(libs: &[PathBuf]) -> String {
    libs.iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

#[test]
fn real_ksp_processor_from_jar_generates_code() {
    if std::env::var("KRUSTY_KSP_E2E").is_err() {
        eprintln!(
            "skipping: set KRUSTY_KSP_E2E=1 (heavy: KSP toolchain + JDK 21 + Kotlin compile)"
        );
        return;
    }
    let Some(resolver) = deps::detect() else {
        eprintln!("skipping: no gradle/mvn/cs resolver to provision the KSP toolchain");
        return;
    };

    let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/cache/ksp-toolchain");
    let libs_dir = tool.join("libs");

    // 1. Provision the KSP2 + kotlin-compiler closure (reused across runs).
    let libs: Vec<PathBuf> = if libs_dir
        .join(format!("symbol-processing-aa-embeddable-{KSP_VER}.jar"))
        .exists()
    {
        std::fs::read_dir(&libs_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jar"))
            .collect()
    } else {
        let coords = vec![
            format!("com.google.devtools.ksp:symbol-processing-aa-embeddable:{KSP_VER}"),
            format!("com.google.devtools.ksp:symbol-processing-api:{KSP_VER}"),
            format!("com.google.devtools.ksp:symbol-processing-common-deps:{KSP_VER}"),
            format!("org.jetbrains.kotlin:kotlin-compiler-embeddable:{KOTLIN_VER}"),
        ];
        match resolver.fetch(&coords, &libs_dir) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("skipping: provisioning failed: {e}");
                return;
            }
        }
    };
    assert!(!libs.is_empty(), "KSP toolchain jars provisioned");

    let Some(jdk) = jdk_home(&tool) else {
        eprintln!("skipping: no JDK 21 available (set KSP_E2E_JDK)");
        return;
    };
    let java = jdk.join("bin/java");
    let jar = jdk.join("bin/jar");
    let cc = classpath(&libs);
    let api = libs
        .iter()
        .find(|p| p.to_string_lossy().contains("symbol-processing-api"))
        .unwrap();
    let std_jar = libs
        .iter()
        .find(|p| p.to_string_lossy().contains("kotlin-stdlib-2"))
        .unwrap();

    // 2. Sources: a real KSP processor + provider (ServiceLoader-registered) and a consumer class.
    let work = tool.join("e2e");
    let _ = std::fs::remove_dir_all(&work);
    let w = |rel: &str, c: &str| {
        let p = work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    };
    w(
        "proc/demo/Builder.kt",
        include_str!("fixtures/ksp/Builder.kt"),
    );
    w(
        "proc/META-INF/services/com.google.devtools.ksp.processing.SymbolProcessorProvider",
        "demo.BuilderProvider\n",
    );
    w(
        "launch/Launcher.kt",
        include_str!("fixtures/ksp/Launcher.kt"),
    );
    w(
        "app/app/Foo.kt",
        "package app\n\
         open class Base\n\
         /**\n * Foo docs.\n */\n\
         @demo.Builder\n\
         class Foo(val a: Int, var b: String?) : Base() {\n\
         \x20   fun greet(x: Int, vararg rest: String, y: Int = 0): String = \"\"\n\
         \x20   companion object { const val K = 1 }\n\
         }\n",
    );

    let kotlinc = |out: &str, cp: &str, src: &str| -> bool {
        run(
            Command::new(&java)
                .arg("-cp")
                .arg(&cc)
                .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
                .args(["-d", &work.join(out).display().to_string()])
                .args(["-cp", cp, "-no-stdlib", "-no-reflect"])
                .arg(work.join(src)),
            &format!("kotlinc {src}"),
        )
    };

    // 3. Compile the processor, package it as a ServiceLoader-registered JAR.
    let proc_out = work.join("proc_out");
    assert!(kotlinc(
        "proc_out",
        &format!("{}:{}", api.display(), std_jar.display()),
        "proc/demo/Builder.kt"
    ));
    let svc = proc_out.join("META-INF/services");
    std::fs::create_dir_all(&svc).unwrap();
    std::fs::copy(
        work.join(
            "proc/META-INF/services/com.google.devtools.ksp.processing.SymbolProcessorProvider",
        ),
        svc.join("com.google.devtools.ksp.processing.SymbolProcessorProvider"),
    )
    .unwrap();
    let proc_jar = work.join("processor.jar");
    assert!(run(
        Command::new(&jar)
            .arg("cf")
            .arg(&proc_jar)
            .arg("-C")
            .arg(&proc_out)
            .arg("."),
        "jar processor"
    ));

    // 4. Compile the launcher.
    assert!(kotlinc(
        "launch_out",
        &format!("{cc}:{}", proc_jar.display()),
        "launch/Launcher.kt"
    ));

    // 5. Run KSP2 over the app source with the processor discovered FROM THE JAR.
    let ksp_out = work.join("kspout");
    let ran = run(
        Command::new(&java)
            .arg("-cp")
            .arg(format!(
                "{cc}:{}:{}",
                proc_jar.display(),
                work.join("launch_out").display()
            ))
            .arg("LauncherKt")
            .arg(work.join("app"))
            .arg(&ksp_out)
            .arg(&proc_jar)
            .arg(&jdk)
            .arg(std_jar) // libraries: stdlib
            .arg(&proc_jar), // libraries: the @Builder annotation
        "run KSP2",
    );
    assert!(ran, "KSP2 run must succeed");

    // 6. Assert the processor observed the resolved model across the KSP capability matrix (mirrors
    // categories from google/ksp's testData — see `just ksp-corpus`). The processor dumps each into
    // FooCaps.kt; we assert the dump line-by-line.
    let caps = ksp_out.join("kotlin/app/FooCaps.kt");
    assert!(
        caps.exists(),
        "KSP generated FooCaps.kt at {}",
        caps.display()
    );
    let c = std::fs::read_to_string(&caps).unwrap();
    let want = [
        ("classKind=CLASS", "class kind"),
        ("visibility=PUBLIC", "visibility"),
        ("qualifiedName=app.Foo", "qualified name"),
        ("Foo docs.", "docString"),
        ("superTypes=[app.Base]", "resolved supertypes"),
        ("hasCompanion=true", "companion detection"),
        (
            "prop a : kotlin.Int mutable=false",
            "val property + resolved type + mutability",
        ),
        (
            "prop b : kotlin.String mutable=true const=false nullable=NULLABLE",
            "var + nullable type resolution",
        ),
        ("rest:kotlin.Array(vararg)", "vararg value parameter"),
        ("y:kotlin.Int(def)", "default value parameter"),
        ("kind=MEMBER", "function kind"),
        ("byName=Foo", "Resolver.getClassDeclarationByName"),
        ("builtinInt=kotlin.Int", "Resolver.builtIns"),
        (
            "option.greeting=hi-from-krusty",
            "SymbolProcessorEnvironment.options",
        ),
    ];
    for (needle, what) in want {
        assert!(
            c.contains(needle),
            "KSP capability '{what}' — missing {needle:?} in:\n{c}"
        );
    }

    // 6b. MULTI-ROUND: FooCapsValidator exists only if the round-1-generated FooCaps (carrying
    // @Validate) was re-fed and processed in a later round — Foo itself has no @Validate.
    let validator = ksp_out.join("kotlin/app/FooCapsValidator.kt");
    assert!(
        validator.exists(),
        "multi-round: FooCapsValidator.kt proves the generated file was re-processed"
    );

    // 7. GENERATED CODE COMPILATION: KSP's output must itself compile. Feed every generated .kt back
    // through kotlinc (with the annotation jar on classpath) and assert the classfiles are produced.
    let mut gen_sources: Vec<PathBuf> = Vec::new();
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    collect(&p, out);
                } else if p.extension().is_some_and(|x| x == "kt") {
                    out.push(p);
                }
            }
        }
    }
    collect(&ksp_out.join("kotlin"), &mut gen_sources);
    assert!(
        gen_sources.len() >= 2,
        "expected >=2 generated .kt (builder + validator)"
    );

    let gen_classes = work.join("gen_classes");
    let compiled = run(
        Command::new(&java)
            .arg("-cp")
            .arg(&cc)
            .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
            .args(["-d", &gen_classes.display().to_string()])
            .args([
                "-cp",
                &format!("{}:{}", std_jar.display(), proc_jar.display()),
                "-no-stdlib",
                "-no-reflect",
            ])
            .args(&gen_sources),
        "compile generated code",
    );
    assert!(compiled, "KSP-generated code must compile");
    assert!(
        gen_classes.join("app/FooCaps.class").exists(),
        "generated FooCaps compiled to bytecode"
    );
    assert!(
        gen_classes.join("app/FooCapsValidator.class").exists(),
        "generated (round-2) FooCapsValidator compiled to bytecode"
    );
    eprintln!("real KSP from-jar run OK — capability matrix + multi-round + generated code compiled:\n{c}");
}
