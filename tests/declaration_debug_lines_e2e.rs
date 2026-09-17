//! Exact source-line parity for declaration-owned generated members.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, member_body, plugin_and_runtime,
};

/// A bodyless `@Serializable` declaration followed by unrelated trivia and a sibling still closes
/// on its own header. The generated serializer initializer exposes that source fact in bytecode.
#[test]
fn a_serializable_declaration_closes_on_its_own_last_line() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               \n\
               @Serializable\n\
               data class First(val a: Int)\n\
               \n\
               /* unrelated\n\
                  trivia */\n\
               @Serializable\n\
               data class Second(val b: String, val c: Int)\n";
    for class in ["First$$serializer", "Second$$serializer"] {
        let Some(built) =
            compare_with_kotlinc_plugin("SiblingDeclarations", src, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let want = member_body(&built.reference, "static {}");
        assert!(
            want.iter().any(|line| line.starts_with("line ")),
            "kotlinc gives the class initializer a line table: {want:?}"
        );
        assert_eq!(
            member_body(&built.krusty, "static {}"),
            want,
            "{class} class initializer"
        );
    }
}
