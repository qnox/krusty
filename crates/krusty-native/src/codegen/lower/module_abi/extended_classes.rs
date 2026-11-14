//! A class declared in one file of the module and extended in a later one.
//!
//! The subclass's file reads what it inherits from the record the declaring file left
//! ([`crate::laid_out_classes`]): its layout, what each table entry holds, and the entry points it
//! calls. Those entry points are this file's to define, because only it has the layout and the
//! bodies. A super-constructor entry per constructor runs this file's once-only initializer and
//! then the constructor on the object the subclass allocated; the activation entry runs the
//! initializer and creates the companions of the class and its superclasses, root first, as
//! constructing the class here would. A table entry or method is recorded under the symbol this
//! object links it by; one this object keeps to itself is recorded as such, and the later file
//! declines where it would inherit it rather than name a symbol no object defines.

use super::super::super::super::laid_out_classes::{LaidOutClass, ObjectFunction};
use super::super::objects::{constructor_parameters, secondary_constructor_parameters};
use super::super::*;
use krusty::fx_hash::HashSet;

/// The entry points a later file's subclass of one class calls.
pub(in super::super) struct ExtensionEntries {
    /// By constructor ordinal, the primary first: the entry running this file's initializer and
    /// then that constructor, absent where the class has no such constructor.
    constructors: Vec<Option<FuncId>>,
    /// Initializes the class the way constructing it here does.
    activation: FuncId,
}

/// The classes of a file whose members a later file's subclass names: each class it extends and
/// that class's superclasses in the file.
pub(in super::super) fn exported_classes(
    model: &ClassModel,
    extended: &HashSet<ClassId>,
) -> HashSet<ClassId> {
    let mut exported = HashSet::default();
    for &class in extended {
        let mut current = Some(class);
        while let Some(class) = current {
            if !exported.insert(class) {
                break;
            }
            current = model.layout(class).superclass;
        }
    }
    exported
}

impl FileLowering<'_> {
    /// The record of the nearest superclass an earlier file laid out for `class`, if it has one.
    pub(in super::super) fn laid_out_superclass(&self, class: ClassId) -> Option<&LaidOutClass> {
        self.model.layout(class).laid_out_superclass.map(|name| {
            self.laid_out
                .get(name)
                .expect("the class model extends only a recorded class")
        })
    }

    /// How another object names `function` of this one.
    pub(in super::super) fn object_function(&self, function: FuncId) -> ObjectFunction {
        let declaration = self.module.declarations().get_function_decl(function);
        let symbol = declaration.linkage_name(function).into_owned();
        match declaration.linkage {
            Linkage::Local => ObjectFunction::Local(symbol),
            _ => ObjectFunction::Linked {
                symbol,
                signature: declaration.signature.clone(),
            },
        }
    }

    /// `function` of an earlier file's object, imported under its symbol.
    pub(in super::super) fn import_object_function(
        &mut self,
        function: &ObjectFunction,
    ) -> Result<FuncId, Unsupported> {
        match function {
            ObjectFunction::Linked { symbol, signature } => {
                self.import_signature(symbol, signature.clone())
            }
            ObjectFunction::Local(symbol) => Err(declined!(
                "inheriting `{symbol}`, which the file declaring the superclass keeps to itself"
            )),
        }
    }

    /// Declare a function of `class` that a later file's subclass may name: linked across the
    /// module when the class is exported, kept to this object otherwise.
    pub(in super::super) fn declare_class_function(
        &mut self,
        class: ClassId,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> Result<FuncId, Unsupported> {
        if !self.exported.contains(&class) {
            return self.declare_local_function(name, params, ret);
        }
        let signature = self.signature_of(params, ret)?;
        self.module
            .declare_function(name, Linkage::Hidden, &signature)
            .map_err(|error| declined!("declaring `{name}` ({error})"))
    }

    /// The constructor entry `ordinal` of the superclass an earlier file laid out for `class`,
    /// with the parameters it takes after the object.
    pub(in super::super) fn laid_out_super_constructor(
        &mut self,
        class: ClassId,
        ordinal: u32,
    ) -> Result<(FuncId, Vec<Ty>), Unsupported> {
        let name = self.ir.classes[class as usize].fq_name();
        let (entry, parameters) = self
            .laid_out_superclass(class)
            .and_then(|record| record.constructors.get(ordinal as usize).cloned().flatten())
            .ok_or_else(|| {
                declined!("a superclass constructor of `{name}` its declaring file does not export")
            })?;
        Ok((self.import_object_function(&entry)?, parameters))
    }

    /// What realizes slot `slot` of the vtable of `class`'s superclass an earlier file laid out.
    pub(in super::super) fn inherited_entry(
        &mut self,
        laid_out: Option<TypeName>,
        slot: Option<u32>,
        number: Option<u32>,
    ) -> Result<FuncId, Unsupported> {
        let record = laid_out
            .and_then(|name| self.laid_out.get(name))
            .ok_or("an inherited table entry of a class extending no recorded superclass")?;
        let function = match (slot, number) {
            (Some(slot), _) => record.vtable.get(slot as usize),
            (None, Some(number)) => record.interface_bodies.get(&number),
            (None, None) => None,
        }
        .cloned()
        .ok_or("an inherited table entry its superclass's record does not hold")?;
        self.import_object_function(&function)
    }

    /// Define the entry points of every class a later file extends.
    pub(in super::super) fn define_extension_entries(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        let mut extended: Vec<ClassId> = self.extended.iter().copied().collect();
        extended.sort_unstable();
        for class in extended {
            let base = self.class_base(class).to_string();
            let mut constructors = Vec::new();
            let primary = self.classes[class as usize]
                .constructor
                .map(|constructor| (constructor, constructor_parameters(self.ir, class)));
            constructors.push(primary);
            for ordinal in 0..self.classes[class as usize].secondaries.len() {
                constructors.push(Some((
                    self.classes[class as usize].secondaries[ordinal],
                    secondary_constructor_parameters(self.ir, class, ordinal),
                )));
            }
            let mut entries = Vec::with_capacity(constructors.len());
            for (ordinal, constructor) in constructors.into_iter().enumerate() {
                let Some((constructor, parameters)) = constructor else {
                    entries.push(None);
                    continue;
                };
                let symbol = format!("kt_{base}__super_{ordinal}");
                entries.push(Some(self.define_super_constructor_entry(
                    &symbol,
                    class,
                    constructor,
                    &parameters,
                    file_init,
                )?));
            }
            let symbol = format!("kt_{base}__activate");
            let signature = self.signature_of(&[], Ty::Unit)?;
            let activation = self
                .module
                .declare_function(&symbol, Linkage::Hidden, &signature)
                .map_err(|error| declined!("declaring `{symbol}` ({error})"))?;
            self.emit_function(activation, signature, Ty::Unit, &symbol, &mut |body, _| {
                let init = body.func_ref(file_init);
                body.emit_call(init, &[])?;
                if body.terminated {
                    return Ok(());
                }
                body.activate_class(class)
            })?;
            self.extension_entries.insert(
                class,
                ExtensionEntries {
                    constructors: entries,
                    activation,
                },
            );
        }
        Ok(())
    }

    fn define_super_constructor_entry(
        &mut self,
        symbol: &str,
        class: ClassId,
        constructor: FuncId,
        parameters: &[Ty],
        file_init: FuncId,
    ) -> Result<FuncId, Unsupported> {
        let mut slots = vec![self.object_type(class)];
        slots.extend_from_slice(parameters);
        let signature = self.signature_of(&slots, Ty::Unit)?;
        let id = self
            .module
            .declare_function(symbol, Linkage::Hidden, &signature)
            .map_err(|error| declined!("declaring `{symbol}` ({error})"))?;
        self.emit_function(id, signature, Ty::Unit, symbol, &mut |body, params| {
            let init = body.func_ref(file_init);
            body.emit_call(init, &[])?;
            if body.terminated {
                return Ok(());
            }
            let constructor = body.func_ref(constructor);
            body.emit_call(constructor, params)?;
            Ok(())
        })?;
        Ok(id)
    }

    /// The record of every class a later file extends, as this file laid it out.
    pub(in super::super) fn laid_out_records(
        &mut self,
    ) -> Result<Vec<(TypeName, LaidOutClass)>, Unsupported> {
        let mut extended: Vec<ClassId> = self.extended.iter().copied().collect();
        extended.sort_unstable();
        let mut callables: HashMap<krusty::ir::FunId, krusty::fir::CallableId> = HashMap::default();
        for (&callable, &function) in &self.ir.checked_callable_functions {
            callables.insert(function, callable);
        }
        let mut records = Vec::with_capacity(extended.len());
        for class in extended {
            let layout = self.model.layout(class).clone();
            let (vtable, itable) = self
                .extension_tables
                .get(&class)
                .cloned()
                .ok_or("an extended class whose tables were not defined")?;
            let table = self.model.laid_out_tables[class as usize].clone();
            let vtable = vtable
                .into_iter()
                .map(|function| self.object_function(function))
                .collect();
            let interface_bodies = table
                .interface_entries
                .iter()
                .filter(|(_, slot)| slot.is_none())
                .filter_map(|(&number, _)| {
                    itable
                        .get(number as usize)
                        .map(|&function| (number, self.object_function(function)))
                })
                .collect();
            let entries = self
                .extension_entries
                .get(&class)
                .ok_or("an extended class without its entry points")?;
            let mut constructors = Vec::with_capacity(entries.constructors.len());
            for (ordinal, entry) in entries.constructors.iter().enumerate() {
                let parameters = match ordinal {
                    0 => constructor_parameters(self.ir, class),
                    n => secondary_constructor_parameters(self.ir, class, n - 1),
                };
                constructors.push(entry.map(|entry| (self.object_function(entry), parameters)));
            }
            let activation = self.object_function(entries.activation);
            let mut methods = HashMap::default();
            let mut current = Some(class);
            let mut root = class;
            while let Some(owner) = current {
                for &function in &self.ir.classes[owner as usize].methods {
                    let (Some(&callable), Some(id)) =
                        (callables.get(&function), self.functions[function as usize])
                    else {
                        continue;
                    };
                    let parameters =
                        super::super::super::super::captures::carried_parameters(self.ir, function);
                    let result = self.ir.functions[function as usize].ret;
                    methods
                        .entry(callable)
                        .or_insert_with(|| (self.object_function(id), parameters, result));
                }
                root = owner;
                current = self.model.layout(owner).superclass;
            }
            if let Some(inherited) = self.laid_out_superclass(root) {
                for (callable, method) in &inherited.methods {
                    methods.entry(*callable).or_insert_with(|| method.clone());
                }
            }
            // A slot or interface number stays one: the subclass reads its own table there, which
            // holds its override where it has one.
            let dependency_implementations = self.dependency_implementations(class)?;
            records.push((
                self.ir.classes[class as usize].fq_name,
                LaidOutClass {
                    table,
                    descriptor: self
                        .module
                        .declarations()
                        .get_data_decl(self.classes[class as usize].descriptor)
                        .name
                        .clone()
                        .ok_or("an extended class with an anonymous descriptor")?,
                    fields_end: layout.fields_end,
                    reference_offsets: layout.reference_offsets,
                    vtable,
                    interface_bodies,
                    constructors,
                    methods,
                    activation,
                    dependency_implementations,
                },
            ));
        }
        Ok(records)
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Call the activation entry of the superclass an earlier file laid out for `class`, which
    /// runs before the companions of `class` and its superclasses in this file.
    pub(in super::super) fn activate_laid_out_superclass(
        &mut self,
        root: ClassId,
    ) -> Result<(), Unsupported> {
        let Some(record) = self.file.laid_out_superclass(root) else {
            return Ok(());
        };
        let activation = record.activation.clone();
        let activation = self.file.import_object_function(&activation)?;
        let func_ref = self.func_ref(activation);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }
}
