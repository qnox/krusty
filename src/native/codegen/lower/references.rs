//! Property references as values: `::foo`, `C::p`, `x::p`.
//!
//! A property reference is an OBJECT, the way a lambda is: one emitted type per site, and one
//! instance per evaluation — or one static instance for the whole program when the site binds no
//! receiver, which is also what makes `::foo == ::foo` true, since the generator answers `equals`
//! by identity and Kotlin answers it by declaration.
//!
//! What makes it more than a lambda is the surface it has to answer. `KProperty` declares `get`,
//! `KMutableProperty` adds `set`, and `KCallable` declares `name`, so the type carries three slots
//! beyond `kotlin.Any`'s three and the site's own bodies fill them. Those bodies are emitted here
//! rather than lowered from IR because there is no IR to lower: common lowering leaves the
//! reference CHECKED — it names the property and says whether a receiver is bound, and nothing
//! more — precisely so that each target may choose its own representation.
//!
//! What is realized: a property of this file, top-level or a class member, bound or not, and
//! however its value is reached — a slot, a source-written accessor pair, or a delegate. A
//! reference to a DEPENDENCY property (`"kotlin"::length`), to a MEMBER extension property, or to
//! one with context parameters declines by name: each wants more operands than the one receiver
//! this object has room for, and answering it wrongly would be worse than skipping the program.

use super::*;
use crate::fir::FirPropertyReferenceTarget;

/// Where the three members live, after `kotlin.Any`'s.
const GET: u32 = 3;
const SET: u32 = 4;
const NAME: u32 = 5;

/// The emitted pieces of one property-reference site.
#[derive(Clone, Copy)]
pub(super) struct ReferenceItems {
    descriptor: DataId,
    instance_size: u32,
    /// Byte offset of the bound receiver, for a site that binds one.
    receiver_offset: Option<u32>,
    /// The one instance a site with no bound receiver has, in static storage.
    singleton: Option<DataId>,
}

/// What a site's `get` and `set` reach.
#[derive(Clone, Copy)]
enum Access {
    /// A top-level property: a global slot, or a source-written accessor pair.
    TopLevel,
    /// A member of a class of this file, read and written through the receiver.
    Member { class: ClassId, index: usize },
    /// A top-level property reached through its ACCESSOR pair rather than through a slot: an
    /// extension property, which has no object of its own to keep a field in, and a delegated or
    /// custom-accessor property, whose value is not in any slot this reference could name. The two
    /// differ only in whether the accessor leads with a receiver, so that is what `receiver` says
    /// — and a site that has none has none to pass, bound or not.
    Accessor {
        getter: FunId,
        setter: Option<FunId>,
        receiver: bool,
    },
}

/// One reference site, as the declaration pass reads it out of the checked node.
struct Site {
    /// The Kotlin name of the property, which `KCallable.name` answers.
    name: String,
    access: Access,
    ty: Ty,
    mutable: bool,
    /// The expression supplying `x` in `x::p`.
    bound: Option<u32>,
}

/// The reference a checked node creates, or `None` when it creates none.
fn reference_site(expr: &IrExpr) -> Option<(&crate::fir::PropertyId, Option<u32>, bool)> {
    let IrExpr::Checked(IrCheckedOperation::PropertyReference {
        target,
        dispatch_receiver,
        extension_receiver,
        mutable,
        ..
    }) = expr
    else {
        return None;
    };
    // The object carries at most ONE receiver, and which of the two it is does not matter to it:
    // `x::p` binds a class's, `"ab"::ext` an extension one, and both arrive as the single operand
    // the accessor leads with. BOTH at once is a member extension property, whose accessor wants
    // two receivers this object has no room for; that shape is left to decline by name.
    let bound = match (dispatch_receiver, extension_receiver) {
        (None, None) => None,
        (Some(receiver), None) | (None, Some(receiver)) => Some(*receiver),
        (Some(_), Some(_)) => return None,
    };
    // `extension_receiver` says the property's receiver is an EXTENSION one rather than a class's,
    // which is a fact about the accessor's parameter list and not about this object: either way the
    // reference carries one receiver, bound here or handed to `get`. What the site cannot realize
    // is a property whose accessor wants more than that, and the layout below is what says so.
    let property = match target {
        FirPropertyReferenceTarget::Module(property)
        | FirPropertyReferenceTarget::SpecializedModule { property, .. } => property,
        _ => return None,
    };
    Some((property, bound, *mutable))
}

impl<'a> FileLowering<'a> {
    /// Declare a type and its member bodies for every property reference in the file.
    ///
    /// One type per (property, bound-or-not), NOT one per site: Kotlin compares callable references
    /// by the declaration they name, so two `::foo`s written in different places are equal — and
    /// with one type and, for an unbound reference, one instance, identity equality answers that
    /// without a word of reflection metadata.
    pub(super) fn declare_property_references(&mut self) -> Result<(), Unsupported> {
        let mut emitted: HashMap<(crate::fir::PropertyId, bool), ReferenceItems> = HashMap::new();
        for index in 0..self.ir.exprs.len() {
            let Some((property, bound, mutable)) = reference_site(&self.ir.exprs[index]) else {
                continue;
            };
            let (property, bound) = (*property, bound);
            let Some(site) = self.reference_of(&property, bound, mutable) else {
                // Not a shape this realizes. The node keeps its decline, raised where it is
                // lowered, so the diagnostic still names the construct rather than this pass.
                continue;
            };
            let key = (property, bound.is_some());
            let items = match emitted.get(&key) {
                Some(items) => *items,
                None => {
                    let items = self.define_reference(emitted.len(), site)?;
                    emitted.insert(key, items);
                    items
                }
            };
            self.references.insert(index as u32, items);
        }
        Ok(())
    }

    /// Declare a type for every LOCAL delegated property's `KProperty` metadata.
    ///
    /// A local delegated property has no storage and no accessors to reach: the metadata exists so
    /// that `getValue(thisRef, property)` can ask the property its name, and that is the whole of
    /// what it answers. `get` and `set` are the runtime's loud failure rather than a body, because
    /// no type a program can name here declares them — a local property reference is a
    /// `KProperty<*>`, and Kotlin gives it no receiver to read through.
    ///
    /// One type per NAME, for the same reason a property reference is one type per property: two
    /// references to the same local property are the same declaration and compare equal.
    pub(super) fn declare_local_property_references(&mut self) -> Result<(), Unsupported> {
        let mut emitted: HashMap<Box<str>, ReferenceItems> = HashMap::new();
        for index in 0..self.ir.exprs.len() {
            let IrExpr::LocalPropertyReference { name, .. } = &self.ir.exprs[index] else {
                continue;
            };
            let name = name.clone();
            let items = match emitted.get(&name) {
                Some(items) => *items,
                None => {
                    let items = self.define_local_reference(emitted.len(), &name)?;
                    emitted.insert(name, items);
                    items
                }
            };
            self.references.insert(index as u32, items);
        }
        Ok(())
    }

    fn define_local_reference(
        &mut self,
        ordinal: usize,
        property: &str,
    ) -> Result<ReferenceItems, Unsupported> {
        let base = format!("kt_local_prop_{ordinal}");
        let instance_size = model::HEADER_SIZE.next_multiple_of(8);
        let name = self.declare_local_function(&format!("{base}_name"), &[any()], any())?;
        let equals =
            self.declare_local_function(&format!("{base}_equals"), &[any(), any()], Ty::Boolean)?;
        let unreachable = self.import("kt_abstract_method_called", &[], Ty::Unit)?;

        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let mut vtable = self.any_vtable()?;
        vtable[0] = equals;
        vtable.push(unreachable);
        vtable.push(unreachable);
        vtable.push(name);
        let any_type = self.import_data("kt_type_any")?;
        self.define_type_descriptor(
            descriptor,
            &base,
            "kotlin.reflect.KProperty",
            instance_size,
            &[],
            &vtable,
            any_type,
            &[],
            None,
        )?;
        self.define_reference_equals(equals, &base, descriptor, None)?;
        self.define_reference_name(name, &base, property)?;

        let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
        description.set_align(8);
        let global = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        description.write_data_addr(0, global, 0);
        self.module
            .define_data(instance, &description)
            .map_err(|error| format!("defining `{base}_instance` ({error})"))?;

        Ok(ReferenceItems {
            descriptor,
            instance_size,
            receiver_offset: None,
            singleton: Some(instance),
        })
    }

    /// Resolve the checked property a site names to what this generator can read and write.
    fn reference_of(
        &self,
        property: &crate::fir::PropertyId,
        bound: Option<u32>,
        mutable: bool,
    ) -> Option<Site> {
        let checked = self.ir.checked_properties.get(property)?;
        let access = match self.ir.local_property_layouts.get(property) {
            // A top-level extension property: one receiver, which this object carries or is handed.
            Some(IrLocalPropertyLayout::TopLevelAccessor {
                getter,
                setter,
                receiver,
                context_parameters,
            }) if context_parameters.is_empty() => Access::Accessor {
                getter: *getter,
                setter: *setter,
                receiver: receiver.is_some(),
            },
            // A property with context parameters, or a MEMBER extension property: its accessor
            // takes operands this object does not carry, so the site is left to decline by name.
            Some(IrLocalPropertyLayout::TopLevelAccessor { .. })
            | Some(IrLocalPropertyLayout::MemberExtension { .. }) => return None,
            _ => match checked.class {
                None => Access::TopLevel,
                Some(class) => {
                    let index = self.ir.classes[class as usize]
                        .properties
                        .iter()
                        .position(|candidate| candidate.name == checked.name)?;
                    Access::Member { class, index }
                }
            },
        };
        Some(Site {
            name: checked.name.clone(),
            access,
            ty: checked.ty,
            mutable,
            bound,
        })
    }

    fn define_reference(
        &mut self,
        ordinal: usize,
        site: Site,
    ) -> Result<ReferenceItems, Unsupported> {
        let base = format!("kt_prop_{ordinal}");
        // The bound receiver is the object's one field, and the one reference the collector traces.
        let (receiver_offset, instance_size, references) = match site.bound {
            None => (None, model::HEADER_SIZE.next_multiple_of(8), Vec::new()),
            Some(_) => (
                Some(model::HEADER_SIZE),
                (model::HEADER_SIZE + 8).next_multiple_of(8),
                vec![model::HEADER_SIZE],
            ),
        };

        let getter = self.declare_local_function(&format!("{base}_get"), &[any(), any()], any())?;
        let setter = site
            .mutable
            .then(|| {
                self.declare_local_function(
                    &format!("{base}_set"),
                    &[any(), any(), any()],
                    Ty::Unit,
                )
            })
            .transpose()?;
        let name = self.declare_local_function(&format!("{base}_name"), &[any()], any())?;

        let equals =
            self.declare_local_function(&format!("{base}_equals"), &[any(), any()], Ty::Boolean)?;
        let mut vtable = self.any_vtable()?;
        // Kotlin compares two references by the declaration they name and the receiver they bind.
        // The type IS the declaration here — one per property — so `equals` is a type comparison,
        // plus the bound receivers when there are any.
        vtable[0] = equals;
        vtable.push(getter);
        vtable.push(match setter {
            Some(setter) => setter,
            // A `val`'s `set` slot is unreachable: `KMutableProperty` is the only type that names
            // one, and a reference to a `val` never wears it.
            None => getter,
        });
        vtable.push(name);
        debug_assert_eq!(vtable.len() as u32, NAME + 1);

        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let any_type = self.import_data("kt_type_any")?;
        let kotlin_name = if site.mutable {
            "kotlin.reflect.KMutableProperty"
        } else {
            "kotlin.reflect.KProperty"
        };
        self.define_type_descriptor(
            descriptor,
            &base,
            kotlin_name,
            instance_size,
            &references,
            &vtable,
            any_type,
            &[],
            None,
        )?;

        self.define_reference_equals(equals, &base, descriptor, receiver_offset)?;
        self.define_reference_get(getter, &base, &site, receiver_offset)?;
        if let Some(setter) = setter {
            self.define_reference_set(setter, &base, &site, receiver_offset)?;
        }
        self.define_reference_name(name, &base, &site.name)?;

        let singleton = match site.bound {
            Some(_) => None,
            None => {
                let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
                let mut description = DataDescription::new();
                description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
                description.set_align(8);
                let global = self
                    .module
                    .declare_data_in_data(descriptor, &mut description);
                description.write_data_addr(0, global, 0);
                self.module
                    .define_data(instance, &description)
                    .map_err(|error| format!("defining `{base}_instance` ({error})"))?;
                Some(instance)
            }
        };

        Ok(ReferenceItems {
            descriptor,
            instance_size,
            receiver_offset,
            singleton,
        })
    }

    /// `equals(self, other)`: the same declaration, and the same bound receiver when there is one.
    fn define_reference_equals(
        &mut self,
        id: FuncId,
        base: &str,
        descriptor: DataId,
        receiver_offset: Option<u32>,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
        self.emit_function(
            id,
            signature,
            Carrier::Scalar(types::I8, false),
            &format!("{base}_equals"),
            &mut |body, params| {
                let no = body.builder.ins().iconst(types::I8, 0);
                let mine = body.data_address(descriptor);
                let theirs = body
                    .builder
                    .ins()
                    .load(types::I64, objects::trusted(), params[1], 0);
                let same_type = body.builder.ins().icmp(IntCC::Equal, mine, theirs);
                // A null `other`, or one of another type, is not this reference.
                let is_null = body.is_null(params[1]);
                let not_null = body.builder.ins().bxor_imm_u(is_null, 1);
                let candidate = body.builder.ins().band(not_null, same_type);
                let compare = body.builder.create_block();
                let done = body.builder.create_block();
                body.builder.append_block_param(done, types::I8);
                body.builder
                    .ins()
                    .brif(candidate, compare, &[], done, &[no.into()]);

                body.continue_in(compare);
                let answer = match receiver_offset {
                    // Unbound: the type alone settles it.
                    None => body.builder.ins().iconst(types::I8, 1),
                    Some(offset) => {
                        let flags = objects::trusted();
                        let mine =
                            body.builder
                                .ins()
                                .load(types::I64, flags, params[0], offset as i32);
                        let theirs =
                            body.builder
                                .ins()
                                .load(types::I64, flags, params[1], offset as i32);
                        body.runtime_call(
                            "kt_equals",
                            &[any(), any()],
                            Ty::Boolean,
                            &[mine, theirs],
                        )?
                        .expect("`kt_equals` returns a Boolean")
                    }
                };
                body.builder.ins().jump(done, &[answer.into()]);

                body.continue_in(done);
                let answer = body.builder.block_params(done)[0];
                body.builder.ins().return_(&[answer]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `get(self, receiver)`: the property's value, as a reference.
    ///
    /// Which receiver is used is the site's, not the caller's: a bound reference reads the one it
    /// stored and ignores what it is passed, which is what `KProperty0.get()` passing nothing
    /// means. An unbound member reference reads its argument.
    fn define_reference_get(
        &mut self,
        id: FuncId,
        base: &str,
        site: &Site,
        receiver_offset: Option<u32>,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any()], any())?;
        let (access, ty, name) = (site.access, site.ty, site.name.clone());
        self.emit_function(
            id,
            signature,
            Carrier::Ref,
            &format!("{base}_get"),
            &mut |body, params| {
                let value = match access {
                    Access::TopLevel => body.top_level_read(&name)?,
                    Access::Member { class, index } => {
                        let object = receiver(body, params, receiver_offset);
                        body.null_check(object)?;
                        body.property_read_of(class, index, object)?
                    }
                    Access::Accessor {
                        getter,
                        receiver: takes_receiver,
                        ..
                    } => {
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        body.accessor_of(getter, object, None)?
                    }
                };
                let answer = match value {
                    Some(value) => body
                        .convert(value, Some(ty), any())?
                        .expect("a reference carrier"),
                    None => body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton"),
                };
                body.builder.ins().return_(&[answer]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `set(self, receiver, value)`.
    fn define_reference_set(
        &mut self,
        id: FuncId,
        base: &str,
        site: &Site,
        receiver_offset: Option<u32>,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any(), any(), any()], Ty::Unit)?;
        let (access, name) = (site.access, site.name.clone());
        self.emit_function(
            id,
            signature,
            Carrier::Void,
            &format!("{base}_set"),
            &mut |body, params| {
                match access {
                    Access::TopLevel => {
                        let ty = body.top_level_written_ty(&name)?;
                        let Some(value) = body.convert(params[2], Some(any()), ty)? else {
                            return Err(format!("a `Unit` value assigned to `{name}`"));
                        };
                        body.top_level_write_value(&name, value)?;
                    }
                    Access::Member { class, index } => {
                        let ty = body.written_property_ty(class, index)?;
                        let object = receiver(body, params, receiver_offset);
                        body.null_check(object)?;
                        let Some(value) = body.convert(params[2], Some(any()), ty)? else {
                            return Err(format!("a `Unit` value assigned to `{name}`"));
                        };
                        body.property_write_of(class, index, object, value)?;
                    }
                    Access::Accessor {
                        setter,
                        receiver: takes_receiver,
                        ..
                    } => {
                        let setter =
                            setter.ok_or_else(|| format!("a write to the read-only `{name}`"))?;
                        let object =
                            takes_receiver.then(|| receiver(body, params, receiver_offset));
                        body.accessor_of(setter, object, Some(params[2]))?;
                    }
                }
                body.builder.ins().return_(&[]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// `name(self)`: the property's Kotlin name.
    fn define_reference_name(
        &mut self,
        id: FuncId,
        base: &str,
        property: &str,
    ) -> Result<(), Unsupported> {
        let signature = self.signature_of(&[any()], any())?;
        let text = property.as_bytes().to_vec();
        self.emit_function(
            id,
            signature,
            Carrier::Ref,
            &format!("{base}_name"),
            &mut |body, _| {
                let value = body.string_literal(&text)?;
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            },
        )
    }
}

/// The receiver a site's `get`/`set` reads: the bound one out of the object, else the argument.
fn receiver(body: &mut BodyLowering<'_, '_, '_>, params: &[Value], offset: Option<u32>) -> Value {
    match offset {
        Some(offset) => {
            body.builder
                .ins()
                .load(types::I64, objects::trusted(), params[0], offset as i32)
        }
        None => params[1],
    }
}

impl BodyLowering<'_, '_, '_> {
    /// The `KProperty` metadata of a local delegated property: one object, for its name.
    pub(super) fn local_property_reference(
        &mut self,
        id: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(items) = self.file.references.get(&id) else {
            return Err("`LocalPropertyReference`".to_string());
        };
        let instance = items
            .singleton
            .expect("a local property reference has one instance");
        Ok(Some(self.data_address(instance)))
    }

    /// `::foo`, `C::p`, `x::p` — the reference object itself.
    pub(super) fn property_reference(&mut self, id: u32) -> Result<Option<Value>, Unsupported> {
        let Some(items) = self.file.references.get(&id) else {
            return Err("`Checked(PropertyReference)`".to_string());
        };
        let (descriptor, size, receiver_offset, singleton) = (
            items.descriptor,
            items.instance_size,
            items.receiver_offset,
            items.singleton,
        );
        if let Some(instance) = singleton {
            // No receiver to hold: one object for the program, which is also what makes
            // `::foo == ::foo` answer true through identity equality.
            return Ok(Some(self.data_address(instance)));
        }
        let Some((_, bound, _)) = reference_site(self.file.ir.expr(id)) else {
            return Err("`Checked(PropertyReference)`".to_string());
        };
        let bound = bound.expect("a site with no singleton binds a receiver");
        let object = self.reference(bound)?;
        if self.terminated {
            return Ok(None);
        }
        let instance = self.allocate(descriptor, size)?;
        let offset = receiver_offset.expect("a bound site stores its receiver");
        self.builder
            .ins()
            .store(objects::trusted(), object, instance, offset as i32);
        Ok(Some(instance))
    }

    /// A member of a property reference, answered through the object's own table.
    ///
    /// Returns `None` when the receiver is not one of these objects, so the caller falls through to
    /// the ordinary dependency-member path. The RECEIVER's type decides, never the call's owner:
    /// `name` is declared on `KCallable`, which every callable reference wears.
    pub(super) fn reference_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !self.type_of(receiver).is_some_and(is_property_reference) {
            return None;
        }
        let slot = match (name, args.len()) {
            ("get" | "invoke", 0 | 1) => GET,
            ("set", 1 | 2) => SET,
            // The PROPERTY's name. `get`/`set`/`invoke` above are methods and keep theirs; `name`
            // is a property of `KCallable`, and `getName` was only the accessor it is realized as.
            ("name", 0) => NAME,
            _ => return None,
        };
        Some(self.reference_call(slot, receiver, args, ret))
    }

    /// Call a top-level extension accessor: the receiver, then the written value for a setter.
    ///
    /// Everything crosses this object's members as a reference, so each operand is converted to the
    /// type the accessor declares before the call rather than handed over as a box.
    pub(super) fn accessor_of(
        &mut self,
        accessor: FunId,
        object: Option<Value>,
        value: Option<Value>,
    ) -> Result<Option<Value>, Unsupported> {
        let params = self.file.ir.functions[accessor as usize].params.clone();
        let mut arguments = Vec::with_capacity(params.len());
        for (operand, ty) in object.into_iter().chain(value).zip(&params) {
            let Some(argument) = self.convert(operand, Some(any()), *ty)? else {
                return Err("a `Unit` operand of a property accessor".to_string());
            };
            arguments.push(argument);
        }
        if arguments.len() != params.len() {
            return Err(format!(
                "a property accessor taking {} operands for {} parameters",
                arguments.len(),
                params.len()
            ));
        }
        let id = self.file.functions[accessor as usize]
            .ok_or_else(|| "an extension accessor with no body".to_string())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// `::foo.name`, for a reference the program wrote HERE.
    ///
    /// `KCallable.name` answers the declaration's own name, and a reference node names its
    /// declaration — so the answer is known at compile time and no reflection metadata has to
    /// exist for it. Returns `None` unless the property really is `KCallable.name` and the receiver
    /// really is such a reference; anything else is somebody else's read.
    pub(super) fn callable_reference_name(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        let IrExpr::CallableReference(reference) = self.file.ir.expr(receiver) else {
            return None;
        };
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        if !super::super::super::intrinsics::is_callable_name(getter.callable.owner, &property.name)
        {
            return None;
        }
        match &reference.target {
            crate::ir::IrCallableReferenceTarget::Local { name, .. } => Some(name.to_string()),
            // Kotlin calls a constructor reference `<init>`, which is the name it answers too.
            crate::ir::IrCallableReferenceTarget::Constructor { .. } => Some("<init>".to_string()),
            // The declaration name is kept beside the call precisely so a reference's identity does
            // not depend on whatever the emitted method ended up being called.
            crate::ir::IrCallableReferenceTarget::Module(id) => self
                .file
                .ir
                .referenced_module_callables
                .get(id)
                .map(|callable| callable.name.to_string()),
        }
    }

    /// The name, with the receiver still EVALUATED: `(state++)::toString.name` answers a constant
    /// and increments, and a program can see the second part.
    pub(super) fn callable_name(
        &mut self,
        receiver: u32,
        name: &str,
    ) -> Result<Option<Value>, Unsupported> {
        self.expression(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        Ok(Some(self.string_literal(name.as_bytes())?))
    }

    /// `p.name` — a checked read of a dependency property whose receiver is a reference.
    ///
    /// Returns `None` when the receiver is not one, so the caller falls through; the name is
    /// resolved through the getter the read names, exactly as an explicit call to it would be.
    pub(super) fn reference_property(
        &mut self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !self.type_of(receiver).is_some_and(is_property_reference) {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        let ret = getter.callable.ret;
        self.reference_member(&property.name, receiver, &[], ret)
    }

    /// `p.getValue(thisRef, property)` and `p.setValue(thisRef, property, value)`.
    ///
    /// `val x by ::top` delegates to the property reference itself, through two stdlib operators
    /// that are `inline` one-liners: `getValue` is `get()` on a `KProperty0` and `get(thisRef)` on
    /// a `KProperty1`, and `setValue` the same for `set`. A dependency `inline` body is not here to
    /// splice, so they are realized rather than inlined — which of the two a site means is the
    /// RECEIVER's type, since that is what selected the overload in the first place.
    ///
    /// `property` is the metadata object, which these operators ignore; it is still evaluated,
    /// because a program that can see the difference is entitled to.
    pub(super) fn reference_delegate(
        &mut self,
        owner: &str,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_property_delegates_facade(owner) {
            return None;
        }
        let takes_receiver = self.type_of(receiver).and_then(reference_receiver)?;
        // Which operands reach the slot: a `KProperty1` is handed `thisRef`, a `KProperty0` is not,
        // and the written value is always the last one.
        let kept: Vec<usize> = match (name, args.len()) {
            ("getValue", 2) => takes_receiver.then_some(0).into_iter().collect(),
            ("setValue", 3) => takes_receiver.then_some(0).into_iter().chain([2]).collect(),
            _ => return None,
        };
        let slot = if name == "setValue" { SET } else { GET };
        Some(self.delegate_call(slot, receiver, args, &kept, ret))
    }

    fn delegate_call(
        &mut self,
        slot: u32,
        receiver: u32,
        args: &[u32],
        kept: &[usize],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        // Every argument is evaluated, in source order, whether or not the slot is handed it.
        let mut evaluated = Vec::with_capacity(args.len());
        for argument in args {
            evaluated.push(self.reference(*argument)?);
        }
        if self.terminated {
            return Ok(None);
        }
        let mut arguments: Vec<Value> = kept.iter().map(|index| evaluated[*index]).collect();
        // A bound reference's slot ignores the receiver operand; a null keeps one signature for
        // both shapes, exactly as `reference_call` does.
        let expected = usize::from(slot == SET) + 1;
        while arguments.len() < expected {
            arguments.insert(0, self.builder.ins().iconst(types::I64, 0));
        }
        let params = vec![any(); arguments.len()];
        let answer = self.dispatch(
            object,
            slot,
            &params,
            if slot == SET { Ty::Unit } else { any() },
            &arguments,
        )?;
        let Some(answer) = answer else {
            return Ok(None);
        };
        self.convert(answer, Some(any()), ret)
    }

    fn reference_call(
        &mut self,
        slot: u32,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        // Every operand crosses as a reference, which is what the emitted bodies take: the
        // receiver a member reference is asked about, and the value a `set` stores.
        let mut arguments = Vec::with_capacity(3);
        for argument in args {
            arguments.push(self.reference(*argument)?);
        }
        if self.terminated {
            return Ok(None);
        }
        // `get()` on a BOUND reference passes no receiver and the body ignores the slot anyway; a
        // null keeps one signature for both shapes.
        let expected = usize::from(slot == SET) + 1;
        while arguments.len() < expected {
            arguments.insert(0, self.builder.ins().iconst(types::I64, 0));
        }
        let params = vec![any(); arguments.len()];
        let answer = self.dispatch(
            object,
            slot,
            &params,
            if slot == SET { Ty::Unit } else { any() },
            &arguments,
        )?;
        let Some(answer) = answer else {
            return Ok(None);
        };
        // The declared result is what the program reads: an `Int` property answers a boxed `Int`
        // here and is unboxed back at the call site.
        self.convert(answer, Some(any()), ret)
    }
}

/// Whether a reference of this type is HANDED the receiver its `get` reads through.
///
/// `KProperty1` is — it names a member and the call supplies the object — while a `KProperty0`
/// carries its own or needs none. `None` for anything else, including the arity-less `KProperty`
/// and `KCallable`: those say nothing about how many receivers a call passes, and a delegate
/// operator's overload was selected on exactly that.
fn reference_receiver(ty: Ty) -> Option<bool> {
    let internal = ty.non_null().obj_internal()?;
    [
        ("kotlin/reflect/KProperty0", false),
        ("kotlin/reflect/KProperty1", true),
        ("kotlin/reflect/KMutableProperty0", false),
        ("kotlin/reflect/KMutableProperty1", true),
    ]
    .iter()
    .find(|(candidate, _)| internal.matches(candidate))
    .map(|(_, takes)| *takes)
}

/// Whether a type is one of the reflection types a property reference wears.
///
/// Spelled out rather than matched by prefix: the set is closed — Kotlin declares these nine and no
/// more — and a prefix would also claim any future or unrelated name that happens to begin the same
/// way, which is how a member of something else ends up dispatched through these slots.
fn is_property_reference(ty: Ty) -> bool {
    let Some(internal) = ty.non_null().obj_internal() else {
        return false;
    };
    [
        "kotlin/reflect/KCallable",
        "kotlin/reflect/KProperty",
        "kotlin/reflect/KProperty0",
        "kotlin/reflect/KProperty1",
        "kotlin/reflect/KProperty2",
        "kotlin/reflect/KMutableProperty",
        "kotlin/reflect/KMutableProperty0",
        "kotlin/reflect/KMutableProperty1",
        "kotlin/reflect/KMutableProperty2",
    ]
    .iter()
    .any(|candidate| internal.matches(candidate))
}
