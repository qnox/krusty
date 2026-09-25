//! A class carries the `StackMapTable` its final instructions imply, as kotlinc's writer computes it.
//!
//! kotlinc's frames are ASM's `COMPUTE_FRAMES` with every common superclass `java/lang/Object`, so
//! two values of different classes meeting at a jump target are an `Object` there. kotlinc casts
//! each to the type the join has in Kotlin before it gets there, and so does krusty: a reassigned
//! local of a base type, an elvis whose sides box to different classes, and a `try` whose body and
//! catch produce different subclasses of its type.

use krusty::jvm::frame_audit::{audit_class, Outcome};

use crate::common::{compare_with_kotlinc_plugin, method_instructions};

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

const TRY: &str = "\
sealed class Either<out L, out R> {
    data class Left<out L>(val value: L) : Either<L, Nothing>()
    data class Right<out R>(val value: R) : Either<Nothing, R>()
    companion object {
        inline fun <R> catching(block: () -> R): Either<Throwable, R> =
            try { Right(block()) } catch (t: Throwable) { Left(t) }
    }
}

fun <R> caught(block: () -> R): Either<Throwable, R> = Either.catching(block)

fun box(): String {
    val right = caught { \"OK\" }
    val left = caught<String> { throw IllegalStateException(\"fail\") }
    if (left !is Either.Left || left.value !is IllegalStateException) return \"fail\"
    return (right as Either.Right<*>).value as String
}
";

const TRY_SUBCLASSES: &str = "\
abstract class Outcome {
    abstract fun text(): String
}

class Success(val value: String) : Outcome() {
    override fun text() = value
}

class Failure(val error: Throwable) : Outcome() {
    override fun text() = error.message ?: \"fail\"
}

fun fail(message: String): Nothing = throw IllegalStateException(message)

fun attempt(block: () -> String): Outcome =
    try { Success(block()) } catch (e: IllegalStateException) { Failure(e) }

fun box(): String = attempt { \"O\" }.text() + attempt { fail(\"K\") }.text()
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
        (TRY, "Try"),
        (TRY_SUBCLASSES, "TrySubclasses"),
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
        (TRY, "Try"),
        (TRY_SUBCLASSES, "TrySubclasses"),
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
    for (source, stem, classes) in [
        (REASSIGNED, "Reassigned", &["ReassignedKt"][..]),
        (ELVIS, "Elvis", &["ElvisKt"][..]),
    ] {
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
        let emitted = compile(source, stem);
        for class in classes {
            let expected = std::fs::read(reference.join(format!("{class}.class")))
                .unwrap_or_else(|_| panic!("kotlinc emitted {class}"));
            let actual = emitted
                .iter()
                .find(|(name, _)| name == class)
                .map(|(_, bytes)| bytes)
                .unwrap_or_else(|| panic!("krusty emitted {class}"));
            assert!(
                *actual == expected,
                "{class}.class differs from kotlinc ({} vs {} bytes)",
                actual.len(),
                expected.len()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

/// A `try` whose body and catch produce different subclasses of its type casts each to that type
/// before the store, as kotlinc does, so the `Object` its frames merge them at is still verifiably
/// the declared return type. Whole-class identity is not asserted: the pool position of a catch
/// type and of a catch variable's debug entry diverge for reasons of their own.
#[test]
fn a_try_casts_each_branch_to_its_type_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "TrySubclasses",
        TRY_SUBCLASSES,
        "TrySubclassesKt",
        &[common::stdlib_jar(), common::jdk_modules()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap");
    let member = "Outcome attempt(";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "{member} not found");
    assert_eq!(method_instructions(&built.krusty, member), reference);
    assert_eq!(
        stack_map(&built.krusty, member),
        stack_map(&built.reference, member)
    );
}

/// The `StackMapTable` javap prints for one method: frame kinds, deltas and the classes they name.
fn stack_map(disassembly: &str, marker: &str) -> Vec<String> {
    let mut lines = disassembly.lines().map(str::trim);
    lines
        .by_ref()
        .find(|line| line.ends_with(';') && line.contains(marker))
        .unwrap_or_else(|| panic!("{marker} not found"));
    let mut lines = lines.skip_while(|line| !line.starts_with("StackMapTable"));
    let header = lines
        .next()
        .unwrap_or_else(|| panic!("{marker} has no frames"));
    let frames = lines
        .take_while(|line| {
            line.starts_with("frame_type")
                || line.starts_with("offset_delta")
                || line.starts_with("locals")
                || line.starts_with("stack")
        })
        .map(str::to_string);
    std::iter::once(header.to_string()).chain(frames).collect()
}
