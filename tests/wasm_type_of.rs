//! What a `KType` prints on both Wasm hosts.
//!
//! The JVM prints a `KType` in its own spelling (Java classes, and a note that reflection is
//! missing without kotlin-reflect), so this program has no JVM oracle. The expected text is what
//! kotlinc-wasm 2.4.0 prints for each of these types with Kotlin/Wasm's `KTypeImpl.toString`.
//! `typeOfStructuralEquality.kt` pins equality and hashing on every lane.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty::wasm::WasmTarget;

const SOURCE: &str = "import kotlin.reflect.typeOf\n\
class G<T>\n\
interface A\n\
fun box(): String {\n\
    val printed = \"${typeOf<List<String>>()}|${typeOf<Map<in Number, out String?>?>()}|\" +\n\
        \"${typeOf<Array<*>>()}|${typeOf<IntArray>()}|${typeOf<Int?>()}|${typeOf<Any>()}|\" +\n\
        \"${typeOf<Unit>()}|${typeOf<G<A>>()}|${typeOf<() -> Unit>()}|\" +\n\
        \"${typeOf<List<Nothing>>()}|${typeOf<List<Nothing?>>()}|${typeOf<String>()}\"\n\
    return if (printed == \"kotlin.collections.List<kotlin.String>|\" +\n\
        \"kotlin.collections.Map<in kotlin.Number, out kotlin.String?>?|kotlin.Array<*>|\" +\n\
        \"kotlin.IntArray|kotlin.Int?|kotlin.Any|kotlin.Unit|G<A>|kotlin.Function0<kotlin.Unit>|\" +\n\
        \"kotlin.collections.List<kotlin.Nothing>|kotlin.collections.List<kotlin.Nothing?>|\" +\n\
        \"kotlin.String\") \"OK\" else printed\n\
}\n";
const STEM: &str = "wasmTypeOfText";

#[test]
fn a_type_prints_what_kotlin_wasm_prints() {
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, STEM),
            "{STEM} requires its {} lane",
            target.name()
        );
        assert_eq!(
            lane.compile_and_run(
                &std::env::temp_dir(),
                STEM,
                &[(format!("{STEM}.kt"), SOURCE.to_owned())],
                SOURCE,
            ),
            Outcome::Pass,
            "{STEM} on {}",
            target.name(),
        );
    }
}
