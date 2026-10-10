//! A class declared in one file of the module and constructed from another.
//!
//! The plan is keyed by the checked constructor declaration and read from its module record,
//! which the declaring file and every constructing file copy from that one declaration. Its ABI
//! is one hidden allocating entry point named from the class's qualified name and the
//! constructor's complete parameter list: it takes an inner class's enclosing instance first, then
//! the declared parameters at their declared types, and answers the instance (a value class's
//! underlying value).
//!
//! Only the declaring file knows the layout and the constructor body. Its entry point runs the
//! file's once-only initializer, allocates, converts each declared parameter to what its local
//! constructor takes, and calls it. A constructing file evaluates every argument first, then
//! calls.

use super::super::*;
use crate::fir::{DeclarationFlags, DeclarationId};
use crate::ir::IrModuleConstructor;

/// The one ABI both files use for a module constructor.
struct ConstructorAbi {
    owner: TypeName,
    /// What the entry point takes: the enclosing instance of an inner class, then the declared
    /// parameters.
    parameters: Vec<Ty>,
    symbol: String,
}

impl ConstructorAbi {
    fn result(&self) -> Ty {
        Ty::obj_name(self.owner)
    }
}

/// The plan for one constructor, or why another file cannot construct through it.
fn plan(record: &IrModuleConstructor) -> Result<ConstructorAbi, Unsupported> {
    let name = record.owner.render();
    let owner = record.owner_flags;
    let declined = [
        (DeclarationFlags::INTERFACE, "an interface"),
        (DeclarationFlags::ANNOTATION_CLASS, "an annotation class"),
        (DeclarationFlags::ENUM, "an enum class"),
        (DeclarationFlags::SINGLETON, "an object"),
        (DeclarationFlags::ABSTRACT, "an abstract class"),
        (DeclarationFlags::SEALED, "a sealed class"),
        (DeclarationFlags::LOCAL_CLASS, "a local class"),
        (DeclarationFlags::ANONYMOUS_OBJECT, "an anonymous object"),
        (DeclarationFlags::EXPECT, "an `expect` class"),
    ];
    if let Some((_, kind)) = declined.iter().find(|(flag, _)| owner.has(*flag)) {
        return Err(format!("a cross-file construction of {kind} (`{name}`)"));
    }
    if record.flags.has(DeclarationFlags::EXPECT) {
        return Err(format!("a cross-file `expect` constructor of `{name}`"));
    }
    if record.visibility == crate::types::Visibility::Private {
        return Err(format!(
            "a private constructor of `{name}` from another file"
        ));
    }
    if record.context_parameter_count != 0 {
        return Err(format!(
            "a cross-file constructor with context parameters (`{name}`)"
        ));
    }
    let symbol = super::super::super::super::symbols::module_constructor_symbol(record)
        .ok_or_else(|| {
            format!("internal: a module constructor of `{name}` with an unchecked parameter type")
        })?;
    let mut parameters: Vec<Ty> = record.outer.map(Ty::obj_name).into_iter().collect();
    parameters.extend(record.parameters.iter().copied());
    Ok(ConstructorAbi {
        owner: record.owner,
        parameters,
        symbol,
    })
}

impl FileLowering<'_> {
    /// Define the allocating entry point of every constructor this file declares whose plan
    /// another file can use.
    ///
    /// A supported plan this file cannot realize — no local class or constructor, or a local
    /// constructor of another arity — is an internal completeness error: another file may
    /// already import the entry point it promises.
    pub(in super::super) fn define_constructor_entry_points(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        let mut constructors: Vec<DeclarationId> = self
            .ir
            .module_constructions
            .records
            .keys()
            .filter(|constructor| self.ir.checked_constructor_bodies.contains_key(constructor))
            .copied()
            .collect();
        constructors.sort_by_key(|constructor| constructor.raw());
        for constructor in constructors {
            // The same decision every constructing file makes: a declined plan has no entry
            // point, and no other file calls one.
            let Ok(abi) = plan(&self.ir.module_constructions.records[&constructor]) else {
                continue;
            };
            let checked = &self.ir.checked_constructor_bodies[&constructor];
            let (class, ordinal) = (checked.class, checked.ordinal);
            let name = abi.owner.render();
            if self.ir.classes[class as usize].fq_name != abi.owner {
                return Err(format!(
                    "internal: the module constructor of `{name}` belongs to another class here"
                ));
            }
            let local = match ordinal {
                0 => self.classes[class as usize].constructor.map(|function| {
                    (
                        function,
                        super::super::objects::constructor_parameters(self.ir, class),
                    )
                }),
                n => self.classes[class as usize]
                    .secondaries
                    .get(n as usize - 1)
                    .copied()
                    .zip(
                        self.ir.classes[class as usize]
                            .secondary_ctors
                            .get(n as usize - 1)
                            .map(|secondary| {
                                let mut parameters = secondary.prefix_params.clone();
                                parameters.extend(secondary.params.iter().copied());
                                parameters
                            }),
                    ),
            };
            let Some((function, physical)) = local else {
                return Err(format!(
                    "internal: the module constructor {ordinal} of `{name}` has no local constructor"
                ));
            };
            if physical.len() != abi.parameters.len() {
                return Err(format!(
                    "internal: the module constructor {ordinal} of `{name}` takes {} parameters here, \
                     not the {} its plan declares",
                    physical.len(),
                    abi.parameters.len()
                ));
            }
            self.define_constructor_entry_point(&abi, class, function, &physical, file_init)?;
        }
        Ok(())
    }

    fn define_constructor_entry_point(
        &mut self,
        abi: &ConstructorAbi,
        class: ClassId,
        constructor: FuncId,
        physical: &[Ty],
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        let result = abi.result();
        let signature = self.signature_of(&abi.parameters, result)?;
        let id = self
            .module
            .declare_function(&abi.symbol, Linkage::Hidden, &signature)
            .map_err(|error| format!("declaring `{}` ({error})", abi.symbol))?;
        let descriptor = self.classes[class as usize].descriptor;
        let size = self.model.layout(class).instance_size;
        let declared = abi.parameters.clone();
        let name = abi.owner.render();
        self.emit_function(id, signature, result, &abi.symbol, &mut |body, params| {
            let init = body.func_ref(file_init);
            body.emit_call(init, &[])?;
            if body.terminated {
                return Ok(());
            }
            let mut arguments = Vec::with_capacity(params.len() + 1);
            for ((value, declared), physical) in params.iter().zip(&declared).zip(physical) {
                let Some(value) = body.convert(*value, Some(*declared), *physical)? else {
                    return Err(format!("a `Unit` constructor parameter of `{name}`"));
                };
                arguments.push(value);
            }
            let object = body.allocate(descriptor, size)?;
            arguments.insert(0, object);
            let constructor = body.func_ref(constructor);
            body.emit_call(constructor, &arguments)?;
            if body.terminated {
                return Ok(());
            }
            let Some(value) = body.constructed(object, class)? else {
                return Err(format!("construction of `{name}` produced no instance"));
            };
            body.builder.ins().return_(&[value]);
            body.terminate();
            Ok(())
        })
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Construct a class another file of this module declares, through the entry point of the
    /// constructor the checker selected.
    pub(in super::super) fn module_construction(
        &mut self,
        constructor: DeclarationId,
        internal: TypeName,
        args: &[u32],
        defaulted: bool,
    ) -> Result<Option<Value>, Unsupported> {
        let name = internal.render();
        let record = self
            .file
            .ir
            .module_constructions
            .records
            .get(&constructor)
            .ok_or_else(|| {
                format!("internal: a cross-file constructor of `{name}` with no module record")
            })?;
        if record.owner != internal {
            return Err(format!(
                "internal: a construction of `{name}` selected another class's constructor"
            ));
        }
        let abi = plan(record)?;
        if defaulted {
            return Err(format!(
                "a cross-file constructor with a default argument (`{name}`)"
            ));
        }
        if args.len() != abi.parameters.len() {
            return Err(format!(
                "internal: a construction of `{name}` passes {} arguments to a constructor of {}",
                args.len(),
                abi.parameters.len()
            ));
        }
        let result = abi.result();
        let id = self.file.import(&abi.symbol, &abi.parameters, result)?;
        let arguments = self.arguments(args, &abi.parameters)?;
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        if self.terminated {
            return Ok(None);
        }
        Ok(self.builder.inst_results(call).first().copied())
    }
}
