//! Declaration-owned slot identities retained with a physical class layout.

use krusty::backend::class_tables::{ClassTable, SlotKey};
use krusty::backend::interface_identity::property_member_identity;
use krusty::backend::{CheckedBackendCallables, DependencyExportSignatures};
use krusty::fir::ResolvedPropertyOverrideTarget;
use krusty::fx_hash::HashMap;

use super::super::laid_out_classes::LaidOutClass;
use super::Unsupported;

pub(super) fn record(
    table: &ClassTable,
    parent: Option<&LaidOutClass>,
    exports: &DependencyExportSignatures,
    frozen: &CheckedBackendCallables,
) -> Result<HashMap<Vec<u8>, u32>, Unsupported> {
    let mut declarations = parent
        .map(|parent| parent.declaration_slots.clone())
        .unwrap_or_default();
    for (member, &slot) in &table.slots {
        let identity = match member {
            SlotKey::Any(_) => None,
            SlotKey::Function(function) => exports.member_slot_identity(*function),
            SlotKey::Getter(property) | SlotKey::Setter(property) => {
                let declaration = match property {
                    ResolvedPropertyOverrideTarget::Module(property) => {
                        exports.property_declaration(*property)
                    }
                    ResolvedPropertyOverrideTarget::External(getter) => {
                        frozen.declaration_identity(*getter)
                    }
                };
                match declaration {
                    Some(declaration) => Some(
                        property_member_identity(declaration, matches!(member, SlotKey::Setter(_)))
                            .ok_or(
                                "a property slot without its exact accessor declaration identity",
                            )?,
                    ),
                    None => None,
                }
            }
        };
        if let Some(identity) = identity {
            if let Some(previous) = declarations.insert(identity, slot) {
                if previous != slot {
                    return Err(
                        "one class member declaration occupies conflicting physical slots".into(),
                    );
                }
            }
        }
    }
    Ok(declarations)
}
