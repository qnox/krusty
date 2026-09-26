//! The class a callable reference compiles to, in the shape kotlinc's `FunctionReferenceLowering`
//! gives it.
//!
//! The carrier declares its own specialized `invoke` over the function type's parameters, which
//! calls the referenced declaration directly (boxing a scalar result that overrides the generic
//! `R`, and returning nothing for `Unit`), plus the erased `FunctionN.invoke(Object…)Object`
//! bridge to it. The class is synthetic, carries the generic header `FunctionReferenceImpl` +
//! `FunctionN<P…, R>`, and its use site casts the carrier to the function type.
//!
//! The comparison pins the carrier contract this pass owns: class/member flags and descriptors,
//! every method's raw `Code` components, and the raw Kotlin-metadata annotation. Enclosure
//! attributes (`EnclosingMethod`/`InnerClasses`) belong to the following local-class enclosure
//! realization and are deliberately outside this test. A carrier with a single dispatching
//! `invoke` also runs, so only this structural contract tells the two paths apart.

use super::common;

fn assert_same_owned_carrier_contract(class: &str, ours: &[u8], reference: &[u8]) {
    use krusty::jvm::classreader::{parse_class, read_class_attribute, read_method_code};

    let ours_info =
        parse_class(ours).unwrap_or_else(|error| panic!("parse krusty {class}: {error:?}"));
    let reference_info =
        parse_class(reference).unwrap_or_else(|error| panic!("parse kotlinc {class}: {error:?}"));

    assert_eq!(
        ours_info.major, reference_info.major,
        "{class}: class version"
    );
    assert_eq!(
        ours_info.access, reference_info.access,
        "{class}: class flags"
    );
    assert_eq!(
        ours_info.this_class, reference_info.this_class,
        "{class}: class name"
    );
    assert_eq!(
        ours_info.super_class, reference_info.super_class,
        "{class}: superclass"
    );
    assert_eq!(
        ours_info.interfaces(),
        reference_info.interfaces(),
        "{class}: interfaces"
    );
    assert_eq!(
        ours_info.signature, reference_info.signature,
        "{class}: generic header"
    );
    assert_eq!(ours_info.fields, reference_info.fields, "{class}: fields");
    assert_eq!(
        ours_info.methods, reference_info.methods,
        "{class}: methods"
    );
    assert_eq!(
        read_class_attribute(ours, "RuntimeVisibleAnnotations"),
        read_class_attribute(reference, "RuntimeVisibleAnnotations"),
        "{class}: raw Kotlin metadata annotation",
    );
    assert_eq!(
        common::raw_kotlin_metadata(ours),
        common::raw_kotlin_metadata(reference),
        "{class}: Kotlin metadata payload",
    );

    for method in &ours_info.methods {
        let ours_code = read_method_code(ours, &method.name, &method.descriptor);
        let reference_code = read_method_code(reference, &method.name, &method.descriptor);
        match (ours_code, reference_code) {
            (None, None) => {}
            (Some(ours), Some(reference)) => {
                assert_eq!(
                    ours.max_stack, reference.max_stack,
                    "{class}.{}: max_stack",
                    method.name
                );
                assert_eq!(
                    ours.max_locals, reference.max_locals,
                    "{class}.{}: max_locals",
                    method.name
                );
                assert_eq!(
                    ours.code, reference.code,
                    "{class}.{}: bytecode",
                    method.name
                );
                assert_eq!(
                    ours.stackmap, reference.stackmap,
                    "{class}.{}: stack map",
                    method.name
                );
                assert_eq!(
                    ours.locals, reference.locals,
                    "{class}.{}: local variables",
                    method.name
                );
                assert_eq!(
                    ours.lines, reference.lines,
                    "{class}.{}: line numbers",
                    method.name
                );
                assert_eq!(
                    ours.source_file, reference.source_file,
                    "{class}.{}: source file",
                    method.name
                );
                assert_eq!(
                    ours.defining_class, reference.defining_class,
                    "{class}.{}: defining class",
                    method.name
                );
                assert_eq!(
                    ours.dependency_source_map, reference.dependency_source_map,
                    "{class}.{}: source map",
                    method.name
                );
                assert_eq!(
                    ours.bootstrap_methods, reference.bootstrap_methods,
                    "{class}.{}: bootstrap methods",
                    method.name
                );
                let ours_handlers = ours
                    .handlers
                    .iter()
                    .map(|handler| {
                        (
                            handler.start_pc,
                            handler.end_pc,
                            handler.handler_pc,
                            handler.catch_type,
                        )
                    })
                    .collect::<Vec<_>>();
                let reference_handlers = reference
                    .handlers
                    .iter()
                    .map(|handler| {
                        (
                            handler.start_pc,
                            handler.end_pc,
                            handler.handler_pc,
                            handler.catch_type,
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    ours_handlers, reference_handlers,
                    "{class}.{}: handlers",
                    method.name
                );
            }
            (ours, reference) => panic!(
                "{class}.{}{}: Code presence differs (krusty {}, kotlinc {})",
                method.name,
                method.descriptor,
                ours.is_some(),
                reference.is_some(),
            ),
        }
    }
}

const SOURCE: &str = "class Parcel(val label: String)\n\
fun wrap(label: String): Parcel = Parcel(label)\n\
fun deliver(parcel: Parcel) {}\n\
fun echo(value: Any?): Any? = value\n\
class Courier(val name: String) {\n\
\x20   fun sign(parcel: Parcel): String = name + parcel.label\n\
}\n\
fun route(): String {\n\
\x20   val make: (String) -> Parcel = ::wrap\n\
\x20   val drop: (Parcel) -> Unit = ::deliver\n\
\x20   val courier = Courier(\"c\")\n\
\x20   val bound: (Parcel) -> String = courier::sign\n\
\x20   val unbound: (Courier, Parcel) -> String = Courier::sign\n\
\x20   val build: (String) -> Parcel = ::Parcel\n\
\x20   val same: (Any?) -> Any? = ::echo\n\
\x20   val first = make(\"x\")\n\
\x20   drop(first)\n\
\x20   val second = build(\"y\")\n\
\x20   val third = build(\"z\")\n\
\x20   return bound(second) + unbound(courier, third)\n\
}\n\
fun box(): String = if (route() == \"cycz\") \"OK\" else \"fail: \" + route()\n";

const CARRIERS: &[&str] = &[
    "ReferenceInvokeKt$route$make$1",
    "ReferenceInvokeKt$route$drop$1",
    "ReferenceInvokeKt$route$bound$1",
    "ReferenceInvokeKt$route$unbound$1",
    "ReferenceInvokeKt$route$build$1",
    // Its specialization is already the erased `Function1.invoke`, so it has no bridge.
    "ReferenceInvokeKt$route$same$1",
];

/// One source compiled by both compilers, and the carrier classes whose contract it pins.
struct Fixture<'a> {
    source: &'a str,
    stem: &'a str,
    carriers: &'a [&'a str],
}

const FIXTURE: Fixture<'static> = Fixture {
    source: SOURCE,
    stem: "ReferenceInvoke",
    carriers: CARRIERS,
};

/// `javap -p -c -s` of `class` from both compilers, with constant-pool indices and column
/// alignment removed so two pools interned in different orders compare equal.
fn disassembly_both(
    fixture: &Fixture<'_>,
    classes: &[&str],
) -> Option<Vec<(String, String, String)>> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let ours_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&ours_dir).ok()?;
    let source_path = dir.join(format!("{}.kt", fixture.stem));
    std::fs::write(&source_path, fixture.source).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted = common::compile_in_process_metadata_cp(
        fixture.source,
        fixture.stem,
        &[common::stdlib_jar()],
    )
    .expect("krusty compiles the carriers");
    for (name, bytes) in &emitted {
        let path = ours_dir.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class parent")).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    let normalize = |dump: String| {
        dump.lines()
            .filter(|line| !line.starts_with("Compiled from"))
            .map(|line| {
                line.split_whitespace()
                    .filter(|word| !word.starts_with('#'))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let dump = |root: &std::path::Path, class: &str| {
        common::javap(&["-p", "-c", "-s", "-cp", &root.to_string_lossy(), class]).map(normalize)
    };
    let mut out = Vec::new();
    for class in classes {
        if fixture.carriers.contains(class) {
            let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
                .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
            let ours = std::fs::read(ours_dir.join(format!("{class}.class")))
                .unwrap_or_else(|error| panic!("krusty did not emit {class}: {error}"));
            assert_same_owned_carrier_contract(class, &ours, &reference);
        }
        out.push((
            class.to_string(),
            dump(&reference_dir, class)?,
            dump(&ours_dir, class)?,
        ));
    }
    Some(out)
}

#[test]
fn reference_carriers_declare_kotlincs_specialized_invoke_and_bridge() {
    let Some(dumps) = disassembly_both(&FIXTURE, CARRIERS) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for (class, reference, ours) in dumps {
        assert_eq!(ours, reference, "{class}: carrier differs from kotlinc");
    }
}

/// The use site hands the carrier on as the function type, as kotlinc's implicit cast does.
#[test]
fn reference_use_site_casts_the_carrier_to_its_function_type() {
    let Some(dumps) = disassembly_both(&FIXTURE, &["ReferenceInvokeKt"]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let route = |dump: &str| {
        dump.lines()
            .skip_while(|line| !line.starts_with("public static final java.lang.String route()"))
            .take_while(|line| !line.starts_with("public static final java.lang.String box()"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    for (class, reference, ours) in dumps {
        let reference = route(&reference);
        assert!(!reference.is_empty(), "{class}: kotlinc emitted route()");
        assert_eq!(
            route(&ours),
            reference,
            "{class}.route differs from kotlinc"
        );
    }
}

#[test]
fn reference_carriers_run() {
    common::expect_box_same_as_kotlinc(SOURCE, "ReferenceInvokeRun");
}

/// Shapes that cannot yet move their adapter into the carrier retain the dispatching `invoke`.
/// This pins executable behavior only. Exact class parity is not claimed for the retained path:
/// its specialized invoke/bridge, capture fields, and metadata remain migration work. The owning
/// backend unit test separately pins the exact decision that keeps these shapes on that path.
#[test]
fn retained_dispatching_reference_shapes_run() {
    let high_parameters = (0..23)
        .map(|ordinal| format!("p{ordinal}: Int"))
        .collect::<Vec<_>>()
        .join(", ");
    let high_function_parameters = std::iter::repeat_n("Int", 23)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "@JvmInline value class Token(val raw: String)\n\
         fun wide({high_parameters}): Int = p0 + p22\n\
         fun unwrap(token: Token): String = token.raw\n\
         fun shapes(): String {{\n\
         \x20   val prefix = \"K\"\n\
         \x20   fun local(value: String): String = prefix + value\n\
         \x20   val captured: (String) -> String = ::local\n\
         \x20   val high: ({high_function_parameters}) -> Int = ::wide\n\
         \x20   val valueClass: (Token) -> String = ::unwrap\n\
         \x20   arrayOf<Any>(captured, high, valueClass)\n\
         \x20   return captured(\"!\") + high({}) + valueClass(Token(\"v\"))\n\
         }}\n\
         fun box(): String = if (shapes() == \"K!22v\") \"OK\" else \"fail: \" + shapes()\n",
        (0..23)
            .map(|ordinal| ordinal.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );
    let emitted = common::compile_in_process_metadata_cp(
        &source,
        "ReferenceInvokeRetainedRun",
        &[common::stdlib_jar()],
    )
    .expect("krusty compiles retained reference shapes");
    let retained_plan = emitted
        .iter()
        .filter(|(name, _)| name.contains("$shapes$"))
        .map(|(name, bytes)| {
            let info =
                krusty::jvm::classreader::parse_class(bytes).expect("parse retained carrier");
            (
                name.clone(),
                info.fields
                    .iter()
                    .map(|field| (field.name.clone(), field.access, field.descriptor.clone()))
                    .collect::<Vec<_>>(),
                info.methods
                    .iter()
                    .map(|method| {
                        (
                            method.name.clone(),
                            method.access,
                            method.descriptor.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let expected = [
        (
            "ReferenceInvokeRetainedRunKt$shapes$captured$1",
            vec![("$captured$0", 0x0012, "Ljava/lang/String;")],
            vec![
                ("<init>", 0, "(Ljava/lang/String;)V"),
                ("invoke", 0x0001, "(Ljava/lang/Object;)Ljava/lang/Object;"),
            ],
        ),
        (
            "ReferenceInvokeRetainedRunKt$shapes$high$1",
            vec![(
                "INSTANCE",
                0x0019,
                "LReferenceInvokeRetainedRunKt$shapes$high$1;",
            )],
            vec![
                ("<init>", 0, "()V"),
                ("invoke", 0x0001, "([Ljava/lang/Object;)Ljava/lang/Object;"),
                ("<clinit>", 0x0008, "()V"),
            ],
        ),
        (
            "ReferenceInvokeRetainedRunKt$shapes$valueClass$1",
            vec![(
                "INSTANCE",
                0x0019,
                "LReferenceInvokeRetainedRunKt$shapes$valueClass$1;",
            )],
            vec![
                ("<init>", 0, "()V"),
                ("invoke", 0x0001, "(Ljava/lang/Object;)Ljava/lang/Object;"),
                ("<clinit>", 0x0008, "()V"),
            ],
        ),
    ];
    let expected = expected
        .into_iter()
        .map(|(name, fields, methods)| {
            (
                name.to_string(),
                fields
                    .into_iter()
                    .map(|(name, access, descriptor)| {
                        (name.to_string(), access, descriptor.to_string())
                    })
                    .collect::<Vec<_>>(),
                methods
                    .into_iter()
                    .map(|(name, access, descriptor)| {
                        (name.to_string(), access, descriptor.to_string())
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        retained_plan, expected,
        "retained shapes must keep the exact dispatching-invoke plan",
    );
    common::expect_box_same_as_kotlinc(&source, "ReferenceInvokeRetainedRun");
}

/// Compare `fixture`'s carriers with kotlinc's, then run its `box()` under both compilers.
fn assert_carriers_match_and_run(fixture: &Fixture<'_>) {
    let dumps =
        disassembly_both(fixture, fixture.carriers).expect("reference kotlinc is provisioned");
    for (class, reference, ours) in dumps {
        assert_eq!(ours, reference, "{class}: carrier differs from kotlinc");
    }
    common::expect_box_same_as_kotlinc(fixture.source, &format!("{}Run", fixture.stem));
}

const SUSPEND_SOURCE: &str = r##"import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext

class Held(vararg val all: Any)

object Unused : Continuation<Any?> {
    override val context: CoroutineContext get() = EmptyCoroutineContext
    override fun resumeWith(result: Result<Any?>) {}
}

suspend fun later(value: Int): Int = value + 1
fun name(value: String): String = value

class Tank(val level: Int) {
    suspend fun fill(by: Int): Int = level + by
}

var drained = 0

suspend fun Tank.drain() {
    drained = level
}

fun carriers(tank: Tank): Held {
    val converted: suspend (String) -> String = ::name
    val declared = ::later
    val bound = tank::fill
    val unbound = Tank::fill
    val boundExtension = tank::drain
    return Held(converted, declared, bound, unbound, boundExtension)
}

@Suppress("UNCHECKED_CAST")
fun box(): String {
    val all = carriers(Tank(10)).all
    if ((all[0] as (String, Continuation<Any?>) -> Any?)("n", Unused) != "n") return "converted"
    if ((all[1] as (Int, Continuation<Any?>) -> Any?)(41, Unused) != 42) return "declared"
    if ((all[2] as (Int, Continuation<Any?>) -> Any?)(5, Unused) != 15) return "bound"
    if ((all[3] as (Tank, Int, Continuation<Any?>) -> Any?)(Tank(1), 2, Unused) != 3) return "unbound"
    if ((all[4] as (Continuation<Any?>) -> Any?)(Unused) != Unit || drained != 10) return "drained"
    return "OK"
}
"##;

/// A suspend reference's carrier declares kotlinc's typed `invoke`, which takes the continuation
/// last and returns an object, plus the erased bridge that casts the continuation to it. This holds
/// for a suspend declaration, bound or not, and for an ordinary one converted to a suspend type.
#[test]
fn suspend_reference_carriers_declare_kotlincs_typed_invoke() {
    assert_carriers_match_and_run(&Fixture {
        source: SUSPEND_SOURCE,
        stem: "SuspendReferenceInvoke",
        carriers: &[
            "SuspendReferenceInvokeKt$carriers$converted$1",
            "SuspendReferenceInvokeKt$carriers$declared$1",
            "SuspendReferenceInvokeKt$carriers$bound$1",
            "SuspendReferenceInvokeKt$carriers$unbound$1",
            "SuspendReferenceInvokeKt$carriers$boundExtension$1",
        ],
    });
}

const EXTENSION_SOURCE: &str = r##"class Held(vararg val all: Any)

class Gauge(val level: Int)

fun Gauge.read(): Int = level
fun String.twice(): String = this + this

fun carriers(gauge: Gauge): Held {
    val unbound = Gauge::read
    val text = String::twice
    val boundText = "x"::twice
    val bound = gauge::read
    return Held(unbound, text, boundText, bound)
}

@Suppress("UNCHECKED_CAST")
fun box(): String {
    val all = carriers(Gauge(4)).all
    if ((all[0] as (Gauge) -> Int)(Gauge(3)) != 3) return "unbound"
    if ((all[1] as (String) -> String)("y") != "yy") return "text"
    if ((all[2] as () -> String)() != "xx") return "bound text"
    if ((all[3] as () -> Int)() != 4) return "bound"
    return "OK"
}
"##;

/// An extension reference is reflected with its receiver as the declaration's first parameter,
/// and a bound one enters the reference's line at its call, after the stored receiver is read.
#[test]
fn extension_reference_carriers_reflect_the_receiver_parameter() {
    assert_carriers_match_and_run(&Fixture {
        source: EXTENSION_SOURCE,
        stem: "ExtensionReferenceInvoke",
        carriers: &[
            "ExtensionReferenceInvokeKt$carriers$unbound$1",
            "ExtensionReferenceInvokeKt$carriers$text$1",
            "ExtensionReferenceInvokeKt$carriers$boundText$1",
            "ExtensionReferenceInvokeKt$carriers$bound$1",
        ],
    });
}
