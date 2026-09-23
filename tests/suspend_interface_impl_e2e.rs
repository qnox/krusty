//! A `suspend fun` with a default body in an interface splits into a trampoline and a static.
//!
//! kotlinc compiles it into two methods: the interface's default method, which forwards, and a
//! `public static synthetic <name>$suspendImpl` taking the receiver first and carrying the body.
//! An overriding class runs the super body by CALLING that static — an `invokespecial` on the
//! default method cannot express it once the body is a state machine.
//!
//! krusty emitted only the default method with the body inlined, so the static was missing from
//! every such interface. Measured on a real corpus: the single most common member-count difference,
//! 262 classes.

use super::common;

const ABI_SRC: &str = "@Target(AnnotationTarget.FUNCTION, AnnotationTarget.VALUE_PARAMETER)\n\
                       @Retention(AnnotationRetention.BINARY)\n\
                       annotation class Mark(val value: String)\n\
                       interface Listener<T> {\n\
                       \x20   @Mark(\"function\")\n\
                       \x20   suspend fun <R : Any> onEvent(\n\
                       \x20       @Mark(\"parameter\") value: T?,\n\
                       \x20       fallback: R? = null,\n\
                       \x20   ): R? = fallback\n\
                       \x20   fun after(): String = \"after\"\n\
                       }\n";

#[test]
fn generic_annotated_nullable_defaulted_member_matches_kotlinc_exactly() {
    let (ours, reference) = abi_dumps();
    assert_eq!(
        method_headers(&ours),
        method_headers(&reference),
        "the complete ordered method surface differs"
    );
    for marker in [" onEvent(", " onEvent$suspendImpl("] {
        assert_eq!(
            method_section(&ours, marker, "krusty"),
            method_section(&reference, marker, "kotlinc"),
            "method section differs for {marker}"
        );
    }
}

fn abi_dumps() -> (String, String) {
    let dir = common::scratch_dir().expect("scratch directory for suspend-interface differential");
    let reference_dir = dir.join("reference");
    let krusty_dir = dir.join("krusty");
    std::fs::create_dir_all(&reference_dir).expect("create reference directory");
    std::fs::create_dir_all(&krusty_dir).expect("create krusty directory");
    let source = dir.join("SuspendInterfaceImpl.kt");
    std::fs::write(&source, ABI_SRC).expect("write differential source");
    let (status, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc must be available");
    assert_eq!(status, 0, "reference kotlinc failed: {stderr}");
    let classes = common::compile_in_process(
        ABI_SRC,
        "SuspendInterfaceImpl",
        &[common::stdlib_jar()],
        None,
    )
    .expect("krusty compiles the ABI fixture");
    for (internal, bytes) in classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create emitted class directory");
        }
        std::fs::write(path, bytes).expect("write emitted class");
    }
    let dump = |root: &std::path::Path, side: &str| {
        common::javap(&["-p", "-c", "-v", "-cp", &root.to_string_lossy(), "Listener"])
            .unwrap_or_else(|| panic!("javap unavailable for {side}"))
    };
    let result = (dump(&krusty_dir, "krusty"), dump(&reference_dir, "kotlinc"));
    let _ = std::fs::remove_dir_all(dir);
    result
}

fn method_headers(dump: &str) -> Vec<String> {
    dump.lines()
        .map(str::trim)
        .filter(|line| {
            line.ends_with(");")
                && ["public ", "private ", "protected "]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
        })
        .map(str::to_string)
        .collect()
}

fn method_section(dump: &str, marker: &str, side: &str) -> String {
    let lines = dump.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim().ends_with(");") && line.contains(marker))
        .unwrap_or_else(|| panic!("{side}: no method containing {marker:?}\n{dump}"));
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim().is_empty())
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    normalize_pool_indices(&lines[start..end].join("\n"))
}

fn normalize_pool_indices(section: &str) -> String {
    let bytes = section.as_bytes();
    let mut normalized = String::with_capacity(section.len());
    let mut index = 0;
    while index < bytes.len() {
        normalized.push(bytes[index] as char);
        if bytes[index] == b'#' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    normalized
}

/// The behaviour, not only the shape: the default body still runs through the new indirection.
///
/// The `super.onEvent(...)` call that is the ABI's whole reason for existing is NOT exercised here:
/// krusty declines it today with "a super call to the suspend member is not supported yet", which
/// this test discovered by failing closed. The static is the shape that call will need.
///
/// `expect_box_run` fails closed; an earlier version of this used a helper that returns `None` when
/// the runner is unavailable, and it passed while running nothing.
#[test]
fn a_suspend_interface_default_body_still_runs() {
    let jdk = common::jdk_modules();
    let main = "import kotlinx.coroutines.runBlocking\n\
                interface Listener {\n\
                \x20   suspend fun onEvent(x: Int): String = \"d\" + x\n\
                \x20   fun after(): String = \"after\"\n\
                }\n\
                class Plain : Listener\n\
                class Custom : Listener {\n\
                \x20   override suspend fun onEvent(x: Int): String = \"c\" + x\n\
                }\n\
                fun box(): String = runBlocking {\n\
                \x20   val plain = Plain()\n\
                \x20   val a = plain.onEvent(7)\n\
                \x20   val b = Custom().onEvent(7)\n\
                \x20   val c = plain.after()\n\
                \x20   if (a == \"d7\" && b == \"c7\" && c == \"after\") \"OK\" else a + \"/\" + b + \"/\" + c\n\
                }\n";
    let coroutines = common::coroutines_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(main, std::slice::from_ref(&coroutines));
    assert_eq!(reference, "OK", "kotlinc runtime fixture must succeed");
    let output = common::expect_box_run(
        main,
        "Main",
        &[common::stdlib_jar(), coroutines, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(
        output.trim(),
        reference,
        "krusty and kotlinc box results differ"
    );
}
