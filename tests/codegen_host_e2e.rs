//! End-to-end proof of the **codegen-host mechanism** krusty's `plugins::ksp` models: a real
//! annotation processor, packaged in a JAR, discovered via `META-INF/services` and run in a JVM over
//! source, generating new source — with automatic multi-round re-processing of generated code.
//!
//! KSP and APT share this exact contract (read a resolved symbol view, emit new files, no mutation of
//! existing decls — see `docs/PLUGIN_API.md`). KSP's own toolchain (the `symbol-processing-aa` and
//! `kotlin-compiler` embeddable jars) is reachable on Maven and runs the *same* way, but its
//! transitive closure is large; this test uses the JDK's built-in APT (`javac -processor`) to prove
//! the host actually loads and runs a third-party processor FROM A JAR and ingests its output.
//!
//! Scope/honesty: this proves the codegen-host MECHANISM via APT (javac) — it is NOT a KSP run. KSP's
//! own jars (`tests/ksp_provision_e2e.rs` downloads them) execute the identical host contract; APT is
//! used here only because it needs no toolchain download. The capability matrix a KSP host must cover
//! (APT analogue ≈ KSP analogue):
//!
//! - annotation query: `RoundEnvironment.getElementsAnnotatedWith` ≈ `Resolver.getSymbolsWithAnnotation`
//! - element/type inspection: `javax.lang.model` ≈ `KSClassDeclaration`/`KSType`
//! - code generation: `Filer` ≈ `CodeGenerator`
//! - multi-round to fixpoint: javac rounds ≈ KSP rounds
//! - processor loaded from JAR: `ServiceLoader` ≈ `SymbolProcessorProvider` discovery
//!
//! Skips (does not fail) if `javac`/`jar` are unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tooldir() -> Option<PathBuf> {
    // Prefer JAVA_HOME/bin; fall back to PATH.
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let b = PathBuf::from(home).join("bin");
        if b.join("javac").exists() {
            return Some(b);
        }
    }
    // PATH lookup via `which`-style probe.
    if Command::new("javac").arg("-version").output().is_ok() {
        return Some(PathBuf::from(""));
    }
    None
}

fn tool(dir: &Path, name: &str) -> Command {
    if dir.as_os_str().is_empty() {
        Command::new(name)
    } else {
        Command::new(dir.join(name))
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn codegen_host_runs_real_processor_from_jar_with_multiround() {
    let Some(bin) = tooldir() else {
        eprintln!("skipping: no javac/JAVA_HOME");
        return;
    };

    // Unique scratch dir: pid + a process-local atomic counter (collision-safe under parallel tests).
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "krusty_codegen_host_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let proc_src = root.join("proc_src");
    let proc_classes = root.join("proc_classes");
    let proc_jar = root.join("processor.jar");
    let app_src = root.join("app_src");
    let gen_out = root.join("gen");
    let app_classes = root.join("app_classes");

    // --- 1. A real annotation processor + its annotations, in Java. ---------------------------
    // @GenerateBuilder Foo -> generates FooBuilder, itself annotated @GenerateValidator
    // @GenerateValidator X -> generates XValidator (un-annotated -> the round chain terminates).
    // This proves annotation query + Filer codegen + automatic MULTI-ROUND (the validator round only
    // happens because javac re-processes the generated builder).
    write(
        &proc_src.join("demo/GenerateBuilder.java"),
        r#"package demo;
public @interface GenerateBuilder {}
"#,
    );
    write(
        &proc_src.join("demo/GenerateValidator.java"),
        r#"package demo;
public @interface GenerateValidator {}
"#,
    );
    write(
        &proc_src.join("demo/ChainProcessor.java"),
        r#"package demo;
import java.io.Writer;
import java.util.Set;
import javax.annotation.processing.*;
import javax.lang.model.SourceVersion;
import javax.lang.model.element.*;

@SupportedAnnotationTypes({"demo.GenerateBuilder","demo.GenerateValidator"})
@SupportedSourceVersion(SourceVersion.RELEASE_17)
public class ChainProcessor extends AbstractProcessor {
    @Override public boolean process(Set<? extends TypeElement> annos, RoundEnvironment env) {
        gen(env, "demo.GenerateBuilder", "Builder", "@demo.GenerateValidator ");
        gen(env, "demo.GenerateValidator", "Validator", "");
        return true;
    }
    private void gen(RoundEnvironment env, String annoFq, String suffix, String nextAnno) {
        TypeElement anno = processingEnv.getElementUtils().getTypeElement(annoFq);
        if (anno == null) return;
        for (Element e : env.getElementsAnnotatedWith(anno)) {
            String name = e.getSimpleName().toString();          // element inspection
            String pkg = processingEnv.getElementUtils().getPackageOf(e).getQualifiedName().toString();
            String genName = name + suffix;
            String fqcn = pkg.isEmpty() ? genName : pkg + "." + genName;
            try {
                Writer w = processingEnv.getFiler().createSourceFile(fqcn, e).openWriter(); // Filer codegen
                if (!pkg.isEmpty()) w.write("package " + pkg + ";\n");
                w.write(nextAnno + "public class " + genName + " { /* generated from " + name + " */ }\n");
                w.close();
            } catch (Exception ex) { throw new RuntimeException(ex); }
        }
    }
}
"#,
    );
    write(
        &proc_src.join("META-INF/services/javax.annotation.processing.Processor"),
        "demo.ChainProcessor\n",
    );

    // --- 2. Compile the processor and package it as a JAR (ServiceLoader-discoverable). --------
    std::fs::create_dir_all(&proc_classes).unwrap();
    let status = tool(&bin, "javac")
        .args(["-d", proc_classes.to_str().unwrap()])
        .arg(proc_src.join("demo/GenerateBuilder.java"))
        .arg(proc_src.join("demo/GenerateValidator.java"))
        .arg(proc_src.join("demo/ChainProcessor.java"))
        .status()
        .expect("run javac");
    assert!(status.success(), "processor must compile");
    // Drop the services file into the classes tree, then jar it up.
    write(
        &proc_classes.join("META-INF/services/javax.annotation.processing.Processor"),
        "demo.ChainProcessor\n",
    );
    let status = tool(&bin, "jar")
        .args([
            "--create",
            "--file",
            proc_jar.to_str().unwrap(),
            "-C",
            proc_classes.to_str().unwrap(),
            ".",
        ])
        .status()
        .expect("run jar");
    assert!(status.success(), "processor jar must build");

    // --- 3. A consumer source using the annotation. -------------------------------------------
    write(
        &app_src.join("app/Foo.java"),
        r#"package app;
@demo.GenerateBuilder
public class Foo { public int a; public String b; }
"#,
    );

    // --- 4. The HOST: run javac with the processor loaded FROM THE JAR, capturing generated src. ---
    std::fs::create_dir_all(&gen_out).unwrap();
    std::fs::create_dir_all(&app_classes).unwrap();
    let out = tool(&bin, "javac")
        .args(["-cp", proc_jar.to_str().unwrap()])
        .args(["-processorpath", proc_jar.to_str().unwrap()])
        .args(["-s", gen_out.to_str().unwrap()])
        .args(["-d", app_classes.to_str().unwrap()])
        .arg(app_src.join("app/Foo.java"))
        .output()
        .expect("run javac with processor");
    assert!(
        out.status.success(),
        "annotation processing run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --- 5. Assert the generated sources exist — proving the from-jar processor ran AND that the
    //        multi-round chain fired (Validator only exists because the generated Builder was
    //        re-processed in a later round). ----------------------------------------------------
    let builder = gen_out.join("app/FooBuilder.java");
    let validator = gen_out.join("app/FooBuilderValidator.java");
    assert!(
        builder.exists(),
        "round 1: FooBuilder generated from @GenerateBuilder"
    );
    // FooBuilderValidator can ONLY exist if round 1's generated FooBuilder (which carries
    // @GenerateValidator) was itself re-processed in a later round — the input `Foo` has no
    // @GenerateValidator, so a single round could never produce it. Its presence isolates multi-round.
    assert!(
        validator.exists(),
        "round 2: FooBuilderValidator proves the generated FooBuilder was re-processed"
    );
    let gen = std::fs::read_to_string(&builder).unwrap();
    assert!(
        gen.contains("generated from Foo"),
        "generated content reflects the inspected element"
    );

    let _ = std::fs::remove_dir_all(&root);
}
