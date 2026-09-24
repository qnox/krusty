//! A class carries the `StackMapTable` its final instructions imply, as kotlinc's writer computes it.
//!
//! kotlinc's frames are ASM's `COMPUTE_FRAMES` with every common superclass `java/lang/Object`, so
//! two values of different classes meeting at a jump target are an `Object` there. kotlinc casts
//! each to the type the join has in Kotlin before it gets there, and so does krusty: a reassigned
//! local of a base type, and an elvis whose sides box to different classes.

use krusty::jvm::frame_audit::{audit_class, Outcome};

use crate::common;

const REASSIGNED: &str = "\
abstract class A {
    abstract fun foo(): String
}

class B : A() {
    override fun foo() = \"OK\"
}

class C : A() {
    override fun foo() = \"fail\"
}

fun test(c: C, cond: Boolean): String {
    var x: A = c
    if (cond) {
        x = B()
    }
    return x.foo()
}

fun box(): String = test(C(), true)
";

const ELVIS: &str = "\
fun <T : Number?> foo(t: T): Int = (t ?: 42).toInt()

fun box(): String = if (foo<Int?>(null) == 42) \"OK\" else \"fail\"
";

const BRANCHY: &str = "\
fun pick(flag: Boolean, n: Int): Any {
    var total = 0
    for (i in 0 until n) {
        total += if (flag) i else -i
    }
    val label = when {
        total > 10 -> \"big\"
        total < 0 -> StringBuilder(\"negative\")
        else -> total
    }
    var size: Any = label
    try {
        size = label.toString().length
    } catch (e: IllegalStateException) {
        size = e
    }
    return size
}

fun box(): String = if (pick(true, 6) == 3 && pick(false, 2) == 8) \"OK\" else \"fail\"
";

fn compile(source: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let classpath = [common::stdlib_jar()];
    let jdk = common::jdk_modules();
    common::compile_in_process(source, stem, &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("krusty compiles {stem}"))
}

#[test]
fn joins_of_different_classes_run() {
    for (source, stem) in [
        (REASSIGNED, "Reassigned"),
        (ELVIS, "Elvis"),
        (BRANCHY, "Branchy"),
    ] {
        let classes = compile(source, stem);
        let facade = format!("{stem}Kt");
        assert_eq!(
            common::run_box(&classes, &facade, &[common::stdlib_jar()]).as_deref(),
            Some("OK"),
            "{stem}"
        );
    }
}

#[test]
fn every_written_table_is_the_computed_one() {
    for (source, stem) in [
        (REASSIGNED, "Reassigned"),
        (ELVIS, "Elvis"),
        (BRANCHY, "Branchy"),
    ] {
        for (class, bytes) in compile(source, stem) {
            let audits = audit_class(&bytes).unwrap_or_else(|error| panic!("{class}: {error}"));
            for audit in audits {
                assert!(
                    matches!(audit.outcome, Outcome::Identical { .. }),
                    "{class}.{}{}: {:?}",
                    audit.name,
                    audit.descriptor,
                    audit.outcome
                );
            }
        }
    }
}

#[test]
fn casts_before_a_join_match_kotlinc_bytes() {
    let scratch = common::scratch_dir().expect("scratch directory");
    for (source, stem) in [(REASSIGNED, "Reassigned"), (ELVIS, "Elvis")] {
        let reference = scratch.join(stem);
        std::fs::create_dir_all(&reference).expect("reference output directory");
        let file = scratch.join(format!("{stem}.kt"));
        std::fs::write(&file, source).expect("write the fixture");
        let Some((status, stderr)) = common::kotlinc_compile(&[
            "-d".to_string(),
            reference.to_string_lossy().into_owned(),
            file.to_string_lossy().into_owned(),
        ]) else {
            panic!("reference kotlinc unavailable");
        };
        assert_eq!(status, 0, "kotlinc rejected {stem}: {stderr}");
        let facade = format!("{stem}Kt");
        let expected =
            std::fs::read(reference.join(format!("{facade}.class"))).expect("kotlinc emitted it");
        let classes = compile(source, stem);
        let actual = classes
            .iter()
            .find(|(name, _)| *name == facade)
            .map(|(_, bytes)| bytes)
            .expect("krusty emitted the facade");
        assert!(
            *actual == expected,
            "{facade}.class differs from kotlinc ({} vs {} bytes)",
            actual.len(),
            expected.len()
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
