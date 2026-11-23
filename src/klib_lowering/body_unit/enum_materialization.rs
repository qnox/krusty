//! A dependency enum class in this closed unit, declared as a source enum is: materialized from
//! its admitted initialization, with its constants on the class, which the shared enum
//! realization builds, slots and all, when the target lowers the unit. Each constant's arguments
//! are lowered in the enum's own initialization frame, and the realization's initializer evaluates
//! them in declaration order before constructing the constant, as kotlinc's class initializer does.

use super::*;
use crate::ir::{IrConstructorTarget, IrEnumEntry};

impl<'s> Linker<'_, 's> {
    /// Materialize the enum class `classifier`, an instance of which a closed body reads, from
    /// its admitted initialization, unless an earlier unit materialized it and exports its
    /// constants.
    pub(super) fn materialize_enum(
        &mut self,
        ir: &mut IrFile,
        classifier: TypeName,
    ) -> Result<(), KlibBodyDeclineReason> {
        if self.realized_classes.contains(&classifier) {
            return Ok(());
        }
        let callees: &'s super::KlibCalleeFacts<'s> = self.sources.callees;
        let class = callees
            .read_class(classifier)
            .filter(|class| class.kind == crate::libraries::TypeKind::Enum)
            .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "an enum value whose class the closure did not read",
            ))?;
        let primary = class.primary_constructor.clone().ok_or(
            KlibBodyDeclineReason::UnsupportedDeclaration("an enum class without a constructor"),
        )?;
        let identity = KlibDependencyCallable {
            module: class.module.clone(),
            declaration: KlibDeclarationSignature::Public(primary),
        };
        self.materialize_declared_instances(ir, &identity, class)?;
        if !self.classes.contains_key(&identity) {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "an enum class without admitted initialization",
            ));
        }
        Ok(())
    }

    /// The enum class whose implicit `values`, `valueOf` or `entries` getter `signature` names,
    /// as its provider record publishes it, materialized for the call, and that member.
    pub(super) fn implicit_enum_member_call(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibDeclarationSignature,
    ) -> Result<
        Option<(TypeName, super::super::body_lowering::ImplicitEnumMember)>,
        KlibBodyDeclineReason,
    > {
        let KlibCalleeOrigin::Exact(identity) = self
            .sources
            .bodies
            .linked_origin(&self.active_module, signature)
        else {
            return Ok(None);
        };
        let Some(KlibCalleeFact::Selected(selected)) = self.sources.callees.get(&identity) else {
            return Ok(None);
        };
        let Some(member) =
            super::super::body_lowering::ImplicitEnumMember::of(selected[0].semantic_role())
        else {
            return Ok(None);
        };
        let Some(crate::types::SemanticCallableOwner::Classifier(classifier)) =
            selected[0].declaration_owner()
        else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an enum class's implicit member has no declaring class".to_owned(),
            ));
        };
        self.materialize_enum(ir, classifier)?;
        Ok(Some((classifier, member)))
    }

    /// Queue the frame that lowers the arguments of each constant of the materialized enum
    /// `class`. It is the enum's static initialization, not a function: the lowered arguments
    /// become the constants' own, which the shared enum realization evaluates.
    pub(super) fn queue_enum_entry_arguments(
        &mut self,
        class: crate::ir::ClassId,
        identity: &KlibDependencyCallable,
        plan: &super::super::class_initialization::ClassInitialization<'s, 's>,
        initializer: FunId,
    ) {
        let classifier = plan.header.contract.classifier;
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena: plan.header.arena,
            symbol: &plan.header.function.base.symbol,
            statements: &[],
            header: super::super::function_lowering::initialization_header(&format!(
                "{}$entries",
                // Boundary conversion for the frame's diagnostic name.
                classifier.render()
            )),
            function: initializer,
            initialization: Some((
                class,
                PendingInitialization::EnumEntries(
                    plan.entries
                        .iter()
                        .map(|entry| entry.arguments.clone())
                        .collect(),
                ),
            )),
            value_member: None,
            local_body: None,
            static_initialization: None,
        });
    }
}

/// The constants of the enum `plan` initializes, in declaration order, as the shared enum
/// realization reads them. Their arguments are the entry frame's to lower.
pub(super) fn declared_entries(
    plan: &super::super::class_initialization::ClassInitialization<'_, '_>,
) -> Vec<IrEnumEntry> {
    plan.entries
        .iter()
        .enumerate()
        .map(|(ordinal, entry)| IrEnumEntry {
            name: entry.name.clone(),
            diagnostic_span: None,
            argument_prelude: Vec::new(),
            args: Vec::new(),
            constructor_parameter_types: Vec::new(),
            constructor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            default_parameters: Vec::new(),
            decl_line: 0,
            source_order: u32::try_from(ordinal).expect("too many enum constants"),
            subclass: None,
        })
        .collect()
}
