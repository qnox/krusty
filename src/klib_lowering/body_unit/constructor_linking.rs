//! Linking a constructor call of a dependency class to the place this unit realizes it: an earlier
//! unit's retained initializer, a target-allocated class, or a class this unit materializes.

use super::*;

impl<'s> Linker<'_, 's> {
    /// Link the constructor `callee` names for a call of `role`, and for a class this unit
    /// materializes, the constructor's place in that class (its primary or nth secondary).
    pub(super) fn link_selected_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
        role: ConstructorRole,
    ) -> Result<LinkedConstructor, KlibBodyDeclineReason> {
        let identity = self.callee_identity(callee)?;
        let (_, function) = self.sources.bodies.function(&identity).ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "a constructor call has no serialized function header".to_owned(),
            )
        })?;
        super::super::constructor_calls::require_constructor_kind(function)?;
        if matches!(self.sources.callees.get(&identity), Some(KlibCalleeFact::Selected(callables))
            if callables[0].kind() != crate::libraries::ExternalCallableKind::Constructor)
        {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a constructor call selected a non-constructor provider declaration".to_owned(),
            ));
        }
        // An earlier unit's retained initializer runs on an instance a subclass allocated; it
        // never allocates its own class.
        let retained_initializer = self.sources.externals.get(&identity).filter(|external| {
            matches!(
                external.realization,
                DependencyExternalRealization::ConstructorInitializer
            )
        });
        if retained_initializer.is_some() && role == ConstructorRole::Allocation {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an initializer-only dependency does not permit class allocation".to_owned(),
            ));
        }
        let declaration = self
            .sources
            .callees
            .constructor(&identity, self.sources.bodies)?
            // Producer lookup above already proved the serialized declaration exists. Without
            // exact provider evidence this unit cannot realize construction; that is not a
            // missing declaration and must not invent a constructor handle.
            .ok_or_else(|| KlibBodyDeclineReason::UnsupportedCallee(Box::new(callee.clone())))?;
        if let Some(initializer) = retained_initializer {
            return Ok(LinkedConstructor {
                target: initializer.target(),
                declaration,
                materialized: None,
            });
        }
        let external = self
            .sources
            .externals
            .get(&identity)
            .filter(|external| external.is_constructor())
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated constructor has no exact constructor external handle"
                        .to_owned(),
                )
            })?;
        let target = external.target();
        let allocating = matches!(
            external.realization,
            DependencyExternalRealization::AllocatingConstructor
        );
        let selected = match self.sources.callees.get(&identity) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            _ => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated constructor lost its provider declaration".to_owned(),
                ))
            }
        };
        let materialized = if allocating {
            None
        } else {
            self.materialize_class(ir, &identity, selected)?;
            self.constructors
                .get(&identity)
                .map(|constructor| crate::ir::IrConstructorTarget {
                    ordinal: constructor.secondary.map_or(0, |index| {
                        u32::try_from(index + 1).expect("too many constructors")
                    }),
                    access: super::super::same_library_class::constructor_access(function),
                })
        };
        Ok(LinkedConstructor {
            target,
            declaration,
            materialized,
        })
    }
}
