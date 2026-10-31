//! Capture storage is physical state, not a Kotlin property declaration.

// PLATFORM: complete JVM class ABI, metadata payloads and runtime against kotlinc.
use super::common;

fn assert_complete_abi_and_metadata(classes: &common::ClassSets) {
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        info.access,
                        info.this_class,
                        info.super_class,
                        info.interfaces(),
                        info.signature,
                        info.inner_classes,
                    )
                );
                (
                    name.clone(),
                    (
                        header,
                        common::member_table(bytes),
                        (
                            format!("{:?}", info.meta),
                            common::kotlin_metadata::kotlin_metadata_elements(bytes),
                        ),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let actual = inventory(&classes.krusty);
    let expected = inventory(&classes.reference);
    let actual_abi = actual
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_abi = expected
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual_abi, expected_abi, "complete class ABI");
    assert_eq!(actual, expected, "complete class metadata");
}


fn assert_capture_metadata(stem: &str, source: &str) {
    let file = format!("{stem}.kt");
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(&file, source)]));
    common::expect_box_same_as_kotlinc(source, stem);
}

#[test]
fn retained_inline_capture_storage_is_not_a_property() {
    assert_capture_metadata("InlineCaptureMetadata", r#"
interface Token { fun value(): Int }
private inline fun copy(value: Int) = object : Token { override fun value(): Int = value }
fun box(): String = if (copy(7).value() == 7) "OK" else "capture"
"#);
}

#[test]
fn a_real_property_can_share_a_captured_values_spelling() {
    assert_capture_metadata("InlineCapturePropertyMetadata", r#"
interface Token { fun value(): Int }
private inline fun copy(value: Int) = object : Token {
    val value: Int = value
    override fun value(): Int = this.value
}
fun box(): String = if (copy(7).value() == 7) "OK" else "property"
"#);
}

#[test]
fn retained_default_capture_storage_is_not_a_property() {
    assert_capture_metadata("DefaultCaptureMetadata", r#"
interface Token { fun value(): Int }
private fun copy(value: Int, token: Token = object : Token { override fun value(): Int = value }): Token = token
fun box(): String = if (copy(7).value() == 7) "OK" else "default"
"#);
}

#[test]
fn ordinary_capture_storage_keeps_its_metadata_and_runtime() {
    assert_capture_metadata("OrdinaryCaptureMetadata", r#"
interface Token { fun value(): Int }
private fun copy(value: Int) = object : Token { override fun value(): Int = value }
fun box(): String = if (copy(7).value() == 7) "OK" else "ordinary"
"#);
}

#[test]
fn a_super_argument_capture_uses_its_parameter_without_an_extra_field() {
    assert_capture_metadata("SuperArgumentCaptureMetadata", r#"
open class Base(val saved: String)
private fun copy(value: String) = object : Base(value) {}
fun box(): String = copy("OK").saved
"#);
}

#[test]
fn member_parameters_do_not_capture_shadowed_enclosing_values() {
    assert_capture_metadata("ShadowedCaptureMetadata", r#"
private fun add(left: Int, right: Int): Int {
    val holder = object { fun add(left: Int, right: Int) = left + right }
    return holder.add(left, right)
}
fun box(): String = if (add(4, 3) == 7) "OK" else "shadowed"
"#);
}

#[test]
fn forwarded_super_arguments_keep_the_enclosing_capture_owner() {
    assert_capture_metadata("ForwardedCaptureOwnerMetadata", r#"
open class Base(val saved: Int)
private fun copy(value: Int) = object {
    fun read(): String {
        val nested = object : Base(value + 1) { fun original() = value }
        return if (nested.saved == 8 && nested.original() == 7) "OK" else "owner"
    }
}
fun box(): String = copy(7).read()
"#);
}

#[test]
fn a_receiver_function_in_an_initializer_reads_its_constructor_parameter() {
    assert_capture_metadata("InitializerFunctionCaptureMetadata", r#"
private fun copy(read: String.() -> Int) = object { val result = "word".read() }
fun box(): String = if (copy { length }.result == 4) "OK" else "function"
"#);
}

#[test]
fn a_selected_local_function_keeps_a_capture_shadowed_at_its_call_site() {
    assert_capture_metadata("TransitiveShadowedCaptureMetadata", r#"
private fun copy(): Int {
    val value = 7
    fun read() = value
    val holder = object { fun get(value: Int) = read() + value }
    return holder.get(1)
}
fun box(): String = if (copy() == 8) "OK" else "transitive"
"#);
}

#[test]
fn a_parameter_in_one_member_does_not_hide_another_members_capture() {
    assert_capture_metadata("IndependentMemberCaptureMetadata", r#"
private fun copy(value: Int) = object {
    fun original() = value
    fun argument(value: Int) = value
}
fun box(): String {
    val holder = copy(7)
    return if (holder.original() == 7 && holder.argument(2) == 2) "OK" else "scope"
}
"#);
}

#[test]
fn a_receiver_function_parameter_hides_the_enclosing_function_value() {
    assert_capture_metadata("ShadowedFunctionCaptureMetadata", r#"
private fun copy(read: String.() -> Int) = object {
    fun run(read: String.() -> Int) = "word".read()
}
fun box(): String = if (copy { length }.run { length + 1 } == 5) "OK" else "function scope"
"#);
}

#[test]
fn a_nested_super_argument_reads_the_enclosing_capture_coordinate() {
    assert_capture_metadata("NestedSuperCaptureCoordinateMetadata", r#"
open class Base(val saved: Int)
private fun copy(value: Int) = object {
    fun read(): String {
        val nested = object : Base(value) {}
        return if (nested.saved == 7) "OK" else "coordinate"
    }
}
fun box(): String = copy(7).read()
"#);
}
