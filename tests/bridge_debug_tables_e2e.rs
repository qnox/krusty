//! A generic supertype's erased BRIDGE carries debug tables in kotlinc and carried none in krusty.
//!
//! The bridge itself was already byte-for-byte correct — `aload_0`, the arguments, a `checkcast`,
//! `invokevirtual` the concrete override — so nothing a program does could show the difference.
//! What it changes is the class file: kotlinc maps the whole bridge to the CLASS declaration line
//! and names its receiver and parameters in a `LocalVariableTable`, and a debugger stepping into
//! `Sink<String>.accept` reads those.
//!
//! The parameter NAMES come from the concrete override the bridge delegates to; their descriptors
//! are the ERASED ones the bridge actually receives (`item Ljava/lang/Object;`, not `String`).
//!
//! Every Kotlin class that implements a generic supertype emits at least one, so the gap was in
//! every such class — and in every `@Serializable` class twice over, since a generated `$serializer`
//! bridges both `serialize` and `deserialize`.
use super::common;

fn assert_code_and_debug_identical(name: &str, src: &str, class: &str) {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: scratch directory unavailable");
        return;
    };
    let reference = dir.join("reference");
    let actual = dir.join("actual");
    std::fs::create_dir_all(&reference).expect("create reference directory");
    std::fs::create_dir_all(&actual).expect("create actual directory");
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).expect("write bridge fixture");
    let args = vec![
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let Some((status, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(status, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(src, name, &[])
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(candidate, _)| candidate == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    let actual_class = actual.join(format!("{class}.class"));
    std::fs::write(&actual_class, bytes).expect("write actual class");
    let reference_class = reference.join(format!("{class}.class"));
    let disassemble = |path: &std::path::Path| {
        common::javap(&["-c", "-l", "-p", &path.to_string_lossy()])
            .expect("pooled javap unavailable")
            .lines()
            .filter(|line| !line.starts_with("Compiled from"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let actual = disassemble(&actual_class);
    let expected = disassemble(&reference_class);
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(actual, expected, "{name}: code and debug tables differ");
}

/// The narrowest shape that produces one: a class implementing a generic interface at a concrete
/// argument. `accept(Object)` bridges to `accept(String)`.
#[test]
fn an_erased_bridge_is_byte_identical_to_kotlinc() {
    let src = "interface Sink<T> {\n\
               \x20   fun accept(item: T)\n\
               }\n\
               \n\
               class StringSink : Sink<String> {\n\
               \x20   override fun accept(item: String) {}\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("ErasedBridge", src, "StringSink") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("StringSink byte-identical to kotlinc");
}

/// Two parameters, so the names cannot come out right by accident: a bridge that took them from
/// its own position, or reused one spelling, would order them `key`, `value` only by luck.
#[test]
fn a_bridge_names_its_parameters_after_the_override() {
    let src = "interface Store<K, V> {\n\
               \x20   fun put(key: K, value: V)\n\
               }\n\
               \n\
               class Note\n\
               \n\
               class NoteStore : Store<String, Note> {\n\
               \x20   override fun put(key: String, value: Note) {}\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("BridgeParamNames", src, "NoteStore")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("NoteStore byte-identical to kotlinc");
}

/// Property bridges do not delegate through an `IrFunction`, so their setter parameter cannot get
/// a source name from `fn_params`. It still has kotlinc's generated accessor spelling rather than a
/// positional fallback.
#[test]
fn a_property_setter_bridge_uses_its_generated_parameter_name() {
    let src = "interface Box<T> {\n\
               \x20   var item: T\n\
               }\n\
               \n\
               class StringBox : Box<String> {\n\
               \x20   override var item: String = \"\"\n\
               }\n";
    // This source has a known unrelated metadata-flag delta, so compare the complete javap code +
    // line/local-table projection rather than weakening the bridge assertion to substrings.
    assert_code_and_debug_identical("PropertyBridge", src, "StringBox");
}

/// An ANNOTATED class: kotlinc roots the bridge at where the DECLARATION starts, annotations
/// included, not at the class header. The two coincide for an unannotated class, which is why the
/// fixtures above could not tell them apart — and `@Serializable`, `@Entity` and friends put the
/// difference on the hot path for real code.
#[test]
fn an_annotated_class_bridge_is_rooted_at_its_declaration() {
    let src = "annotation class Mark\n\
               \n\
               interface Sink<T> {\n\
               \x20   fun accept(item: T)\n\
               }\n\
               \n\
               @Mark\n\
               class StringSink : Sink<String> {\n\
               \x20   override fun accept(item: String) {}\n\
               }\n";
    let Some(result) = common::byte_diff_against_kotlinc("AnnotatedBridge", src, "StringSink")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("StringSink byte-identical to kotlinc");
}
