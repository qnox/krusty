//! APT (javax.annotation.processing) hosting — `docs/JAVA_INTEROP.md` slice 4. javac owns the
//! multi-round loop: krusty's seam only passes `-processorpath` through the persistent JavaRunner
//! (`common::javac_compile_proc`), and generated sources are compiled in the same invocation.
//!
//! Self-contained: the processor itself is built in-test with the same in-process javac (JDK
//! only — no external processor jars), and a `META-INF/services` entry in its class DIR makes it
//! discoverable (javac accepts directories on the processor path).
//!
//! The processor proves MULTI-ROUND processing: round 1 sees `@Gen Src` and generates `SrcMid`
//! annotated `@Gen2`; round 2 sees `@Gen2 SrcMid` and generates `SrcMidEnd`. Kotlin then compiles
//! against the APT output and calls the round-2-generated class.

use super::common;

const GEN_ANN: &str = "public @interface Gen {}";
const GEN2_ANN: &str = "public @interface Gen2 {}";

/// Two-round processor. Uses the `annotations` set (TypeElements) rather than annotation `Class`
/// objects, so it needs no class-identity between javac's loader and the processor's.
const PROCESSOR: &str = r#"
import java.io.Writer;
import java.util.Set;
import javax.annotation.processing.*;
import javax.lang.model.SourceVersion;
import javax.lang.model.element.Element;
import javax.lang.model.element.TypeElement;

@SupportedAnnotationTypes({"Gen", "Gen2"})
public class MiniProcessor extends AbstractProcessor {
    @Override public SourceVersion getSupportedSourceVersion() { return SourceVersion.latest(); }

    @Override
    public boolean process(Set<? extends TypeElement> annotations, RoundEnvironment env) {
        for (TypeElement ann : annotations) {
            String annName = ann.getSimpleName().toString();
            for (Element e : env.getElementsAnnotatedWith(ann)) {
                String n = e.getSimpleName().toString();
                try {
                    if (annName.equals("Gen")) {
                        Writer w = processingEnv.getFiler().createSourceFile(n + "Mid").openWriter();
                        w.write("@Gen2 public class " + n + "Mid {}");
                        w.close();
                    } else {
                        Writer w = processingEnv.getFiler().createSourceFile(n + "End").openWriter();
                        w.write("public class " + n + "End { public static String ping() { return \"OK\"; } }");
                        w.close();
                    }
                } catch (Exception ex) {
                    processingEnv.getMessager().printMessage(
                        javax.tools.Diagnostic.Kind.ERROR, ex.toString());
                }
            }
        }
        return true;
    }
}
"#;

/// Build the processor into a class DIR with its ServiceLoader registration; return the dir.
fn build_processor() -> Option<std::path::PathBuf> {
    let (procdir, _) = common::javac_compile(
        &[
            ("MiniProcessor.java".to_string(), PROCESSOR.to_string()),
            ("Gen.java".to_string(), GEN_ANN.to_string()),
            ("Gen2.java".to_string(), GEN2_ANN.to_string()),
        ],
        &[],
    )?;
    let services = procdir.join("META-INF/services");
    std::fs::create_dir_all(&services).ok()?;
    std::fs::write(
        services.join("javax.annotation.processing.Processor"),
        "MiniProcessor\n",
    )
    .ok()?;
    Some(procdir)
}

fn cleanup(classes_dir: &std::path::Path) {
    if let Some(root) = classes_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// javac runs the processor over `@Gen Src`; both generation rounds happen inside ONE javac
/// invocation and every class (written + generated) comes back.
#[test]
fn apt_generates_across_two_rounds() {
    let Some(procdir) = build_processor() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let out = common::javac_compile_proc(
        &[(
            "Src.java".to_string(),
            "@Gen public class Src {}".to_string(),
        )],
        std::slice::from_ref(&procdir),
        std::slice::from_ref(&procdir),
    );
    cleanup(&procdir);
    let (dir, classes) = out.expect("APT compile succeeds");
    let mut names: Vec<&str> = classes.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    cleanup(&dir);
    assert_eq!(
        names,
        ["Src", "SrcMid", "SrcMidEnd"],
        "round-1 (SrcMid) AND round-2 (SrcMidEnd) outputs expected"
    );
}

/// Full loop: Kotlin compiled by krusty against the APT output dir, calling the class the SECOND
/// processing round generated.
#[test]
fn kotlin_calls_apt_generated_class() {
    let Some(procdir) = build_processor() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        cleanup(&procdir);
        return;
    };
    let out = common::javac_compile_proc(
        &[(
            "Src.java".to_string(),
            "@Gen public class Src {}".to_string(),
        )],
        std::slice::from_ref(&procdir),
        std::slice::from_ref(&procdir),
    );
    cleanup(&procdir);
    let (aptdir, apt_classes) = out.expect("APT compile succeeds");
    let jars = common::classpath_jars_for("");
    let mut cp = jars.clone();
    cp.push(aptdir.clone());
    let kotlin = common::compile_in_process(
        "fun box(): String = SrcMidEnd.ping()",
        "MainKt",
        &cp,
        Some(jdk.as_path()),
    );
    cleanup(&aptdir);
    let mut classes = kotlin.expect("krusty compiles against the APT-generated class");
    classes.extend(apt_classes);
    let box_class = common::find_box_class(&classes).expect("box class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// A processor that reports an ERROR fails the compile — the seam surfaces javac's failure as
/// `None` (harness semantics: skip, never mis-grade).
#[test]
fn apt_processor_error_fails_compile() {
    let Some(procdir) = build_processor() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    // `@Gen` on a class whose generated name collides with an EXISTING class → Filer error →
    // javac reports an error and the compile fails.
    let out = common::javac_compile_proc(
        &[
            (
                "Src.java".to_string(),
                "@Gen public class Src {}".to_string(),
            ),
            (
                "SrcMid.java".to_string(),
                "public class SrcMid {}".to_string(),
            ),
        ],
        std::slice::from_ref(&procdir),
        std::slice::from_ref(&procdir),
    );
    cleanup(&procdir);
    assert!(
        out.is_none(),
        "filer collision must surface as a failed compile"
    );
}
