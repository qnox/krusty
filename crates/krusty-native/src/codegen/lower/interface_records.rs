//! What a type descriptor tells the link about the interfaces it implements: one record per
//! implemented member number of its module's interfaces, holding the number, the type and the
//! function at that number of the type's interface table; one record per implemented member of a
//! dependency's interfaces, by the member's KLIB identity; and, per file, the identity of every
//! member its interfaces declare (see `native::interface_calls`).

use super::super::super::interface_calls::{
    identities_section, implementations_section, padded, DEPENDENCY_IMPLEMENTATIONS_SECTION,
    RECORD_SIZE,
};
use super::*;
use krusty::backend::class_tables::module_slots::PlanKey;
use krusty::backend::interface_identity::{member_identity, property_member_identity};
use krusty::fir::ResolvedFunctionOverrideTarget;

use super::super::super::laid_out_classes::DependencyImplementation;

impl FileLowering<'_> {
    /// Record, for the link, which function stands at each number of `itable`, the interface
    /// table of the type `descriptor` describes: see `native::interface_calls`. A number the type
    /// does not implement holds the abstract-member trap only to keep the table dense, and is left
    /// out.
    pub(super) fn record_interface_implementations(
        &mut self,
        descriptor: DataId,
        base: &str,
        itable: &[FuncId],
    ) -> Result<(), Unsupported> {
        if itable.is_empty() {
            return Ok(());
        }
        let abstract_member = self.import("kt_abstract_method_called", &[], Ty::Unit)?;
        let implemented = itable
            .iter()
            .enumerate()
            .filter(|(_, function)| **function != abstract_member)
            .map(|(number, function)| (number as u32, *function))
            .collect::<Vec<_>>();
        if implemented.is_empty() {
            return Ok(());
        }
        let id = self.declare_local_data(&format!("kt_itfimpl_{base}"), false)?;
        let mut description = DataDescription::new();
        let mut contents = vec![0; implemented.len() * RECORD_SIZE as usize];
        for (record, &(number, _)) in contents
            .as_chunks_mut::<{ RECORD_SIZE as usize }>()
            .0
            .iter_mut()
            .zip(&implemented)
        {
            record[..8].copy_from_slice(&u64::from(number).to_le_bytes());
        }
        description.define(contents.into_boxed_slice());
        description.set_align(8);
        description.set_custom_section(&implementations_section(self.module_name));
        let ty = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        for (index, &(_, function)) in implemented.iter().enumerate() {
            let at = (index as u64 * RECORD_SIZE) as u32;
            description.write_data_addr(at + 8, ty, 0);
            let function = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(at + 16, function);
        }
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `kt_itfimpl_{base}` ({error})"))?;
        Ok(())
    }
}

impl FileLowering<'_> {
    /// The stub number of one dependency interface member this file calls. The same member keeps
    /// one number, after the numbers of this module's own interfaces.
    pub(super) fn dependency_interface_number(
        &mut self,
        identity: &krusty::libraries::KlibDependencyCallable,
    ) -> u32 {
        let bytes = krusty::backend::interface_identity::member_identity(identity);
        if let Some((number, _)) = self
            .dependency_interface_calls
            .iter()
            .find(|(_, known)| *known == bytes)
        {
            return *number;
        }
        let number = self.slot_plan.interfaces().count()
            + u32::try_from(self.dependency_interface_calls.len())
                .expect("a dependency interface number fits u32");
        self.dependency_interface_calls.push((number, bytes));
        number
    }

    /// Name, for the link, the KLIB identity of every function an interface of this file
    /// declares, beside its member number: what a dependent module's class implementing the member
    /// records its implementation under (see `native::interface_calls`); likewise every getter
    /// and setter of a property an interface of this file declares.
    pub(super) fn define_interface_identities(&mut self) -> Result<(), Unsupported> {
        let numbers = self.slot_plan.interfaces();
        let mut entries = self
            .ir
            .checked_callable_functions
            .iter()
            .filter_map(|(callable, function)| {
                let identity = self.dependency_signatures.interface_member(*function)?;
                let number = numbers.number(PlanKey::Callable(*callable))?;
                Some((number, member_identity(identity)))
            })
            .chain(
                self.ir
                    .local_property_layouts
                    .keys()
                    .filter_map(|property| {
                        Some((
                            *property,
                            self.dependency_signatures.interface_property(*property)?,
                        ))
                    })
                    .flat_map(|(property, getter)| {
                        [
                            (PlanKey::Getter(property), false),
                            (PlanKey::Setter(property), true),
                        ]
                        .into_iter()
                        .filter_map(move |(key, setter)| {
                            Some((
                                numbers.number(key)?,
                                property_member_identity(getter, setter)?,
                            ))
                        })
                    }),
            )
            .collect::<Vec<_>>();
        entries.extend(
            self.dependency_interface_calls
                .iter()
                .map(|(number, identity)| (*number, identity.clone())),
        );
        if entries.is_empty() {
            return Ok(());
        }
        entries.sort_unstable();
        entries.dedup();
        let mut contents = Vec::new();
        for (number, identity) in &entries {
            contents.extend_from_slice(&u64::from(*number).to_le_bytes());
            contents.extend_from_slice(&(identity.len() as u64).to_le_bytes());
            push_padded(&mut contents, identity);
        }
        let id = self.declare_local_data("kt_itfids", false)?;
        let mut description = DataDescription::new();
        description.define(contents.into_boxed_slice());
        description.set_align(8);
        description.set_custom_section(&identities_section(self.module_name));
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `kt_itfids` ({error})"))?;
        Ok(())
    }

    /// Record, for the link, the function each member of a DEPENDENCY's interface stands at for
    /// the type `descriptor` describes, an instance of `class`, by the member's KLIB identity:
    /// the dependency's calls through the member reach this type only through these records.
    /// `tables` are the class's own virtual and interface tables: an override of a dependency
    /// member takes a slot of its own, so the most derived implementation is read there, and a
    /// default of this module's interface stands at that interface member's number.
    pub(super) fn record_dependency_implementations(
        &mut self,
        descriptor: DataId,
        base: &str,
        class: ClassId,
        tables: (&[FuncId], &[FuncId]),
    ) -> Result<(), Unsupported> {
        let (vtable, itable) = tables;
        let mut implemented: Vec<(Vec<u8>, FuncId)> = Vec::new();
        for (identity, implementation) in self.dependency_implementations(class)? {
            let function = match implementation {
                DependencyImplementation::Slot(slot) => vtable.get(slot as usize).copied(),
                DependencyImplementation::Interface(number) => itable.get(number as usize).copied(),
                DependencyImplementation::Function(function) => {
                    Some(self.import_object_function(&function)?)
                }
                DependencyImplementation::Bridge {
                    slot,
                    declared,
                    implemented,
                } => Some(self.dependency_bridge(slot, &declared, &implemented)?),
            }
            .ok_or_else(|| {
                declined!(
                    "an implementation of a dependency's interface member outside the tables of \
                     `{}`",
                    self.kotlin_name(class)
                )
            })?;
            implemented.push((identity, function));
        }
        if implemented.is_empty() {
            return Ok(());
        }
        let id = self.declare_local_data(&format!("kt_itfdep_{base}"), false)?;
        let mut description = DataDescription::new();
        let mut contents = Vec::new();
        let mut fields = Vec::with_capacity(implemented.len());
        for (identity, function) in &implemented {
            let at = contents.len() as u32;
            contents.extend_from_slice(&(identity.len() as u64).to_le_bytes());
            contents.extend_from_slice(&[0; 16]);
            push_padded(&mut contents, identity);
            fields.push((at, *function));
        }
        description.define(contents.into_boxed_slice());
        description.set_align(8);
        description.set_custom_section(DEPENDENCY_IMPLEMENTATIONS_SECTION);
        let ty = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        for (at, function) in fields {
            description.write_data_addr(at + 8, ty, 0);
            let function = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(at + 16, function);
        }
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `kt_itfdep_{base}` ({error})"))?;
        Ok(())
    }

    /// A bridge standing for the member a dependency's interface declares as `declared`, which
    /// the override at `slot` implements as `implemented`; see
    /// [`DependencyImplementation::Bridge`].
    fn dependency_bridge(
        &mut self,
        slot: u32,
        declared: &(Vec<Ty>, Ty),
        implemented: &(Vec<Ty>, Ty),
    ) -> Result<FuncId, Unsupported> {
        let mut params = vec![any()];
        params.extend(declared.0.iter().copied());
        let name = format!("kt_dependency_bridge_{}", self.dependency_bridges);
        self.dependency_bridges += 1;
        let id = self.declare_local_function(&name, &params, declared.1)?;
        self.define_converting_bridge(
            (&declared.0, declared.1),
            (&implemented.0, implemented.1),
            super::object_entries::BridgeTarget::Slot(slot),
            &format!("bridge to slot {slot}"),
            id,
        )?;
        Ok(id)
    }

    /// How an instance of `class` implements each member of another source module's interface,
    /// by the member's KLIB identity, most derived first: by the function at a slot of its vtable
    /// (an override takes a slot of its own, so the most derived implementation is read there),
    /// at a number of its interface table (a default of this module's interface), or by a
    /// dependency's default. Past its superclasses in this file, a superclass an earlier file laid
    /// out answers from its record.
    pub(super) fn dependency_implementations(
        &mut self,
        class: ClassId,
    ) -> Result<Vec<(Vec<u8>, DependencyImplementation)>, Unsupported> {
        let mut implemented: Vec<(Vec<u8>, DependencyImplementation)> = Vec::new();
        // A checked override or property edge carries its exact provider identity. Do not require
        // that identity's callable body to have been imported into this module: the link joins
        // implementation records with the interface identities its call stubs name.
        let source_modules: krusty::fx_hash::HashSet<&str> = self
            .dependency_symbols
            .keys()
            .map(|member| member.module.as_str())
            .collect();
        let mut root = class;
        while let Some(parent) = self.model.layout(root).superclass {
            root = parent;
        }
        let inherits_record = self.laid_out_superclass(root).is_some();
        let mut current = Some(class);
        while let Some(owner) = current {
            let name = self.ir.classes[owner as usize].fq_name_id();
            for edge in self.ir.function_overrides.get(&name).into_iter().flatten() {
                let ResolvedFunctionOverrideTarget::External(target) = edge.overridden else {
                    continue;
                };
                if !edge.overridden_is_interface {
                    continue;
                }
                let Some(identity) = self
                    .callables
                    .callable(target)
                    .and_then(|fact| fact.dependency_callable.as_ref())
                else {
                    continue;
                };
                let identity = member_identity(identity);
                if implemented.iter().any(|(known, _)| *known == identity) {
                    continue;
                }
                let carried_alike = edge.declared_parameters.len()
                    == edge.implementation_parameters.len()
                    && edge
                        .declared_parameters
                        .iter()
                        .zip(&edge.implementation_parameters)
                        .chain([(&edge.declared_result, &edge.implementation_result)])
                        .all(|(declared, implementation)| {
                            model::representation(self.values, *declared)
                                == model::representation(self.values, *implementation)
                        });
                let implementation = match edge.implementation {
                    ResolvedFunctionOverrideTarget::Module(callable) => Some(callable),
                    // A dependency class's member a class here inherits: the record of the
                    // superclass an earlier file laid out answers it below.
                    ResolvedFunctionOverrideTarget::External(_)
                        if edge.implementation_function.is_none() && inherits_record =>
                    {
                        continue;
                    }
                    ResolvedFunctionOverrideTarget::External(_) => None,
                };
                // An interface of this module implementing the member stands at its own number
                // of the type's interface table; a class's implementation, at its slot.
                // A bridge dispatches through a slot, so only a class's implementation is bridged.
                if let Some(number) = implementation
                    .filter(|_| carried_alike && edge.implementation_function.is_none())
                    .and_then(|callable| {
                        self.slot_plan
                            .interfaces()
                            .number(PlanKey::Callable(callable))
                    })
                {
                    implemented.push((identity, DependencyImplementation::Interface(number)));
                    continue;
                }
                let function = edge
                    .implementation_function
                    .or_else(|| {
                        implementation.and_then(|callable| {
                            self.ir.checked_callable_functions.get(&callable).copied()
                        })
                    })
                    .ok_or_else(|| {
                        declined!(
                            "an implementation of a dependency's interface member with no body \
                             here (`{}.{}`)",
                            self.kotlin_name(owner),
                            edge.name
                        )
                    })?;
                let key = model::function_key(self.ir, owner, function);
                let slot = self.model.slot(owner, &key).ok_or_else(|| {
                    declined!(
                        "an implementation of a dependency's interface member with no slot \
                         (`{}.{}`)",
                        self.kotlin_name(owner),
                        edge.name
                    )
                })?;
                let implementation = if carried_alike {
                    DependencyImplementation::Slot(slot)
                } else {
                    DependencyImplementation::Bridge {
                        slot,
                        declared: (edge.declared_parameters.clone(), edge.declared_result),
                        implemented: (
                            edge.implementation_parameters.clone(),
                            edge.implementation_result,
                        ),
                    }
                };
                implemented.push((identity, implementation));
            }
            self.record_dependency_property_implementations(owner, &mut implemented)?;
            for default in self.ir.inherited_defaults.get(&name).into_iter().flatten() {
                let krusty::fir::InheritedDefaultBody::DependencyInterfaceMethod(declaration) =
                    default.body
                else {
                    continue;
                };
                let Some(member) = self
                    .callables
                    .callable(declaration)
                    .and_then(|fact| fact.dependency_callable.as_ref())
                    .filter(|member| source_modules.contains(member.module.as_str()))
                else {
                    continue;
                };
                let identity = match &default.name {
                    krusty::fir::InheritedMemberName::Function(_) => Some(member_identity(member)),
                    krusty::fir::InheritedMemberName::PropertyGetter(_) => {
                        property_member_identity(member, false)
                    }
                    krusty::fir::InheritedMemberName::PropertySetter(_) => {
                        property_member_identity(member, true)
                    }
                }
                .ok_or_else(|| {
                    declined!(
                        "an inherited accessor default of a dependency's interface with no \
                         accessor identity (`{}`)",
                        self.kotlin_name(owner)
                    )
                })?;
                if implemented.iter().any(|(known, _)| *known == identity) {
                    continue;
                }
                let export = self.dependency_symbols.get(member).ok_or_else(|| {
                    declined!(
                        "a default a dependency's interface declares without a body to call \
                         (`{}`)",
                        self.kotlin_name(owner)
                    )
                })?;
                let (symbol, parameters, result) = (
                    export.symbol.clone(),
                    export.signature.parameters().to_vec(),
                    export.signature.result(),
                );
                let function = self.import(&symbol, &parameters, result)?;
                implemented.push((
                    identity,
                    DependencyImplementation::Function(self.object_function(function)),
                ));
            }
            current = self.model.layout(owner).superclass;
            if current.is_none() {
                if let Some(record) = self.laid_out_superclass(owner) {
                    for (identity, implementation) in &record.dependency_implementations {
                        if !implemented.iter().any(|(known, _)| known == identity) {
                            implemented.push((identity.clone(), implementation.clone()));
                        }
                    }
                }
            }
        }
        Ok(implemented)
    }
}

impl FileLowering<'_> {
    /// Add to `implemented` the getter and setter `owner` implements each dependency-interface
    /// property with, by the accessor's identity: its own override's slot in
    /// `tables.0`, or a default of this module's interface at that property's number in `tables.1`.
    fn record_dependency_property_implementations(
        &mut self,
        owner: ClassId,
        implemented: &mut Vec<(Vec<u8>, DependencyImplementation)>,
    ) -> Result<(), Unsupported> {
        let name = self.ir.classes[owner as usize].fq_name_id();
        for edge in self.ir.property_overrides.get(&name).into_iter().flatten() {
            let krusty::fir::ResolvedPropertyOverrideTarget::External(target) = edge.overridden
            else {
                continue;
            };
            if !edge.overridden_is_interface {
                continue;
            }
            let Some(member) = self
                .callables
                .callable(target)
                .and_then(|fact| fact.dependency_callable.as_ref())
            else {
                continue;
            };
            if model::representation(self.values, edge.declared_type)
                != model::representation(self.values, edge.implementation_type)
            {
                return Err(declined!(
                    "a property of a dependency's interface implemented with another carrier \
                     (`{}.{}`)",
                    self.kotlin_name(owner),
                    edge.name
                ));
            }
            let roles = [false, true]
                .into_iter()
                .filter(|setter| !setter || edge.overridden_setter_value.is_some());
            for setter in roles {
                let identity = property_member_identity(member, setter).ok_or_else(|| {
                    declined!(
                        "a property of a dependency's interface with no accessor identity \
                         (`{}.{}`)",
                        self.kotlin_name(owner),
                        edge.name
                    )
                })?;
                if implemented.iter().any(|(known, _)| *known == identity) {
                    continue;
                }
                let interface_number = match edge.implementation {
                    krusty::fir::ResolvedPropertyOverrideTarget::Module(property) => {
                        let key = if setter {
                            PlanKey::Setter(property)
                        } else {
                            PlanKey::Getter(property)
                        };
                        self.slot_plan.interfaces().number(key)
                    }
                    krusty::fir::ResolvedPropertyOverrideTarget::External(_) => None,
                };
                let implementation = if let Some(number) = interface_number {
                    Some(DependencyImplementation::Interface(number))
                } else {
                    let key = if setter {
                        crate::classes::SlotKey::Setter(edge.overridden)
                    } else {
                        crate::classes::SlotKey::Getter(edge.overridden)
                    };
                    self.model
                        .slot(owner, &key)
                        .map(DependencyImplementation::Slot)
                }
                .ok_or_else(|| {
                    declined!(
                        "an implementation of a dependency's interface property with no slot \
                         (`{}.{}`)",
                        self.kotlin_name(owner),
                        edge.name
                    )
                })?;
                implemented.push((identity, implementation));
            }
        }
        Ok(())
    }
}

/// `bytes`, then zeros up to the next word.
fn push_padded(contents: &mut Vec<u8>, bytes: &[u8]) {
    contents.extend_from_slice(bytes);
    contents.resize(
        contents.len() + (padded(bytes.len() as u64) as usize - bytes.len()),
        0,
    );
}
