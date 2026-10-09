//! A property declared in one file of the module and read or written from another.
//!
//! The plan is keyed by the checked [`PropertyId`]. Its ABI is two hidden entry points named from
//! that identity: a getter, and a setter for a `var`. The value is carried at the property's
//! declared type, so a value-class property is its projected value, the same in both files.
//!
//! Only the defining file touches storage. Its entry points are the ordinary local realization of
//! a read or a write of that property — a slot, or a source-written or delegated accessor —
//! converted between what that realization carries and the declared type. A package property's
//! entry point runs the file's once-only initializer first: a read observes the initialized
//! value, and a write is not overwritten by a later initializer. A using file evaluates the
//! assigned value before it calls.

use super::super::*;
use crate::fir::{DeclarationFlags, PropertyId};

/// The checked declaration facts the plan reads. A defining file reads them from its own checked
/// declaration and layout, a using file from the module record; both copy them from one
/// declaration header, so the two views agree.
struct PropertyDeclaration {
    property: PropertyId,
    ty: Ty,
    /// Declared in a classifier rather than in a package.
    member: bool,
    /// An extension receiver, a context parameter, or a companion-block association: a property
    /// whose accessors take parameters.
    receiver_parameters: bool,
    flags: DeclarationFlags,
    visibility: crate::types::Visibility,
}

/// The one ABI both files use for a module property.
#[derive(Clone, Copy)]
pub(in super::super) struct PropertyAbi {
    pub(in super::super) property: PropertyId,
    /// The declared type: what the getter returns and the setter takes.
    pub(in super::super) ty: Ty,
    pub(in super::super) writable: bool,
}

impl PropertyAbi {
    fn getter_symbol(&self) -> String {
        super::super::super::super::symbols::module_property_getter_symbol(self.property)
    }

    fn setter_symbol(&self) -> String {
        super::super::super::super::symbols::module_property_setter_symbol(self.property)
    }
}

/// The plan for one declaration, or why another file cannot use it through this ABI.
fn plan(declaration: &PropertyDeclaration) -> Result<PropertyAbi, Unsupported> {
    let flags = declaration.flags;
    if declaration.receiver_parameters {
        return Err("a cross-file property with an extension receiver or context".to_string());
    }
    if flags.has(DeclarationFlags::EXPECT) {
        return Err("a cross-file `expect` property".to_string());
    }
    if declaration.visibility == crate::types::Visibility::Private {
        return Err("a private property of another file".to_string());
    }
    if declaration.member {
        return Err("a cross-file member property".to_string());
    }
    Ok(PropertyAbi {
        property: declaration.property,
        ty: declaration.ty,
        writable: flags.has(DeclarationFlags::MUTABLE),
    })
}

impl FileLowering<'_> {
    /// Whether this property is owned by another source file. The module-reference table also
    /// contains declarations copied for uses in their own file, so presence alone is not an
    /// ownership test.
    pub(in super::super) fn is_imported_property(&self, target: &PropertyId) -> bool {
        self.ir
            .referenced_module_properties
            .get(target)
            .is_some_and(|property| property.source.source != self.source)
    }

    /// The plan for a property declared in another file, from the module record common lowering
    /// copied into this file.
    pub(in super::super) fn imported_property(
        &self,
        target: &PropertyId,
    ) -> Result<PropertyAbi, Unsupported> {
        let record = self
            .ir
            .referenced_module_properties
            .get(target)
            .ok_or_else(|| "a cross-file property with no module record".to_string())?;
        let declaration = PropertyDeclaration {
            property: *target,
            ty: record.ty,
            member: record.owner.is_some(),
            receiver_parameters: record.extension_receiver.is_some()
                || !record.context_parameters.is_empty()
                || !matches!(record.placement, crate::ir::IrStaticPlacement::Package),
            flags: record.flags,
            visibility: record.visibility,
        };
        plan(&declaration)
    }

    /// The checked shape of a property this file declares.
    fn declared_property(&self, property: PropertyId) -> Option<PropertyDeclaration> {
        let checked = self.ir.checked_properties.get(&property)?;
        let receiver_parameters = match self.ir.local_property_layouts.get(&property) {
            Some(IrLocalPropertyLayout::TopLevelAccessor {
                receiver,
                context_parameters,
                ..
            }) => receiver.is_some() || !context_parameters.is_empty(),
            Some(IrLocalPropertyLayout::Member {
                context_parameters, ..
            }) => !context_parameters.is_empty(),
            Some(IrLocalPropertyLayout::MemberExtension { .. }) => true,
            Some(IrLocalPropertyLayout::TopLevelStorage { .. }) | None => false,
        } || checked.flags.has(DeclarationFlags::COMPANION_BLOCK_MEMBER);
        Some(PropertyDeclaration {
            property,
            ty: checked.ty,
            member: checked.class.is_some(),
            receiver_parameters,
            flags: checked.flags,
            visibility: checked.visibility,
        })
    }

    /// Define the getter, and the setter of a `var`, for every property this file declares whose
    /// plan another file can use.
    ///
    /// A supported plan with no package layout in this file is an internal completeness error:
    /// another file may already import the entry point it promises.
    pub(in super::super) fn define_property_entry_points(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        let mut properties: Vec<PropertyId> = self.ir.checked_properties.keys().copied().collect();
        properties.sort_by_key(|property| property.raw());
        for property in properties {
            let Some(declaration) = self.declared_property(property) else {
                continue;
            };
            // The same decision every using file makes: a declined plan has no entry point, and
            // no other file calls one.
            let Ok(abi) = plan(&declaration) else {
                continue;
            };
            let name = self.ir.checked_properties[&property].name.clone();
            if !matches!(
                self.ir.local_property_layouts.get(&property),
                Some(
                    IrLocalPropertyLayout::TopLevelStorage {
                        qualifier: None,
                        ..
                    } | IrLocalPropertyLayout::TopLevelAccessor { .. }
                )
            ) {
                return Err(format!(
                    "internal: the module property `{name}` has no package layout in its file"
                ));
            }
            self.define_property_getter(&abi, file_init, &name)?;
            if abi.writable {
                self.define_property_setter(&abi, file_init, &name)?;
            }
        }
        Ok(())
    }

    fn define_property_getter(
        &mut self,
        abi: &PropertyAbi,
        file_init: FuncId,
        name: &str,
    ) -> Result<(), Unsupported> {
        let symbol = abi.getter_symbol();
        let signature = self.signature_of(&[], abi.ty)?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Hidden, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let (target, ty) = (abi.property, abi.ty);
        let name = name.to_string();
        self.emit_function(id, signature, ty, &symbol, &mut |body, _| {
            let init = body.func_ref(file_init);
            body.emit_call(init, &[])?;
            if body.terminated {
                return Ok(());
            }
            let produced = body.top_level_read_typed(&target)?;
            if body.terminated {
                return Ok(());
            }
            let Some((value, produced)) = produced else {
                return Err(format!("a read of `{name}` produced no value"));
            };
            let Some(value) = body.convert(value, Some(produced), ty)? else {
                return Err(format!("a read of `{name}` produced no value"));
            };
            body.builder.ins().return_(&[value]);
            body.terminate();
            Ok(())
        })
    }

    fn define_property_setter(
        &mut self,
        abi: &PropertyAbi,
        file_init: FuncId,
        name: &str,
    ) -> Result<(), Unsupported> {
        let symbol = abi.setter_symbol();
        let signature = self.signature_of(&[abi.ty], Ty::Unit)?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Hidden, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let (target, ty) = (abi.property, abi.ty);
        let name = name.to_string();
        self.emit_function(id, signature, Ty::Unit, &symbol, &mut |body, params| {
            let init = body.func_ref(file_init);
            body.emit_call(init, &[])?;
            if body.terminated {
                return Ok(());
            }
            let stored = body.top_level_written_ty(&target)?;
            let Some(value) = body.convert(params[0], Some(ty), stored)? else {
                return Err(format!("a `Unit` value assigned to `{name}`"));
            };
            body.top_level_write_value(&target, value)?;
            if body.terminated {
                return Ok(());
            }
            body.builder.ins().return_(&[]);
            body.terminate();
            Ok(())
        })
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Read a property another file of this module declares, through its getter entry point.
    pub(in super::super) fn module_property_read(
        &mut self,
        target: &PropertyId,
        receiver: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        let abi = self.file.imported_property(target)?;
        if receiver.is_some() {
            return Err("a cross-file package property read through a receiver".to_string());
        }
        let id = self.file.import(&abi.getter_symbol(), &[], abi.ty)?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &[])?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// Assign a property another file of this module declares, through its setter entry point.
    ///
    /// The value is evaluated first. The file initializer runs inside the setter, after it: a
    /// `putstatic` evaluates its value, then initializes the declaring class, then stores.
    pub(in super::super) fn module_property_write(
        &mut self,
        target: &PropertyId,
        receiver: Option<u32>,
        value: u32,
    ) -> Result<(), Unsupported> {
        let abi = self.file.imported_property(target)?;
        if !abi.writable {
            return Err("a read-only cross-file property was written".to_string());
        }
        if receiver.is_some() {
            return Err("a cross-file package property written through a receiver".to_string());
        }
        let value = self.coerce(value, abi.ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err("a `Unit` value assigned to a cross-file property".to_string());
        };
        let id = self
            .file
            .import(&abi.setter_symbol(), &[abi.ty], Ty::Unit)?;
        let func_ref = self.func_ref(id);
        self.emit_call(func_ref, &[value])?;
        Ok(())
    }

    /// The type a read of a property declared in another file produces: the declared type the
    /// getter returns. A consumer adapts it to its own use-site type the way it adapts a read of a
    /// property this file declares.
    pub(in super::super) fn module_property_ty(&self, target: &PropertyId) -> Option<Ty> {
        self.file.imported_property(target).ok().map(|abi| abi.ty)
    }
}
