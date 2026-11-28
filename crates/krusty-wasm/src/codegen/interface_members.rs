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
//! with the member's signature. A collection bridge (`contains`/`remove` answering `Boolean`,
//! `indexOf`/`lastIndexOf` answering `Int`, `Map.get`/`remove` answering a nullable reference),
//! recognized by the overridden declaration's collection role, tests that narrower reference and
//! answers the rejection (`false`, `-1` or `null`) when the argument is some other type, as the
//! erased interface method does; any other narrower reference is cast. A member that carries a value the
//! implementation does not (a primitive behind a type parameter) is unboxed or boxed
//! (see `super::super::boxes`).

use krusty::fx_hash::HashMap;

use krusty::backend::class_tables::module_interfaces::ModuleInterfaces;
use krusty::backend::class_tables::module_slots::{carried, PlanKey};
use krusty::backend::class_tables::planned_itable::InterfaceEntry;
use krusty::backend::class_tables::{function_key, Slot, SlotKey};
use krusty::backend::interface_identity::{member_identity, property_member_identity};
use krusty::backend::{BackendCallableFact, CheckedBackendCallables, DependencyExportSignatures};
use krusty::fir::{
    InheritedDefaultBody, InheritedMemberName, ResolvedFunctionOverrideTarget,
    ResolvedPropertyOverrideTarget,
};
use krusty::ir::{ClassId, IrFile, IrLocalPropertyLayout};
use krusty::types::{SemanticCallRole, Ty};

use super::super::boxes::BoxForm;
use super::super::encode::{BlockType, Code, Function, Module, ValType};
use super::super::interface_dispatch::{ClassRef, InterfaceDispatch, MemberRef};
use super::super::objects::REFERENCE;
use super::super::runtime::Runtime;
use super::callable_registry::CallableRegistry;
use super::classes::{accessor_function, field_carrier, Accessor, FileClasses};
use super::laid_out_classes::{
    BridgeRejection, DependencyImplementation, ResultAdapter, SignatureAdapters,
};
use super::lower::library_members::{
    is_abstract_class_member, library_member_identity, library_member_owner,
    library_member_signature, MemberRole,
};
use super::lower::{carrier, parameter_carrier_of, Unsupported};

/// A function's wasm parameters (the receiver first) and results.
pub(super) type Signature = (Vec<ValType>, Vec<ValType>);

/// The signature of a member taking `params` after its receiver and answering `result`.
pub(super) fn member_signature(params: &[Ty], result: Ty) -> Result<Signature, Unsupported> {
    let mut carriers = vec![REFERENCE];
    for param in params {
        carriers.push(parameter_carrier_of(*param)?);
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

/// The parameters and result of a property accessor of a value of type `ty` on a member extension
/// whose receiver is `receiver`: the receiver comes first, then (for a setter) the value.
fn extension_accessor_types(receiver: Option<Ty>, ty: Ty, setter: bool) -> (Vec<Ty>, Ty) {
    let (value, result) = accessor_types(ty, setter);
    (receiver.into_iter().chain(value).collect(), result)
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
    /// The frozen classifier facts, whose published supertypes name the interfaces a class
    /// inherits through a library superclass.
    pub(super) classifiers: &'a dyn krusty::backend::BackendClassifierSource,
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
    /// Exact box operations selected while the implementation's logical types were live.
    adapters: SignatureAdapters,
}

/// A member of a dependency's interface a class implements.
#[derive(Clone, Copy)]
pub(super) struct DependencyMember {
    pub(super) member: MemberRef,
    /// Whether it is a library classifier's member rather than a source module's.
    pub(super) library: bool,
}

/// The member the dependency declaration `fact` is in `role`: a member of a source module's
/// interface, by the identity its module published, or else a library classifier's member,
/// which a call through it may name too. `None` for a declaration no member stands for.
pub(super) fn dependency_member(
    interfaces: &mut InterfaceDispatch,
    module: &mut Module,
    fact: &BackendCallableFact,
    role: MemberRole,
) -> Result<Option<DependencyMember>, Unsupported> {
    let Some(identity) = library_member_identity(fact, role) else {
        return Ok(None);
    };
    let published = interfaces.identified(&identity).is_some();
    let signature = library_member_signature(fact, role)?;
    let owner = library_member_owner(fact)
        .ok_or("an interface member without its checked declaring classifier")?;
    let member = interfaces.library(module, owner, &identity, signature)?;
    Ok(Some(DependencyMember {
        member,
        library: !published,
    }))
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
        let mut arities = classes
            .laid_out_superclass(id)
            .map(|parent| parent.function_arities.clone())
            .unwrap_or_default();
        let mut suspend_arities = classes
            .laid_out_superclass(id)
            .map(|parent| parent.suspend_function_arities.clone())
            .unwrap_or_default();
        let mut current = Some(id);
        while let Some(owner) = current {
            let name = recording.ir.classes[owner as usize].fq_name_id();
            for edge in recording
                .ir
                .function_overrides
                .get(&name)
                .into_iter()
                .flatten()
            {
                if edge.overridden_is_interface
                    && edge.overridden_semantic_role == Some(SemanticCallRole::KotlinFunctionInvoke)
                    && matches!(edge.overridden, ResolvedFunctionOverrideTarget::External(_))
                {
                    let arity = edge.declared_parameters.len() + usize::from(edge.suspend);
                    if !arities.contains(&arity) {
                        arities.push(arity);
                    }
                    if edge.suspend && !suspend_arities.contains(&arity) {
                        suspend_arities.push(arity);
                    }
                }
            }
            current = classes.tables.table(owner).superclass;
        }
        if let Some(record) = record {
            for &arity in &arities {
                program.runtime.objects.interfaces.function_instance_of(
                    record,
                    arity,
                    suspend_arities.contains(&arity),
                );
            }
        }
        classes.function_arities[id as usize] = arities;
        classes.suspend_function_arities[id as usize] = suspend_arities;
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
        program.runtime.objects.interfaces.publish(
            member_identity(identity),
            declaration
                .dispatch_receiver
                .ok_or("an interface publication without its checked declaring owner")?,
            member,
        )?;
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
            program.runtime.objects.interfaces.publish(
                identity,
                match layout {
                    IrLocalPropertyLayout::Member { owner, .. }
                    | IrLocalPropertyLayout::MemberExtension { owner, .. } => *owner,
                    _ => {
                        return Err(
                            "an interface property publication without its declaring owner".into(),
                        )
                    }
                },
                member,
            )?;
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
                        adapters: SignatureAdapters::none(signature.0.len()),
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

    /// Whether class slot `slot` of this class is abstract: no body stands there.
    fn slot_is_abstract(&self, slot: u32) -> bool {
        matches!(
            self.classes.tables.table(self.id).vtable[slot as usize],
            Slot::Abstract
        )
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
    /// reference. A collection bridge answers `false` or `-1` when its narrower reference
    /// argument is a different type; any other narrower reference is cast. A scalar the
    /// implementation carries itself (`next(): Int` implementing `Iterator<T>.next(): T`) is
    /// read out of its box, or boxed as the result.
    fn thunk(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        (function, carried): Placed,
    ) -> Result<Placed, Unsupported> {
        // A member this file declares is never a collection's special bridge.
        let signature = member_signature(declared.0, declared.1)?;
        let implementation_signature = member_signature(implemented.0, implemented.1)?;
        let placed = (function, carried);
        let adapters = self.adapters_for_placed(
            declared,
            implemented,
            &signature,
            &implementation_signature,
            &placed,
            false,
        )?;
        self.emit_thunk(signature, implementation_signature, adapters, placed)
    }

    /// Select exact boxes while the selected implementation's logical types are available.
    /// Only their program-owned module identities are retained for later physical emission.
    fn thunk_adapters(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        signature: &Signature,
        implemented_signature: &Signature,
        special_bridge: bool,
    ) -> Result<SignatureAdapters, Unsupported> {
        if declared.0.len() != implemented.0.len()
            || signature.0.len() != implemented_signature.0.len()
        {
            return Err(self.declined("an interface member with incompatible adaptation arity"));
        }
        let mut adapters = SignatureAdapters::none(signature.0.len());
        adapters.rejection = special_bridge
            .then(|| collection_bridge_rejection(declared, signature))
            .flatten();
        if adapters.rejection.is_some() {
            for (local, ty) in implemented.0.iter().enumerate() {
                adapters.rejects_null[local + 1] = !ty.upper_bound_admits_null();
            }
        }
        let class = self.recording.ir.classes[self.id as usize].fq_name();
        let unadapted =
            || format!("an interface member implemented with a value it cannot adapt (`{class}`)");
        for local in 1..signature.0.len() {
            let (wide, narrow) = (&signature.0[local], &implemented_signature.0[local]);
            match (wide, narrow) {
                (ValType::Ref { .. }, scalar) if !matches!(scalar, ValType::Ref { .. }) => {
                    let ty = *implemented.0.get(local - 1).ok_or_else(unadapted)?;
                    adapters.parameters[local] =
                        Some(self.box_of(ty, *scalar)?.ok_or_else(unadapted)?.adapter());
                }
                (ValType::Ref { .. }, ValType::Ref { .. }) => {}
                (left, right) if left == right => {}
                _ => return Err(unadapted()),
            }
        }
        match (&signature.1[..], &implemented_signature.1[..]) {
            ([ValType::Ref { .. }], [ValType::Ref { .. }]) => {}
            ([wide @ ValType::Ref { .. }], [scalar]) if *wide == REFERENCE => {
                adapters.result = Some(ResultAdapter::Box(
                    self.box_of(implemented.1, *scalar)?
                        .ok_or_else(unadapted)?
                        .adapter(),
                ));
            }
            ([wide], []) if *wide == REFERENCE && implemented.1 == Ty::Unit => {
                adapters.result = Some(ResultAdapter::Unit(
                    self.program.runtime.unit_object(self.program.module),
                ));
            }
            ([ValType::Ref { .. }], []) if implemented.1 == Ty::Nothing => adapters.diverges = true,
            (declared, implementation) if declared == implementation => {}
            _ => return Err(unadapted()),
        }
        Ok(adapters)
    }

    /// Preserve the direct-slot path; otherwise authenticate the physical function before
    /// selecting boxes from the current logical signature.
    fn adapters_for_placed(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        signature: &Signature,
        implemented_signature: &Signature,
        placed: &Placed,
        special_bridge: bool,
    ) -> Result<SignatureAdapters, Unsupported> {
        // A bridge whose override excludes `null` stands behind a thunk even where both carry the
        // same references: the thunk is what rejects the `null`.
        let guards_null = special_bridge
            && collection_bridge_rejection(declared, signature).is_some()
            && implemented.0.iter().any(|ty| !ty.upper_bound_admits_null());
        if signature == &placed.1 && !guards_null {
            return Ok(SignatureAdapters::none(signature.0.len()));
        }
        if implemented_signature != &placed.1 {
            return Err(self.declined("an interface member implemented with other carriers"));
        }
        self.thunk_adapters(
            declared,
            implemented,
            signature,
            implemented_signature,
            special_bridge,
        )
    }

    /// Emit one bridge from frozen physical signatures and exact program-owned box operations.
    fn emit_thunk(
        &mut self,
        signature: Signature,
        implemented: Signature,
        adapters: SignatureAdapters,
        (function, carried): Placed,
    ) -> Result<Placed, Unsupported> {
        if signature == carried && !adapters.guards_null() {
            return Ok((function, carried));
        }
        let class = self.recording.ir.classes[self.id as usize].fq_name();
        let unadapted =
            || format!("an interface member implemented with a value it cannot adapt (`{class}`)");
        if signature.0.len() != carried.0.len()
            || implemented != carried
            || adapters.parameters.len() != signature.0.len()
        {
            return Err(unadapted());
        }
        let mut code = Code::default();
        let mut locals = Vec::new();
        for (local, (wide, narrow)) in signature.0.iter().zip(&carried.0).enumerate() {
            code.local_get(local as u32);
            // A bridge answers its rejection for a `null` where the override's type excludes it,
            // whether or not the two carriers differ.
            let excludes_null = adapters.rejects_null.get(local).copied().unwrap_or(false);
            if let (Some(rejected), true) = (adapters.rejection, excludes_null) {
                code.ref_is_null().if_(BlockType::Empty);
                emit_rejection(&mut code, rejected, &signature.1).ok_or_else(unadapted)?;
                code.end();
                code.local_get(local as u32);
            }
            if wide == narrow {
                continue;
            }
            match (wide, narrow) {
                (ValType::Ref { .. }, ValType::Ref { nullable, heap }) => {
                    if let Some(rejected) = adapters.rejection {
                        // The erased interface method accepts any reference. An argument of
                        // another type is not an element, so the bridge answers its rejection
                        // instead of trapping on the cast.
                        code.ref_test(*nullable, *heap)
                            .if_(BlockType::Empty)
                            .else_();
                        emit_rejection(&mut code, rejected, &signature.1).ok_or_else(unadapted)?;
                        code.end();
                        code.local_get(local as u32);
                    }
                    code.ref_cast(*nullable, *heap);
                }
                (ValType::Ref { .. }, _) => {
                    let unbox = adapters.parameters[local].ok_or_else(unadapted)?;
                    if let Some(rejected) = adapters.rejection {
                        // A box of another type is not an element either: the bridge answers
                        // its rejection instead of trapping on the cast to this box.
                        unbox.emit_is_box(&mut code);
                        code.if_(BlockType::Empty).else_();
                        emit_rejection(&mut code, rejected, &signature.1).ok_or_else(unadapted)?;
                        code.end();
                        code.local_get(local as u32);
                    }
                    unbox.emit_unbox(&mut code);
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
            // The checker proved the value is of the declared type; only the carriers differ (a
            // `Nothing?` override of a `String?` member carries its null as a plain reference).
            ([ValType::Ref { nullable, heap }], [ValType::Ref { .. }]) => {
                code.ref_cast(*nullable, *heap);
            }
            ([wide], [scalar]) if *wide == REFERENCE => {
                let Some(ResultAdapter::Box(form)) = adapters.result else {
                    return Err(unadapted());
                };
                let scratch = form.needs_scratch().then(|| {
                    locals.push(*scalar);
                    signature.0.len() as u32
                });
                form.emit_box(&mut code, scratch);
            }
            ([ValType::Ref { .. }], []) if adapters.diverges => {
                code.unreachable();
            }
            ([wide], []) if *wide == REFERENCE => {
                let Some(ResultAdapter::Unit(unit)) = adapters.result else {
                    return Err(unadapted());
                };
                code.global_get(unit);
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
                // A call of a library class's abstract member reaches the overrides as one of a
                // library interface's does.
                if !edge.overridden_is_interface
                    && !self
                        .recording
                        .callables
                        .callable(target)
                        .is_some_and(is_abstract_class_member)
                {
                    continue;
                }
                if edge.overridden_semantic_role == Some(SemanticCallRole::KotlinFunctionInvoke) {
                    let arity = edge.declared_parameters.len() + usize::from(edge.suspend);
                    let protocol = self
                        .program
                        .runtime
                        .objects
                        .interfaces
                        .function_invoke(self.program.module, arity);
                    if !self.implements(protocol) {
                        let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
                        let parameters = vec![any; arity];
                        let implemented = match self.function_edge_implementation(
                            owner,
                            edge,
                            numbered,
                            (&parameters, any),
                        ) {
                            Ok(Some(carried)) => Some(self.implement_carried(protocol, carried)),
                            // An abstract class's abstract override implements nothing; each
                            // concrete subclass's table holds the body.
                            Ok(None) => None,
                            Err(reason) => Some(Err(reason)),
                        };
                        // An abstract/non-instantiated producer still publishes callable
                        // membership. A later concrete class must supply its invocation; a
                        // missing body here must not poison every call of this arity.
                        let recorded = self.record.is_some();
                        if let Some(implemented) =
                            implemented.filter(|implemented| recorded || implemented.is_ok())
                        {
                            self.settle(
                                DependencyMember {
                                    member: protocol,
                                    library: true,
                                },
                                implemented,
                            )?;
                        }
                    }
                }
                // An accessor overriding an interface property's implements that property's.
                let role = match self
                    .recording
                    .callables
                    .callable(target)
                    .and_then(|fact| fact.klib_body_accessor.as_ref())
                {
                    Some(accessor) if accessor.is_setter() => MemberRole::Setter,
                    Some(_) => MemberRole::Getter,
                    None => MemberRole::Function,
                };
                let Some(member) = self.dependency_member(target, role)? else {
                    continue;
                };
                if self.implements(member.member) {
                    continue;
                }
                // A default an interface publishes, which the class does not override, is its own
                // implementation: the declared or exported body, not a slot of the class. That is
                // the overridden member's own default, or an inherited interface's default
                // overriding it, such as `CoroutineContext.Element.fold` for
                // `CoroutineContext.fold`.
                let interface_default = match edge.implementation {
                    ResolvedFunctionOverrideTarget::External(implementation) => (implementation
                        == target
                        || self
                            .recording
                            .callables
                            .callable(implementation)
                            .is_some_and(|fact| fact.owner_is_interface))
                    .then_some(implementation),
                    ResolvedFunctionOverrideTarget::Module(_) => None,
                };
                let implemented = if let Some(default) = interface_default.filter(|_| {
                    edge.implementation_function.is_some_and(|function| {
                        ir.functions[function as usize].dispatch_receiver
                            != Some(ir.classes[owner as usize].fq_name_id())
                    })
                }) {
                    self.dependency_default_implementation(
                        member,
                        default,
                        edge.implementation_function,
                    )
                } else if let Some(default) = self.inherited_interface_default(edge) {
                    self.dependency_default_implementation(member, default, None)
                } else {
                    match self.function_edge_implementation(
                        owner,
                        edge,
                        numbered,
                        (&edge.declared_parameters, edge.declared_result),
                    ) {
                        Ok(Some(carried)) => self.implement_carried(member.member, carried),
                        // An abstract class's abstract override implements nothing; each concrete
                        // subclass's table holds the body.
                        Ok(None) => continue,
                        Err(reason) => Err(reason),
                    }
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
                let Some(member) = self.dependency_member(declaration, role)? else {
                    continue;
                };
                if self.implements(member.member) {
                    continue;
                }
                let implemented = self.dependency_default_implementation(member, declaration, None);
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
                    adapters,
                } => {
                    let placed = self.slot_function(slot)?;
                    self.implement_carried(
                        member,
                        Carried {
                            placed,
                            slot: Some(slot),
                            declared,
                            implemented,
                            adapters,
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

    /// Record the body of the default `declaration` as what the class implements `member` with:
    /// the function this file declares for it (`declared`), or else the one an earlier module
    /// exported.
    fn dependency_default_implementation(
        &mut self,
        member: DependencyMember,
        declaration: krusty::fir::ExternalCallableId,
        declared: Option<krusty::ir::FunId>,
    ) -> Result<(), Unsupported> {
        let function = declared
            .and_then(|function| self.recording.functions.get(function as usize).copied())
            .flatten()
            .or_else(|| {
                self.recording
                    .callables
                    .callable(declaration)
                    .and_then(|fact| fact.dependency_callable.as_ref())
                    .and_then(|identity| self.program.callables.dependency(identity))
            })
            .ok_or_else(|| {
                self.declined("a default a dependency's interface declares with no body")
            })?;
        self.record_implementation(
            member.member,
            function,
            DependencyImplementation::Function(function),
        )
    }

    /// The dependency interface default the class implements `edge`'s member with, when it
    /// inherits that default (directly or through a library superclass) without overriding it.
    fn inherited_interface_default(
        &self,
        edge: &krusty::ir::IrFunctionOverride,
    ) -> Option<krusty::fir::ExternalCallableId> {
        let ResolvedFunctionOverrideTarget::External(implementation) = edge.implementation else {
            return None;
        };
        let fact = self.recording.callables.callable(implementation)?;
        (edge.implementation_function.is_none()
            && fact.owner_is_interface
            && fact
                .klib_body_declaration_modifiers
                .is_some_and(|modifiers| !modifiers.is_abstract()))
        .then_some(implementation)
    }

    /// The function `owner` implements the dependency member `edge` overrides with: its own or
    /// inherited override, read off its own table so the most derived one stands, or a default of
    /// this module's interface, at that interface member's number (`numbered`).
    fn function_edge_implementation(
        &mut self,
        owner: ClassId,
        edge: &krusty::ir::IrFunctionOverride,
        numbered: &HashMap<u32, Placed>,
        declaration: (&[Ty], Ty),
    ) -> Result<Option<Carried>, Unsupported> {
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
                let function = edge.implementation_function.or_else(|| {
                    implementation
                        .and_then(|callable| ir.checked_callable_functions.get(&callable).copied())
                });
                let slot = match function {
                    Some(function) => {
                        let key = function_key(ir, owner, function);
                        self.classes.tables.slot(owner, &key)
                    }
                    // A member the class inherits from a superclass an earlier file laid out:
                    // the slot that file's table holds it at. A dependency member, an accessor
                    // among them, is found by its declaration's slot identity: the callable
                    // identity another module's file gave it numbers nothing here.
                    None => {
                        let callables = self.recording.callables;
                        self.classes
                            .laid_out_superclass(self.id)
                            .and_then(|superclass| match edge.implementation {
                                ResolvedFunctionOverrideTarget::Module(callable) => superclass
                                    .table
                                    .members
                                    .get(&PlanKey::Callable(callable))
                                    .copied(),
                                ResolvedFunctionOverrideTarget::External(target) => callables
                                    .callable(target)
                                    .and_then(BackendCallableFact::member_slot_identity)
                                    .and_then(|identity| {
                                        superclass.declaration_slots.get(&identity).copied()
                                    }),
                            })
                    }
                }
                .ok_or_else(|| self.declined("a dependency's interface member with no slot"))?;
                if self.slot_is_abstract(slot) {
                    return Ok(None);
                }
                (self.slot_function(slot)?, Some(slot))
            }
        };
        let declared = member_signature(declaration.0, declaration.1)?;
        // What the placed function physically takes: a `suspend` override's frame carries its
        // continuation after the source parameters and answers the erased result.
        let (implemented_parameters, implemented_result) = carried(
            &edge.implementation_parameters,
            edge.implementation_result,
            edge.suspend,
        );
        let implemented = member_signature(&implemented_parameters, implemented_result)?;
        let special_bridge = self
            .recording
            .classifiers
            .classifier(edge.overridden_owner)
            .and_then(|fact| fact.role)
            .is_some_and(|role| {
                matches!(
                    role,
                    krusty::types::ClassifierRole::MappedCollection(collection)
                        if collection.has_special_bridge(&edge.name)
                )
            });
        let adapters = self.adapters_for_placed(
            declaration,
            (&implemented_parameters, implemented_result),
            &declared,
            &implemented,
            &placed,
            special_bridge,
        )?;
        Ok(Some(Carried {
            placed,
            slot,
            declared,
            implemented,
            adapters,
        }))
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
            if !edge.overridden_is_interface {
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
                let Some(member) = self.dependency_member(target, role)? else {
                    continue;
                };
                if self.implements(member.member) {
                    continue;
                }
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
        let declared_types =
            extension_accessor_types(edge.declared_receiver, edge.declared_type, setter);
        let implemented_types = extension_accessor_types(
            edge.implementation_receiver,
            edge.implementation_type,
            setter,
        );
        let declared = member_signature(&declared_types.0, declared_types.1)?;
        let implemented = member_signature(&implemented_types.0, implemented_types.1)?;
        let placed = placed
            .ok_or_else(|| self.declined("a dependency's interface property with no slot"))?;
        let adapters = self.adapters_for_placed(
            (&declared_types.0, declared_types.1),
            (&implemented_types.0, implemented_types.1),
            &declared,
            &implemented,
            &placed,
            false,
        )?;
        Ok(Carried {
            placed,
            slot,
            declared,
            implemented,
            adapters,
        })
    }

    /// The member the dependency declaration `target` is in `role` (`dependency_member`).
    fn dependency_member(
        &mut self,
        target: krusty::fir::ExternalCallableId,
        role: MemberRole,
    ) -> Result<Option<DependencyMember>, Unsupported> {
        let Some(fact) = self.recording.callables.callable(target) else {
            return Ok(None);
        };
        dependency_member(
            &mut self.program.runtime.objects.interfaces,
            self.program.module,
            fact,
            role,
        )
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
            adapters,
        }: Carried,
    ) -> Result<(), Unsupported> {
        if self.implements(member) {
            return Ok(());
        }
        let (function, signature) = self.emit_thunk(
            declared.clone(),
            implemented.clone(),
            adapters.clone(),
            placed,
        )?;
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
                adapters,
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
        // A class below a library class is also an instance of every interface that class
        // implements, which neither the module numbering nor the class's own list repeats.
        let classifiers = self.recording.classifiers;
        let inherited = [class.fq_name_id(), class.superclass]
            .into_iter()
            .chain(class.interfaces.iter())
            .flat_map(|supertype| classifiers.inherited_interfaces(supertype))
            .collect::<Vec<_>>();
        for interface in module
            .chain(hierarchy)
            .chain(class.interfaces.iter())
            .chain(inherited)
        {
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

/// Leave the thunk with the bridge's rejection as its answer.
fn emit_rejection(code: &mut Code, rejected: BridgeRejection, result: &[ValType]) -> Option<()> {
    match (rejected, result) {
        (BridgeRejection::Value(value), _) => {
            code.i32_const(value);
        }
        (BridgeRejection::Null, [ValType::Ref { heap, .. }]) => {
            code.ref_null(*heap);
        }
        (BridgeRejection::Null, _) => return None,
    }
    code.return_();
    Some(())
}

/// What a collection special bridge answers when its argument is not the override's element.
///
/// `contains` and `remove` answer `false`. `indexOf` and `lastIndexOf` answer `-1`. A member of
/// one argument that answers a nullable reference (`Map.get`, `Map.remove`) answers `null`. Any
/// other result keeps the cast: a mismatch there is a use the frontend said was the narrower type.
fn collection_bridge_rejection(
    declared: (&[Ty], Ty),
    signature: &Signature,
) -> Option<BridgeRejection> {
    match declared.1 {
        Ty::Boolean => Some(BridgeRejection::Value(0)),
        Ty::Int => Some(BridgeRejection::Value(-1)),
        _ if declared.0.len() == 1
            && matches!(&signature.1[..], [ValType::Ref { nullable: true, .. }]) =>
        {
            Some(BridgeRejection::Null)
        }
        _ => None,
    }
}
