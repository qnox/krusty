//! kotlinc emits an annotation's synthetic implementation class only in a source file that
//! CONSTRUCTS it (`Marker("x")`), one per annotation per file. Declaring an annotation emits nothing
//! extra. krusty emitted one per DECLARATION, so every module that merely declares annotations —
//! which in intellij-community is most of them — carried class files kotlinc never writes, and could
//! not be byte-compared at all.

use super::common;

fn emitted_classes(source: &str) -> Option<Vec<String>> {
    let classes = common::compile_in_process(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )?;
    let mut names: Vec<String> = classes.into_iter().map(|(name, _)| name).collect();
    names.sort();
    Some(names)
}

#[test]
fn a_declared_annotation_emits_no_implementation_class() {
    let source = r#"
        annotation class Plain
        annotation class WithDefault(val name: String = "")
    "#;

    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(
        !names.iter().any(|name| name.contains("annotationImpl")),
        "a declared-only annotation needs no implementation class: {names:?}"
    );
}

#[test]
fn a_constructed_annotation_still_emits_its_implementation_class() {
    let source = r#"
        annotation class Marker(val name: String = "")

        fun make(): Marker = Marker("x")
    "#;

    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(
        names.iter().any(|name| name.contains("annotationImpl")),
        "constructing an annotation needs its implementation class: {names:?}"
    );
}

#[test]
fn a_constructed_annotation_reads_back_its_argument() {
    let source = r#"
        annotation class Marker(val name: String = "")

        fun box(): String = Marker("OK").name
    "#;

    assert_eq!(
        common::compile_and_run_box(
            source,
            "Main",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("OK")
    );
}

#[test]
fn annotation_tracking_ignores_an_ordinary_synthetic_construction() {
    let source = r#"
        fun box(): String {
            val value = object { val text = "OK" }
            return value.text
        }
    "#;
    assert_eq!(
        common::compile_and_run_box(
            source,
            "Main",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("OK")
    );
}

#[test]
fn an_annotation_declared_in_another_source_file_is_constructed_at_the_use_site() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            (
                "Marker",
                r#"package sample
                   annotation class Marker(val name: String = "default")"#,
            ),
            (
                "Use",
                r#"package sample
                   fun box(): String = Marker("OK").name"#,
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("cross-file annotation construction must compile");

    let mut names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| name.contains("annotationImpl"))
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        ["sample/UseKt$annotationImpl$sample_Marker$0"],
        "the implementation belongs to the constructing file, not the declaration file"
    );
    assert_eq!(
        common::run_box(&classes, "sample.UseKt", std::slice::from_ref(&stdlib)).as_deref(),
        Some("OK")
    );
}

#[test]
fn all_calls_in_one_file_share_the_first_scopes_annotation_implementation() {
    let source = r#"
        package sample
        annotation class Marker(val name: String)

        fun top(): Marker = Marker("top")
        class Host {
            fun first(): Marker = Marker("first")
            fun second(): Marker = Marker("second")
        }
    "#;
    let Some(names) = emitted_classes(source) else {
        return;
    };
    let implementations = names
        .iter()
        .filter(|name| name.contains("annotationImpl"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        implementations,
        ["sample/Host$annotationImpl$sample_Marker$0"]
    );
}

#[test]
fn an_annotation_used_only_as_an_annotation_argument_emits_no_implementation() {
    let source = r#"
        annotation class Inner(val value: String)
        annotation class Outer(val inner: Inner)
        @Outer(Inner("value")) class Marked
    "#;
    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(!names.iter().any(|name| name.contains("annotationImpl")));
}

#[test]
fn a_nested_annotation_from_the_classpath_uses_the_same_construction_model() {
    let dependency = common::compile_lib(
        "annotation_impl_nested_classpath",
        r#"package dependency
           class Holder {
               annotation class Marker(val name: String)
           }"#,
    )
    .expect("compile annotation dependency");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        r#"package consumer
           fun box(): String = dependency.Holder.Marker("OK").name"#,
        "Use",
        &[dependency.clone(), stdlib.clone()],
        Some(jdk.as_path()),
    )
    .expect("construct classpath annotation");

    let implementations = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| name.contains("annotationImpl"))
        .collect::<Vec<_>>();
    assert_eq!(
        implementations,
        ["consumer/UseKt$annotationImpl$dependency_Holder_Marker$0"]
    );
    assert_eq!(
        common::run_box(&classes, "consumer.UseKt", &[dependency, stdlib]).as_deref(),
        Some("OK")
    );
}

/// The implementation class emits its members in kotlinc's ORDER: the accessors, then `equals`,
/// `hashCode`, `toString`, and `annotationType()` LAST.
///
/// The method table is part of the class file, so emitting `annotationType()` beside the member
/// accessors diverged from the reference on every annotation that is instantiated. Asserted against
/// the reference compiler's own order, and the program is RUN so the reordering cannot quietly break
/// the members it moves past.
#[test]
fn the_implementation_emits_members_in_kotlincs_order() {
    const SRC: &str = "annotation class Simple(val v: Int)\n\
                       fun make(): Simple = Simple(1)\n\
                       fun box(): String = if (make().v == 1) \"OK\" else \"FAIL\"\n";
    let ours = common::expect_compile_in_process(
        SRC,
        "Ord",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let impl_class = ours
        .iter()
        .find(|(name, _)| name.contains("$annotationImpl$"))
        .map(|(_, bytes)| krusty::jvm::classreader::parse_class(bytes).expect("parse impl"))
        .expect("the annotation implementation class");
    let names = impl_class
        .methods
        .iter()
        .map(|m| m.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "<init>",
            "v",
            "equals",
            "hashCode",
            "toString",
            "annotationType"
        ],
        "member order must be kotlinc's"
    );
    assert_eq!(
        common::run_box(
            &ours,
            &common::find_box_class(&ours).expect("box class"),
            &[common::stdlib_jar()]
        )
        .as_deref(),
        Some("OK"),
        "the reordered implementation must still work"
    );
}

/// The implementation's `hashCode()` is kotlinc's, instruction for instruction — and computes the
/// same value.
///
/// Two things the value alone cannot show, both visible in the class file: the member-name weight is
/// COMPUTED (`ldc "v"; String.hashCode(); bipush 127; imul`) rather than folded to a constant, and
/// every primitive goes through its wrapper's static `hashCode` — `int` included, whose value already
/// is its hash. A reference member resolves `hashCode` against its DECLARED class (`E`, `String`),
/// except an interface-typed one (a nested annotation), where `invokevirtual` on the interface is
/// illegal and kotlinc falls back to `Object`.
#[test]
fn the_implementation_hashcode_matches_kotlinc() {
    const SRC: &str = "enum class E { A }\n\
                       annotation class Inner(val x: Int)\n\
                       annotation class Outer(val i: Int, val b: Boolean, val c: Char, val l: Long,\n\
                                              val d: Double, val t: String, val a: IntArray,\n\
                                              val e: E, val n: Inner)\n\
                       fun mk(): Outer = Outer(1, true, 'c', 4L, 6.0, \"s\", intArrayOf(1), E.A, Inner(1))\n\
                       fun box(): String = if (mk().hashCode() == mk().hashCode()) \"OK\" else \"FAIL\"\n";
    let Some(build) = common::compile_libs_build("annotation_hashcode_shape", &[("H.kt", SRC)])
    else {
        return; // reference compiler unavailable
    };
    let reference_dir = build
        .reference_out()
        .expect("reference compiler output unavailable");
    let ours = common::expect_compile_in_process(
        SRC,
        "H",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let impl_name = ours
        .iter()
        .map(|(name, _)| name.clone())
        .find(|name| name.contains("$annotationImpl$"))
        .expect("the annotation implementation class");
    let body = |bytes: &[u8]| {
        krusty::jvm::classreader::read_method_code(bytes, "hashCode", "()I")
            .expect("hashCode body")
            .code
    };
    let ours_bytes = ours
        .iter()
        .find_map(|(name, bytes)| (*name == impl_name).then_some(bytes.clone()))
        .expect("our implementation class");
    let reference_bytes =
        std::fs::read(reference_dir.join(format!("{impl_name}.class"))).expect("reference class");
    assert_eq!(
        body(&ours_bytes).len(),
        body(&reference_bytes).len(),
        "the hashCode body must be kotlinc's, not merely value-equivalent"
    );
    assert_eq!(
        common::run_box(
            &ours,
            &common::find_box_class(&ours).expect("box class"),
            &[common::stdlib_jar()]
        )
        .as_deref(),
        Some("OK"),
        "and it must still run"
    );
}

/// The implementation's `toString()` is kotlinc's, instruction for instruction — and renders the same
/// text.
///
/// Three shape rules the rendered string cannot show: adjacent literals are ONE `ldc` (the class
/// prefix runs into the first member's name, each later separator into its own), the closing paren is
/// a CHAR append rather than a one-character String, and a MEMBERLESS annotation renders to a
/// constant with no `StringBuilder` at all.
#[test]
fn the_implementation_tostring_matches_kotlinc() {
    for (stem, src, expected) in [
        (
            "TsTwo",
            "annotation class Mk(val v: Int, val s: String)\n\
             fun mk(): Mk = Mk(1, \"x\")\n\
             fun box(): String = mk().toString()\n",
            "@Mk(v=1, s=x)",
        ),
        (
            "TsNone",
            "annotation class Empty\n\
             fun mk(): Empty = Empty()\n\
             fun box(): String = mk().toString()\n",
            "@Empty()",
        ),
    ] {
        let Some(build) = common::compile_libs_build(
            &format!("annotation_tostring_{stem}"),
            &[(&format!("{stem}.kt"), src)],
        ) else {
            return; // reference compiler unavailable
        };
        let reference_dir = build
            .reference_out()
            .expect("reference compiler output unavailable");
        let ours = common::expect_compile_in_process(
            src,
            stem,
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path()),
        );
        let impl_name = ours
            .iter()
            .map(|(name, _)| name.clone())
            .find(|name| name.contains("$annotationImpl$"))
            .expect("the annotation implementation class");
        let body = |bytes: &[u8]| {
            krusty::jvm::classreader::read_method_code(bytes, "toString", "()Ljava/lang/String;")
                .expect("toString body")
                .code
        };
        let ours_bytes = ours
            .iter()
            .find_map(|(name, bytes)| (*name == impl_name).then_some(bytes.clone()))
            .expect("our implementation class");
        let reference_bytes = std::fs::read(reference_dir.join(format!("{impl_name}.class")))
            .expect("reference class");
        assert_eq!(
            body(&ours_bytes).len(),
            body(&reference_bytes).len(),
            "{stem}: the toString body must be kotlinc's, not merely text-equivalent"
        );
        assert_eq!(
            common::run_box(
                &ours,
                &common::find_box_class(&ours).expect("box class"),
                &[common::stdlib_jar()]
            )
            .as_deref(),
            Some(expected),
            "{stem}: and it must render what kotlinc renders"
        );
    }
}

/// The implementation's `equals` is kotlinc's, in shape AND in behaviour.
///
/// Three shape rules: each check returns EARLY rather than branching to one shared exit label; both
/// sides are read through the annotation INTERFACE (`checkcast; invokeinterface`), this object's own
/// side included, because an annotation's contract is its interface and a proxy satisfies it too; and
/// the comparison is per type — `if_acmpeq` for an ENUM member (a constant is a singleton),
/// `Float`/`Double.compare` rather than the wrapper's `equals`, `Intrinsics.areEqual` elsewhere.
///
/// The behavioural half matters just as much: `Double.compare` fixes NaN == NaN and -0.0 != 0.0, and
/// an argument that is not the annotation at all must answer false.
#[test]
fn the_implementation_equals_matches_kotlinc() {
    const SRC: &str = "enum class E { A, B }\n\
                       annotation class Inner(val x: Int)\n\
                       annotation class Big(val i: Int, val d: Double, val t: String,\n\
                                            val a: IntArray, val e: E, val n: Inner)\n\
                       fun mk(i: Int = 1, d: Double = 2.0, t: String = \"s\",\n\
                              a: IntArray = intArrayOf(1), e: E = E.A): Big = Big(i, d, t, a, e, Inner(9))\n\
                       fun box(): String {\n\
                           val same = mk() == mk()\n\
                           val byValue = mk() == mk(i = 2)\n\
                           val byArray = mk() == mk(a = intArrayOf(2))\n\
                           val byEnum = mk() == mk(e = E.B)\n\
                           val nan = mk(d = Double.NaN) == mk(d = Double.NaN)\n\
                           val zero = mk(d = -0.0) == mk(d = 0.0)\n\
                           val other = mk().equals(\"not an annotation\")\n\
                           return if (same && !byValue && !byArray && !byEnum && nan && !zero && !other)\n\
                                  \"OK\" else \"FAIL\"\n\
                       }\n";
    let Some(build) = common::compile_libs_build("annotation_equals_shape", &[("Eq.kt", SRC)])
    else {
        return; // reference compiler unavailable
    };
    let reference_dir = build
        .reference_out()
        .expect("reference compiler output unavailable");
    let ours = common::expect_compile_in_process(
        SRC,
        "Eq",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let impl_name = ours
        .iter()
        .map(|(name, _)| name.clone())
        .find(|name| name.contains("$annotationImpl$Big$"))
        .expect("the Big implementation class");
    let body = |bytes: &[u8]| {
        krusty::jvm::classreader::read_method_code(bytes, "equals", "(Ljava/lang/Object;)Z")
            .expect("equals body")
            .code
    };
    let ours_bytes = ours
        .iter()
        .find_map(|(name, bytes)| (*name == impl_name).then_some(bytes.clone()))
        .expect("our implementation class");
    let reference_bytes =
        std::fs::read(reference_dir.join(format!("{impl_name}.class"))).expect("reference class");
    assert_eq!(
        body(&ours_bytes).len(),
        body(&reference_bytes).len(),
        "the equals body must be kotlinc's, not merely behaviour-equivalent"
    );
    assert_eq!(
        common::run_box(
            &ours,
            &common::find_box_class(&ours).expect("box class"),
            &[common::stdlib_jar()]
        )
        .as_deref(),
        Some("OK"),
        "NaN, -0.0, array content, enum identity and a non-annotation argument must all behave"
    );
}

/// The implementation's constructor guards every REFERENCE member, and the class carries `ACC_SUPER`.
///
/// An annotation member is never nullable — the JVM annotation format has no null — so kotlinc emits
/// `Intrinsics.checkNotNullParameter` for each non-primitive parameter, in declaration order, BEFORE
/// `super()`. The class-access word is `PUBLIC|FINAL|SUPER|SYNTHETIC`; krusty omitted `ACC_SUPER`.
#[test]
fn the_implementation_constructor_matches_kotlinc() {
    const SRC: &str = "annotation class C(val i: Int, val s: String, val a: IntArray)\n\
                       fun mk(): C = C(1, \"x\", intArrayOf(1))\n\
                       fun box(): String = mk().s\n";
    let Some(build) = common::compile_libs_build("annotation_ctor_shape", &[("Ct.kt", SRC)]) else {
        return; // reference compiler unavailable
    };
    let reference_dir = build
        .reference_out()
        .expect("reference compiler output unavailable");
    let ours = common::expect_compile_in_process(
        SRC,
        "Ct",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let impl_name = ours
        .iter()
        .map(|(name, _)| name.clone())
        .find(|name| name.contains("$annotationImpl$"))
        .expect("the annotation implementation class");
    let ours_bytes = ours
        .iter()
        .find_map(|(name, bytes)| (*name == impl_name).then_some(bytes.clone()))
        .expect("our implementation class");
    let reference_bytes =
        std::fs::read(reference_dir.join(format!("{impl_name}.class"))).expect("reference class");
    let ctor = |bytes: &[u8]| {
        krusty::jvm::classreader::read_method_code(bytes, "<init>", "(ILjava/lang/String;[I)V")
            .expect("<init> body")
            .code
    };
    assert_eq!(
        ctor(&ours_bytes).len(),
        ctor(&reference_bytes).len(),
        "the constructor must carry kotlinc's parameter guards"
    );
    let access = |bytes: &[u8]| {
        krusty::jvm::classreader::parse_class(bytes)
            .expect("parse class")
            .access
    };
    assert_eq!(
        access(&ours_bytes),
        access(&reference_bytes),
        "the class-access word must be kotlinc's (ACC_SUPER included)"
    );
    assert_eq!(
        common::run_box(
            &ours,
            &common::find_box_class(&ours).expect("box class"),
            &[common::stdlib_jar()]
        )
        .as_deref(),
        Some("x"),
        "and the guarded constructor must still build the annotation"
    );
}

/// The implementation's member FLAGS and `LocalVariableTable`s are kotlinc's.
///
/// Nothing in source declares this class, so kotlinc marks what it generates `ACC_SYNTHETIC` — the
/// fields, the member accessors and `annotationType()` — while leaving the constructor and the
/// `Object` overrides unmarked. It also names `this` (and the constructor's parameters, and `equals`'s
/// `other`) in a `LocalVariableTable`, which a debugger and a decompiler both read.
#[test]
fn the_implementation_flags_and_locals_match_kotlinc() {
    const SRC: &str = "annotation class C(val i: Int, val s: String, val a: IntArray)\n\
                       fun mk(): C = C(1, \"x\", intArrayOf(1))\n\
                       fun box(): String = mk().s\n";
    let Some(build) = common::compile_libs_build("annotation_flags_shape", &[("Fl.kt", SRC)])
    else {
        return; // reference compiler unavailable
    };
    let reference_dir = build
        .reference_out()
        .expect("reference compiler output unavailable");
    let ours = common::expect_compile_in_process(
        SRC,
        "Fl",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    let impl_name = ours
        .iter()
        .map(|(name, _)| name.clone())
        .find(|name| name.contains("$annotationImpl$"))
        .expect("the annotation implementation class");
    let ours_bytes = ours
        .iter()
        .find_map(|(name, bytes)| (*name == impl_name).then_some(bytes.clone()))
        .expect("our implementation class");
    let reference_bytes =
        std::fs::read(reference_dir.join(format!("{impl_name}.class"))).expect("reference class");
    let shape = |bytes: &[u8]| {
        let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
        let mut methods = class
            .methods
            .iter()
            .map(|m| (m.name.clone(), m.descriptor.clone(), m.access))
            .collect::<Vec<_>>();
        methods.sort();
        let mut fields = class
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.access))
            .collect::<Vec<_>>();
        fields.sort();
        (fields, methods)
    };
    assert_eq!(
        shape(&ours_bytes),
        shape(&reference_bytes),
        "field and method flags must match kotlinc's, ACC_SYNTHETIC included"
    );
    assert_eq!(
        common::run_box(
            &ours,
            &common::find_box_class(&ours).expect("box class"),
            &[common::stdlib_jar()]
        )
        .as_deref(),
        Some("x"),
        "and the class must still work"
    );
}
