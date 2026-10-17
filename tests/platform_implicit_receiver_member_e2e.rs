//! A platform type `T!` is usable as `T` for member calls through an implicit receiver.
//!
//! `Collections.synchronizedList(…).apply { add("a") }` gives the `apply` lambda the receiver
//! `(Mutable)List<String!>!`. A bare `add` resolves to the member exactly as `this.add` does; the
//! implicit-receiver rung must not skip the member family because the receiver is flexible.

use super::common;

const SOURCE: &str = r#"
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.util.Collections

fun keys(): KeyPair = KeyPairGenerator.getInstance("RSA").apply { initialize(1024) }.generateKeyPair()

fun size(): Int = Collections.synchronizedList(ArrayList<String>()).apply { add("a"); add(0, "b") }.size

fun box(): String {
    if (size() != 2) return "size ${size()}"
    if (keys().public.algorithm != "RSA") return "keys"
    return "OK"
}
"#;

#[test]
fn a_platform_implicit_receiver_selects_its_members() {
    common::assert_classes_identical_to_kotlinc(
        "PlatformImplicitReceiver",
        SOURCE,
        &["PlatformImplicitReceiverKt"],
    );
    common::expect_box_same_as_kotlinc(SOURCE, "PlatformImplicitReceiver");
}
