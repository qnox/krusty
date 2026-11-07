//! What `KSFunctionDeclaration.findOverridee` and `KSPropertyDeclaration.findOverridee` answer for
//! the members of a source classifier.
//!
//! KSP2 answers the first member the declaration directly overrides, unwrapped to the declaration
//! a fake override stands for, and `null` for a constructor, a member the compiler generates, and a
//! member that overrides nothing. A library member is described by itself; one the library view
//! does not describe, one of a classifier with type parameters, and one sharing its name with
//! another member of its classifier leave the answer undescribed.

use krusty_plugin_ir::{
    KspDeclaration, KspDeclarationOrigin, KspFile, KspFunctionKind, KspMember, KspModifier,
    KspOverridee,
};

use super::all_members::ClassLedger;
use super::type_model::qualified_name;
use super::KspLibraries;
use crate::frontend::{FrontendDeclarationId, OverriddenMember, SourceSetAnalysis};
use crate::fx_hash::HashMap;
use crate::types::TypeName;

/// Fills in what `findOverridee` answers for every member of a source classifier of `files`.
pub(super) fn assign(
    files: &mut [KspFile],
    ledger: &HashMap<String, ClassLedger>,
    analysis: &SourceSetAnalysis,
) {
    let mut places = HashMap::default();
    for (owner, class) in ledger {
        for (index, declaration) in class.resolved.iter().enumerate() {
            if let Some(declaration) = declaration {
                places.insert(*declaration, (owner.as_str(), index as u32));
            }
        }
    }
    let overridees = Overridees {
        ledger,
        places,
        analysis,
        libraries: KspLibraries::new(analysis),
    };
    for file in files {
        overridees.assign(&mut file.declarations, &[]);
    }
}

struct Overridees<'a> {
    ledger: &'a HashMap<String, ClassLedger>,
    /// The owner and index of each source member declaration the round described.
    places: HashMap<FrontendDeclarationId, (&'a str, u32)>,
    analysis: &'a SourceSetAnalysis,
    libraries: KspLibraries<'a>,
}

impl Overridees<'_> {
    /// Fills in the answers for `declarations`, whose resolved members `resolved` lists by index:
    /// a classifier's, or a file's with none.
    fn assign(
        &self,
        declarations: &mut [KspDeclaration],
        resolved: &[Option<FrontendDeclarationId>],
    ) {
        for (index, declaration) in declarations.iter_mut().enumerate() {
            let resolved_member = resolved.get(index).copied().flatten();
            match declaration {
                KspDeclaration::Function(function) => {
                    function.overridee = self.overridee(
                        function.kind != KspFunctionKind::Member,
                        function.origin,
                        function.modifiers.as_deref(),
                        resolved_member,
                        false,
                    );
                }
                KspDeclaration::Property(property) => {
                    property.overridee = self.overridee(
                        false,
                        property.origin,
                        property.modifiers.as_deref(),
                        resolved_member,
                        true,
                    );
                }
                KspDeclaration::Class(class) => {
                    let members = self
                        .ledger
                        .get(&class.qualified_name)
                        .map_or(&[][..], |ledger| ledger.resolved.as_slice());
                    self.assign(&mut class.declarations, members);
                }
                KspDeclaration::TypeAlias(_) | KspDeclaration::LibraryClass(_) => {}
            }
        }
    }

    /// The answer for a declaration; `no_member` for a constructor or a top-level or static one,
    /// none of which overrides anything.
    fn overridee(
        &self,
        no_member: bool,
        origin: KspDeclarationOrigin,
        modifiers: Option<&[KspModifier]>,
        resolved: Option<FrontendDeclarationId>,
        property: bool,
    ) -> KspOverridee {
        match origin {
            KspDeclarationOrigin::Synthetic => return KspOverridee::Nothing,
            KspDeclarationOrigin::Library(_) => return KspOverridee::Undescribed,
            KspDeclarationOrigin::Source => {}
        }
        if no_member {
            return KspOverridee::Nothing;
        }
        let Some(modifiers) = modifiers else {
            return KspOverridee::Undescribed;
        };
        if !modifiers.contains(&KspModifier::Override) {
            return KspOverridee::Nothing;
        }
        let Some(declaration) = resolved else {
            return KspOverridee::Undescribed;
        };
        let Some(owner) = self.analysis.member_owner(declaration) else {
            return KspOverridee::Undescribed;
        };
        let member = match self.analysis.overridden_members(owner, declaration).first() {
            Some(OverriddenMember::Declared(overridden)) => {
                self.places
                    .get(overridden)
                    .map(|(owner, index)| KspMember::Source {
                        owner: owner.to_string(),
                        index: *index,
                    })
            }
            Some(OverriddenMember::Library { owner, name }) => {
                self.library_member(*owner, name, property)
            }
            None => None,
        };
        member.map_or(KspOverridee::Undescribed, KspOverridee::Member)
    }

    /// The library member `name` of `owner` it overrides, when the round can single it out.
    fn library_member(&self, owner: TypeName, name: &str, property: bool) -> Option<KspMember> {
        let declaration = self.libraries.named_member(owner, name, property)?;
        Some(KspMember::Inherited {
            owner: qualified_name(owner),
            declaration: Box::new(declaration),
        })
    }
}
