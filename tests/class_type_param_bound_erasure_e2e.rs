//! A CLASS type parameter with a declared upper bound erases to that bound, exactly as a FUNCTION
//! type parameter already did: `class Bounded<T : Cargo>(val t: T)` signs its constructor, its
//! backing field, and its getter with `Lapp/Cargo;`, never `Ljava/lang/Object;`. Erasing to a real
//! class also makes the parameter NON-NULL, which kotlinc records with `@NotNull` on the field, the
//! getter and the constructor parameter plus an `Intrinsics.checkNotNullParameter` guard at `<init>`.
//! With both, the bounded ABI row is byte-identical to kotlinc. Runtime regressions additionally
//! assert krusty's own representation invariants, so they do not fail on semantically irrelevant
//! instruction choices made by the reference compiler.
//!
//! Bound shapes, probed on kotlinc 2.4.10 (`class C<T : B>(val t: T)`):
//!
//! | declared bound | descriptor    | `@NotNull` + `checkNotNullParameter` | byte-identical |
//! |----------------|---------------|--------------------------------------|----------------|
//! | none           | `Object`      | no                                   | yes            |
//! | `Cargo`        | `Lapp/Cargo;` | yes                                  | yes            |
//! | `Cargo?`       | `Lapp/Cargo;` | no                                   | tested below   |
//! | `Any`          | `Object`      | yes                                  | yes            |
//! | `Any?`         | `Object`      | no                                   | yes            |
//!
//! Descriptor erasure and nullability remain independent: `Cargo?` still erases to `Cargo`, while
//! admitting null means the field/accessors/constructor parameter carry neither a nullability
//! annotation nor a constructor guard.

use super::common;

/// `javap -v -p` of one class, compiled by BOTH compilers: `(kotlinc, krusty)`. `None` when the
/// reference toolchain is unavailable (the caller then skips).
fn javap_both(stem: &str, src: &str, class: &str) -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let kref = dir.join("ref");
    let kout = dir.join("out");
    std::fs::create_dir_all(&kref).ok()?;
    std::fs::create_dir_all(&kout).ok()?;
    let src_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&src_path, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp(src, stem, &[])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
    let path = kout.join(format!("{class}.class"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();

    let dump = |root: &std::path::Path| {
        common::javap(&["-v", "-p", "-cp", &root.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("{stem}: javap failed"))
    };
    let both = (dump(&kref), dump(&kout));
    let _ = std::fs::remove_dir_all(&dir);
    Some(both)
}

/// `javap -c -p` for a krusty-emitted class. Unlike [`javap_both`], internal JVM conformance checks
/// do not depend on the reference compiler being available.
fn javap_krusty(stem: &str, src: &str, class: &str) -> String {
    let classes = common::compile_in_process_metadata_cp(src, stem, &[])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == class)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join(format!("{class}.class"));
    std::fs::create_dir_all(path.parent().expect("class parent")).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let dump = common::javap(&["-c", "-p", &path.to_string_lossy()])
        .unwrap_or_else(|| panic!("{stem}: javap failed"));
    let _ = std::fs::remove_dir_all(dir);
    dump
}

/// Every member descriptor, in `javap` order.
fn descriptors(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|l| l.trim().strip_prefix("descriptor: "))
        .map(str::to_string)
        .collect()
}

/// The `@Metadata` `d2` string table, as `javap -v` renders it.
fn d2(dump: &str) -> String {
    dump.lines()
        .map(str::trim)
        .find(|l| l.starts_with("d2="))
        .unwrap_or_else(|| panic!("no d2 string table in:\n{dump}"))
        .to_string()
}

const BOUNDED: &str = r#"package app

open class Cargo(val v: Int)

class Bounded<T : Cargo>(val t: T)
"#;

/// The ctor parameter, the backing field, and the getter all carry the BOUND, not `Object`.
#[test]
fn class_type_param_erases_to_its_declared_class_bound() {
    let Some((kotlinc, krusty)) = javap_both("Bcls", BOUNDED, "app/Bounded") else {
        eprintln!("skip (Bcls: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        ["Lapp/Cargo;", "(Lapp/Cargo;)V", "()Lapp/Cargo;"]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// The whole class file matches kotlinc byte for byte: the erased descriptors, and the `@NotNull` +
/// `Intrinsics.checkNotNullParameter` a NON-null-bounded parameter carries with them.
#[test]
fn bounded_class_is_byte_identical_to_kotlinc() {
    match common::byte_diff_against_kotlinc("Bcls3", BOUNDED, "app/Bounded") {
        None => eprintln!("skip (Bcls3: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// `<T : Any>` erases to `Object` yet is still non-null, so it takes the annotations and the guard
/// WITHOUT the descriptor change — the two halves of the rule are independent.
#[test]
fn any_bounded_class_is_byte_identical_to_kotlinc() {
    let src = r#"package app

class AnyB<T : Any>(val t: T)
"#;
    match common::byte_diff_against_kotlinc("BanyB", src, "app/AnyB") {
        None => eprintln!("skip (BanyB: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// An unbounded `<T>` (implicitly `Any?`) and an explicit `<T : Any?>` admit null: no annotations, no
/// guard, and still byte-identical. This is the row that would break if the non-null rule over-fired.
#[test]
fn null_admitting_bounds_take_no_annotations_and_stay_byte_identical() {
    for (stem, src, class) in [
        (
            "Bfree2",
            "package app\n\nclass Free<T>(val t: T)\n",
            "app/Free",
        ),
        (
            "BnAnyB",
            "package app\n\nclass NAnyB<T : Any?>(val t: T)\n",
            "app/NAnyB",
        ),
    ] {
        match common::byte_diff_against_kotlinc(stem, src, class) {
            None => eprintln!("skip ({stem}: reference toolchain unavailable)"),
            Some(Ok(())) => {}
            Some(Err(e)) => panic!("{e}"),
        }
    }
}

/// A nullable class bound still supplies the JVM erasure. Nullability affects Kotlin's admissible
/// type arguments, not the descriptor's bound head, and neither compiler emits nullability
/// annotations for the type-parameter-backed property/parameter/result occurrences.
#[test]
fn nullable_bound_erases_to_its_declared_class() {
    let src = r#"package app

open class Cargo(val v: Int)

class NBound<T : Cargo?>(val t: T)
"#;
    let Some((kotlinc, krusty)) = javap_both("Bnb", src, "app/NBound") else {
        eprintln!("skip (Bnb: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        ["Lapp/Cargo;", "(Lapp/Cargo;)V", "()Lapp/Cargo;"],
        "krusty erases a nullable bound to its declared class"
    );
    assert_eq!(
        descriptors(&kotlinc),
        ["Lapp/Cargo;", "(Lapp/Cargo;)V", "()Lapp/Cargo;"],
        "kotlinc erases a nullable bound to the bound — the open facet"
    );
    assert!(
        !kotlinc.contains("checkNotNullParameter"),
        "kotlinc does not guard a nullable-bound parameter"
    );
    assert!(
        !krusty.contains("checkNotNullParameter"),
        "krusty must not guard a nullable-bound parameter"
    );
}

/// The `d2` string table — the medium the gap was reported in — matches kotlinc's exactly.
#[test]
fn bounded_class_metadata_string_table_matches_kotlinc() {
    let Some((kotlinc, krusty)) = javap_both("Bcls2", BOUNDED, "app/Bounded") else {
        eprintln!("skip (Bcls2: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        d2(&krusty),
        r#"d2=["Lapp/Bounded;","T","Lapp/Cargo;","","t","<init>","(Lapp/Cargo;)V","getT","()Lapp/Cargo;","Lapp/Cargo;"]"#
    );
    assert_eq!(d2(&krusty), d2(&kotlinc));
}

/// A bound on a MEMBER's type-parameter-typed signature erases the same way.
#[test]
fn bounded_class_members_erase_to_the_bound() {
    let src = r#"package app

open class Cargo(val v: Int)

class Holder<T : Cargo> {
    fun keep(t: T): T = t
}
"#;
    let Some((kotlinc, krusty)) = javap_both("Bmem", src, "app/Holder") else {
        eprintln!("skip (Bmem: reference toolchain unavailable)");
        return;
    };
    assert!(
        descriptors(&krusty).contains(&"(Lapp/Cargo;)Lapp/Cargo;".to_string()),
        "member signature erases to the bound: {:?}",
        descriptors(&krusty)
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// The FUNCTION case, which was already correct — kept as the contrast row.
#[test]
fn function_type_param_erases_to_its_declared_class_bound() {
    let src = r#"package app

open class Cargo(val v: Int)

fun <T : Cargo> bound(t: T): T = t
"#;
    let Some((kotlinc, krusty)) = javap_both("Bfun", src, "app/BfunKt") else {
        eprintln!("skip (Bfun: reference toolchain unavailable)");
        return;
    };
    assert_eq!(descriptors(&krusty), ["(Lapp/Cargo;)Lapp/Cargo;"]);
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// An UNBOUNDED class type parameter still erases to `Object` — the declared bound is what moves it.
#[test]
fn unbounded_class_type_param_still_erases_to_object() {
    let src = r#"package app

class Free<T>(val t: T)
"#;
    let Some((kotlinc, krusty)) = javap_both("Bfree", src, "app/Free") else {
        eprintln!("skip (Bfree: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        [
            "Ljava/lang/Object;",
            "(Ljava/lang/Object;)V",
            "()Ljava/lang/Object;"
        ]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// A bound naming ANOTHER parameter of the same class (`<A : Cargo, B : A>`) follows the chain to
/// the first real class, so `B` erases to `Cargo` too.
#[test]
fn class_type_param_bound_by_another_parameter_follows_the_chain() {
    let src = r#"package app

open class Cargo(val v: Int)

class Chain<A : Cargo, B : A>(val a: A, val b: B)
"#;
    let Some((kotlinc, krusty)) = javap_both("Bchain", src, "app/Chain") else {
        eprintln!("skip (Bchain: reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        descriptors(&krusty),
        [
            "Lapp/Cargo;",
            "Lapp/Cargo;",
            "(Lapp/Cargo;Lapp/Cargo;)V",
            "()Lapp/Cargo;",
            "()Lapp/Cargo;"
        ]
    );
    assert_eq!(descriptors(&krusty), descriptors(&kotlinc));
}

/// Erasing to the BOUND moves the consumption site too: reading `Holder<Sub>.value` off an accessor
/// whose descriptor is now `()LBase;` needs the `checkcast Sub` kotlinc emits. The emitter's narrowing
/// used to fire only for the erased TOP (`Object`), so without this the read fed `Base` to
/// `Sub.tag()` — a `VerifyError`, not a wrong answer.
#[test]
fn read_through_an_explicit_type_argument_narrows_from_the_bound() {
    let src = r#"open class Base { open fun tag(): String = "B" }
class Sub : Base() { override fun tag(): String = "S" }
class Holder<T : Base>(val value: T)
fun box(): String {
    val h: Holder<Sub> = Holder(Sub())
    val t = h.value.tag()
    return if (t == "S") "OK" else "FAIL: $t"
}
"#;
    assert_eq!(common::expect_box_run_with_stdlib(src, "Main"), "OK");
    let krusty = javap_krusty("Main", src, "MainKt");
    let box_code = krusty
        .lines()
        .skip_while(|line| !line.contains("box();"))
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(
        box_code
            .iter()
            .any(|line| line.contains("Holder.getValue:()LBase;")),
        "the getter retains its erased bound descriptor:\n{krusty}"
    );
    assert!(
        box_code
            .iter()
            .any(|line| line.contains("checkcast") && line.contains("class Sub")),
        "the read narrows from the bound:\n{krusty}"
    );
    assert!(
        box_code
            .iter()
            .any(|line| line.contains("Sub.tag:()Ljava/lang/String;")),
        "the narrowed value dispatches through Sub:\n{krusty}"
    );
}

/// An INNER class sees its outer class's bounded parameter, and erases it to the same bound.
#[test]
fn inner_class_erases_the_outer_bounded_parameter() {
    let src = r#"package app

open class Cargo(val v: Int)

class Outer<T : Cargo>(val t: T) {
    inner class Inner(val u: T)
}
"#;
    let Some((kotlinc, krusty)) = javap_both("Binner", src, "app/Outer$Inner") else {
        eprintln!("skip (Binner: reference toolchain unavailable)");
        return;
    };
    assert!(
        descriptors(&krusty).contains(&"()Lapp/Cargo;".to_string()),
        "inner getter erases the outer bound: {:?}",
        descriptors(&krusty)
    );
    // Compared as a MULTISET: krusty declares the synthetic `this$0` field before the property's,
    // kotlinc after it. That field ORDER divergence is unrelated to erasure and predates this file.
    let (mut krusty, mut kotlinc) = (descriptors(&krusty), descriptors(&kotlinc));
    krusty.sort();
    kotlinc.sort();
    assert_eq!(krusty, kotlinc);
}
