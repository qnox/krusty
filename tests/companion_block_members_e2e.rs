//! `companion { … }` block members are static members of the classifier that declares the block:
//! kotlinc places their functions, accessors and backing fields on that class (initialized by the
//! class's `<clinit>`) and records them in the class's `@Metadata`, while a written
//! `companion fun C.f()` stays on the file facade.
use super::common;

const LANGUAGE: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n";

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(&format!("{LANGUAGE}{src}"), "Main")
}

/// Compile `src` with kotlinc (the feature enabled by flag) and krusty, and require each of
/// `classes` to carry exactly kotlinc's fields and methods, in class-file order with their access
/// flags, descriptors and generic signatures, and exactly kotlinc's `@Metadata`.
fn assert_members_and_metadata_match_kotlinc(stem: &str, src: &str, classes: &[&str]) {
    assert_members_and_metadata_match_kotlinc_on(stem, src, classes, &[common::stdlib_jar()]);
}

/// [`assert_members_and_metadata_match_kotlinc`] over `classpath`, also requiring each field's
/// `ConstantValue`. Returns the comparisons for further checks.
fn assert_members_and_metadata_match_kotlinc_on(
    stem: &str,
    src: &str,
    classes: &[&str],
    classpath: &[std::path::PathBuf],
) -> Vec<common::ReferenceComparison> {
    let source = format!("{LANGUAGE}{src}");
    let mut comparisons = Vec::new();
    for class in classes {
        let comparison = common::compare_with_kotlinc_plugin(
            stem,
            &source,
            class,
            classpath,
            "17",
            &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
        )
        .expect("reference kotlinc and javap are provisioned");
        let members = |bytes: &[u8]| {
            let info = krusty::jvm::classreader::parse_class(bytes).expect("a readable class file");
            let fields = info
                .fields
                .iter()
                .map(|field| {
                    format!(
                        "field {:#06x} {} {} {:?} {:?}",
                        field.access,
                        field.name,
                        field.descriptor,
                        field.signature,
                        field.const_value
                    )
                })
                .collect::<Vec<_>>();
            let methods = info.methods.iter().map(|method| {
                format!(
                    "method {:#06x} {}{} {:?}",
                    method.access, method.name, method.descriptor, method.signature
                )
            });
            fields.into_iter().chain(methods).collect::<Vec<_>>()
        };
        assert_eq!(
            members(&comparison.krusty_bytes),
            members(&comparison.reference_bytes),
            "{class}: kotlinc's member table"
        );
        assert_eq!(
            common::raw_kotlin_metadata(&comparison.krusty_bytes),
            common::raw_kotlin_metadata(&comparison.reference_bytes),
            "{class}: kotlinc's @Metadata"
        );
        comparisons.push(comparison);
    }
    comparisons
}

#[test]
fn block_members_are_static_members_of_their_class() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       val v: String = \"O\"\n\
        \x20       fun f() = v + g()\n\
        \x20   }\n\
        }\n\
        companion fun A.g() = \"K\"\n\
        fun box() = A.f()\n";
    assert_members_and_metadata_match_kotlinc("BlockMembers", SRC, &["A", "BlockMembersKt"]);
    assert_eq!(run(SRC).expect("block members"), "OK");
}

#[test]
fn block_member_calls_block_member_and_companion_extension_unqualified() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(s: String) = g(s) + h()\n\
        \x20       fun h() = \"K\"\n\
        \x20   }\n\
        }\n\
        companion fun A.g(s: String) = s\n\
        fun box() = A.f(\"O\")\n";
    assert_eq!(run(SRC).expect("unqualified companion calls"), "OK");
}

#[test]
fn instance_member_calls_inherited_and_private_block_members() {
    const SRC: &str = "open class Base {\n\
        \x20   companion { fun base() = \"O\" }\n\
        }\n\
        class C : Base() {\n\
        \x20   companion {\n\
        \x20       private val k = \"K\"\n\
        \x20       private fun own() = k\n\
        \x20   }\n\
        \x20   fun ok() = base() + own()\n\
        }\n\
        fun box() = C().ok()\n";
    assert_eq!(run(SRC).expect("instance member calls"), "OK");
}

#[test]
fn block_property_initializes_with_its_class_not_the_file() {
    const SRC: &str = "var initialized = false\n\
        fun initialize(): String {\n\
        \x20   initialized = true\n\
        \x20   return \"\"\n\
        }\n\
        class Foo {\n\
        \x20   companion { val p = initialize() }\n\
        }\n\
        companion val Foo.greeting: String = \"hi\"\n\
        fun box(): String {\n\
        \x20   if (Foo.greeting != \"hi\") return \"greeting\"\n\
        \x20   if (initialized) return \"a companion extension initialized its classifier\"\n\
        \x20   Foo.p\n\
        \x20   return if (initialized) \"OK\" else \"reading a block property did not\"\n\
        }\n";
    assert_eq!(run(SRC).expect("initialization order"), "OK");
}

#[test]
fn nested_class_block_members_belong_to_the_nested_class() {
    const SRC: &str = "class Outer {\n\
        \x20   class Nested {\n\
        \x20       companion { val v = \"OK\" }\n\
        \x20   }\n\
        }\n\
        fun box() = Outer.Nested.v\n";
    assert_members_and_metadata_match_kotlinc("NestedBlock", SRC, &["Outer$Nested"]);
    assert_eq!(run(SRC).expect("nested block"), "OK");
}

#[test]
fn block_property_beside_companion_object_property_keeps_its_field_name() {
    const SRC: &str = "class E {\n\
        \x20   companion { val value = \"O\" }\n\
        \x20   companion object { val value = \"K\" }\n\
        }\n\
        fun box() = E.value + E.Companion.value\n";
    assert_members_and_metadata_match_kotlinc("FieldNames", SRC, &["E"]);
    assert_eq!(run(SRC).expect("field names"), "OK");
}

#[test]
fn property_reference_to_block_property_reads_its_class() {
    const SRC: &str = "class C {\n\
        \x20   companion { var p = \"FAIL\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   C::p.set(\"OK\")\n\
        \x20   return (C::p)()\n\
        }\n";
    assert_eq!(run(SRC).expect("property reference"), "OK");
}

/// A block property with custom accessors is read and written through its class's static
/// accessors, unqualified from the class body and qualified from outside it.
#[test]
fn block_property_accessors_are_statics_of_their_class() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       val p: String get() = \"O\"\n\
        \x20       var backing: String = \"\"\n\
        \x20       var q: String\n\
        \x20           get() = backing\n\
        \x20           set(value) { backing = value }\n\
        \x20   }\n\
        \x20   fun g(): String { q = \"K\"; return p + q }\n\
        }\n\
        fun box(): String {\n\
        \x20   val r = C().g() + C.p + C.q\n\
        \x20   return if (r == \"OKOK\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("block property accessors"), "OK");
}

#[test]
fn bare_classifier_call_invokes_block_and_extension_operators() {
    const SRC: &str = "class C(val s: String) {\n\
        \x20   companion { operator fun invoke(i: Int) = \"O\" }\n\
        \x20   companion object { operator fun invoke(c: Char) = \"FAIL\" }\n\
        }\n\
        class E\n\
        companion operator fun E.invoke(s: String) = s\n\
        fun box() = C(\"\").s + C(1) + E(\"K\")\n";
    assert_eq!(run(SRC).expect("implicit companion invoke"), "OK");
}

#[test]
fn companion_extensions_with_context_parameters_see_their_classifier_scope() {
    const SRC: &str = "class A { val k = \"K\" }\n\
        class C\n\
        context(a: A)\n\
        companion val C.o get() = \"O\"\n\
        context(a: A)\n\
        companion fun C.k() = a.k\n\
        context(_: A)\n\
        companion fun C.ok(): String = o + k()\n\
        fun <T, R> within(value: T, block: T.() -> R): R = value.block()\n\
        fun box() = within(A()) { C.ok() }\n";
    assert_eq!(run(SRC).expect("context companion extensions"), "OK");
}

#[test]
fn references_inside_a_block_name_block_members() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       lateinit var value: String\n\
        \x20       fun initialized() = ::value.isInitialized\n\
        \x20       fun k() = \"K\"\n\
        \x20       fun ref() = ::k\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (C.initialized()) return \"initialized\"\n\
        \x20   C.value = \"O\"\n\
        \x20   return C.value + C.ref()()\n\
        }\n";
    assert_eq!(run(SRC).expect("block member references"), "OK");
}

#[test]
fn static_scope_writes_and_increments_companion_properties() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       var n = 0\n\
        \x20       var s = \"\"\n\
        \x20       fun bump() { n++; ++n; n += 2; s += \"K\" }\n\
        \x20   }\n\
        }\n\
        companion var C.m = 1\n\
        companion fun C.more() { m++; m *= 3 }\n\
        fun box(): String {\n\
        \x20   C.bump()\n\
        \x20   C.more()\n\
        \x20   return if (\"${C.n}${C.s}${C.m}\" == \"4K6\") \"OK\" else \"${C.n}${C.s}${C.m}\"\n\
        }\n";
    assert_eq!(run(SRC).expect("static-scope writes"), "OK");
}

#[test]
fn companion_extension_has_no_value_receiver() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C { fun m() = 1 }\n\
        companion fun C.f() = this\n\
        companion fun C.g() = m()\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn kotlinc_resolves_block_members_from_krusty_metadata() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = \"\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   A.bar = \"K\"\n\
        \x20   return A.foo() + A.bar\n\
        }\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{LIB}"), "Lib");
    let dir = common::scratch_dir().expect("scratch directory");
    let lib = dir.join("lib");
    for (name, bytes) in &classes {
        let path = lib.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class directory")).expect("mkdir");
        std::fs::write(&path, bytes).expect("write class file");
    }
    let main = dir.join("Main.kt");
    std::fs::write(&main, MAIN).expect("write main");
    let args = [
        "-d".to_string(),
        dir.join("out").to_string_lossy().into_owned(),
        "-XXLanguage:+CompanionBlocksAndExtensions".to_string(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        main.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        return;
    };
    assert_eq!(
        code, 0,
        "kotlinc rejected krusty's class metadata: {stderr}"
    );
}

/// A block declared in another file of the module is reached through the module's declarations:
/// calls (with a default argument), reads, writes and property references all name the declaring
/// class, never a file facade.
#[test]
fn block_members_from_another_file_are_statics_of_their_class() {
    const DECLARING: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(suffix: String = \"K\"): String = \"O\" + suffix\n\
        \x20       var v: String = \"\"\n\
        \x20       val p: String get() = \"!\"\n\
        \x20   }\n\
        }\n";
    const USING: &str = "fun box(): String {\n\
        \x20   A.v = A.f()\n\
        \x20   val read = A::p\n\
        \x20   val written = A::v\n\
        \x20   val r = A.f(\"k\") + written() + read()\n\
        \x20   return if (r == \"OkOK!\") \"OK\" else r\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("A.kt", &format!("{LANGUAGE}{DECLARING}")),
            ("Main.kt", &format!("{LANGUAGE}{USING}")),
        ],
        "SeparateFileBlock",
    );
}

#[test]
fn library_block_members_are_statics_of_their_class() {
    const LIB: &str = "var initialized = false\n\
        fun initialize(): String { initialized = true; return \"\" }\n\
        open class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = initialize()\n\
        \x20       const val SUFFIX = \"!\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "class B : A() {\n\
        \x20   companion { fun own() = \"K\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (initialized) return \"a companion block initialized before its class\"\n\
        \x20   A.bar = B.own()\n\
        \x20   if (!initialized) return \"writing A.bar did not initialize A\"\n\
        \x20   return if (A.SUFFIX == \"!\") A.foo() + A.bar else \"A.SUFFIX is \" + A.SUFFIX\n\
        }\n";
    let result = common::expect_box_run_against(
        "companion-block-library",
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// A library block's `const val` is a compile-time constant where it is used: kotlinc folds `A.N`
/// into another `const val`'s `ConstantValue`, into an annotation argument and into an ordinary
/// expression, and none of them reads a field of `A`.
#[test]
fn library_block_const_is_a_compile_time_constant() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       const val N = 7\n\
        \x20   }\n\
        }\n\
        annotation class Tag(val n: Int)\n";
    const MAIN: &str = "const val M = A.N + 1\n\
        @Tag(A.N) fun tagged() {}\n\
        fun read(): Int = A.N\n\
        fun box(): String = if (M == 8 && read() == 7) \"OK\" else \"M is \" + M\n";
    let library = common::kotlinc_library(&format!("{LANGUAGE}{LIB}"))
        .expect("reference kotlinc is provisioned");
    let comparisons = assert_members_and_metadata_match_kotlinc_on(
        "LibraryConst",
        MAIN,
        &["LibraryConstKt"],
        &[library, common::stdlib_jar()],
    );
    let comparison = &comparisons[0];
    for method in ["read()", "tagged()"] {
        assert_eq!(
            common::method_instructions(&comparison.krusty, method),
            common::method_instructions(&comparison.reference, method),
            "LibraryConstKt.{method}: kotlinc's instructions"
        );
    }
    let annotations = |javap: &str| {
        javap
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("Tag("))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        annotations(&comparison.krusty),
        annotations(&comparison.reference),
        "tagged's annotation argument"
    );
}

/// The same uses as [`block_members_from_another_file_are_statics_of_their_class`] against a
/// compiled library: krusty builds the library, reads its block members back through its classpath
/// provider, and names their declaring class for calls with defaults, reads, writes and references.
#[test]
fn compiled_library_block_members_are_statics_of_their_class() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(suffix: String = \"K\"): String = \"O\" + suffix\n\
        \x20       var v: String = \"\"\n\
        \x20       val p: String get() = \"!\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   A.v = A.f()\n\
        \x20   val read = A::p\n\
        \x20   val written = A::v\n\
        \x20   val r = A.f(\"k\") + written() + read()\n\
        \x20   return if (r == \"OkOK!\") \"OK\" else r\n\
        }\n";
    let result = common::expect_box_run_against(
        "companion-block-library-uses",
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

#[test]
fn static_scope_selects_nearest_applicable_associated_function() {
    // Inside a class, its own block members precede its companion object's members and top-level
    // functions; a farther classifier's block member is reached when the nearer one does not
    // apply, and an instance member still precedes every block member.
    const SRC: &str = "fun f() = \"top\"\n\
        fun h(x: Int) = \"top-h\"\n\
        open class Base { companion { fun g(s: String) = \"base-g\"; fun k() = \"base-k\" } }\n\
        class C : Base() {\n\
        \x20   companion { fun f() = \"block\"; fun g(i: Int) = \"c-g\"; fun k() = \"c-k\" }\n\
        \x20   companion object { fun f() = \"obj\"; fun h(x: Int) = \"obj-h\" }\n\
        \x20   fun t1() = f()\n\
        \x20   fun t2() = g(\"s\")\n\
        \x20   fun t3() = k()\n\
        }\n\
        class D {\n\
        \x20   fun f(x: Int = 0) = \"member\"\n\
        \x20   companion { fun f() = \"block\" }\n\
        \x20   fun t() = f()\n\
        }\n\
        class E { companion { fun h(x: Int) = \"block-h\" } }\n\
        companion fun E.t() = h(1)\n\
        fun box(): String {\n\
        \x20   val r = C().t1() + \",\" + C().t2() + \",\" + C().t3() + \",\" + D().t() + \",\" + E.t()\n\
        \x20   return if (r == \"block,base-g,c-k,member,block-h\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("static scope precedence"), "OK");
}

#[test]
fn associated_declarations_are_not_members_of_instances() {
    // A companion extension names its classifier, not a value of it: kotlinc rejects `C().f()`.
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C\n\
        companion fun C.f() = \"f\"\n\
        companion val C.q get() = 2\n\
        fun use() {\n\
        \x20   C().f()\n\
        \x20   C().q\n\
        }\n";
    common::assert_errors_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

/// A library block property with a context parameter is read through its class's static getter,
/// which takes the context argument and nothing else.
#[test]
fn library_block_property_takes_its_context_argument() {
    const LIB: &str = "class Ctx(val s: String)\n\
        class A {\n\
        \x20   companion {\n\
        \x20       context(c: Ctx) val greeting: String get() = c.s + \"K\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun <T, R> within(value: T, block: T.() -> R): R = value.block()\n\
        fun box(): String = within(Ctx(\"O\")) { A.greeting }\n";
    let result = common::expect_box_run_against_kotlinc(
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// An `internal` block property of a kotlinc-built library is invisible outside its module even
/// though its JVM accessor is public: both compilers reject the read with the same diagnostics,
/// and a friend module reads it.
#[test]
fn library_internal_block_property_is_invisible_outside_its_module() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       internal val hidden: Int get() = 1\n\
        \x20       val shown: Int get() = 2\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun read(): Int = A.hidden + A.shown\n";
    let library = common::kotlinc_library(&format!("{LANGUAGE}{LIB}"))
        .expect("reference kotlinc is provisioned");
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let main = format!("{LANGUAGE}{MAIN}");
    let result = common::compiler_diagnostics_with_reference_args(
        &[("Main.kt", &main)],
        &classpath,
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
    common::expect_identical_rejection(&result, "internal library block property");
    assert_eq!(
        common::front_end_diagnostics_with_friend_paths(
            &main,
            &classpath,
            std::slice::from_ref(&library),
            None,
        ),
        Vec::<String>::new()
    );
}

/// An overloaded `::f` naming a block function from its class's static scope is selected by the
/// expected function type, in an inferred signature as in a body.
#[test]
fn static_scope_reference_is_selected_by_the_expected_function_type() {
    const SRC: &str = "class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"int\"\n\
        \x20       fun f(x: String): String = \"string\"\n\
        \x20   }\n\
        \x20   fun pick() = apply1(::f)\n\
        \x20   fun body(): String {\n\
        \x20       val g: (Int) -> String = ::f\n\
        \x20       return g(1)\n\
        \x20   }\n\
        }\n\
        fun apply1(g: (String) -> String) = g(\"s\")\n\
        fun box(): String {\n\
        \x20   val r = C().pick() + \",\" + C().body()\n\
        \x20   return if (r == \"string,int\") \"OK\" else r\n\
        }\n";
    assert_eq!(run(SRC).expect("expected reference type"), "OK");
}

/// Without an expected type the same overloaded `::f` is kotlinc's ambiguity, reported with both
/// candidates.
#[test]
fn static_scope_reference_without_an_expected_type_is_ambiguous() {
    const SRC: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
        class C {\n\
        \x20   companion {\n\
        \x20       fun f(x: Int): String = \"int\"\n\
        \x20       fun f(x: String): String = \"string\"\n\
        \x20   }\n\
        \x20   fun pick() = ::f\n\
        }\n";
    common::assert_error_blocks_match_kotlinc(
        &[("Main.kt", SRC)],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

/// A class's static scope directly follows its own instance receiver in the unqualified tower, so
/// an inner class's block declaration shadows the outer class's member of the same name, for a
/// call, a read and a `::f` reference, in checked bodies and inferred signatures alike.
#[test]
fn inner_static_scope_precedes_an_outer_implicit_receiver() {
    const SRC: &str = "class Outer {\n\
        \x20   fun f(): String = \"outer\"\n\
        \x20   val p: String get() = \"outer\"\n\
        \x20   inner class Inner {\n\
        \x20       companion {\n\
        \x20           fun f(): String = \"inner\"\n\
        \x20           val p: String get() = \"inner\"\n\
        \x20       }\n\
        \x20       fun call(): String = f()\n\
        \x20       fun read(): String = p\n\
        \x20       fun ref(): String = (::f)()\n\
        \x20       fun inferredCall() = f()\n\
        \x20       fun inferredRead() = p\n\
        \x20       fun inferredRef() = ::f\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val i = Outer().Inner()\n\
        \x20   val r = i.call() + i.read() + i.ref() + i.inferredCall() + i.inferredRead() +\n\
        \x20       i.inferredRef()()\n\
        \x20   return if (r == \"innerinnerinnerinnerinnerinner\") \"OK\" else r\n\
        }\n";
    common::expect_box_same_as_kotlinc(&format!("{LANGUAGE}{SRC}"), "InnerStaticScope");
}
