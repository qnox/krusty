//! Cross-phase contract for metadata and debug publication of generated class members.

use super::{FunId, IrFile};
use crate::types::{TypeName, Visibility};
use std::num::NonZeroU32;

/// Source/debug representation explicitly owned by a generated declaration's producer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IrGeneratedDeclarationDebug {
    #[default]
    None,
    LocalsOnly,
    DeclarationLine(NonZeroU32),
}

impl IrGeneratedDeclarationDebug {
    /// Retain a known declaration line while making the frontend's `0 = unknown` convention an
    /// explicit locals-only publication rather than an invalid enum state.
    pub fn declaration_line(line: u32) -> Self {
        NonZeroU32::new(line).map_or(Self::LocalsOnly, Self::DeclarationLine)
    }

    pub fn records_locals(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn line(self) -> Option<u32> {
        match self {
            Self::DeclarationLine(line) => Some(line.get()),
            Self::None | Self::LocalsOnly => None,
        }
    }
}

/// Whether generated function metadata replaces the class's inferred declared-function set or is
/// appended to a source-declared class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrGeneratedFunctionMetadataScope {
    Exclusive,
    Additive,
}

/// Kotlin declaration facts for one generated function. JVM spelling/shape stays on its exact
/// [`super::IrFunction`] identity; this record contains only language-level publication facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrGeneratedFunctionMetadata {
    pub source_name: String,
    pub visibility: Visibility,
}

/// The single metadata/debug contract for one compiler-generated function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrGeneratedFunctionPublication {
    pub function: FunId,
    /// Complete source-level parameter identities, exactly parallel to `IrFunction::params`.
    pub parameter_names: Vec<String>,
    /// `None` means the physical function has no Kotlin `Function` record (for example a generated
    /// property's getter). Debug publication remains independent and explicit.
    pub metadata: Option<IrGeneratedFunctionMetadata>,
    pub debug: IrGeneratedDeclarationDebug,
}

/// One producer-owned generated-member surface for a class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrGeneratedMemberPublication {
    pub metadata_scope: IrGeneratedFunctionMetadataScope,
    /// Metadata-bearing entries are in Kotlin declaration order. Debug-only entries may follow.
    pub functions: Vec<IrGeneratedFunctionPublication>,
}

impl IrFile {
    pub(crate) fn publish_generated_members(
        &mut self,
        owner: TypeName,
        publication: IrGeneratedMemberPublication,
    ) {
        let mut identities = std::collections::HashSet::new();
        for member in &publication.functions {
            assert!(
                identities.insert(member.function),
                "a generated function has one publication entry"
            );
            assert!(
                self.generated_function_publication(member.function)
                    .is_none(),
                "a generated function has one owning publication"
            );
            let function = self
                .functions
                .get(member.function as usize)
                .expect("a generated publication names an existing function");
            assert_eq!(
                member.parameter_names.len(),
                function.params.len(),
                "generated parameter identities exactly match semantic function arity"
            );
            assert!(
                member.parameter_names.iter().all(|name| !name.is_empty()),
                "generated parameter identities are never empty"
            );
        }
        assert!(
            self.generated_member_publications
                .insert(owner, publication)
                .is_none(),
            "one producer owns a class's generated-member publication"
        );
    }

    pub(crate) fn generated_member_publication(
        &self,
        owner: TypeName,
    ) -> Option<&IrGeneratedMemberPublication> {
        self.generated_member_publications.get(&owner)
    }

    pub(crate) fn generated_function_publication(
        &self,
        function: FunId,
    ) -> Option<&IrGeneratedFunctionPublication> {
        self.generated_member_publications
            .values()
            .flat_map(|publication| publication.functions.iter())
            .find(|member| member.function == function)
    }
}

#[cfg(test)]
mod tests {
    use super::IrGeneratedDeclarationDebug;

    #[test]
    fn unknown_declaration_line_is_an_explicit_locals_only_contract() {
        assert_eq!(
            IrGeneratedDeclarationDebug::declaration_line(0),
            IrGeneratedDeclarationDebug::LocalsOnly
        );
        assert_eq!(
            IrGeneratedDeclarationDebug::declaration_line(7).line(),
            Some(7)
        );
    }
}
