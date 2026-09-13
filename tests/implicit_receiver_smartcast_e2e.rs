use super::common;

fn compiles(name: &str, source: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    common::compile_in_process_files(
        &[(name, source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .unwrap_or_else(|| panic!("{name}: krusty rejected source kotlinc accepts"))
    .into_iter()
    .map(|(class, _)| class)
    .collect()
}

/// A bare property name inside an EXTENSION function reads through the extension receiver, and a
/// preceding `name != null` narrows it — the same flow fact the member and `this.name` spellings
/// already honored. krusty consulted no narrowing on that path, so `if (ref != null) take(ref)` in an
/// extension still saw `String?` and the file was rejected.
#[test]
fn an_extension_receivers_property_smart_casts_under_a_bare_name() {
    let classes = compiles(
        "ExtensionSmartCast",
        "package demo\n\
        fun take(v: String): String = v\n\
        class Holder(val ref: String?)\n\
        fun Holder.ext(): String = if (ref != null) take(ref) else \"x\"\n",
    );
    assert!(classes
        .iter()
        .any(|class| class == "demo/ExtensionSmartCastKt"));
}

/// The spellings that already worked stay working — the narrowing must not start depending on how
/// the receiver is written.
#[test]
fn the_other_receiver_spellings_still_smart_cast() {
    let classes = compiles(
        "ReceiverSpellings",
        "package demo\n\
        fun take(v: String): String = v\n\
        class Holder(val ref: String?) {\n\
        \x20 fun member(): String = if (ref != null) take(ref) else \"x\"\n\
        }\n\
        fun explicit(h: Holder): String = if (h.ref != null) take(h.ref) else \"x\"\n\
        fun Holder.qualified(): String = if (this.ref != null) take(this.ref) else \"x\"\n",
    );
    assert!(classes.iter().any(|class| class == "demo/Holder"));
}

/// A `var` is not stable, so no spelling may narrow it — the extension path must not become a hole.
#[test]
fn a_mutable_property_never_smart_casts_through_an_extension_receiver() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        fun take(v: String): String = v\n\
        class Holder(var ref: String?)\n\
        fun Holder.ext(): String = if (ref != null) take(ref) else \"x\"\n";
    assert!(
        common::compile_in_process_files(
            &[("MutableNoSmartCast", source)],
            &[stdlib, jdk.clone()],
            Some(jdk.as_path()),
        )
        .is_none(),
        "a `var` property must not smart-cast through an extension receiver"
    );
}
