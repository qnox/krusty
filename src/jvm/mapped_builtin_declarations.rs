//! Federation of Kotlin builtins declarations with their mapped JVM realizations.
//!
//! A mapped classifier such as `kotlin.Throwable` has two provider inputs: Kotlin builtins metadata
//! describes its source declaration, while the JDK class describes the method that realizes it. Core
//! resolution must receive one ordinary declaration containing both views.

use crate::libraries::LibraryMember;

/// Overlay source-semantic constructor facts onto the matching physical constructors.
///
/// Constructors that exist only on the JVM remain available: Kotlin's JVM builtins customizer may
/// expose platform constructors beyond the common builtins declaration. Conversely, a declaration
/// without a physical match is not published from this path because it has no JVM realization.
pub(super) fn overlay_constructor_semantics(
    physical: &mut [LibraryMember],
    semantic: Vec<LibraryMember>,
) {
    for declaration in semantic {
        let Some(realization) =
            physical.iter_mut().find(|candidate| {
                candidate.params.len() == declaration.params.len()
                    && candidate.params.iter().zip(&declaration.params).all(
                        |(&physical, &semantic)| {
                            physical.non_null().canonical_semantic()
                                == semantic.non_null().canonical_semantic()
                        },
                    )
            })
        else {
            continue;
        };

        // Keep descriptor/owner/physical types from the classfile realization. Everything below is
        // declaration semantics and therefore comes from Kotlin builtins metadata.
        realization.params = declaration.params;
        realization.ret = declaration.ret;
        realization.generic_sig = declaration.generic_sig;
        realization.visibility = declaration.visibility;
        realization.call_sig = declaration.call_sig;
    }
}

#[cfg(test)]
mod tests {
    use super::overlay_constructor_semantics;
    use crate::libraries::{CallSig, LibraryMember};
    use crate::types::Ty;

    #[test]
    fn semantic_overlay_preserves_the_physical_constructor_realization() {
        let mut physical = vec![LibraryMember::new(
            "<init>".to_string(),
            vec![Ty::platform_nullable(Ty::String)],
            Ty::Unit,
            "(Ljava/lang/String;)V".to_string(),
        )];
        let mut semantic = LibraryMember::new(
            "<init>".to_string(),
            vec![Ty::nullable(Ty::String)],
            Ty::Unit,
            String::new(),
        );
        semantic.call_sig =
            CallSig::metadata_member(1, vec!["message".to_string()], vec![false], None);

        overlay_constructor_semantics(&mut physical, vec![semantic]);

        assert_eq!(physical[0].params, [Ty::nullable(Ty::String)]);
        assert_eq!(
            physical[0].physical_params,
            [Ty::platform_nullable(Ty::String)]
        );
        assert_eq!(physical[0].call_sig.param_names, ["message"]);
        assert_eq!(physical[0].descriptor, "(Ljava/lang/String;)V");
    }
}
