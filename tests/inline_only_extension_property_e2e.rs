//! A public extension property whose JVM getter is private is an inline body.
//!
//! The class file accessor is not a callable. The property is still a public declaration, and a
//! read splices that body. A public extension property with a public getter stays an ordinary call.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn public_inline_only_extension_property_splices_its_private_getter() {
    expect_box_same_as_kotlinc(
        r#"
import java.nio.file.Paths
import kotlin.io.path.extension
import kotlin.io.path.pathString
import kotlin.time.Instant
import kotlin.time.isDistantFuture
import kotlin.time.isDistantPast

fun box(): String {
    val path = Paths.get("dir", "file.txt")
    if (path.pathString != path.toString()) return "path"
    if (path.extension != "txt") return "ext"
    if (!Instant.DISTANT_PAST.isDistantPast) return "past"
    if (Instant.DISTANT_FUTURE.isDistantPast) return "future-past"
    if (!Instant.DISTANT_FUTURE.isDistantFuture) return "future"
    if (Instant.DISTANT_PAST.isDistantFuture) return "past-future"
    return "OK"
}
"#,
        "InlineOnlyExtensionProperty",
    );
}
