//! Exact property identities in one stateful Wasm source module.
//!
//! Files reserve or publish the Wasm global chosen for each top-level property and private
//! getter/setter entries for each ordinary member property. Other files reach either realization
//! by the frontend's stable [`PropertyId`]; neither spelling nor a file facade is reconstructed.

use crate::fx_hash::HashMap;

use crate::fir::{DeclarationFlags, PropertyId};
use crate::ir::{IrFile, IrLocalPropertyLayout};
use crate::types::{Ty, TypeName};

use super::super::encode::Module;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TopLevelProperty {
    pub(super) global: u32,
    pub(super) ty: Ty,
    pub(super) mutable: bool,
    pub(super) direct_read: bool,
    pub(super) direct_write: bool,
}

/// The private Wasm entry points through which another source file reads or writes an ordinary
/// member property. A `PropertyId` is the only join between the using and defining files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MemberProperty {
    pub(super) getter: u32,
    pub(super) setter: Option<u32>,
    pub(super) owner: TypeName,
    pub(super) ty: Ty,
}

/// One property this file defines, paired with the program-wide entries reserved for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MemberPropertyDefinition {
    pub(super) property: PropertyId,
    pub(super) entry: MemberProperty,
}

#[derive(Default)]
pub(super) struct PropertyRegistry {
    source_module: Option<String>,
    module: HashMap<PropertyId, TopLevelProperty>,
    members: HashMap<PropertyId, MemberProperty>,
}

impl PropertyRegistry {
    /// Property ids belong to one frontend analysis, so they never cross a source-module boundary.
    pub(super) fn enter_source_module(&mut self, module_name: &str) {
        match self.source_module.as_deref() {
            None => self.source_module = Some(module_name.to_owned()),
            Some(current) if current == module_name => {}
            Some(_) => {
                self.source_module = Some(module_name.to_owned());
                self.module.clear();
                self.members.clear();
            }
        }
    }

    /// Reserve the member-property entries this file defines or references. Definitions are
    /// returned so their bodies can be emitted after this file's classes and accessors exist.
    pub(super) fn prepare_member_properties(
        &mut self,
        ir: &IrFile,
        module: &mut Module,
    ) -> Result<Vec<MemberPropertyDefinition>, String> {
        let mut local = ir
            .local_property_layouts
            .iter()
            .filter_map(|(&property, layout)| match layout {
                IrLocalPropertyLayout::Member {
                    class,
                    owner,
                    ty,
                    mutable,
                    context_parameters,
                    ..
                } if context_parameters.is_empty() => {
                    Some((property, *class, *owner, *ty, *mutable))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(property, ..)| property.raw());

        let mut definitions = Vec::new();
        for (property, class, owner, ty, mutable) in local {
            let declaration = ir
                .checked_properties
                .get(&property)
                .ok_or_else(|| "a member property with no checked declaration".to_string())?;
            let classifier = ir
                .classes
                .get(class as usize)
                .ok_or_else(|| "a member property with an invalid owner".to_string())?;
            if declaration.visibility == crate::types::Visibility::Private
                || declaration.flags.has(DeclarationFlags::EXPECT)
                || declaration.flags.has(DeclarationFlags::CONST)
                || classifier.is_interface
                || classifier.is_annotation
                || classifier.is_local_class
                || classifier.is_anonymous_object
                || classifier.is_enum_entry
                || !classifier.is_source_declared
            {
                continue;
            }
            let entry = self.reserve_member(module, property, owner, ty, mutable)?;
            definitions.push(MemberPropertyDefinition { property, entry });
        }

        let mut referenced = ir.referenced_module_properties.iter().collect::<Vec<_>>();
        referenced.sort_by_key(|(property, _)| **property);
        for (&property, declaration) in referenced {
            // Common IR may publish a module record for a same-file use too. Its local layout is
            // authoritative here: only a property absent from that inventory needs a cross-file
            // entry. In particular, local and anonymous classes cannot publish an entry another
            // file can call, and the local-definition pass above deliberately skips them.
            if ir.local_property_layouts.contains_key(&property) {
                continue;
            }
            let Some(owner) = declaration.owner else {
                continue;
            };
            if declaration.extension_receiver.is_some()
                || !declaration.context_parameters.is_empty()
                || declaration.visibility == crate::types::Visibility::Private
                || declaration.flags.has(DeclarationFlags::EXPECT)
                || declaration.flags.has(DeclarationFlags::CONST)
                || !matches!(
                    declaration.owner_kind,
                    Some(
                        crate::ir::IrClassifierKind::Class
                            | crate::ir::IrClassifierKind::Object
                            | crate::ir::IrClassifierKind::Enum
                    )
                )
                || !matches!(declaration.placement, crate::ir::IrStaticPlacement::Package)
            {
                continue;
            }
            self.reserve_member(module, property, owner, declaration.ty, declaration.mutable)?;
        }
        Ok(definitions)
    }

    fn reserve_member(
        &mut self,
        module: &mut Module,
        property: PropertyId,
        owner: TypeName,
        ty: Ty,
        mutable: bool,
    ) -> Result<MemberProperty, String> {
        if let Some(entry) = self.members.get(&property).copied() {
            if entry.owner != owner || entry.ty != ty || entry.setter.is_some() != mutable {
                return Err("a member property reservation disagrees with its declaration".into());
            }
            return Ok(entry);
        }
        let entry = MemberProperty {
            getter: module.declare(),
            setter: mutable.then(|| module.declare()),
            owner,
            ty,
        };
        self.members.insert(property, entry);
        Ok(entry)
    }

    pub(super) fn member(&self, property: PropertyId) -> Option<MemberProperty> {
        self.members.get(&property).copied()
    }

    /// Allocate this file's globals, reusing indices reserved by an earlier file, then reserve
    /// receiver-less properties referenced from files which have not been lowered yet.
    pub(super) fn prepare_file(
        &mut self,
        ir: &IrFile,
        mut allocate: impl FnMut(Ty) -> Result<u32, String>,
    ) -> Result<Vec<u32>, String> {
        let mut identities = vec![Vec::new(); ir.statics.len()];
        for (&property, layout) in &ir.local_property_layouts {
            let IrLocalPropertyLayout::TopLevelStorage {
                storage,
                qualifier: None,
                ..
            } = layout
            else {
                continue;
            };
            let targets = identities
                .get_mut(*storage as usize)
                .ok_or_else(|| "an invalid top-level property storage".to_string())?;
            targets.push(property);
        }

        let mut globals = Vec::with_capacity(ir.statics.len());
        for (storage, declaration) in ir.statics.iter().enumerate() {
            let declared = TopLevelProperty {
                global: 0,
                ty: declaration.ty,
                mutable: declaration.is_var,
                direct_read: declaration.accessors.getter != crate::ir::IrStaticAccessor::Declared,
                direct_write: declaration.is_var
                    && declaration.accessors.setter != crate::ir::IrStaticAccessor::Declared,
            };
            let mut reserved = identities[storage]
                .iter()
                .filter_map(|identity| self.module.get(identity).copied());
            let target = match reserved.next() {
                Some(target) => {
                    if target.ty != declared.ty
                        || target.mutable != declared.mutable
                        || target.direct_read != declared.direct_read
                        || target.direct_write != declared.direct_write
                    {
                        return Err(
                            "a reserved top-level property disagrees with its declaration"
                                .to_string(),
                        );
                    }
                    target
                }
                None => TopLevelProperty {
                    global: allocate(declaration.ty)?,
                    ..declared
                },
            };
            if reserved.any(|other| other != target) {
                return Err(
                    "two source property identities reserved different Wasm globals".to_string(),
                );
            }
            for &identity in &identities[storage] {
                self.bind(identity, target)?;
            }
            globals.push(target.global);
        }

        let mut references = ir.referenced_module_properties.iter().collect::<Vec<_>>();
        references.sort_by_key(|(property, _)| **property);
        for (&property, reference) in references {
            if reference.owner.is_some()
                || reference.extension_receiver.is_some()
                || !reference.context_parameters.is_empty()
            {
                continue;
            }
            let Some((direct_read, direct_write)) =
                storage_capabilities(reference.flags, reference.mutable)
            else {
                continue;
            };
            let target = match self.module.get(&property).copied() {
                Some(target) => target,
                None => TopLevelProperty {
                    global: allocate(reference.ty)?,
                    ty: reference.ty,
                    mutable: reference.mutable,
                    direct_read,
                    direct_write,
                },
            };
            if target.ty != reference.ty
                || target.mutable != reference.mutable
                || target.direct_read != direct_read
                || target.direct_write != direct_write
            {
                return Err(
                    "a top-level property reservation disagrees with its module record".to_string(),
                );
            }
            self.bind(property, target)?;
        }
        Ok(globals)
    }

    pub(super) fn get(&self, property: PropertyId) -> Option<TopLevelProperty> {
        self.module.get(&property).copied()
    }

    fn bind(&mut self, property: PropertyId, target: TopLevelProperty) -> Result<(), String> {
        match self.module.insert(property, target) {
            Some(previous) if previous != target => {
                Err("two top-level properties published one stable identity".to_string())
            }
            _ => Ok(()),
        }
    }
}

/// Direct storage is valid only for the accessor directions whose source declaration left the
/// compiler default. A computed or delegated accessor must never become a zero-initialized global.
fn storage_capabilities(flags: DeclarationFlags, mutable: bool) -> Option<(bool, bool)> {
    if !flags.has(DeclarationFlags::HAS_INITIALIZER)
        || flags.has(DeclarationFlags::DELEGATED)
        || flags.has(DeclarationFlags::EXPLICIT_BACKING_FIELD)
        || flags.has(DeclarationFlags::LATEINIT)
        || flags.has(DeclarationFlags::EXTERNAL)
        || flags.has(DeclarationFlags::EXPECT)
        || flags.has(DeclarationFlags::ABSTRACT)
    {
        return None;
    }
    let direct_read = !flags.has(DeclarationFlags::CUSTOM_GETTER);
    let direct_write = mutable && !flags.has(DeclarationFlags::SETTER_HAS_BODY);
    (direct_read || direct_write).then_some((direct_read, direct_write))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computed_and_declared_accessors_are_not_fabricated_as_storage() {
        let computed = DeclarationFlags::default().with(DeclarationFlags::CUSTOM_GETTER, true);
        assert_eq!(storage_capabilities(computed, false), None);

        let custom_accessors = DeclarationFlags::default()
            .with(DeclarationFlags::HAS_INITIALIZER, true)
            .with(DeclarationFlags::CUSTOM_GETTER, true)
            .with(DeclarationFlags::SETTER_HAS_BODY, true);
        assert_eq!(storage_capabilities(custom_accessors, true), None);

        let default_accessors =
            DeclarationFlags::default().with(DeclarationFlags::HAS_INITIALIZER, true);
        assert_eq!(
            storage_capabilities(default_accessors, true),
            Some((true, true))
        );
    }
}
