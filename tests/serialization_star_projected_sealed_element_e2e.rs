//! A `@Serializable` class property whose type is a generic sealed type with a star projection
//! (`Level<*>`). A star projection names no type to serialize, so kotlinc's plugin hands the sealed
//! type's generated `serializer(…)` a `UnitSerializer` for it, exactly as it does for an abstract
//! class. krusty looked for a serializer of the star's bound and rejected the whole file ("not yet
//! supported by the IR backend").
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   sealed interface Level<T : Any> { val label: String }\n\
                   @Serializable\n\
                   class Low(override val label: String = \"low\") : Level<Int>\n\
                   @Serializable\n\
                   sealed class Kind<T>(val weight: Int = 1)\n\
                   @Serializable\n\
                   class Small : Kind<Int>()\n\
                   @Serializable\n\
                   data class Holder(val level: Level<*>, val kind: Kind<*>)\n";

/// The star-projected elements are not cached: the generated `childSerializers()` builds each one
/// in place, handing the sealed type's accessor the `UnitSerializer` narrowed to `KSerializer`.
#[test]
fn a_star_projected_sealed_element_gets_a_unit_serializer_like_kotlinc() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("StarSealed", SRC, "Holder$$serializer", &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    let member = "childSerializers()";
    let want = member_body(&built.reference, member);
    assert!(want.len() > 1, "kotlinc must emit {member}");
    assert_eq!(member_body(&built.krusty, member), want, "{member}");
}
