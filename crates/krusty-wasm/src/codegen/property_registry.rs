//! Exact property identities in one stateful Wasm source module.
//!
//! Files reserve or publish the Wasm global chosen for each top-level property, private
//! getter/setter entries for each member property, and private getter/setter entries for each
//! property without a dispatch receiver realized by its accessors (a computed or delegated package
//! property, an extension or context package property, or a property of a `companion { }` block).
//! An entry of an extension or context property takes its accessors' declared operands, in the
//! order [`receiver_property_accessors`] gives both files. Other files reach any of these
//! realizations by the frontend's stable [`PropertyId`]; neither spelling nor a file facade is
//! reconstructed.

use krusty::fx_hash::HashMap;

use krusty::backend::receiver_property_accessors;
use krusty::fir::{DeclarationFlags, PropertyId};
use krusty::ir::{IrFile, IrLocalPropertyLayout};
use krusty::types::{Ty, TypeName};

use super::super::encode::Module;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TopLevelProperty {
    pub(super) global: u32,
    pub(super) ty: Ty,
    pub(super) mutable: bool,
    pub(super) direct_read: bool,
    pub(super) direct_write: bool,
}

/// The private Wasm entry points through which another source file reads or writes a member
/// property. A `PropertyId` is the only join between the using and defining files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MemberProperty {
    pub(super) getter: u32,
    pub(super) setter: Option<u32>,
    pub(super) owner: TypeName,
    pub(super) ty: Ty,
    /// What the accessors take after the receiver and before a setter's value: an extension or
    /// context property's context parameters, then its extension receiver.
    pub(super) parameters: Box<[Ty]>,
}

/// One property this file defines, paired with the program-wide entries reserved for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MemberPropertyDefinition {
    pub(super) property: PropertyId,
    pub(super) entry: MemberProperty,
}

/// The private Wasm entry points through which another source file reads or writes a property
/// with no dispatch receiver that its defining file realizes through accessors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AccessorProperty {
    pub(super) getter: u32,
    pub(super) setter: Option<u32>,
    pub(super) ty: Ty,
    /// What the accessors take before a setter's value: an extension or context property's
    /// context parameters, then its extension receiver.
    pub(super) parameters: Box<[Ty]>,
}

/// One accessor property this file defines, paired with its program-wide entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AccessorPropertyDefinition {
    pub(super) property: PropertyId,
    pub(super) entry: AccessorProperty,
}

#[derive(Default)]
pub(super) struct PropertyRegistry {
    source_module: Option<String>,
    module: HashMap<PropertyId, TopLevelProperty>,
    members: HashMap<PropertyId, MemberProperty>,
    accessors: HashMap<PropertyId, AccessorProperty>,
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
                self.accessors.clear();
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
                    ..
                } => Some((property, Some(*class), *owner, *ty, *mutable, layout)),
                // A member extension's layout names its owner, not the owner's class index: the
                // checked declaration records that.
                IrLocalPropertyLayout::MemberExtension { owner, ty, .. } => {
                    let mutable = ir
                        .checked_properties
                        .get(&property)
                        .is_some_and(|checked| checked.flags.has(DeclarationFlags::MUTABLE));
                    Some((property, None, *owner, *ty, mutable, layout))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(property, ..)| property.raw());

        let mut definitions = Vec::new();
        for (property, class, owner, ty, mutable, layout) in local {
            let declaration = ir
                .checked_properties
                .get(&property)
                .ok_or_else(|| "a member property with no checked declaration".to_string())?;
            let class = class
                .or(declaration.class)
                .ok_or_else(|| "a member property with no owning class".to_string())?;
            let classifier = ir
                .classes
                .get(class as usize)
                .ok_or_else(|| "a member property with an invalid owner".to_string())?;
            if declaration.visibility == krusty::types::Visibility::Private
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
            let parameters = receiver_property_accessors::layout_parameters(layout);
            let entry = self.reserve_member(module, property, owner, ty, mutable, &parameters)?;
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
            if declaration.visibility == krusty::types::Visibility::Private
                || declaration.flags.has(DeclarationFlags::EXPECT)
                || declaration.flags.has(DeclarationFlags::CONST)
                || !matches!(
                    declaration.owner_kind,
                    Some(
                        krusty::ir::IrClassifierKind::Class
                            | krusty::ir::IrClassifierKind::Object
                            | krusty::ir::IrClassifierKind::Enum
                    )
                )
                || !matches!(
                    declaration.placement,
                    krusty::ir::IrStaticPlacement::Package
                )
            {
                continue;
            }
            let parameters = receiver_property_accessors::record_parameters(declaration);
            self.reserve_member(
                module,
                property,
                owner,
                declaration.ty,
                declaration.mutable,
                &parameters,
            )?;
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
        parameters: &[Ty],
    ) -> Result<MemberProperty, String> {
        if let Some(entry) = self.members.get(&property) {
            if entry.owner != owner
                || entry.ty != ty
                || entry.setter.is_some() != mutable
                || *entry.parameters != *parameters
            {
                return Err("a member property reservation disagrees with its declaration".into());
            }
            return Ok(entry.clone());
        }
        let entry = MemberProperty {
            getter: module.declare(),
            setter: mutable.then(|| module.declare()),
            owner,
            ty,
            parameters: parameters.into(),
        };
        self.members.insert(property, entry.clone());
        Ok(entry)
    }

    pub(super) fn member(&self, property: PropertyId) -> Option<MemberProperty> {
        self.members.get(&property).cloned()
    }

    /// Reserve the accessor-property entries this file defines or references. A file defines an
    /// entry for each receiver-less property another file may reach only through its accessors;
    /// a using file reserves the same entry from the module record, by the same declaration facts.
    pub(super) fn prepare_accessor_properties(
        &mut self,
        ir: &IrFile,
        module: &mut Module,
    ) -> Result<Vec<AccessorPropertyDefinition>, String> {
        let mut local = ir
            .local_property_layouts
            .iter()
            .filter_map(|(&property, layout)| match layout {
                IrLocalPropertyLayout::TopLevelStorage {
                    qualifier: None, ..
                }
                | IrLocalPropertyLayout::TopLevelAccessor { .. } => Some((property, layout)),
                _ => None,
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(property, _)| property.raw());

        let mut definitions = Vec::new();
        for (property, layout) in local {
            // A property common lowering synthesized, such as a local declaration copied out of an
            // inlined body, has no checked declaration and no module record another file could
            // name.
            let Some(declaration) = ir.checked_properties.get(&property) else {
                continue;
            };
            let mutable = declaration.flags.has(DeclarationFlags::MUTABLE);
            if declaration.visibility == krusty::types::Visibility::Private
                || !accessor_realized(declaration.flags, mutable)
            {
                continue;
            }
            let parameters = receiver_property_accessors::layout_parameters(layout);
            let entry =
                self.reserve_accessor(module, property, declaration.ty, mutable, &parameters)?;
            definitions.push(AccessorPropertyDefinition { property, entry });
        }

        let mut referenced = ir.referenced_module_properties.iter().collect::<Vec<_>>();
        referenced.sort_by_key(|(property, _)| **property);
        for (&property, declaration) in referenced {
            if ir.local_property_layouts.contains_key(&property) {
                continue;
            }
            // A block property's recorded owner is its declaring classifier: the coordinate that
            // associates it with the class, which its accessors do not take.
            let dispatched = match declaration.placement {
                krusty::ir::IrStaticPlacement::CompanionBlock { .. } => false,
                krusty::ir::IrStaticPlacement::Package => declaration.owner.is_some(),
            };
            if dispatched
                || declaration.visibility == krusty::types::Visibility::Private
                || !accessor_realized(declaration.flags, declaration.mutable)
            {
                continue;
            }
            let parameters = receiver_property_accessors::record_parameters(declaration);
            self.reserve_accessor(
                module,
                property,
                declaration.ty,
                declaration.mutable,
                &parameters,
            )?;
        }
        Ok(definitions)
    }

    fn reserve_accessor(
        &mut self,
        module: &mut Module,
        property: PropertyId,
        ty: Ty,
        mutable: bool,
        parameters: &[Ty],
    ) -> Result<AccessorProperty, String> {
        if let Some(entry) = self.accessors.get(&property) {
            if entry.ty != ty
                || entry.setter.is_some() != mutable
                || *entry.parameters != *parameters
            {
                return Err(
                    "an accessor property reservation disagrees with its declaration".into(),
                );
            }
            return Ok(entry.clone());
        }
        let entry = AccessorProperty {
            getter: module.declare(),
            setter: mutable.then(|| module.declare()),
            ty,
            parameters: parameters.into(),
        };
        self.accessors.insert(property, entry.clone());
        Ok(entry)
    }

    pub(super) fn accessor(&self, property: PropertyId) -> Option<AccessorProperty> {
        self.accessors.get(&property).cloned()
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
                direct_read: declaration.accessors.getter != krusty::ir::IrStaticAccessor::Declared,
                direct_write: declaration.is_var
                    && declaration.accessors.setter != krusty::ir::IrStaticAccessor::Declared,
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

/// Whether a receiver-less property is realized only through its accessors: it has no storage a
/// global could hold, or its getter and setter both have bodies. A `lateinit`, an explicit backing
/// field, an `expect`, an `external`, an `abstract` and a `const` declaration are not.
fn accessor_realized(flags: DeclarationFlags, mutable: bool) -> bool {
    !(flags.has(DeclarationFlags::LATEINIT)
        || flags.has(DeclarationFlags::EXPLICIT_BACKING_FIELD)
        || flags.has(DeclarationFlags::EXTERNAL)
        || flags.has(DeclarationFlags::EXPECT)
        || flags.has(DeclarationFlags::ABSTRACT)
        || flags.has(DeclarationFlags::CONST))
        && storage_capabilities(flags, mutable).is_none()
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

    #[test]
    fn only_properties_without_direct_storage_are_reached_through_accessor_entries() {
        let computed = DeclarationFlags::default().with(DeclarationFlags::CUSTOM_GETTER, true);
        assert!(accessor_realized(computed, false));

        let custom_accessors = DeclarationFlags::default()
            .with(DeclarationFlags::HAS_INITIALIZER, true)
            .with(DeclarationFlags::CUSTOM_GETTER, true)
            .with(DeclarationFlags::SETTER_HAS_BODY, true);
        assert!(accessor_realized(custom_accessors, true));

        let stored = DeclarationFlags::default().with(DeclarationFlags::HAS_INITIALIZER, true);
        assert!(!accessor_realized(stored, true));

        for excluded in [
            DeclarationFlags::LATEINIT,
            DeclarationFlags::EXPLICIT_BACKING_FIELD,
            DeclarationFlags::EXTERNAL,
            DeclarationFlags::EXPECT,
            DeclarationFlags::ABSTRACT,
            DeclarationFlags::CONST,
        ] {
            assert!(!accessor_realized(computed.with(excluded, true), false));
        }
    }
}
