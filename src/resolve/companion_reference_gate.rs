//! `CompanionBlocksAndExtensions` at the references that select a companion-associated
//! declaration.
//!
//! kotlinc keeps a `companion { … }` block member a candidate while the feature is off but marks a
//! selected one with `UNSUPPORTED_FEATURE` at the reference's name. It does not look for written
//! companion extensions (`companion fun C.name`) at all then, so a reference to one is unresolved.

use super::*;

/// Which feature-gated kind of associated declaration a selected callable is.
#[derive(Clone, Copy)]
enum CompanionAssociated {
    BlockMember,
    WrittenExtension,
}

impl CompanionAssociated {
    fn of_function(function: &crate::libraries::FunctionInfo) -> Option<Self> {
        function.associated_classifier?;
        Some(if function.associated_access_owner.is_some() {
            Self::BlockMember
        } else {
            Self::WrittenExtension
        })
    }

    /// A platform static field is associated with its classifier too, but it is neither kind: only a
    /// Kotlin declaration with accessors is a written companion extension.
    fn of_property(property: &crate::libraries::PropertyInfo) -> Option<Self> {
        property.associated_classifier?;
        if property.associated_access_owner.is_some() {
            Some(Self::BlockMember)
        } else if property.producer == crate::libraries::PropertyProducer::KotlinAccessor {
            Some(Self::WrittenExtension)
        } else {
            None
        }
    }
}

impl Checker<'_> {
    /// A reference at `span` selected `function`.
    pub(super) fn gate_companion_function(
        &mut self,
        span: Span,
        function: &crate::libraries::FunctionInfo,
    ) {
        if let Some(kind) = CompanionAssociated::of_function(function) {
            self.gate_companion_reference(span, &function.callable.name, kind);
        }
    }

    /// A reference at `span` selected `property`.
    pub(super) fn gate_companion_property(
        &mut self,
        span: Span,
        property: &crate::libraries::PropertyInfo,
    ) {
        if let Some(kind) = CompanionAssociated::of_property(property) {
            self.gate_companion_reference(span, &property.name, kind);
        }
    }

    fn gate_companion_reference(&mut self, span: Span, name: &str, kind: CompanionAssociated) {
        let Some(message) = self
            .file
            .language_gates
            .companion_blocks_and_extensions
            .unsupported_message()
            .cloned()
        else {
            return;
        };
        match kind {
            CompanionAssociated::BlockMember => self.diags.error(span, &*message),
            CompanionAssociated::WrittenExtension => self
                .diags
                .error(span, format!("unresolved reference '{name}'.")),
        }
    }
}
