//! The combined KSP + APT outer fixpoint (`docs/JAVA_INTEROP.md` §4, slice 5) with the REAL java
//! side: `run_codegen_loop` drives the in-process KSP host, and its java step is the persistent
//! JavaRunner's javac with a real `-processorpath` (the two-round `MiniProcessor` from
//! `apt_host_e2e`). Round trip:
//!
//!   KSP sees `@NeedsBridge Marker` → generates `Bridge.java` annotated `@Gen`
//!   → java step: javac runs APT (its own two rounds: `BridgeMid`, `BridgeMidEnd`)
//!   → the APT-generated `BridgeMidEnd` symbol re-enters the KSP view
//!   → KSP reacts with `Done.kt` → joint fixpoint.
//!
//! Finally krusty compiles Kotlin calling `BridgeMidEnd.ping()` against the java classes.

use super::apt_host_e2e;
use super::common;

use krusty::plugins::codegen_loop::run_codegen_loop;
use krusty::plugins::ksp::{CodeGenerator, KsClass, KspHost, Resolver, SymbolProcessor};

fn class(fq: &str, ann: &[&str]) -> KsClass {
    KsClass {
        fq_name: fq.to_string(),
        annotations: ann.iter().map(|s| s.to_string()).collect(),
        properties: Vec::new(),
        span: Default::default(),
    }
}

/// KSP side of the loop: emits an APT-annotated Java source, then reacts to the class the APT
/// side generated from it.
struct BridgeProcessor;
impl SymbolProcessor for BridgeProcessor {
    fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator) {
        if !resolver
            .get_symbols_with_annotation("NeedsBridge")
            .is_empty()
            && resolver.span_of("Bridge").is_none()
        {
            gen.create_new_file(
                "Bridge.java",
                "@Gen public class Bridge {}",
                Vec::new(), // java symbols arrive via the java step, not `declares`
            );
        }
        if resolver.span_of("BridgeMidEnd").is_some() && resolver.span_of("Done").is_none() {
            gen.create_new_file("Done.kt", "class Done", vec![class("Done", &[])]);
        }
    }
}

#[test]
fn ksp_generates_java_apt_generates_class_ksp_sees_it() {
    let Some(procdir) = apt_host_e2e::build_processor() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let mut host = KspHost::new();
    host.register(Box::new(BridgeProcessor));

    // The java step: real javac + APT over the CURRENT .java set; contributed symbols = the
    // resulting class names (the production adapter would read full signatures — names suffice
    // for the loop protocol). Keeps the final output dir + classes for the Kotlin stage.
    let mut last: Option<common::JavacOutput> = None;
    let result = run_codegen_loop(
        &mut host,
        vec![class("Marker", &["NeedsBridge"])],
        Vec::new(),
        &mut |srcs| {
            if let Some((dir, _)) = last.take() {
                if let Some(root) = dir.parent() {
                    let _ = std::fs::remove_dir_all(root);
                }
            }
            let (dir, classes) = common::javac_compile_proc(
                srcs,
                std::slice::from_ref(&procdir),
                std::slice::from_ref(&procdir),
            )?;
            let symbols = classes.iter().map(|(n, _)| class(n, &[])).collect();
            last = Some((dir, classes));
            Some(symbols)
        },
        10,
    )
    .expect("joint fixpoint reached");

    let names: Vec<&str> = result.generated.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"Bridge.java"), "{names:?}");
    assert!(
        names.contains(&"Done.kt"),
        "APT-generated symbol must round-trip into the KSP view: {names:?}"
    );
    assert!(!result.capped);
    assert_eq!(result.outer_rounds, 2, "KSP → java/APT → KSP");

    // The java side really produced the two-round APT chain.
    let (aptdir, apt_classes) = last.expect("java step ran");
    let mut cnames: Vec<&str> = apt_classes.iter().map(|(n, _)| n.as_str()).collect();
    cnames.sort();
    assert_eq!(cnames, ["Bridge", "BridgeMid", "BridgeMidEnd"]);

    // And krusty compiles Kotlin against the loop's java output.
    let jars = common::classpath_jars_for("");
    let mut cp = jars.clone();
    cp.push(aptdir.clone());
    let kotlin = common::compile_in_process(
        "fun box(): String = BridgeMidEnd.ping()",
        "MainKt",
        &cp,
        Some(jdk.as_path()),
    );
    if let Some(root) = aptdir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    if let Some(root) = procdir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut classes = kotlin.expect("krusty compiles against the loop's java classes");
    classes.extend(apt_classes);
    let box_class = common::find_box_class(&classes).expect("box class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}
