//! Stable lexical roots shared by header preparation and bounded body checking.

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex, StableDeclarationAnchor,
};

pub(crate) fn checker_root(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    anchor: impl Fn(DeclarationId) -> Option<StableDeclarationAnchor>,
    lexical_root: impl Fn(DeclarationId) -> Option<DeclarationId>,
) -> DeclarationId {
    let ordinary_root = |mut declaration: DeclarationId| loop {
        let Some(anchor) = anchor(declaration) else {
            break declaration;
        };
        let local_classifier = anchor.kind == DeclarationKind::Classifier
            && index
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(DeclarationFlags::LOCAL_CLASS));
        if (anchor.kind == DeclarationKind::Classifier && !local_classifier)
            || anchor.owner.is_none()
        {
            break declaration;
        }
        declaration = anchor.owner.expect("a non-root declaration has an owner");
    };
    let mut declaration = declaration;
    loop {
        let Some(current) = anchor(declaration) else {
            break declaration;
        };
        let local_classifier = current.kind == DeclarationKind::Classifier
            && index
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(DeclarationFlags::LOCAL_CLASS));
        if local_classifier {
            if let Some(lexical) = lexical_root(declaration) {
                let root = ordinary_root(lexical);
                if root != declaration {
                    crate::trace_compiler!("signature", "checker local classifier lexical root declaration={declaration:?} lexical={lexical:?} selected={root:?}");
                    declaration = root;
                    continue;
                }
            }
        }
        if (current.kind == DeclarationKind::Classifier && !local_classifier)
            || current.owner.is_none()
        {
            break declaration;
        }
        declaration = current.owner.expect("a non-root declaration has an owner");
    }
}
