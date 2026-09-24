//! The class a callable reference compiles to, in the shape kotlinc's `FunctionReferenceLowering`
//! gives it.
//!
//! The carrier declares its own specialized `invoke` over the function type's parameters, which
//! calls the referenced declaration directly (boxing a scalar result that overrides the generic
//! `R`, and returning nothing for `Unit`), plus the erased `FunctionN.invoke(Object…)Object`
//! bridge to it. The class is synthetic, carries the generic header `FunctionReferenceImpl` +
//! `FunctionN<P…, R>`, and its use site casts the carrier to the function type.
//!
//! The comparison is every member's instruction sequence and the class header, both from the
//! reference compiler: a carrier with a single dispatching `invoke` also runs, so only the shape
//! tells the two apart.

use super::common;

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

/// `javap -p -c -s` of `class` from both compilers, with constant-pool indices and column
/// alignment removed so two pools interned in different orders compare equal.
fn disassembly_both(classes: &[&str]) -> Option<Vec<(String, String, String)>> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let ours_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&ours_dir).ok()?;
    let source_path = dir.join("ReferenceInvoke.kt");
    std::fs::write(&source_path, SOURCE).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted =
        common::compile_in_process_metadata_cp(SOURCE, "ReferenceInvoke", &[common::stdlib_jar()])
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
    let Some(dumps) = disassembly_both(CARRIERS) else {
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
    let Some(dumps) = disassembly_both(&["ReferenceInvokeKt"]) else {
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
