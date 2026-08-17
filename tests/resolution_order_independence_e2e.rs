//! The type of a declaration does not depend on the order the compiler was asked in.
//!
//! Signature collection used to type an implicitly-typed property in FILE ARGUMENT ORDER, so a
//! property initialized from a declaration in a later file saw nothing and was rejected:
//!
//! ```text
//! A.kt: val base = listOf(1, 2, 3)
//! B.kt: val derived = base.map { it + 1 }
//!
//! krusty A.kt B.kt -> ok            krusty B.kt A.kt -> cannot infer the type of property 'derived'
//! kotlinc A.kt B.kt -> ok           kotlinc B.kt A.kt -> ok, byte-identical
//! ```
//!
//! An undetermined declaration is now resolved ON DEMAND by the engine and memoised, so every
//! permutation produces the same answer. The assertions are the emitted DESCRIPTORS, not merely that
//! both permutations compile: a run can succeed with the wrong type, and that is the failure mode
//! this whole area is prone to — a wrong declared type becomes the field descriptor, the getter
//! descriptor and `@Metadata`, and the program still runs.
use super::common;
use std::collections::BTreeMap;

/// Compile `sources` in exactly the order given. A rejection is a failure, never a skip.
fn compile_ordered(label: &str, sources: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = [stdlib];
    let classes = common::compile_in_process_files(sources, &cp, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            let texts = sources.iter().map(|(_, src)| *src).collect::<Vec<_>>();
            let diagnostics = common::front_end_diagnostics_files(&texts, &cp, Some(jdk.as_path()));
            panic!("{label}: compile returned None; diagnostics: {diagnostics:?}")
        });
    classes.into_iter().collect()
}

/// The member declarations javap prints for one emitted class — field and accessor DESCRIPTORS,
/// which is what a downstream module consumes and what a value assertion can silently agree with.
fn member_declarations(label: &str, class: &str, bytes: &[u8]) -> String {
    let scratch = common::scratch_dir().expect("scratch filesystem unavailable");
    let dir = scratch.join("resolution-order").join(label);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let simple = class.rsplit('/').next().unwrap_or(class);
    let path = dir.join(format!("{simple}.class"));
    std::fs::write(&path, bytes).expect("write class file");
    let text = common::javap(&["-p", path.to_str().expect("utf-8 path")])
        .expect("pooled javap unavailable");
    text.lines()
        .map(str::trim)
        .filter(|line| line.contains(simple) || line.ends_with(';'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every permutation of `sources` must compile and emit the same declarations for `class`.
fn assert_order_independent(tag: &str, class: &str, sources: &[(&str, &str)]) {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let permutations = permutations(sources);
    let mut expected: Option<(String, String)> = None;
    for (index, order) in permutations.iter().enumerate() {
        let label = format!("{tag}-{index}");
        let names = order
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(" ");
        let classes = compile_ordered(&label, order);
        let bytes = classes
            .get(class)
            .unwrap_or_else(|| panic!("{label}: {class} was not emitted for order [{names}]"));
        let declarations = member_declarations(&label, class, bytes);
        match &expected {
            None => expected = Some((names, declarations)),
            Some((first_names, first)) => assert_eq!(
                first, &declarations,
                "{tag}: [{first_names}] and [{names}] disagree on {class}'s declarations"
            ),
        }
    }
}

/// Every ordering of `sources`, so the assertion covers the permutation that used to fail rather
/// than only the two the bug was first seen with.
fn permutations<'a>(sources: &[(&'a str, &'a str)]) -> Vec<Vec<(&'a str, &'a str)>> {
    if sources.len() <= 1 {
        return vec![sources.to_vec()];
    }
    let mut all = Vec::new();
    for (index, head) in sources.iter().enumerate() {
        let mut rest = sources.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, *head);
            all.push(tail);
        }
    }
    all
}

#[test]
fn a_property_reading_another_file_types_the_same_in_either_order() {
    // The reported case. `base` is implicitly typed too, so `derived` cannot be typed without
    // resolving it first — which is the demand the engine answers.
    assert_order_independent(
        "base-derived",
        "BKt",
        &[
            ("A", "val base = listOf(1, 2, 3)\n"),
            ("B", "val derived = base.map { it + 1 }\n"),
        ],
    );
}

#[test]
fn a_chain_across_three_files_types_the_same_in_every_order() {
    // A dependency CHAIN, not a single edge: resolving `last` demands `middle`, which demands
    // `first`. A pass that walks files once in argument order needs the exact opposite order to
    // succeed; on demand, every one of the six permutations agrees.
    assert_order_independent(
        "three-file-chain",
        "CKt",
        &[
            ("A", "val first = listOf(\"a\", \"b\")\n"),
            ("B", "val middle = first.map { it.length }\n"),
            ("C", "val last = middle.map { it + 1 }\n"),
        ],
    );
}

#[test]
fn a_declaration_read_before_it_is_declared_in_one_file_types_the_same() {
    // Within ONE file the same question arises for an expression getter, which Kotlin allows to
    // read a declaration written later. The answer must not depend on which of the two the walk
    // reached first, so both spellings are compiled and compared.
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let forward = compile_ordered(
        "same-file-forward",
        &[(
            "F",
            "val early get() = later\nval later = listOf(1, 2, 3)\n",
        )],
    );
    let backward = compile_ordered(
        "same-file-backward",
        &[(
            "F",
            "val later = listOf(1, 2, 3)\nval early get() = later\n",
        )],
    );
    let forward_getter = member_declarations("same-file-forward", "FKt", &forward["FKt"]);
    let backward_getter = member_declarations("same-file-backward", "FKt", &backward["FKt"]);
    assert!(
        forward_getter.contains("getEarly()"),
        "a getter reading a later declaration must be emitted: {forward_getter}"
    );
    let getter_line = |text: &str| {
        text.lines()
            .find(|line| line.contains("getEarly()"))
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        getter_line(&forward_getter),
        getter_line(&backward_getter),
        "the declaration order within a file must not change the getter's descriptor"
    );
}

#[test]
fn a_class_member_reading_another_file_types_the_same_in_either_order() {
    // A class member asks the same question a top-level property does. It used to be answered by a
    // separate retry pass that ran AFTER the top-level one, so the two could not both be waiting on
    // each other; one queue and one memo removes that ordering entirely.
    assert_order_independent(
        "member-reads-module",
        "Holder",
        &[
            ("A", "val source = listOf(1, 2, 3)\n"),
            ("B", "class Holder { val mapped = source.map { it + 1 } }\n"),
        ],
    );
}

#[test]
fn a_module_property_reading_a_class_member_types_the_same_in_either_order() {
    // The other direction, which two ordered passes cannot both serve: the module property is
    // waiting on the member rather than the member on the module property.
    assert_order_independent(
        "module-reads-member",
        "BKt",
        &[
            ("A", "object Source { val items = listOf(1, 2, 3) }\n"),
            ("B", "val doubled = Source.items.map { it * 2 }\n"),
        ],
    );
}

#[test]
fn a_member_read_through_a_receiver_resolves_the_same_as_a_bare_name() {
    // A read THROUGH a receiver does not reach the bare-name path: it resolves against the symbol
    // table's member records, which hold a placeholder while that member's own type is still being
    // determined. Reading the placeholder as the answer rejected a program kotlinc accepts.
    assert_order_independent(
        "member-through-receiver",
        "User",
        &[
            (
                "A",
                "class Box  { val a = Helper.text() }\nclass User { val b = Box().a }\n",
            ),
            ("B", "object Helper { fun text(): String = \"\" }\n"),
        ],
    );
}

#[test]
fn a_member_initializer_resolves_its_class_bodys_nested_names() {
    // A class body extends the file's classifier names with its nested classes under their SIMPLE
    // spelling; the file-level projection registers a nested class only under its dotted declared
    // name. Resolving a deferred member against the file's names either fails to find `Nested` or,
    // worse, silently binds a same-named top-level class into the field descriptor.
    assert_order_independent(
        "member-nested-names",
        "Outer",
        &[
            (
                "A",
                "class Outer {\n    class Nested\n    val x = Helper.wrap(Nested())\n}\n",
            ),
            ("B", "object Helper { fun <T> wrap(t: T): T = t }\n"),
        ],
    );
}

#[test]
fn an_inherited_member_is_not_answered_by_a_module_property_of_the_same_name() {
    // The demand index is keyed by the DECLARING owner, so an inherited member is a miss. Falling
    // through to the module-wide index on that miss typed `Derived.b` from the top-level `a`
    // (`String`) instead of the inherited one (`Int`) — a wrong field descriptor, not a diagnostic.
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let classes = compile_ordered(
        "inherited-vs-module",
        &[
            (
                "A",
                "val a = Helper.text()\n\
                 open class Base { val a = Helper.count() }\n\
                 class Derived : Base() { val b = a }\n",
            ),
            (
                "B",
                "object Helper { fun text(): String = \"\"\n fun count(): Int = 0 }\n",
            ),
        ],
    );
    let declarations = member_declarations("inherited-vs-module", "Derived", &classes["Derived"]);
    assert!(
        declarations.contains("private final int b;"),
        "the inherited member decides the type, not the module property: {declarations}"
    );
}

#[test]
fn an_eager_initializer_still_observes_declaration_order_within_its_file() {
    // Resolving on demand must not silently widen the initialization model. An initializer runs in
    // declaration order, so a declaration written LATER IN THE SAME FILE has no value yet: kotlinc
    // rejects this pair with "variable 'later' must be initialized" (measured on 2.4.10) while
    // accepting the identical pair split across two files, which the tests above cover.
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics_files(
        &["val eager = later\nval later = 1\n"],
        &[stdlib],
        Some(jdk.as_path()),
    );
    assert!(
        diagnostics
            .iter()
            .any(|line| line.contains("cannot infer the type of property 'eager'")),
        "a same-file forward reference from an initializer must still be rejected, got {diagnostics:?}"
    );
}

#[test]
fn the_inferred_descriptors_are_the_ones_kotlinc_writes() {
    // Order independence alone would be satisfied by being consistently WRONG. The reference
    // compiler decides what the field and getter descriptors are.
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let sources: [(&str, &str); 2] = [
        ("A", "val base = listOf(1, 2, 3)\n"),
        ("B", "val derived = base.map { it + 1 }\n"),
    ];
    let Some(reference) = kotlinc_classes(&sources) else {
        return; // no reference compiler provisioned
    };
    let ours = compile_ordered("kotlinc-diff", &sources);
    let theirs = member_declarations("kotlinc-diff-ref", "BKt", &reference);
    let mine = member_declarations("kotlinc-diff-krusty", "BKt", &ours["BKt"]);
    assert_eq!(
        mine, theirs,
        "BKt's declarations must match the reference compiler's"
    );
}

/// Compile `sources` with the reference kotlinc and return `BKt`'s bytes. `None` when no reference
/// compiler is provisioned.
fn kotlinc_classes(sources: &[(&str, &str)]) -> Option<Vec<u8>> {
    let stdlib = common::stdlib_jar();
    let work = common::scratch_dir()?.join("resolution-order-reference");
    std::fs::create_dir_all(&work).ok()?;
    let out = work.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-nowarn".to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.to_string_lossy().into_owned(),
    ];
    for (name, source) in sources {
        let path = work.join(format!("{name}.kt"));
        std::fs::write(&path, source).ok()?;
        args.push(path.to_string_lossy().into_owned());
    }
    match common::kotlinc_compile(&args) {
        Some((0, _)) => std::fs::read(out.join("BKt.class")).ok(),
        Some((code, err)) => panic!("kotlinc failed ({code}): {err}"),
        None => None,
    }
}
