//! A property declared in one file of the module and read or written from another.
//!
//! The plan is keyed by the checked [`PropertyId`]. Its ABI is two hidden entry points named from
//! that identity: a getter, and a setter for a `var`. A member takes its receiver first, carried
//! as the declaring classifier; a package property takes none. The value is carried at the
//! property's declared type, so a generic property is its erased declaration type at the boundary
//! and a value-class property its projected value, the same in both files.
//!
//! Only the defining file touches storage. Its entry points are the ordinary local realization of
//! a read or a write of that property — a slot, a field, a source-written or delegated accessor, a
//! dispatch slot of an `open` member — converted between what that realization carries and the
//! declared type. A package property's entry point runs the file's once-only initializer first: a
//! read observes the initialized value, and a write is not overwritten by a later initializer. A
//! using file evaluates the receiver and then the assigned value before it calls.

use super::super::*;
use crate::fir::{DeclarationFlags, PropertyId};

/// The checked declaration facts the plan reads. A defining file reads them from its own checked
/// declaration and layout, a using file from the module record; both copy them from one
/// declaration header, so the two views agree.
struct PropertyDeclaration {
    property: PropertyId,
    ty: Ty,
    owner: Option<Owner>,
    /// An extension receiver, a context parameter, or a companion-block association: a property
    /// whose accessors take more than a dispatch receiver.
    receiver_parameters: bool,
    flags: DeclarationFlags,
    visibility: crate::types::Visibility,
}

#[derive(Clone, Copy)]
struct Owner {
    classifier: TypeName,
    kind: OwnerKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnerKind {
    /// A class, an object, or an enum: a receiver whose storage the declaring file lays out.
    Concrete,
    Interface,
    Annotation,
}

/// Where the property lives, which decides the receiver its entry points take.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum PropertyPlacement {
    /// A package-level property: no receiver, and the declaring file initializes first.
    Package,
    /// A member: the receiver is an instance of `owner`.
    Member { owner: TypeName },
}

/// The one ABI both files use for a module property.
#[derive(Clone, Copy)]
pub(in super::super) struct PropertyAbi {
    pub(in super::super) property: PropertyId,
    pub(in super::super) placement: PropertyPlacement,
    /// The declared type: what the getter returns and the setter takes.
    pub(in super::super) ty: Ty,
    pub(in super::super) writable: bool,
}

impl PropertyAbi {
    fn receiver(&self) -> Option<Ty> {
        match self.placement {
            PropertyPlacement::Package => None,
            PropertyPlacement::Member { owner } => Some(Ty::obj_name(owner)),
        }
    }

    fn getter_parameters(&self) -> Vec<Ty> {
        self.receiver().into_iter().collect()
    }

    fn setter_parameters(&self) -> Vec<Ty> {
        let mut parameters = self.getter_parameters();
        parameters.push(self.ty);
        parameters
    }

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
    let placement = match declaration.owner {
        None => PropertyPlacement::Package,
        Some(owner) => match owner.kind {
            OwnerKind::Interface => {
                return Err("a cross-file interface property".to_string());
            }
            OwnerKind::Annotation => {
                return Err("a cross-file annotation property".to_string());
            }
            // A classifier constant is folded at its uses and stored as a static of the
            // classifier, not as a member of its instance.
            OwnerKind::Concrete if flags.has(DeclarationFlags::CONST) => {
                return Err("a cross-file classifier constant".to_string());
            }
            OwnerKind::Concrete => PropertyPlacement::Member {
                owner: owner.classifier,
            },
        },
    };
    Ok(PropertyAbi {
        property: declaration.property,
        placement,
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
        let owner = record
            .owner
            .map(|classifier| {
                let kind = match record.owner_kind {
                    Some(crate::ir::IrClassifierKind::Interface) => OwnerKind::Interface,
                    Some(crate::ir::IrClassifierKind::Annotation) => OwnerKind::Annotation,
                    Some(
                        crate::ir::IrClassifierKind::Class
                        | crate::ir::IrClassifierKind::Object
                        | crate::ir::IrClassifierKind::Enum,
                    ) => OwnerKind::Concrete,
                    None => {
                        return Err(format!(
                            "a module property owned by a classifier of no recorded kind (`{}`)",
                            record.name
                        ))
                    }
                };
                Ok(Owner { classifier, kind })
            })
            .transpose()?;
        let declaration = PropertyDeclaration {
            property: *target,
            ty: record.ty,
            owner,
            receiver_parameters: record.extension_receiver.is_some()
                || !record.context_parameters.is_empty()
                || !matches!(record.placement, crate::ir::IrStaticPlacement::Package),
            flags: record.flags,
            visibility: record.visibility,
        };
        plan(&declaration)
    }

    /// The checked shape of a property this file declares, when another file can name it at all.
    ///
    /// A property of a local or anonymous class, or of an enum entry's body, is reached only from
    /// the code that declares it, and no other file holds a module record for it.
    fn declared_property(&self, property: PropertyId) -> Option<PropertyDeclaration> {
        let checked = self.ir.checked_properties.get(&property)?;
        let owner = match checked.class {
            None => None,
            Some(class) => {
                let class = &self.ir.classes[class as usize];
                if class.is_local_class
                    || class.is_anonymous_object
                    || class.is_enum_entry
                    || !class.is_source_declared
                {
                    return None;
                }
                let kind = if class.is_annotation {
                    OwnerKind::Annotation
                } else if class.is_interface {
                    OwnerKind::Interface
                } else {
                    OwnerKind::Concrete
                };
                Some(Owner {
                    classifier: class.fq_name,
                    kind,
                })
            }
        };
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
            owner,
            receiver_parameters,
            flags: checked.flags,
            visibility: checked.visibility,
        })
    }

    /// Define the getter, and the setter of a `var`, for every property this file declares whose
    /// plan another file can use.
    ///
    /// A supported plan whose local layout does not have the plan's shape is an internal
    /// completeness error: another file may already import the entry point it promises.
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
            let Ok(abi) = plan(&declaration) else {
                continue;
            };
            let name = self.ir.checked_properties[&property].name.clone();
            let realization = self.local_realization(&abi, &name)?;
            self.define_property_getter(&abi, realization, file_init, &name)?;
            if abi.writable {
                self.define_property_setter(&abi, realization, file_init, &name)?;
            }
        }
        Ok(())
    }

    /// Where this file realizes a property it exports, checked against the plan's placement.
    fn local_realization(
        &self,
        abi: &PropertyAbi,
        name: &str,
    ) -> Result<LocalRealization, Unsupported> {
        let layout = self.ir.local_property_layouts.get(&abi.property);
        match (abi.placement, layout) {
            (
                PropertyPlacement::Package,
                Some(
                    IrLocalPropertyLayout::TopLevelStorage {
                        qualifier: None, ..
                    }
                    | IrLocalPropertyLayout::TopLevelAccessor { .. },
                ),
            ) => Ok(LocalRealization::Package),
            (
                PropertyPlacement::Member { owner },
                Some(IrLocalPropertyLayout::Member {
                    class,
                    property,
                    owner: declared,
                    ..
                }),
            ) if *declared == owner => Ok(LocalRealization::Member {
                class: *class,
                index: *property as usize,
                value_class: self.values.is_value_class(owner),
            }),
            _ => Err(format!(
                "internal: the module property `{name}` has no local layout of its planned shape"
            )),
        }
    }

    fn define_property_getter(
        &mut self,
        abi: &PropertyAbi,
        realization: LocalRealization,
        file_init: FuncId,
        name: &str,
    ) -> Result<(), Unsupported> {
        let symbol = abi.getter_symbol();
        let signature = self.signature_of(&abi.getter_parameters(), abi.ty)?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Hidden, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let (target, ty) = (abi.property, abi.ty);
        let name = name.to_string();
        self.emit_function(id, signature, ty, &symbol, &mut |body, params| {
            let produced = match realization {
                LocalRealization::Package => {
                    let init = body.func_ref(file_init);
                    body.emit_call(init, &[])?;
                    if body.terminated {
                        return Ok(());
                    }
                    body.top_level_read_typed(&target)?
                }
                LocalRealization::Member {
                    class,
                    index,
                    value_class: true,
                } => body.value_property_read_typed(class, index, params[0])?,
                LocalRealization::Member { class, index, .. } => {
                    body.property_read_typed(class, index, params[0])?
                }
            };
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
        realization: LocalRealization,
        file_init: FuncId,
        name: &str,
    ) -> Result<(), Unsupported> {
        let symbol = abi.setter_symbol();
        let signature = self.signature_of(&abi.setter_parameters(), Ty::Unit)?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Hidden, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let (target, ty) = (abi.property, abi.ty);
        let name = name.to_string();
        self.emit_function(id, signature, Ty::Unit, &symbol, &mut |body, params| {
            match realization {
                LocalRealization::Package => {
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
                }
                LocalRealization::Member {
                    value_class: true, ..
                } => {
                    return Err(format!(
                        "internal: a writable property of a value class (`{name}`)"
                    ));
                }
                LocalRealization::Member { class, index, .. } => {
                    let stored = body.written_property_ty(class, index)?;
                    let Some(value) = body.convert(params[1], Some(ty), stored)? else {
                        return Err(format!("a `Unit` value assigned to `{name}`"));
                    };
                    body.property_write_of(class, index, params[0], value)?;
                }
            }
            if body.terminated {
                return Ok(());
            }
            body.builder.ins().return_(&[]);
            body.terminate();
            Ok(())
        })
    }
}

/// How the defining file realizes an exported property locally.
#[derive(Clone, Copy)]
enum LocalRealization {
    Package,
    Member {
        class: ClassId,
        index: usize,
        value_class: bool,
    },
}

impl BodyLowering<'_, '_, '_> {
    /// Read a property another file of this module declares, through its getter entry point.
    pub(in super::super) fn module_property_read(
        &mut self,
        target: &PropertyId,
        receiver: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        let abi = self.file.imported_property(target)?;
        let Some(arguments) = self.module_property_receiver(&abi, receiver)? else {
            return Ok(None);
        };
        let id = self
            .file
            .import(&abi.getter_symbol(), &abi.getter_parameters(), abi.ty)?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// Assign a property another file of this module declares, through its setter entry point.
    ///
    /// The receiver is evaluated first and the value next, which is Kotlin's order for
    /// `obj.prop = expr`. A package property's file initializer runs inside the setter, after
    /// both: a `putstatic` evaluates its value, then initializes the declaring class, then stores.
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
        let Some(mut arguments) = self.module_property_receiver(&abi, receiver)? else {
            return Ok(());
        };
        let value = self.coerce(value, abi.ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err("a `Unit` value assigned to a cross-file property".to_string());
        };
        arguments.push(value);
        let id = self
            .file
            .import(&abi.setter_symbol(), &abi.setter_parameters(), Ty::Unit)?;
        let func_ref = self.func_ref(id);
        self.emit_call(func_ref, &arguments)?;
        Ok(())
    }

    /// The receiver operand of an entry point, at the declaring classifier's carrier. `None`
    /// when evaluating it left the block.
    fn module_property_receiver(
        &mut self,
        abi: &PropertyAbi,
        receiver: Option<u32>,
    ) -> Result<Option<Vec<Value>>, Unsupported> {
        match (abi.receiver(), receiver) {
            (None, None) => Ok(Some(Vec::new())),
            (Some(ty), Some(receiver)) => {
                let value = self.coerce(receiver, ty)?;
                if self.terminated {
                    return Ok(None);
                }
                let Some(value) = value else {
                    return Err("a `Unit` receiver of a cross-file property".to_string());
                };
                Ok(Some(vec![value]))
            }
            (None, Some(_)) => {
                Err("a cross-file package property read through a receiver".to_string())
            }
            (Some(_), None) => {
                Err("a receiver-less access to a cross-file member property".to_string())
            }
        }
    }

    /// The type a read of a property declared in another file produces: the declared type the
    /// getter returns. A consumer adapts it to its own use-site type the way it adapts a read of a
    /// property this file declares.
    pub(in super::super) fn module_property_ty(&self, target: &PropertyId) -> Option<Ty> {
        self.file.imported_property(target).ok().map(|abi| abi.ty)
    }
}
