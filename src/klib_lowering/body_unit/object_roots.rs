//! Transactional materialization of an object use, without a public constructor/callable root.

use super::*;

pub(super) fn constructor_identity(
    class: &crate::libraries::KlibBodyClassContract,
) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
    Ok(KlibDependencyCallable {
        module: class.module.clone(),
        declaration: KlibDeclarationSignature::Public(class.primary_constructor.clone().ok_or(
            KlibBodyDeclineReason::UnsupportedDeclaration(
                "an object without a private primary constructor",
            ),
        )?),
    })
}

impl DependencyBodyUnit {
    pub(crate) fn lower_objects_with_declarations<'a>(
        &mut self,
        classes: &'a [crate::libraries::KlibBodyClassContract],
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
        declarations: &'a dyn crate::symbol_source::SymbolSource,
    ) -> Result<(), KlibBodyDecline> {
        let mut pending = Vec::new();
        for class in classes {
            if callees.object_is_published(class) {
                continue;
            }
            let identity = constructor_identity(class).map_err(|reason| {
                KlibBodyDecline::new(
                    KlibDeclarationSignature::Public(class.signature.clone()),
                    reason,
                )
            })?;
            if !self.classes.contains_key(&identity) {
                pending.push(class);
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let checkpoint = UnitCheckpoint::of(self);
        self.lower_reachable_root(
            UnitRoot::Objects(&pending),
            Sources {
                bodies,
                callees,
                intrinsics,
                externals,
                declarations,
            },
        )
        .map(|_| ())
        .map_err(|reason| {
            checkpoint.restore(self);
            reason
        })
    }
}
