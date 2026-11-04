//! One file's side of interface dispatch (`super::super::interface_dispatch`): what the file's
//! classes implement, and the identities its interfaces publish.
//!
//! A class with instances records, for each member number of its module's interfaces, the function
//! at that number of its interface table (`class_tables::planned_itable`); for each member of a
//! dependency's interface, by the member's published identity
//! (`crate::backend::interface_identity`), its override or the default it inherits; and every
//! interface it is an instance of. A file declaring an interface publishes each member's identity
//! beside its number, which is what a dependent module's classes name their implementations by.
//!
//! An implementation carrying a reference more narrowly than its member does stands behind a thunk
//! with the member's signature that casts the operand and calls it; one whose member carries a
//! value the implementation does not (a primitive behind a type parameter) declines, since that
//! thunk does not box or unbox yet (see `super::super::boxes`).

use crate::fx_hash::HashMap;

use crate::backend::class_tables::module_interfaces::ModuleInterfaces;
use crate::backend::class_tables::module_slots::PlanKey;
use crate::backend::class_tables::planned_itable::InterfaceEntry;
use crate::backend::class_tables::{function_key, Slot, SlotKey};
use crate::backend::interface_identity::{member_identity, property_member_identity};
use crate::backend::{CheckedBackendCallables, DependencyExportSignatures};
use crate::fir::{
    InheritedDefaultBody, InheritedMemberName, ResolvedFunctionOverrideTarget,
    ResolvedPropertyOverrideTarget,
};
use crate::ir::{ClassId, IrFile, IrLocalPropertyLayout};
use crate::types::Ty;

use super::super::encode::{Code, Function, Module, ValType};
use super::super::interface_dispatch::{ClassRef, MemberRef};
use super::super::objects::{ObjectRoot, REFERENCE};
use super::callable_registry::CallableRegistry;
use super::classes::{accessor_function, field_carrier, Accessor, FileClasses};
use super::lower::{carrier, Unsupported};

/// A function's wasm parameters (the receiver first) and results.
pub(super) type Signature = (Vec<ValType>, Vec<ValType>);

/// The signature of a member taking `params` after its receiver and answering `result`.
pub(super) fn member_signature(params: &[Ty], result: Ty) -> Result<Signature, Unsupported> {
    let mut carriers = vec![REFERENCE];
    for param in params {
        carriers.push(carrier(*param)?.ok_or_else(|| "a `Unit` parameter".to_string())?);
    }
    Ok((carriers, carrier(result)?.into_iter().collect()))
}

/// The parameters and result of a property accessor of a value of type `ty`.
pub(super) fn accessor_types(ty: Ty, setter: bool) -> (Vec<Ty>, Ty) {
    if setter {
        (vec![ty], Ty::Unit)
    } else {
        (Vec::new(), ty)
    }
}

/// What every recording of one file reads.
pub(super) struct Recording<'a> {
    pub(super) ir: &'a IrFile,
    pub(super) module_name: &'a str,
    pub(super) numbers: &'a ModuleInterfaces,
    /// The Wasm function of each emitted function, by `FunId`.
    pub(super) functions: &'a [Option<u32>],
    pub(super) callables: &'a CheckedBackendCallables,
    pub(super) exports: &'a DependencyExportSignatures,
}

/// Where a recording writes: the module, the object model and the callables of the program.
pub(super) struct Destination<'a> {
    pub(super) module: &'a mut Module,
    pub(super) objects: &'a mut ObjectRoot,
    pub(super) callables: &'a mut CallableRegistry,
}

/// A function and the signature it is carried as.
type Placed = (u32, Signature);

/// Publish the identities of the file's interface members and record what its classes implement.
pub(super) fn record(
    recording: &Recording<'_>,
    classes: &mut FileClasses,
    program: &mut Destination<'_>,
) -> Result<(), Unsupported> {
    publish_identities(recording, program)?;
    for &id in &classes.tables.order.clone() {
        let Some(record) = classes.forms[id as usize].record else {
            continue;
        };
        let mut file = FileRecorder {
            recording,
            classes: &mut *classes,
            program: &mut *program,
            id,
            record,
        };
        let numbered = file.module_implementations()?;
        file.dependency_implementations(&numbered)?;
        file.instances();
    }
    Ok(())
}

/// Name every member the file's interfaces declare by the identity its module publishes.
fn publish_identities(
    recording: &Recording<'_>,
    program: &mut Destination<'_>,
) -> Result<(), Unsupported> {
    let ir = recording.ir;
    let mut callables = ir.checked_callable_functions.iter().collect::<Vec<_>>();
    callables.sort_unstable();
    for (callable, function) in callables {
        let (Some(identity), Some(number)) = (
            recording.exports.interface_member(*function),
            recording.numbers.number(PlanKey::Callable(*callable)),
        ) else {
            continue;
        };
        let declaration = &ir.functions[*function as usize];
        let signature = member_signature(&declaration.params, declaration.ret)?;
        let member = numbered(recording, program, number, signature)?;
        program
            .objects
            .interfaces
            .publish(member_identity(identity), member)?;
    }
    let mut properties = ir.local_property_layouts.iter().collect::<Vec<_>>();
    properties.sort_unstable_by_key(|(property, _)| **property);
    for (property, layout) in properties {
        let Some(getter) = recording.exports.interface_property(*property) else {
            continue;
        };
        for setter in [false, true] {
            let key = if setter {
                PlanKey::Setter(*property)
            } else {
                PlanKey::Getter(*property)
            };
            let (Some(number), Some(identity)) = (
                recording.numbers.number(key),
                property_member_identity(getter, setter),
            ) else {
                continue;
            };
            let signature = match layout {
                IrLocalPropertyLayout::Member { ty, .. } => {
                    let (params, result) = accessor_types(*ty, setter);
                    member_signature(&params, result)?
                }
                // A member extension's accessor takes its extension receiver as a parameter.
                IrLocalPropertyLayout::MemberExtension {
                    getter,
                    setter: setter_function,
                    ..
                } => {
                    let function = if setter {
                        *setter_function
                    } else {
                        Some(*getter)
                    };
                    let Some(function) = function else {
                        continue;
                    };
                    let declaration = &ir.functions[function as usize];
                    member_signature(&declaration.params, declaration.ret)?
                }
                _ => continue,
            };
            let member = numbered(recording, program, number, signature)?;
            program.objects.interfaces.publish(identity, member)?;
        }
    }
    Ok(())
}

fn numbered(
    recording: &Recording<'_>,
    program: &mut Destination<'_>,
    number: u32,
    signature: Signature,
) -> Result<MemberRef, Unsupported> {
    program
        .objects
        .interfaces
        .numbered(program.module, recording.module_name, number, signature)
}

/// The recording of one class with instances.
struct FileRecorder<'r, 'a, 'd> {
    recording: &'r Recording<'a>,
    classes: &'r mut FileClasses,
    program: &'r mut Destination<'d>,
    id: ClassId,
    record: ClassRef,
}

impl FileRecorder<'_, '_, '_> {
    /// Record the function at every number of the class's interface table; the placed functions
    /// by number, for a dependency member a default of this module stands for.
    fn module_implementations(&mut self) -> Result<HashMap<u32, Placed>, Unsupported> {
        let entries = self.classes.tables.interface_tables.tables[self.id as usize].clone();
        let mut placed = HashMap::default();
        for (number, entry) in entries.iter().enumerate() {
            let Some((function, signature)) = self.entry(entry)? else {
                continue;
            };
            let member = numbered(
                self.recording,
                self.program,
                number as u32,
                signature.clone(),
            )?;
            self.program
                .objects
                .interfaces
                .implement(self.record, member, function)?;
            placed.insert(number as u32, (function, signature));
        }
        Ok(placed)
    }

    /// The function an interface table entry holds, or `None` for an abstract one.
    fn entry(&mut self, entry: &InterfaceEntry) -> Result<Option<Placed>, Unsupported> {
        let ir = self.recording.ir;
        match entry {
            InterfaceEntry::Slot(Slot::Abstract) => Ok(None),
            InterfaceEntry::Slot(Slot::Bridge {
                declared,
                target_slot,
                target,
            }) => {
                let implementation = match target_slot {
                    Some(slot) => self.slot_function(*slot)?,
                    None => self.slot_entry(&Slot::Function(*target))?,
                };
                let declared = &ir.functions[*declared as usize];
                let target = &ir.functions[*target as usize];
                self.thunk(
                    (&declared.params, declared.ret),
                    (&target.params, target.ret),
                    implementation,
                )
                .map(Some)
            }
            InterfaceEntry::Slot(Slot::AccessorBridge {
                declared,
                implemented,
                receiver: None,
                setter,
                target_slot,
            }) => {
                let implementation = self.slot_function(*target_slot)?;
                let (declared_params, declared_result) = accessor_types(*declared, *setter);
                let (implemented_params, implemented_result) =
                    accessor_types(*implemented, *setter);
                self.thunk(
                    (&declared_params, declared_result),
                    (&implemented_params, implemented_result),
                    implementation,
                )
                .map(Some)
            }
            InterfaceEntry::Slot(slot) => self.slot_entry(slot).map(Some),
            InterfaceEntry::ModuleFunction(callable) => {
                let (params, result) = self
                    .recording
                    .numbers
                    .function_carriers(PlanKey::Callable(*callable))
                    .ok_or("an interface default of another file with no carriers")?;
                let signature = member_signature(params, result)?;
                let function = self
                    .program
                    .callables
                    .module_index(*callable, self.program.module);
                Ok(Some((function, signature)))
            }
        }
    }

    /// The function class slot `slot` of this class holds.
    fn slot_function(&mut self, slot: u32) -> Result<Placed, Unsupported> {
        let entry = self.classes.tables.table(self.id).vtable[slot as usize].clone();
        self.slot_entry(&entry)
    }

    /// The function a class table entry stands for.
    fn slot_entry(&mut self, slot: &Slot) -> Result<Placed, Unsupported> {
        let ir = self.recording.ir;
        match slot {
            Slot::Function(function) => {
                let index = self
                    .recording
                    .functions
                    .get(*function as usize)
                    .copied()
                    .flatten()
                    .ok_or("an interface table entry with no emitted function")?;
                let declaration = &ir.functions[*function as usize];
                Ok((
                    index,
                    member_signature(&declaration.params, declaration.ret)?,
                ))
            }
            Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field } => {
                let setter = matches!(slot, Slot::FieldSetter { .. });
                let value = field_carrier(ir, *class, *field)?;
                let signature = if setter {
                    (vec![REFERENCE, value], Vec::new())
                } else {
                    (vec![REFERENCE], vec![value])
                };
                let ty = self
                    .program
                    .module
                    .func_type(signature.0.clone(), signature.1.clone());
                let accessor = Accessor {
                    class: *class,
                    field: *field,
                    setter,
                };
                let function = accessor_function(
                    self.program.module,
                    &mut self.classes.accessors,
                    accessor,
                    ty,
                );
                Ok((function, signature))
            }
            _ => Err(format!(
                "an interface member implemented through a bridge this form has no thunk for \
                 (`{}`)",
                ir.classes[self.id as usize].fq_name()
            )),
        }
    }

    /// `implementation`, which takes `implemented`, as a function taking `declared`: itself when
    /// both are carried alike, else a thunk casting each operand the member carries more broadly.
    fn thunk(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        (function, carried): Placed,
    ) -> Result<Placed, Unsupported> {
        let signature = member_signature(declared.0, declared.1)?;
        if signature == carried {
            return Ok((function, carried));
        }
        let unboxed = || {
            format!(
                "an interface member implemented with a value it would have to box (`{}`)",
                self.recording.ir.classes[self.id as usize].fq_name()
            )
        };
        if signature.0.len() != carried.0.len()
            || member_signature(implemented.0, implemented.1)? != carried
        {
            return Err(unboxed());
        }
        let mut code = Code::default();
        for (local, (wide, narrow)) in signature.0.iter().zip(&carried.0).enumerate() {
            code.local_get(local as u32);
            if wide == narrow {
                continue;
            }
            match (wide, narrow) {
                (ValType::Ref { .. }, ValType::Ref { nullable, heap }) => {
                    code.ref_cast(*nullable, *heap);
                }
                _ => return Err(unboxed()),
            }
        }
        // A result is widened by subtyping alone: every reference this form carries is an `eq`.
        match (&signature.1[..], &carried.1[..]) {
            ([wide], [narrow]) if wide == narrow => {}
            ([wide], [ValType::Ref { .. }]) if *wide == REFERENCE => {}
            ([], []) => {}
            _ => return Err(unboxed()),
        }
        code.call(function);
        let module = &mut *self.program.module;
        let ty = module.func_type(signature.0.clone(), signature.1.clone());
        let thunk = module.declare();
        module.define(
            thunk,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        Ok((thunk, signature))
    }

    /// Record, by identity, what the class implements each member of a dependency's interface
    /// with: its own or inherited override, read off its own table so the most derived one
    /// stands; a default of this module's interface, at that interface member's number
    /// (`numbered`); or the default the dependency publishes.
    fn dependency_implementations(
        &mut self,
        numbered: &HashMap<u32, Placed>,
    ) -> Result<(), Unsupported> {
        let ir = self.recording.ir;
        let mut current = Some(self.id);
        while let Some(owner) = current {
            let name = ir.classes[owner as usize].fq_name_id();
            for edge in ir.function_overrides.get(&name).into_iter().flatten() {
                let ResolvedFunctionOverrideTarget::External(target) = edge.overridden else {
                    continue;
                };
                if !edge.overridden_is_interface {
                    continue;
                }
                let Some(member) =
                    self.dependency_member(target, |identity| Some(member_identity(identity)))
                else {
                    continue;
                };
                let implementation = match edge.implementation {
                    ResolvedFunctionOverrideTarget::Module(callable) => Some(callable),
                    ResolvedFunctionOverrideTarget::External(_) => None,
                };
                let interface_number = implementation
                    .filter(|_| edge.implementation_function.is_none())
                    .and_then(|callable| {
                        self.recording.numbers.number(PlanKey::Callable(callable))
                    });
                let placed = match interface_number {
                    Some(number) => numbered.get(&number).cloned().ok_or_else(|| {
                        self.declined("an interface default implementing a dependency's member")
                    })?,
                    None => {
                        let function = edge
                            .implementation_function
                            .or_else(|| {
                                implementation.and_then(|callable| {
                                    ir.checked_callable_functions.get(&callable).copied()
                                })
                            })
                            .ok_or_else(|| {
                                self.declined("a dependency's interface member with no body here")
                            })?;
                        let key = function_key(ir, owner, function);
                        let slot = self.classes.tables.slot(owner, &key).ok_or_else(|| {
                            self.declined("a dependency's interface member with no slot")
                        })?;
                        self.slot_function(slot)?
                    }
                };
                let placed = self.thunk(
                    (&edge.declared_parameters, edge.declared_result),
                    (&edge.implementation_parameters, edge.implementation_result),
                    placed,
                )?;
                self.implement(member, placed)?;
            }
            self.dependency_property_implementations(owner, numbered)?;
            for default in ir.inherited_defaults.get(&name).into_iter().flatten() {
                let InheritedDefaultBody::DependencyInterfaceMethod(declaration) = default.body
                else {
                    continue;
                };
                let setter = match default.name {
                    InheritedMemberName::Function(_) => None,
                    InheritedMemberName::PropertyGetter(_) => Some(false),
                    InheritedMemberName::PropertySetter(_) => Some(true),
                };
                let Some(member) = self.dependency_member(declaration, |identity| match setter {
                    None => Some(member_identity(identity)),
                    Some(setter) => property_member_identity(identity, setter),
                }) else {
                    continue;
                };
                if self
                    .program
                    .objects
                    .interfaces
                    .implements(self.record, member)
                {
                    continue;
                }
                let function = self
                    .recording
                    .callables
                    .callable(declaration)
                    .and_then(|fact| fact.dependency_callable.as_ref())
                    .and_then(|identity| self.program.callables.dependency(identity))
                    .ok_or_else(|| {
                        self.declined("a default a dependency's interface declares with no body")
                    })?;
                self.program
                    .objects
                    .interfaces
                    .implement(self.record, member, function)?;
            }
            current = self.classes.tables.table(owner).superclass;
        }
        Ok(())
    }

    /// The getter and setter `owner` implements each property of a dependency's interface with.
    fn dependency_property_implementations(
        &mut self,
        owner: ClassId,
        numbered: &HashMap<u32, Placed>,
    ) -> Result<(), Unsupported> {
        let ir = self.recording.ir;
        let name = ir.classes[owner as usize].fq_name_id();
        for edge in ir.property_overrides.get(&name).into_iter().flatten() {
            let ResolvedPropertyOverrideTarget::External(target) = edge.overridden else {
                continue;
            };
            if !edge.overridden_is_interface || edge.declared_receiver.is_some() {
                continue;
            }
            let roles = [false, true]
                .into_iter()
                .filter(|setter| !setter || edge.overridden_setter_value.is_some());
            for setter in roles {
                let Some(member) = self.dependency_member(target, |identity| {
                    property_member_identity(identity, setter)
                }) else {
                    continue;
                };
                let interface_number = match edge.implementation {
                    ResolvedPropertyOverrideTarget::Module(property) => {
                        self.recording.numbers.number(if setter {
                            PlanKey::Setter(property)
                        } else {
                            PlanKey::Getter(property)
                        })
                    }
                    ResolvedPropertyOverrideTarget::External(_) => None,
                };
                let placed = match interface_number {
                    Some(number) => numbered.get(&number).cloned(),
                    None => {
                        let key = if setter {
                            SlotKey::Setter(edge.overridden)
                        } else {
                            SlotKey::Getter(edge.overridden)
                        };
                        match self.classes.tables.slot(owner, &key) {
                            Some(slot) => Some(self.slot_function(slot)?),
                            None => None,
                        }
                    }
                }
                .ok_or_else(|| self.declined("a dependency's interface property with no slot"))?;
                let (declared_params, declared_result) = accessor_types(edge.declared_type, setter);
                let (implemented_params, implemented_result) =
                    accessor_types(edge.implementation_type, setter);
                let placed = self.thunk(
                    (&declared_params, declared_result),
                    (&implemented_params, implemented_result),
                    placed,
                )?;
                self.implement(member, placed)?;
            }
        }
        Ok(())
    }

    /// The member of a source module's interface the dependency declaration `target` is, by the
    /// identity `identity` computes from its published one; `None` for a library's member.
    fn dependency_member(
        &self,
        target: crate::fir::ExternalCallableId,
        identity: impl FnOnce(&crate::backend::KlibDependencyCallable) -> Option<Vec<u8>>,
    ) -> Option<MemberRef> {
        let published = self
            .recording
            .callables
            .callable(target)?
            .dependency_callable
            .as_ref()?;
        self.program
            .objects
            .interfaces
            .identified(&identity(published)?)
    }

    /// Record `placed` for `member` unless a more derived class of the chain already did.
    fn implement(
        &mut self,
        member: MemberRef,
        (function, signature): Placed,
    ) -> Result<(), Unsupported> {
        let interfaces = &mut self.program.objects.interfaces;
        if interfaces.implements(self.record, member) {
            return Ok(());
        }
        let ty = self
            .program
            .module
            .func_type(signature.0.clone(), signature.1.clone());
        if ty != interfaces.ty(member) {
            return Err(
                self.declined("a dependency's interface member implemented with other carriers")
            );
        }
        interfaces.implement(self.record, member, function)
    }

    /// Record every interface the class's instances are instances of.
    fn instances(&mut self) {
        let ir = self.recording.ir;
        let class = &ir.classes[self.id as usize];
        let module = self.classes.tables.interface_tables.implemented[self.id as usize]
            .iter()
            .copied();
        let hierarchy = ir
            .classifier_hierarchies
            .get(&class.fq_name_id())
            .into_iter()
            .flatten()
            .map(|applied| applied.classifier);
        for interface in module.chain(hierarchy).chain(class.interfaces.iter()) {
            self.program
                .objects
                .interfaces
                .instance_of(self.record, interface);
        }
    }

    fn declined(&self, construct: &str) -> Unsupported {
        format!(
            "{construct} (`{}`)",
            self.recording.ir.classes[self.id as usize].fq_name()
        )
    }
}
