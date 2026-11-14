//! One file's side of interface dispatch (`super::super::interface_dispatch`): what the file's
//! classes implement, and the identities its interfaces publish.
//!
//! A class with instances records, for each member number of its module's interfaces, the function
//! at that number of its interface table (`class_tables::planned_itable`); for each member of a
//! dependency's interface, by the member's published identity
//! (`krusty::backend::interface_identity`), its override or the default it inherits; and every
//! interface it is an instance of. A file declaring an interface publishes each member's identity
//! beside its number, which is what a dependent module's classes name their implementations by.
//!
//! An implementation carrying a reference more narrowly than its member does stands behind a thunk
//! with the member's signature that casts the operand and calls it; one whose member carries a
//! value the implementation does not (a primitive behind a type parameter) declines, since that
//! thunk does not box or unbox yet (see `super::super::boxes`).

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::module_interfaces::ModuleInterfaces;
use krusty::backend::class_tables::module_slots::PlanKey;
use krusty::backend::class_tables::planned_itable::InterfaceEntry;
use krusty::backend::class_tables::{function_key, Slot, SlotKey};
use krusty::backend::interface_identity::{member_identity, property_member_identity};
use krusty::backend::{CheckedBackendCallables, DependencyExportSignatures};
use krusty::fir::{
    InheritedDefaultBody, InheritedMemberName, ResolvedFunctionOverrideTarget,
    ResolvedPropertyOverrideTarget,
};
use krusty::ir::{ClassId, IrFile, IrLocalPropertyLayout};
use krusty::types::Ty;

use super::super::boxes::BoxForm;
use super::super::encode::{Code, Function, Module, ValType};
use super::super::interface_dispatch::{ClassRef, MemberRef};
use super::super::objects::REFERENCE;
use super::super::runtime::Runtime;
use super::callable_registry::CallableRegistry;
use super::classes::{accessor_function, field_carrier, Accessor, FileClasses};
use super::laid_out_classes::DependencyImplementation;
use super::lower::library_members::{
    library_member_identity, library_member_owner, library_member_signature, MemberRole,
};
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

/// Where a recording writes: the module, the runtime and the callables of the program.
pub(super) struct Destination<'a> {
    pub(super) module: &'a mut Module,
    pub(super) runtime: &'a mut Runtime,
    pub(super) callables: &'a mut CallableRegistry,
}

use super::laid_out_classes::Placed;

/// What a class implements a dependency's member with, before it is carried as the member is.
struct Carried {
    placed: Placed,
    /// The class slot `placed` was read from, which a subclass reads its own implementation from.
    slot: Option<u32>,
    /// The member's parameters and result.
    declared: Signature,
    /// The implementation's parameters and result.
    implemented: Signature,
}

/// A member of a dependency's interface a class implements.
#[derive(Clone, Copy)]
struct DependencyMember {
    member: MemberRef,
    /// Whether it is a library classifier's member rather than a source module's.
    library: bool,
}

/// Publish the identities of the file's interface members and record what its classes implement.
///
/// A class with no instances of its own records nothing in the program, but what it would
/// implement is kept for a later file extending it; where it needs what this form does not
/// provide, that file declines with the reason instead.
pub(super) fn record(
    recording: &Recording<'_>,
    classes: &mut FileClasses,
    program: &mut Destination<'_>,
) -> Result<(), Unsupported> {
    publish_identities(recording, program)?;
    for &id in &classes.tables.order.clone() {
        if recording.ir.classes[id as usize].is_interface {
            continue;
        }
        let record = classes.forms[id as usize].record;
        if record.is_none() && classes.refusals[id as usize].is_some() {
            continue;
        }
        let mut file = FileRecorder {
            recording,
            classes: &mut *classes,
            program: &mut *program,
            id,
            record,
            implemented: Vec::new(),
        };
        let recorded = file.module_implementations().and_then(|numbered| {
            file.dependency_implementations(&numbered)?;
            Ok(numbered)
        });
        let numbered = match recorded {
            Ok(numbered) => numbered,
            Err(reason) if record.is_none() => {
                classes.refusals[id as usize] = Some(reason);
                continue;
            }
            Err(reason) => return Err(reason),
        };
        file.instances();
        let FileRecorder { implemented, .. } = file;
        let members = &classes.tables.interface_tables.members[id as usize];
        classes.interface_bodies[id as usize] = numbered
            .into_iter()
            .filter(|(number, _)| members.get(number) == Some(&None))
            .collect();
        classes.dependency_implementations[id as usize] = implemented;
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
            .runtime
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
            program
                .runtime
                .objects
                .interfaces
                .publish(identity, member)?;
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
    program.runtime.objects.interfaces.numbered(
        program.module,
        recording.module_name,
        number,
        signature,
    )
}

/// The recording of one class.
struct FileRecorder<'r, 'a, 'd> {
    recording: &'r Recording<'a>,
    classes: &'r mut FileClasses,
    program: &'r mut Destination<'d>,
    id: ClassId,
    /// The class as interface dispatch knows it; `None` for a class with no instances of its own,
    /// whose implementations are only kept for a later file extending it.
    record: Option<ClassRef>,
    /// How the class implements each member of a dependency's interface, most derived first.
    implemented: Vec<(MemberRef, DependencyImplementation)>,
}

impl FileRecorder<'_, '_, '_> {
    /// Record the function at every number of the class's interface table; the placed functions
    /// by number, for a dependency member a default of this module stands for.
    fn module_implementations(&mut self) -> Result<HashMap<u32, Placed>, Unsupported> {
        let entries = self.classes.tables.interface_tables.tables[self.id as usize].clone();
        let mut placed = HashMap::default();
        for (number, entry) in entries.iter().enumerate() {
            let Some((function, signature)) = self.entry(number as u32, entry)? else {
                continue;
            };
            let member = numbered(
                self.recording,
                self.program,
                number as u32,
                signature.clone(),
            )?;
            if let Some(record) = self.record {
                self.program
                    .runtime
                    .objects
                    .interfaces
                    .implement(record, member, function)?;
            }
            if !self.implemented.iter().any(|(known, _)| *known == member) {
                let how = match self.classes.tables.interface_tables.members[self.id as usize]
                    .get(&(number as u32))
                    .copied()
                    .flatten()
                {
                    Some(slot) => DependencyImplementation::Slot {
                        slot,
                        declared: signature.clone(),
                        implemented: signature.clone(),
                    },
                    None => DependencyImplementation::Function(function),
                };
                self.implemented.push((member, how));
            }
            placed.insert(number as u32, (function, signature));
        }
        Ok(placed)
    }

    /// The function the interface table entry at `number` holds, or `None` for an abstract one.
    fn entry(
        &mut self,
        number: u32,
        entry: &InterfaceEntry,
    ) -> Result<Option<Placed>, Unsupported> {
        let ir = self.recording.ir;
        match entry {
            InterfaceEntry::Slot(Slot::Abstract) => Ok(None),
            InterfaceEntry::Inherited => self
                .classes
                .laid_out_superclass(self.id)
                .and_then(|superclass| superclass.interface_bodies.get(&number))
                .cloned()
                .map(Some)
                .ok_or_else(|| {
                    self.declined(
                        "an interface member a superclass of another file bridges with nothing \
                         recorded",
                    )
                }),
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
            Slot::Inherited { slot } => {
                let placed = self
                    .classes
                    .laid_out_superclass(self.id)
                    .and_then(|superclass| {
                        let function = superclass.entries.get(*slot as usize).copied().flatten()?;
                        let ty = superclass
                            .slot_types
                            .get(*slot as usize)
                            .copied()
                            .flatten()?;
                        Some((function, self.program.module.func_signature(ty)?))
                    });
                placed.ok_or_else(|| {
                    self.declined("an inherited slot with no function of another file")
                })
            }
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
                let value = field_carrier(self.program.module, ir, *class, *field)?;
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
    /// both are carried alike, else a thunk adapting each operand the member carries as a
    /// reference: a narrower reference is cast, and a scalar the implementation carries itself
    /// (`next(): Int` implementing `Iterator<T>.next(): T`) is read out of its box, or boxed as the
    /// result.
    fn thunk(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        (function, carried): Placed,
    ) -> Result<Placed, Unsupported> {
        let signature = member_signature(declared.0, declared.1)?;
        let implemented = member_signature(implemented.0, implemented.1)?;
        self.thunk_signatures(signature, implemented, (function, carried))
    }

    /// Bridge already lowered signatures without retaining source-analysis type handles.
    fn thunk_signatures(
        &mut self,
        signature: Signature,
        implemented: Signature,
        (function, carried): Placed,
    ) -> Result<Placed, Unsupported> {
        if signature == carried {
            return Ok((function, carried));
        }
        let class = self.recording.ir.classes[self.id as usize].fq_name();
        let unadapted =
            || format!("an interface member implemented with a value it cannot adapt (`{class}`)");
        if signature.0.len() != carried.0.len()
            || member_signature(implemented.0, implemented.1)? != carried
        {
            return Err(unadapted());
        }
        let mut code = Code::default();
        let mut locals = Vec::new();
        for (local, (wide, narrow)) in signature.0.iter().zip(&carried.0).enumerate() {
            code.local_get(local as u32);
            if wide == narrow {
                continue;
            }
            match (wide, narrow) {
                (ValType::Ref { .. }, ValType::Ref { nullable, heap }) => {
                    code.ref_cast(*nullable, *heap);
                }
                (ValType::Ref { .. }, scalar) => {
                    // The receiver is a reference on both sides, so a scalar is a parameter.
                    let parameter = local
                        .checked_sub(1)
                        .and_then(|index| implemented.0.get(index))
                        .copied()
                        .ok_or_else(unadapted)?;
                    let form = self.box_of(parameter, *scalar)?.ok_or_else(unadapted)?;
                    form.emit_unbox(&mut code);
                }
                _ => return Err(unadapted()),
            }
        }
        code.call(function);
        // A reference result is widened by subtyping alone: every reference this form carries is
        // an `eq`.
        match (&signature.1[..], &carried.1[..]) {
            ([wide], [narrow]) if wide == narrow => {}
            ([wide], [ValType::Ref { .. }]) if *wide == REFERENCE => {}
            ([wide], [scalar]) if *wide == REFERENCE => {
                let form = self.box_of(implemented.1, *scalar)?.ok_or_else(unadapted)?;
                let scratch = form.needs_scratch().then(|| {
                    locals.push(*scalar);
                    signature.0.len() as u32
                });
                form.emit_box(&mut code, scratch);
            }
            ([], []) => {}
            _ => return Err(unadapted()),
        }
        let module = &mut *self.program.module;
        let ty = module.func_type(signature.0.clone(), signature.1.clone());
        let thunk = module.declare();
        module.define(thunk, Function { ty, locals, code });
        Ok((thunk, signature))
    }

    /// The box of `ty`'s values, which are carried as `value`.
    fn box_of(&mut self, ty: Ty, value: ValType) -> Result<Option<BoxForm>, Unsupported> {
        self.program
            .runtime
            .box_form(self.program.module, ty.non_null(), value)
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
                let Some(member) = self.dependency_member(target, MemberRole::Function) else {
                    continue;
                };
                let implemented = match self.function_edge_implementation(owner, edge, numbered) {
                    Ok(carried) => self.implement_carried(member.member, carried),
                    Err(reason) => Err(reason),
                };
                self.settle(member, implemented)?;
            }
            self.dependency_property_implementations(owner, numbered)?;
            for default in ir.inherited_defaults.get(&name).into_iter().flatten() {
                let InheritedDefaultBody::DependencyInterfaceMethod(declaration) = default.body
                else {
                    continue;
                };
                let role = match default.name {
                    InheritedMemberName::Function(_) => MemberRole::Function,
                    InheritedMemberName::PropertyGetter(_) => MemberRole::Getter,
                    InheritedMemberName::PropertySetter(_) => MemberRole::Setter,
                };
                let Some(member) = self.dependency_member(declaration, role) else {
                    continue;
                };
                if self.implements(member.member) {
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
                    });
                let implemented = match function {
                    Ok(function) => self.record_implementation(
                        member.member,
                        function,
                        DependencyImplementation::Function(function),
                    ),
                    Err(reason) => Err(reason),
                };
                self.settle(member, implemented)?;
            }
            current = self.classes.tables.table(owner).superclass;
        }
        // What the superclass an earlier file laid out implements, where nothing here did: the
        // member at its slot is this class's own at that number, overridden or inherited.
        let inherited = self
            .classes
            .laid_out_superclass(self.id)
            .map(|superclass| superclass.dependency_implementations.clone())
            .unwrap_or_default();
        for (member, how) in inherited {
            if self.implements(member) {
                continue;
            }
            match how {
                DependencyImplementation::Slot {
                    slot,
                    declared,
                    implemented,
                } => {
                    let placed = self.slot_function(slot)?;
                    self.implement_carried(
                        member,
                        Carried {
                            placed,
                            slot: Some(slot),
                            declared,
                            implemented,
                        },
                    )?;
                }
                DependencyImplementation::Function(function) => {
                    self.record_implementation(member, function, how)?;
                }
            }
        }
        Ok(())
    }

    /// The function `owner` implements the dependency member `edge` overrides with: its own or
    /// inherited override, read off its own table so the most derived one stands, or a default of
    /// this module's interface, at that interface member's number (`numbered`).
    fn function_edge_implementation(
        &mut self,
        owner: ClassId,
        edge: &krusty::ir::IrFunctionOverride,
        numbered: &HashMap<u32, Placed>,
    ) -> Result<Carried, Unsupported> {
        let ir = self.recording.ir;
        let implementation = match edge.implementation {
            ResolvedFunctionOverrideTarget::Module(callable) => Some(callable),
            ResolvedFunctionOverrideTarget::External(_) => None,
        };
        let interface_number = implementation
            .filter(|_| edge.implementation_function.is_none())
            .and_then(|callable| self.recording.numbers.number(PlanKey::Callable(callable)));
        let (placed, slot) = match interface_number {
            Some(number) => (
                numbered.get(&number).cloned().ok_or_else(|| {
                    self.declined("an interface default implementing a dependency's member")
                })?,
                None,
            ),
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
                let slot =
                    self.classes.tables.slot(owner, &key).ok_or_else(|| {
                        self.declined("a dependency's interface member with no slot")
                    })?;
                (self.slot_function(slot)?, Some(slot))
            }
        };
        Ok(Carried {
            placed,
            slot,
            declared: member_signature(&edge.declared_parameters, edge.declared_result)?,
            implemented: member_signature(
                &edge.implementation_parameters,
                edge.implementation_result,
            )?,
        })
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
                let role = if setter {
                    MemberRole::Setter
                } else {
                    MemberRole::Getter
                };
                let Some(member) = self.dependency_member(target, role) else {
                    continue;
                };
                let implemented =
                    match self.property_edge_implementation(owner, edge, setter, numbered) {
                        Ok(carried) => self.implement_carried(member.member, carried),
                        Err(reason) => Err(reason),
                    };
                self.settle(member, implemented)?;
            }
        }
        Ok(())
    }

    /// The getter, or with `setter` the setter, `owner` implements the dependency property `edge`
    /// overrides with.
    fn property_edge_implementation(
        &mut self,
        owner: ClassId,
        edge: &krusty::ir::IrPropertyOverride,
        setter: bool,
        numbered: &HashMap<u32, Placed>,
    ) -> Result<Carried, Unsupported> {
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
        let (placed, slot) = match interface_number {
            Some(number) => (numbered.get(&number).cloned(), None),
            None => {
                let key = if setter {
                    SlotKey::Setter(edge.overridden)
                } else {
                    SlotKey::Getter(edge.overridden)
                };
                match self.classes.tables.slot(owner, &key) {
                    Some(slot) => (Some(self.slot_function(slot)?), Some(slot)),
                    None => (None, None),
                }
            }
        };
        Ok(Carried {
            placed: placed
                .ok_or_else(|| self.declined("a dependency's interface property with no slot"))?,
            slot,
            declared: {
                let (params, result) = accessor_types(edge.declared_type, setter);
                member_signature(&params, result)?
            },
            implemented: {
                let (params, result) = accessor_types(edge.implementation_type, setter);
                member_signature(&params, result)?
            },
        })
    }

    /// The member the dependency declaration `target` is in `role`: a member of a source module's
    /// interface, by the identity its module published, or else a library classifier's member,
    /// which a call through it may name too. `None` for a declaration no member stands for.
    fn dependency_member(
        &mut self,
        target: krusty::fir::ExternalCallableId,
        role: MemberRole,
    ) -> Option<DependencyMember> {
        let fact = self.recording.callables.callable(target)?;
        let identity = library_member_identity(fact, role)?;
        let interfaces = &mut self.program.runtime.objects.interfaces;
        if let Some(member) = interfaces.identified(&identity) {
            return Some(DependencyMember {
                member,
                library: false,
            });
        }
        let signature = library_member_signature(fact, role).ok()?;
        let owner = library_member_owner(fact)?;
        let member = interfaces
            .library(self.program.module, owner, &identity, signature)
            .ok()?;
        Some(DependencyMember {
            member,
            library: true,
        })
    }

    /// Settle the outcome of recording what the class implements `member` with. A library
    /// member's implementation this generator cannot write declines only a call through the
    /// member, so it is recorded on the member instead.
    fn settle(
        &mut self,
        member: DependencyMember,
        implemented: Result<(), Unsupported>,
    ) -> Result<(), Unsupported> {
        match implemented {
            Err(reason) if member.library => {
                self.program
                    .runtime
                    .objects
                    .interfaces
                    .unimplementable(member.member, reason);
                Ok(())
            }
            implemented => implemented,
        }
    }

    /// Whether the class already implements `member`: a more derived class of the chain recorded
    /// it, or the module's own numbering did.
    fn implements(&self, member: MemberRef) -> bool {
        self.implemented.iter().any(|(known, _)| *known == member)
            || self.record.is_some_and(|record| {
                self.program
                    .runtime
                    .objects
                    .interfaces
                    .implements(record, member)
            })
    }

    /// Record what `carried` implements `member` with, unless a more derived class of the chain
    /// already did: through a thunk where the two are carried differently.
    fn implement_carried(
        &mut self,
        member: MemberRef,
        Carried {
            placed,
            slot,
            declared,
            implemented,
        }: Carried,
    ) -> Result<(), Unsupported> {
        if self.implements(member) {
            return Ok(());
        }
        let (function, signature) =
            self.thunk_signatures(declared.clone(), implemented.clone(), placed)?;
        let ty = self
            .program
            .module
            .func_type(signature.0.clone(), signature.1.clone());
        if ty != self.program.runtime.objects.interfaces.ty(member) {
            return Err(
                self.declined("a dependency's interface member implemented with other carriers")
            );
        }
        let how = match slot {
            Some(slot) => DependencyImplementation::Slot {
                slot,
                declared,
                implemented,
            },
            None => DependencyImplementation::Function(function),
        };
        self.record_implementation(member, function, how)
    }

    /// Record `function` for `member`, kept as `how` for a later file extending the class.
    fn record_implementation(
        &mut self,
        member: MemberRef,
        function: u32,
        how: DependencyImplementation,
    ) -> Result<(), Unsupported> {
        if self.implements(member) {
            return Ok(());
        }
        self.implemented.push((member, how));
        match self.record {
            Some(record) => self
                .program
                .runtime
                .objects
                .interfaces
                .implement(record, member, function),
            None => Ok(()),
        }
    }

    /// Record every interface the class's instances are instances of.
    fn instances(&mut self) {
        let Some(record) = self.record else {
            return;
        };
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
                .runtime
                .objects
                .interfaces
                .instance_of(record, interface);
        }
    }

    fn declined(&self, construct: &str) -> Unsupported {
        format!(
            "{construct} (`{}`)",
            self.recording.ir.classes[self.id as usize].fq_name()
        )
    }
}
