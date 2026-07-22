//! The combined KSP + APT **outer fixpoint** — `docs/JAVA_INTEROP.md` §4, slice 5.
//!
//! Two inner engines already have their own fixpoints: [`KspHost`] drives KSP processors to a
//! round-fixpoint in-process, and javac (through the harness's `-processorpath` seam) runs APT's
//! multi-round loop internally. This driver owns the loop that connects them:
//!
//! ```text
//! loop:
//!   1. KSP rounds over the current symbol view       → generated .kt / .java
//!   2. NEW .java (incl. KSP-generated)? → java step  → javac+APT; its classes' symbols
//!                                                      join the view
//!   3. did 1 or 2 contribute anything new? → repeat, else done
//! ```
//!
//! Termination: each KSP invocation is bounded by [`KspHost`]'s round backstop and generated file
//! NAMES are deduplicated across outer iterations (an idempotent processor re-emitting the same
//! file contributes nothing new); the java step runs only when the `.java` set grew. The driver
//! additionally carries its own outer backstop so a pathological processor/JAVA interaction is
//! capped deterministically, keeping the work already done.
//!
//! The java step is a CALLBACK (`&[(file name, source)] → contributed symbols`), so this module
//! stays free of any JVM/javac dependency: the harness backs it with the persistent JavaRunner's
//! `javac_compile_proc` (javac + APT), unit tests with a pure function.

use super::ksp::{GeneratedFile, KsClass, KspHost};

/// The java-compile step: the CURRENT full `.java` set (`(file name, source)` pairs) in,
/// the compiled classes' symbol contributions out (`None` = the java side failed to compile).
pub type JavaStep<'a> = dyn FnMut(&[(String, String)]) -> Option<Vec<KsClass>> + 'a;

/// Result of a combined run.
#[derive(Debug, Default)]
pub struct CodegenLoopResult {
    /// Every file KSP generated, `.kt` and `.java` alike, deduplicated by name across the run.
    pub generated: Vec<GeneratedFile>,
    /// Outer iterations executed (1 = KSP reached its fixpoint without new java feedback).
    pub outer_rounds: u32,
    /// Whether the run ended by hitting the outer backstop instead of a true fixpoint.
    pub capped: bool,
}

/// Drive `ksp` and a java-compile step to the joint fixpoint. `symbols` is the initial resolved
/// view (module classes); `java_sources` the module's initial `.java` files. `java_step` compiles
/// the CURRENT full `.java` set (javac + APT rounds happen inside it) and returns the symbols its
/// classes contribute — or `None` when the java side fails to compile, which aborts the whole run
/// (`None`; callers skip, never mis-grade).
pub fn run_codegen_loop(
    ksp: &mut KspHost,
    mut symbols: Vec<KsClass>,
    mut java_sources: Vec<(String, String)>,
    java_step: &mut JavaStep<'_>,
    outer_max: u32,
) -> Option<CodegenLoopResult> {
    let mut result = CodegenLoopResult::default();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    // The view may receive the same class from repeated java_step calls (each recompiles the FULL
    // `.java` set — APT's rounds need it whole); dedup by FqName keeps the resolver view clean.
    let dedup_symbols = |symbols: &mut Vec<KsClass>| {
        let mut seen = std::collections::HashSet::new();
        symbols.retain(|c| seen.insert(c.fq_name.clone()));
    };
    // Compile the module's OWN java before the first KSP round, so its symbols are in the view a
    // processor queries (mirrors kotlinc: the frontend sees java sources from the start).
    if !java_sources.is_empty() {
        symbols.extend(java_step(&java_sources)?);
        dedup_symbols(&mut symbols);
    }
    loop {
        result.outer_rounds += 1;
        // 1. KSP to ITS fixpoint over the current view. `KspHost::run` dedups within the
        // invocation; dedup ACROSS outer iterations happens here by file name.
        let ksp_out = ksp.run(symbols.clone());
        let fresh: Vec<GeneratedFile> = ksp_out
            .generated
            .into_iter()
            .filter(|f| !seen_names.contains(f.name.as_str()))
            .collect();
        let mut contributed = false;
        let mut new_java = false;
        for f in &fresh {
            seen_names.insert(f.name.clone());
            // A generated KOTLIN file's declarations join the view directly (re-resolve).
            symbols.extend(f.declares.iter().cloned());
            contributed = true;
            if f.name.ends_with(".java") {
                java_sources.push((f.name.clone(), f.content.clone()));
                new_java = true;
            }
        }
        result.generated.extend(fresh);
        // 2. The `.java` set grew → recompile it (javac runs APT's own rounds inside) and fold the
        // resulting symbols — INCLUDING APT-generated classes — back into the view.
        if new_java {
            symbols.extend(java_step(&java_sources)?);
        }
        dedup_symbols(&mut symbols);
        // 3. Nothing new from either engine → joint fixpoint.
        if !contributed {
            result.outer_rounds -= 1; // the final, empty probe round isn't a work round
            break;
        }
        if result.outer_rounds >= outer_max {
            result.capped = true;
            break;
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::super::ksp::{CodeGenerator, Resolver, SymbolProcessor};
    use super::*;

    fn class(fq: &str, ann: &[&str]) -> KsClass {
        KsClass {
            fq_name: fq.to_string(),
            annotations: ann.iter().map(|s| s.to_string()).collect(),
            properties: Vec::new(),
            span: Default::default(),
        }
    }

    /// Generates `<N>Gen.kt` for every `@Trigger` class; generates `<N>.java` for every
    /// `@NeedsJava` class; generates `Final.kt` once `FromJavaEnd` (an APT product) is visible.
    struct P;
    impl SymbolProcessor for P {
        fn process(&mut self, resolver: &Resolver, gen: &mut CodeGenerator) {
            for c in resolver.get_symbols_with_annotation("Trigger") {
                let n = format!("{}Gen.kt", c.fq_name);
                gen.create_new_file(
                    &n,
                    "class Gen",
                    vec![class(&format!("{}Gen", c.fq_name), &[])],
                );
            }
            for c in resolver.get_symbols_with_annotation("NeedsJava") {
                let n = format!("{}.java", c.fq_name);
                gen.create_new_file(&n, "public class J {}", vec![]);
            }
            if resolver.span_of("FromJavaEnd").is_some() && resolver.span_of("Final").is_none() {
                gen.create_new_file("Final.kt", "class Final", vec![class("Final", &[])]);
            }
        }
    }

    #[test]
    fn kotlin_only_reaches_fixpoint_in_one_outer_round() {
        let mut host = KspHost::new();
        host.register(Box::new(P));
        let mut java_calls = 0u32;
        let r = run_codegen_loop(
            &mut host,
            vec![class("A", &["Trigger"])],
            Vec::new(),
            &mut |_| {
                java_calls += 1;
                Some(Vec::new())
            },
            10,
        )
        .expect("loop completes");
        assert_eq!(r.outer_rounds, 1);
        assert!(!r.capped);
        assert_eq!(java_calls, 0, "no .java anywhere → java step never runs");
        assert_eq!(r.generated.len(), 1);
        assert_eq!(r.generated[0].name, "AGen.kt");
    }

    #[test]
    fn java_feedback_triggers_a_second_ksp_round() {
        let mut host = KspHost::new();
        host.register(Box::new(P));
        // The java step models javac+APT: whatever `.java` set arrives, the APT side contributes
        // `FromJavaEnd` — which P reacts to with `Final.kt` in the NEXT outer round.
        let mut java_sets: Vec<usize> = Vec::new();
        let r = run_codegen_loop(
            &mut host,
            vec![class("B", &["NeedsJava"])],
            Vec::new(),
            &mut |srcs| {
                java_sets.push(srcs.len());
                Some(vec![class("FromJavaEnd", &[])])
            },
            10,
        )
        .expect("loop completes");
        assert_eq!(java_sets, vec![1], "one recompile of the grown .java set");
        let names: Vec<&str> = r.generated.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"B.java"), "{names:?}");
        assert!(
            names.contains(&"Final.kt"),
            "APT-contributed symbol must reach the next KSP round: {names:?}"
        );
        assert!(!r.capped);
    }

    #[test]
    fn initial_java_compiles_before_the_first_ksp_round() {
        let mut host = KspHost::new();
        host.register(Box::new(P));
        // The module's own .java contributes `FromJavaEnd` up front → `Final.kt` in round 1.
        let r = run_codegen_loop(
            &mut host,
            Vec::new(),
            vec![("Own.java".to_string(), "public class Own {}".to_string())],
            &mut |_| Some(vec![class("FromJavaEnd", &[])]),
            10,
        )
        .expect("loop completes");
        assert!(r.generated.iter().any(|f| f.name == "Final.kt"));
        assert_eq!(r.outer_rounds, 1);
    }

    #[test]
    fn failing_java_step_aborts_the_run() {
        let mut host = KspHost::new();
        host.register(Box::new(P));
        assert!(run_codegen_loop(
            &mut host,
            vec![class("B", &["NeedsJava"])],
            Vec::new(),
            &mut |_| None,
            10,
        )
        .is_none());
    }

    /// A processor that emits a NEW uniquely-named file every time it runs — never converges.
    struct Runaway(u32);
    impl SymbolProcessor for Runaway {
        fn process(&mut self, _r: &Resolver, gen: &mut CodeGenerator) {
            self.0 += 1;
            let n = format!("R{}.kt", self.0);
            gen.create_new_file(&n, "class R", vec![class(&format!("R{}", self.0), &[])]);
        }
    }

    #[test]
    fn outer_backstop_caps_a_runaway_chain_keeping_its_work() {
        let mut host = KspHost::new().with_max_rounds(1);
        host.register(Box::new(Runaway(0)));
        let r = run_codegen_loop(&mut host, Vec::new(), Vec::new(), &mut |_| None, 3)
            .expect("capped, not aborted");
        assert!(r.capped);
        assert_eq!(r.outer_rounds, 3);
        assert!(!r.generated.is_empty());
    }
}
