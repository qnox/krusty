use super::common;
use std::collections::BTreeMap;

/// Every method's `MethodParameters` rows as `name/flags`, keyed by `name+descriptor`.
fn method_parameters(bytes: &[u8], file_name: &str) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);

    let mut out = Vec::new();
    let mut member = String::new();
    let mut rows: Option<Vec<String>> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if line.starts_with("  ") && !line.starts_with("   ") && trimmed.contains('(') {
            if let Some(collected) = rows.take() {
                out.push(format!("{member} -> {}", collected.join(",")));
            }
            member = trimmed.to_string();
        }
        if trimmed == "MethodParameters:" {
            rows = Some(Vec::new());
            continue;
        }
        if let Some(collected) = rows.as_mut() {
            if trimmed.starts_with("Name") && trimmed.contains("Flags") {
                continue;
            }
            // A parameter row is `<name>` or `<name>  <flags>`; anything else ends the attribute.
            let mut fields = trimmed.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), flags, None)
                    if name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '$' | '_' | '-' | '#')) =>
                {
                    collected.push(format!("{name}/{}", flags.unwrap_or("-")));
                }
                _ => {
                    out.push(format!("{member} -> {}", collected.join(",")));
                    rows = None;
                }
            }
        }
    }
    if let Some(collected) = rows {
        out.push(format!("{member} -> {}", collected.join(",")));
    }
    out
}

fn parameters_by_method(rows: Vec<String>) -> BTreeMap<String, String> {
    let mut keyed = BTreeMap::new();
    for row in rows {
        let (method, parameters) = row
            .split_once(" -> ")
            .expect("MethodParameters row has a method key");
        assert!(
            keyed
                .insert(method.to_string(), parameters.to_string())
                .is_none(),
            "duplicate MethodParameters row for {method}"
        );
    }
    keyed
}

fn kotlinc_classes(source_name: &str, source: &str) -> std::path::PathBuf {
    let root = common::scratch_dir().expect("reference scratch dir");
    let out = root.join("classes");
    std::fs::create_dir_all(&out).expect("reference output dir");
    let source_path = root.join(format!("{source_name}.kt"));
    std::fs::write(&source_path, source).expect("reference source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-java-parameters".to_string(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    out
}

fn compile_java_parameters(
    source_name: &str,
    source: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::source::SourceInput;

    let mut diagnostics = DiagSink::new();
    let stems = vec![source_name.to_string()];
    let inputs = vec![SourceInput::kotlin(source).with_file_stem(source_name)];
    let classpath = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );
    let backend = krusty::jvm::JvmBackend::new(classpath).with_java_parameters(true);
    let outputs =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diagnostics);
    (!diagnostics.has_errors()).then(|| {
        outputs
            .into_iter()
            .map(|(path, bytes)| {
                (
                    path.strip_suffix(".class").unwrap_or(&path).to_string(),
                    bytes,
                )
            })
            .collect()
    })
}

const SOURCE: &str = "package demo\n\
    data class Acc(val redirectTo: String, val count: Int)\n\
    class Leaf {\n\
    \x20 suspend fun pull(id: String): String = id\n\
    }\n\
    class Svc(private val dep: String, private val leaf: Leaf) {\n\
    \x20 fun plain(one: String, two: Int): String = one + two\n\
    \x20 suspend fun waits(one: String): String {\n\
    \x20\x20 val first = leaf.pull(one)\n\
    \x20\x20 return first + dep\n\
    \x20 }\n\
    }\n\
    fun topLevel(a: String, b: Int): String = a + b\n";

const GENERATED_SOURCE: &str = "package demo\n\
    enum class Entry(val code: Int) { ONE(1), TWO(2) }\n\
    data class GeneratedData(val text: String, val count: Int = 1)\n\
    class GeneratedOuter(val seed: Int) {\n\
    \x20 inner class Inner(val value: String)\n\
    \x20 constructor(seed: Int, suffix: String) : this(seed + suffix.length)\n\
    \x20 fun String.extension(step: Int = 1): String = this + step + seed\n\
    \x20 suspend fun suspended(input: String): String = input\n\
    }\n\
    @JvmInline value class Wrapped(val raw: String)\n\
    @JvmInline value class Rich(val raw: String) {\n\
    \x20 val size: Int get() = raw.length\n\
    \x20 fun append(suffix: String): String = raw + suffix\n\
    }\n\
    fun localFactory(seed: String): Any {\n\
    \x20 class Local(val count: Int) { fun text(): String = seed + count }\n\
    \x20 return Local(1)\n\
    }\n\
    fun anonymousFactory(seed: String): Any = object { fun text(): String = seed }\n\
    interface Face { fun declared(name: String): String = name }\n";

const ENUM_CONSTRUCTOR_SOURCE: &str = "package demo\n\
    enum class Shape(val label: String, val sides: Int) {\n\
    \x20 BOX(\"box\", 4),\n\
    \x20 BLANK,\n\
    \x20 NAMED(\"named\"),\n\
    \x20 SIDED(6);\n\
    \x20 constructor(label: String) : this(label, 1)\n\
    \x20 constructor(sides: Int = 0) : this(\"auto\", sides)\n\
    }\n";

fn assert_parameter_parity(source_name: &str, source: &str, expected_classes: &[&str]) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let reference_dir = kotlinc_classes(source_name, source);
    let classes = compile_java_parameters(
        source_name,
        source,
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile with -java-parameters");

    let mut rows = 0;
    let mut mismatches = Vec::new();
    for name in expected_classes {
        let reference = std::fs::read(reference_dir.join(format!("{name}.class")))
            .unwrap_or_else(|error| panic!("read kotlinc {name}: {error}"));
        let ours = classes
            .iter()
            .find_map(|(class, bytes)| (class == name).then_some(bytes))
            .unwrap_or_else(|| {
                panic!(
                    "krusty did not emit expected class {name}; emitted {:?}",
                    classes.iter().map(|(class, _)| class).collect::<Vec<_>>()
                )
            });
        let simple = name.rsplit('/').next().expect("simple name");
        let reference_rows = parameters_by_method(method_parameters(
            &reference,
            &format!("ref-{simple}.class"),
        ));
        rows += reference_rows.len();
        let ours_rows =
            parameters_by_method(method_parameters(ours, &format!("ours-{simple}.class")));
        if ours_rows != reference_rows {
            mismatches.push(format!(
                "{name}:\n  krusty:  {ours_rows:?}\n  kotlinc: {reference_rows:?}"
            ));
        }
    }
    assert!(
        rows > expected_classes.len(),
        "fixture must cover parameterized methods"
    );
    assert!(
        mismatches.is_empty(),
        "ordered MethodParameters attributes must match kotlinc exactly:\n{}",
        mismatches.join("\n")
    );
    let root = reference_dir
        .parent()
        .expect("reference output has a scratch root");
    let _ = std::fs::remove_dir_all(root);
}

/// `-java-parameters` makes kotlinc write a `MethodParameters` attribute naming each declared
/// parameter — including `$completion`, the continuation a `suspend fun` appends. krusty accepted the
/// flag and ignored it, so every class of a module built with it differed. A framework that reads
/// parameter names by reflection needs the attribute, and byte parity needs it exactly where kotlinc
/// puts it: after the annotation attributes, and never on a `$default` bridge or a synthetic marker
/// constructor.
#[test]
fn java_parameters_names_every_declared_parameter() {
    assert_parameter_parity(
        "JavaParameters",
        SOURCE,
        &["demo/Acc", "demo/Leaf", "demo/Svc", "demo/JavaParametersKt"],
    );
}

/// Compiler-generated methods and parameters have declaration-specific names and flags. Missing
/// the attribute is not an acceptable fallback: all supported generated shapes match kotlinc.
#[test]
fn java_parameters_names_generated_and_prefixed_parameters() {
    assert_parameter_parity(
        "JavaParametersGenerated",
        GENERATED_SOURCE,
        &[
            "demo/Entry",
            "demo/GeneratedData",
            "demo/GeneratedOuter",
            "demo/GeneratedOuter$Inner",
            "demo/Wrapped",
            "demo/Rich",
            "demo/JavaParametersGeneratedKt$anonymousFactory$1",
            "demo/JavaParametersGeneratedKt",
            "demo/Face",
            "demo/Face$DefaultImpls",
        ],
    );
}

/// New anonymous context parameters are not legacy context receivers. Reflection still sees a
/// type-derived parameter name, and equal type stems are disambiguated in declaration order.
#[test]
fn java_parameters_names_anonymous_context_parameters() {
    assert_parameter_parity(
        "JavaParametersAnonymousContext",
        "// LANGUAGE: +ContextParameters\n\
         package demo\n\
         context(_: String, _: String) fun inspect(value: String): String = value\n\
         context(_: Int) fun count(value: Int): Int = value\n",
        &["demo/JavaParametersAnonymousContextKt"],
    );
}

/// An enum's SECONDARY constructors carry the same synthetic `(String, int)` prefix its primary
/// does, and kotlinc describes it: `$enum$name`, `$enum$ordinal`, then the source parameters, the
/// two synthetics flagged. krusty threaded the prefix into the DESCRIPTOR only, so the identity
/// description still counted the declared parameters alone and this path asserted out —
/// "secondary constructor identities must match its physical JVM parameters" — the moment
/// `-java-parameters` was on. Both an argument-less and a parameterized secondary are covered,
/// because only the second distinguishes "the prefix is described" from "the table is empty" — and
/// a DEFAULTED one, whose synthetic overload carries the prefix in its descriptor while kotlinc
/// gives it no `MethodParameters` at all.
#[test]
fn java_parameters_names_an_enum_secondary_constructors_prefix() {
    assert_parameter_parity(
        "JavaParametersEnumSecondary",
        ENUM_CONSTRUCTOR_SOURCE,
        &["demo/Shape"],
    );
}

/// A lambda or local function lifted out of a value class's static member, accessor or
/// `constructor-impl` reflects its captured receiver under the name of the value that static
/// realizes it as (`$arg0`, `$tmp0`, `$tmp0_$this`), synthetic like every captured parameter, and
/// the member's own carrier `arg0` is synthetic too. kotlinc flags every captured parameter of a
/// lifted callable synthetic, an ordinary class's `this$0` and a captured value or extension
/// receiver included.
#[test]
fn java_parameters_names_a_value_class_receiver_capture() {
    assert_parameter_parity(
        "JavaParametersValueCapture",
        "package demo\n\
         @JvmInline value class Tag(val raw: String)\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 init { val f = { this as Any }; f() }\n\
         \x20 constructor(t: Tag, u: Tag) : this(u.raw) { val g = { this as Any }; g() }\n\
         \x20 fun m(): () -> Any = { this }\n\
         \x20 val p: () -> Any get() = { this }\n\
         \x20 fun local(): Any {\n\
         \x20\x20 fun loc(): Any = this\n\
         \x20\x20 return loc()\n\
         \x20 }\n\
         }\n\
         class Plain { fun m(): () -> Any = { this } }\n\
         fun top(t: Tag): () -> Any = { t }\n\
         fun Tag.ext(): () -> Any = { this }\n",
        &[
            "demo/Held",
            "demo/Plain",
            "demo/JavaParametersValueCaptureKt",
        ],
    );
}

/// A lambda compiled to a class of its own, such as one returning a value class, reflects its
/// constructor's captures under their field names, every one synthetic: a value-class static's
/// receiver as `$arg0`, `$tmp0` or `$tmp0_$this`, an ordinary instance as `this$0` and an extension
/// receiver as `$this_ext`, though the constructor's local-variable table calls the latter two
/// `$receiver`.
#[test]
fn java_parameters_names_a_lambda_class_constructors_captures() {
    assert_parameter_parity(
        "JavaParametersLambdaClass",
        "package demo\n\
         @JvmInline value class Tag(val raw: String)\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 init { val f: () -> Held = { this }; f() }\n\
         \x20 constructor(t: Tag, u: Tag) : this(u.raw) { val g: () -> Held = { this }; g() }\n\
         \x20 fun m(x: Int): (Int) -> Held = { y -> if (x > y) this else this }\n\
         }\n\
         class Plain(val raw: String) { fun m(): () -> Held = { Held(raw) } }\n\
         fun Held.ext(): () -> Held = { this }\n",
        &[
            "demo/Held$f$1",
            "demo/Held$g$1",
            "demo/Held$m$1",
            "demo/Plain$m$1",
            "demo/JavaParametersLambdaClassKt$ext$1",
        ],
    );
}

/// An anonymous object or local class written in a value class reflects the receiver it captures,
/// synthetic, under the name of the value the member's static holds it in (`$arg0`, `$tmp0`,
/// `$tmp0_$this`); an ordinary class's instance stays `this$0`.
#[test]
fn java_parameters_names_an_anonymous_objects_value_class_receiver() {
    assert_parameter_parity(
        "JavaParametersObjectCapture",
        "package demo\n\
         interface Box { fun get(k: Int): Any }\n\
         @JvmInline value class Tag(val raw: String)\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 init { val o = object : Box { override fun get(k: Int): Any = this@Held }; o.get(0) }\n\
         \x20 constructor(t: Tag, u: Tag) : this(u.raw) {\n\
         \x20   val o2 = object : Box { override fun get(k: Int): Any = this@Held }; o2.get(0)\n\
         \x20 }\n\
         \x20 fun m(x: Int): Box = object : Box {\n\
         \x20   override fun get(k: Int): Any = if (raw.length > x + k) this@Held else x\n\
         \x20 }\n\
         \x20 fun loc(): Box { class L : Box { override fun get(k: Int): Any = this@Held }; return L() }\n\
         }\n\
         class Plain(val raw: String) {\n\
         \x20 fun m(): Box = object : Box { override fun get(k: Int): Any = this@Plain }\n\
         }\n",
        &[
            "demo/Held$o$1",
            "demo/Held$o2$1",
            "demo/Held$m$1",
            "demo/Held$loc$L",
            "demo/Plain$m$1",
        ],
    );
}

/// A value-class member's extension receiver is a parameter of the member's static, so the class
/// of a lambda, suspend lambda, anonymous object or local class written there reflects it as a
/// captured value, synthetic, under its field name `$this_mext`; so does an ordinary class's or a
/// local function's, though their constructors' local-variable tables call it `$receiver`. A suspend
/// lambda's class reflects its constructor's captures and `$completion`, `create`'s `value` and
/// `$completion`, and the typed `invoke`'s `p1`, `p2`, … like kotlinc.
#[test]
fn java_parameters_names_a_value_class_member_extension_receiver_capture() {
    assert_parameter_parity(
        "JavaParametersMemberExtension",
        "package demo\n\
         interface Box { fun get(k: Int): Any }\n\
         class Two(val first: Any, val second: Any)\n\
         @JvmInline value class Tag(val raw: String)\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 fun Tag.mext(): () -> Held = { Held(raw + this.raw) }\n\
         \x20 fun Tag.msus(): suspend () -> Two = { Two(this, this@Held) }\n\
         \x20 fun Tag.mo(): Box = object : Box { override fun get(k: Int): Any = this@mo }\n\
         \x20 fun Tag.mlc(): Box { class L : Box { override fun get(k: Int): Any = this@mlc }; return L() }\n\
         \x20 fun m(): Any {\n\
         \x20\x20 fun Tag.loc(): () -> Held = { Held(this.raw) }\n\
         \x20\x20 return Tag(raw).loc()\n\
         \x20 }\n\
         \x20 fun su(): suspend () -> Box = { object : Box { override fun get(k: Int): Any = this@Held } }\n\
         }\n\
         class Plain(val raw: String) {\n\
         \x20 fun Tag.pext(): () -> Held = { Held(raw + this.raw) }\n\
         }\n\
         fun ps(x: Int): suspend (Int) -> Two = { y -> Two(x, y) }\n\
         fun pair(): suspend (Int, Long) -> Long = { a, b -> a + b }\n",
        &[
            "demo/Held$mext$1",
            "demo/Held$msus$1",
            "demo/Held$mo$1",
            "demo/Held$mlc$L",
            "demo/Held$m$loc$1",
            "demo/Held$su$1",
            "demo/Held$su$1$1",
            "demo/Plain$pext$1",
            "demo/JavaParametersMemberExtensionKt$ps$1",
            "demo/JavaParametersMemberExtensionKt$pair$1",
        ],
    );
}

/// An object written in a member of an anonymous object or local class inside a value-class member
/// reflects the value class's instance as the static's value it captures, `$arg0`, synthetic,
/// ahead of the enclosing class's instance `this$0` when it captures that too.
#[test]
fn java_parameters_names_a_nested_objects_value_class_receiver() {
    assert_parameter_parity(
        "JavaParametersNestedObjectCapture",
        "package demo\n\
         interface Box { fun get(k: Int): Any }\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 fun inObj(): Box = object : Box {\n\
         \x20\x20 override fun get(k: Int): Any {\n\
         \x20\x20\x20 val o = object : Box { override fun get(k: Int): Any = this@Held }\n\
         \x20\x20\x20 return o.get(k)\n\
         \x20\x20 }\n\
         \x20 }\n\
         \x20 fun inLocal(): Box {\n\
         \x20\x20 class L(val n: Int) : Box {\n\
         \x20\x20\x20 override fun get(k: Int): Any {\n\
         \x20\x20\x20\x20 val o = object : Box { override fun get(k: Int): Any = this@Held.raw + this@L.n }\n\
         \x20\x20\x20\x20 return o.get(k)\n\
         \x20\x20\x20 }\n\
         \x20\x20 }\n\
         \x20\x20 return L(1)\n\
         \x20 }\n\
         }\n",
        &[
            "demo/Held$inObj$1",
            "demo/Held$inObj$1$get$o$1",
            "demo/Held$inLocal$L",
            "demo/Held$inLocal$L$get$o$1",
        ],
    );
}

/// kotlinc's value-class lowering moves a member's dispatch receiver into the static's carrier
/// `arg0`, flagged synthetic, and its extension receiver into an ordinary parameter `$this$name`,
/// flagged mandated: a member function's and a property accessor's alike. An extension receiver
/// that stays one has no flag: an ordinary class's member extension and a top-level extension.
#[test]
fn java_parameters_flags_a_value_class_static_members_extension_receiver_mandated() {
    assert_parameter_parity(
        "JavaParametersMovedExtensionReceiver",
        "package demo\n\
         @JvmInline value class Tag(val raw: String)\n\
         @JvmInline value class Held(val raw: String) {\n\
         \x20 fun Tag.mext(x: Int): Int = x\n\
         \x20 var String.pp: Int\n\
         \x20\x20 get() = length\n\
         \x20\x20 set(v) {}\n\
         \x20 fun String.withDefault(x: Int = 1): Int = x\n\
         }\n\
         class Plain(val raw: String) {\n\
         \x20 fun Tag.pmext(x: Int): Int = x\n\
         }\n\
         fun Tag.top(x: Int): Int = x\n",
        &[
            "demo/Held",
            "demo/Plain",
            "demo/JavaParametersMovedExtensionReceiverKt",
        ],
    );
}

/// A value class keeps an interface entry on its box for each member lowered to a static `-impl`:
/// the boxed override `abs(String)` calling `abs-impl`. kotlinc reflects the entry's parameters as
/// the static names them, less the carrier, and its extension receiver `$this$abs` is unflagged
/// there: it is the receiver of an instance method again. krusty wrote no `MethodParameters` on an
/// entry, nor on an abstract interface member. The boxed `equals(other)`, `constructor-impl`, the
/// `-impl` statics and `equals-impl0` keep theirs, `box-impl`, `unbox-impl`, the private
/// constructor and the bridges none.
#[test]
fn java_parameters_names_a_value_class_boxed_members() {
    assert_parameter_parity(
        "JavaParametersBoxedMembers",
        "package demo\n\
         interface Abs { fun String.abs(): Int; fun plain(x: Int): Int }\n\
         interface Gen<T> { fun f(t: T): T; fun Int.prop2(): String }\n\
         interface Sized { val pp: Int }\n\
         @JvmInline value class Held(val raw: String) : Abs, Gen<String>, Sized, Comparable<Held> {\n\
         \x20 override fun String.abs(): Int = length + raw.length\n\
         \x20 override fun plain(x: Int): Int = x\n\
         \x20 override fun f(t: String): String = t + raw\n\
         \x20 override fun Int.prop2(): String = raw\n\
         \x20 override val pp: Int get() = 1\n\
         \x20 override fun compareTo(other: Held): Int = 0\n\
         \x20 fun own(y: String): Int = y.length\n\
         }\n",
        &["demo/Held", "demo/Abs", "demo/Gen"],
    );
}

/// kotlinc writes an interface member's body on `DefaultImpls` as a static that takes the
/// interface instance as `$this`, synthetic, and the extension receiver as an ordinary parameter
/// `$receiver`, flagged mandated like every receiver a static moves; so do the forward a
/// sub-interface republishes for an inherited member and a class's `<name>$suspendImpl`. krusty
/// named it `$this$ie`, unflagged.
#[test]
fn java_parameters_flags_a_default_impls_extension_receiver_mandated() {
    assert_parameter_parity(
        "JavaParametersDefaultImplsReceiver",
        "package demo\n\
         interface Face {\n\
         \x20 fun String.ie(n: Int): Int = length + n\n\
         \x20 val String.pe: Int get() = length\n\
         \x20 fun plain(y: Int): Int = y\n\
         }\n\
         interface Sub : Face\n\
         open class Open { open suspend fun String.se(n: Int): Int = n + length }\n",
        &[
            "demo/Face$DefaultImpls",
            "demo/Face",
            "demo/Sub$DefaultImpls",
            "demo/Open",
        ],
    );
}

#[test]
fn method_parameters_remain_opt_in() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classes = common::compile_in_process_files(
        &[(
            "JavaParametersDisabled",
            "package demo\nfun plain(value: String) = value\n",
        )],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile without -java-parameters");
    let (_, facade) = classes
        .iter()
        .find(|(name, _)| name == "demo/JavaParametersDisabledKt")
        .expect("expected facade");
    assert_eq!(
        parameters_by_method(method_parameters(facade, "java-parameters-disabled.class")),
        BTreeMap::new()
    );
}
