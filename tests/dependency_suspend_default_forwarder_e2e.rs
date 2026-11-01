//! A class implementing a dependency interface's `suspend` default member gets a forwarder with
//! the member's CPS shape. Its generic `Signature` is that shape too, as kotlinc writes it: the
//! declared result travels as the trailing `Continuation<? super R>` and the method returns
//! `Object`, never the source-level `(String)V`.

use super::common;

const LIBRARY: &str = "\
package lib\n\
\n\
interface Backend {\n\
    val id: String\n\
    suspend fun create(name: String): String\n\
    suspend fun destroy(name: String) {\n\
        error(\"unsupported $id\")\n\
    }\n\
    suspend fun status(name: String): Int? = null\n\
    suspend fun list(prefix: String): List<String> = listOf(prefix)\n\
    fun plain(x: List<String>): List<String> = x\n\
}\n\
";

const CONSUMER: &str = "\
package app\n\
\n\
import lib.Backend\n\
\n\
class Impl : Backend {\n\
    override val id: String = \"impl\"\n\
    override suspend fun create(name: String): String = name\n\
}\n\
";

#[test]
fn a_dependency_suspend_default_forwarder_signs_its_cps_shape() {
    let library = common::compile_libs_build(
        "DependencySuspendDefaultForwarderLibrary",
        &[("Lib.kt", LIBRARY)],
    )
    .expect("krusty builds the dependency");
    let reference = library
        .reference_out()
        .expect("reference kotlinc builds the dependency")
        .to_path_buf();
    common::assert_classes_identical_to_kotlinc_over_own_libraries(
        "DependencySuspendDefaultForwarder",
        CONSUMER,
        &["app/Impl"],
        &[reference],
        &[library.krusty_out().to_path_buf()],
    );
}
