//! Objects: construction, fields, dispatch, `is`/`as`, and the functions every class needs
//! beyond its methods — its constructors, the accessors its table synthesizes, and an `object`'s
//! lazily built instance.
//!
//! A value of a class type is carried as `(ref null eq)` (see `super::super::super::objects`), so
//! reading a field or a dispatch table first casts it: to the class's struct for a field, to
//! `$Object` for its table. The frontend proved the value is of the class; the cast is what tells
//! the validator.

use crate::backend::class_tables::{function_key, local_property_target, AnyMember, Slot, SlotKey};
use crate::fir::{PropertyId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::ir::{
    Callee, ClassId, CtorDelegateTarget, FunId, IrLocalPropertyLayout, IrSuperCallKind, IrTypeOp,
    IrVirtualTarget,
};
use crate::types::{Ty, TyKind};

use super::super::super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::super::super::objects::REFERENCE;
use super::super::super::runtime::{Runtime, EQZ32, OR32};
use super::super::classes::{Accessor, FileClasses};
use super::super::property_registry::{MemberProperty, MemberPropertyDefinition, TopLevelProperty};
use super::{carrier, Body, FileContext, Unsupported, STRING};

/// The heap type of the runtime's `String`, type 1 (see `STRING`).
const STRING_HEAP: HeapType = HeapType::Concrete(1);

impl Body<'_> {
    fn file_classes(&self) -> Result<&FileClasses, Unsupported> {
        self.classes
            .ok_or_else(|| "an object of a class declared in another file".to_string())
    }

    /// The class of this file a type names, if it names one.
    pub(super) fn class_of_type(&self, ty: Ty) -> Option<ClassId> {
        self.classes?;
        match ty.non_null().kind() {
            TyKind::Obj(name, _) => self.ir.class_id_by_name(name),
            _ => None,
        }
    }

    /// The type `class`'s instances have.
    fn object_type(&self, class: ClassId) -> Ty {
        Ty::obj_name(self.ir.classes[class as usize].fq_name)
    }

    /// Push `id`, a value of a class type, as `class`'s struct.
    fn object(&mut self, id: u32, class: ClassId) -> Result<u32, Unsupported> {
        let structure = self.file_classes()?.structure(class);
        self.value(id, self.object_type(class))?;
        self.code.ref_cast(false, HeapType::Concrete(structure));
        Ok(structure)
    }

    pub(super) fn field_read(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
    ) -> Result<(), Unsupported> {
        let structure = self.object(receiver, class)?;
        let field = self.file_classes()?.field(class, index);
        self.code.struct_get(structure, field);
        Ok(())
    }

    pub(super) fn field_write(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let structure = self.object(receiver, class)?;
        let ty = self.ir.classes[class as usize].fields[index as usize].ty;
        self.value(value, ty)?;
        let field = self.file_classes()?.field(class, index);
        self.code.struct_set(structure, field);
        Ok(())
    }

    /// `C(args)`: a new instance of `C`, then the constructor `selected` names run on it.
    pub(super) fn construction(
        &mut self,
        internal: crate::types::TypeName,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Result<(), Unsupported> {
        let Some(class) = self.classes.and(self.ir.class_id_by_name(internal)) else {
            // Boundary conversion for a diagnostic.
            return Err(format!(
                "construction of the library class `{}`",
                internal.render().replace('/', ".")
            ));
        };
        let declaration = &self.ir.classes[class as usize];
        let name = declaration.fq_name();
        if declaration.is_interface || declaration.is_abstract || declaration.is_sealed {
            return Err(format!("construction of the abstract class `{name}`"));
        }
        if declaration.is_object || declaration.is_companion {
            return Err(format!("construction of the object declaration `{name}`"));
        }
        let form = &self.file_classes()?.forms[class as usize];
        let primary: Vec<Ty> = declaration.ctor_args.iter().map(|arg| arg.ty).collect();
        let (constructor, params) = match selected {
            None => (form.constructor, primary),
            Some(selected) => match declaration.secondary_ctors.iter().position(|candidate| {
                candidate.params == selected || {
                    let mut physical = candidate.prefix_params.clone();
                    physical.extend(candidate.params.iter().copied());
                    !candidate.prefix_params.is_empty() && physical == selected
                }
            }) {
                Some(ordinal) => {
                    let secondary = &declaration.secondary_ctors[ordinal];
                    let mut params = secondary.prefix_params.clone();
                    params.extend(secondary.params.iter().copied());
                    (Some(form.secondaries[ordinal]), params)
                }
                None if primary == selected => (form.constructor, primary),
                None => return Err(format!("a call to an unknown constructor of `{name}`")),
            },
        };
        let constructor =
            constructor.ok_or_else(|| format!("a primary constructor `{name}` lacks"))?;
        if params.len() != args.len() {
            return Err(format!(
                "a constructor call with omitted arguments (`{name}`)"
            ));
        }
        let object = self.local(REFERENCE);
        self.allocate(class)?;
        self.code.local_tee(object);
        for (arg, param) in args.iter().zip(params) {
            self.value(*arg, param)?;
        }
        self.code.call(constructor).local_get(object);
        Ok(())
    }

    /// Push a new instance of `class` with every field at its default value.
    fn allocate(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let classes = self.file_classes()?;
        let form = &classes.forms[class as usize];
        let table = form
            .table
            .ok_or("an instance of a class with no dispatch table")?;
        let structure = classes.structure(class);
        let mut chain = vec![class];
        while let Some(parent) = classes
            .tables
            .table(*chain.last().expect("non-empty"))
            .superclass
        {
            chain.push(parent);
        }
        self.code.global_get(table);
        for &owner in chain.iter().rev() {
            for field in &self.ir.classes[owner as usize].fields {
                let value = carrier(field.ty)?.ok_or("a `Unit`-typed field")?;
                default_value(&mut self.code, value);
            }
        }
        self.code.struct_new(structure);
        Ok(())
    }

    /// The instance of an `object` declaration, built on first use.
    pub(super) fn singleton(
        &mut self,
        classifier: crate::types::TypeName,
    ) -> Result<(), Unsupported> {
        let singleton = self
            .classes
            .and(self.ir.class_id_by_name(classifier))
            .and_then(|class| self.classes?.forms[class as usize].singleton)
            .ok_or_else(|| {
                // Boundary conversion for a diagnostic.
                format!(
                    "the library object `{}`",
                    classifier.render().replace('/', ".")
                )
            })?;
        self.code.call(singleton.getter);
        Ok(())
    }

    /// `receiver.f(args)` through the slot of `class`'s method `index`.
    pub(super) fn method_call(
        &mut self,
        class: ClassId,
        index: u32,
        receiver: u32,
        args: &[Option<u32>],
    ) -> Result<(), Unsupported> {
        let function = self.ir.classes[class as usize].methods[index as usize];
        let Some(args) = args.iter().copied().collect::<Option<Vec<u32>>>() else {
            return Err(format!(
                "a call to `{}` with omitted arguments",
                self.ir.functions[function as usize].name
            ));
        };
        self.dispatch_function(class, function, receiver, &args)
    }

    fn dispatch_function(
        &mut self,
        class: ClassId,
        function: FunId,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let key = function_key(self.ir, class, function);
        let slot = self
            .file_classes()?
            .tables
            .slot(class, &key)
            .ok_or_else(|| {
                format!(
                    "a method with no dispatch slot (`{}`)",
                    self.ir.functions[function as usize].name
                )
            })?;
        let params = self.ir.functions[function as usize].params.clone();
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        self.dispatch(class, slot, receiver, args, &params)
    }

    /// Dispatch a member-extension property's recorded accessor. Its extension receiver and any
    /// context parameters are ordinary accessor arguments in common IR; the dispatch receiver is
    /// the separate object whose table owns the slot.
    fn dispatch_member_extension_property(
        &mut self,
        class: ClassId,
        property: PropertyId,
        setter: bool,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let function = match self.ir.local_property_layouts.get(&property) {
            Some(IrLocalPropertyLayout::MemberExtension {
                getter,
                setter: property_setter,
                ..
            }) => {
                if setter {
                    *property_setter
                } else {
                    Some(*getter)
                }
            }
            _ => None,
        }
        .ok_or_else(|| "a member-extension property call with no recorded accessor".to_string())?;
        let params = self.ir.functions[function as usize].params.clone();
        if params.len() != args.len() {
            return Err("a property call whose arguments do not match its accessor".into());
        }
        let target = ResolvedPropertyOverrideTarget::Module(property);
        let key = if setter {
            SlotKey::Setter(target)
        } else {
            SlotKey::Getter(target)
        };
        let slot = self
            .file_classes()?
            .tables
            .slot(class, &key)
            .ok_or_else(|| "a member-extension property with no dispatch slot".to_string())?;
        self.dispatch(class, slot, receiver, args, &params)
    }

    /// A call through `slot` of the table of `receiver`, statically of `class`.
    fn dispatch(
        &mut self,
        class: ClassId,
        slot: u32,
        receiver: u32,
        args: &[u32],
        params: &[Ty],
    ) -> Result<(), Unsupported> {
        let classes = self.file_classes()?;
        let ty = classes
            .slot_type(class, slot)
            .ok_or("a call through a slot with no signature")?;
        let table_type = match classes.forms[class as usize].table_type {
            Some(table_type) => table_type,
            None => classes.file_table,
        };
        let this = self.local(REFERENCE);
        self.value(receiver, self.object_type(class))?;
        self.code.local_tee(this);
        for (arg, param) in args.iter().zip(params) {
            self.value(*arg, *param)?;
        }
        self.code.local_get(this);
        self.table_of_object();
        self.code
            .ref_cast(false, HeapType::Concrete(table_type))
            .struct_get(table_type, slot)
            .call_ref(ty);
        Ok(())
    }

    /// Replace the object on the stack with its dispatch table, as `$AnyTable`.
    fn table_of_object(&mut self) {
        let object = self.runtime.objects.object;
        self.code
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0);
    }

    /// A call to member `function` of this file, without dispatch.
    pub(super) fn direct_member_call(
        &mut self,
        function: FunId,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let declaration = &self.ir.functions[function as usize];
        let owner = declaration
            .dispatch_receiver
            .ok_or("a member call to a function without a receiver")?;
        if declaration.body.is_none() {
            return Err(format!(
                "a direct call to the abstract `{}`",
                declaration.name
            ));
        }
        let params = declaration.params.clone();
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        self.value(receiver, Ty::obj_name(owner))?;
        for (arg, param) in args.iter().zip(params) {
            self.value(*arg, param)?;
        }
        let function = self.emitted_function(function)?;
        self.code.call(function);
        Ok(())
    }

    /// A member call the frontend selected: a virtual one, or a `super` one.
    pub(super) fn member_call(
        &mut self,
        callee: &Callee,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        match callee {
            Callee::Virtual {
                owner,
                target: Some(target),
                ..
            } => {
                let class = self
                    .classes
                    .and(self.ir.class_id_by_name(*owner))
                    .ok_or("a call to a library member")?;
                match target {
                    IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::Module(callable)) => {
                        let function = *self
                            .ir
                            .checked_callable_functions
                            .get(callable)
                            .ok_or("a call to a member of another file")?;
                        self.dispatch_function(class, function, receiver, args)
                    }
                    IrVirtualTarget::PropertyGetter(ResolvedPropertyOverrideTarget::Module(
                        property,
                    )) => match self.ir.local_property_layouts.get(property) {
                        Some(IrLocalPropertyLayout::MemberExtension { .. }) => self
                            .dispatch_member_extension_property(
                                class, *property, false, receiver, args,
                            ),
                        _ if args.is_empty() => self.property_read(*property, Some(receiver)),
                        _ => Err("a property getter call with arguments".to_string()),
                    },
                    IrVirtualTarget::PropertySetter(ResolvedPropertyOverrideTarget::Module(
                        property,
                    )) => match self.ir.local_property_layouts.get(property) {
                        Some(IrLocalPropertyLayout::MemberExtension { .. }) => self
                            .dispatch_member_extension_property(
                                class, *property, true, receiver, args,
                            ),
                        _ if args.len() == 1 => {
                            self.property_write(*property, Some(receiver), args[0])
                        }
                        _ => Err("a property setter call with the wrong arguments".to_string()),
                    },
                    _ => Err("a call to a library member".to_string()),
                }
            }
            Callee::Super {
                kind: IrSuperCallKind::Function,
                declaration: Some(ResolvedFunctionOverrideTarget::Module(callable)),
                ..
            } => {
                let function = *self
                    .ir
                    .checked_callable_functions
                    .get(callable)
                    .ok_or("a `super` call to a member of another file")?;
                self.direct_member_call(function, receiver, args)
            }
            Callee::Super { .. } => Err("a `super` call to a library member".to_string()),
            _ => Err("a member call through this calling convention".to_string()),
        }
    }

    /// The class property a checked property identity names, and its index there.
    fn member_property(&self, target: PropertyId) -> Result<(ClassId, usize), Unsupported> {
        match self.ir.local_property_layouts.get(&target) {
            Some(IrLocalPropertyLayout::Member {
                class, property, ..
            }) if self.classes.is_some() => Ok((*class, *property as usize)),
            Some(_) => Err("a property that is not a class member".to_string()),
            None => Err("a property of another file".to_string()),
        }
    }

    /// A member read common realization left for this backend (`receiver.p`).
    pub(super) fn realized_property_read(
        &mut self,
        operation: u32,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let target = self.realized_property(operation)?;
        self.property_read(target, Some(receiver))
    }

    /// A member write common realization left for this backend (`receiver.p = value`).
    pub(super) fn realized_property_write(
        &mut self,
        operation: u32,
        receiver: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let target = self.realized_property(operation)?;
        self.property_write(target, Some(receiver), value)
    }

    fn realized_property(&self, operation: u32) -> Result<PropertyId, Unsupported> {
        self.file_classes()?
            .realized_property(operation)
            .ok_or_else(|| "a member property access with no checked property".to_string())
    }

    pub(super) fn property_read(
        &mut self,
        target: PropertyId,
        receiver: Option<u32>,
    ) -> Result<(), Unsupported> {
        if receiver.is_none() {
            if let Some(storage) = self.top_level_property(target)? {
                if !storage.direct_read {
                    return Err("a top-level property read that requires an accessor".to_string());
                }
                self.code.global_get(storage.global);
                return Ok(());
            }
        }
        if !self.ir.local_property_layouts.contains_key(&target) {
            if let Some(entry) = self.module_member_property(target)? {
                let receiver = receiver.ok_or_else(|| {
                    "a receiver-less read of a cross-file member property".to_string()
                })?;
                self.value(receiver, Ty::obj_name(entry.owner))?;
                self.code.call(entry.getter);
                return Ok(());
            }
        }
        let (class, index) = self.member_property(target)?;
        let property = self.ir.classes[class as usize].properties[index].clone();
        let receiver =
            receiver.ok_or_else(|| format!("a receiver-less read of `{}`", property.name))?;
        if property
            .storage_ty
            .is_some_and(|storage| storage != property.ty)
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                property.name
            ));
        }
        let slot = local_property_target(self.ir, class, index).and_then(|target| {
            self.file_classes()
                .ok()?
                .tables
                .slot(class, &SlotKey::Getter(target))
        });
        if let Some(slot) = slot {
            return self.dispatch(class, slot, receiver, &[], &[]);
        }
        if let Some(getter) = property.getter {
            return self.direct_member_call(getter, receiver, &[]);
        }
        match property.backing_field {
            Some(field) => self.field_read(receiver, class, field),
            None => Err(format!(
                "a property with neither storage nor a getter (`{}`)",
                property.name
            )),
        }
    }

    pub(super) fn property_write(
        &mut self,
        target: PropertyId,
        receiver: Option<u32>,
        value: u32,
    ) -> Result<(), Unsupported> {
        if receiver.is_none() {
            if let Some(storage) = self.top_level_property(target)? {
                if !storage.mutable {
                    return Err("an assignment to an immutable top-level property".to_string());
                }
                if !storage.direct_write {
                    return Err("a top-level property write that requires an accessor".to_string());
                }
                self.value(value, storage.ty)?;
                self.code.global_set(storage.global);
                return Ok(());
            }
        }
        if !self.ir.local_property_layouts.contains_key(&target) {
            if let Some(entry) = self.module_member_property(target)? {
                let receiver = receiver.ok_or_else(|| {
                    "a receiver-less write of a cross-file member property".to_string()
                })?;
                let setter = entry
                    .setter
                    .ok_or_else(|| "a read-only cross-file property was written".to_string())?;
                self.value(receiver, Ty::obj_name(entry.owner))?;
                self.value(value, entry.ty)?;
                self.code.call(setter);
                return Ok(());
            }
        }
        let (class, index) = self.member_property(target)?;
        let property = self.ir.classes[class as usize].properties[index].clone();
        let receiver =
            receiver.ok_or_else(|| format!("a receiver-less write of `{}`", property.name))?;
        if property
            .storage_ty
            .is_some_and(|storage| storage != property.ty)
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                property.name
            ));
        }
        let slot = local_property_target(self.ir, class, index).and_then(|target| {
            self.file_classes()
                .ok()?
                .tables
                .slot(class, &SlotKey::Setter(target))
        });
        if let Some(slot) = slot {
            return self.dispatch(class, slot, receiver, &[value], &[property.ty]);
        }
        if let Some(setter) = property.setter {
            return self.direct_member_call(setter, receiver, &[value]);
        }
        match property.backing_field {
            Some(field) => self.field_write(receiver, class, field, value),
            None => Err(format!(
                "a property with neither storage nor a setter (`{}`)",
                property.name
            )),
        }
    }

    /// The program-wide entry reserved for an ordinary member property another source file owns.
    /// The local module record and registry entry were produced from the same checked declaration;
    /// validate that exact join rather than reconstructing an accessor from its spelling.
    fn module_member_property(
        &self,
        target: PropertyId,
    ) -> Result<Option<MemberProperty>, Unsupported> {
        let Some(entry) = self.properties.member(target) else {
            return Ok(None);
        };
        let declaration = self
            .ir
            .referenced_module_properties
            .get(&target)
            .ok_or_else(|| "a cross-file member property with no module record".to_string())?;
        if declaration.owner != Some(entry.owner)
            || declaration.ty != entry.ty
            || declaration.mutable != entry.setter.is_some()
            || declaration.extension_receiver.is_some()
            || !declaration.context_parameters.is_empty()
        {
            return Err("a cross-file member property disagrees with its module record".into());
        }
        Ok(Some(entry))
    }

    /// The storage selected for a receiver-less property in this or an earlier source file.
    /// Property identity, never spelling, is the cross-file key.
    fn top_level_property(
        &self,
        target: PropertyId,
    ) -> Result<Option<TopLevelProperty>, Unsupported> {
        if let Some(layout) = self.ir.local_property_layouts.get(&target) {
            return match layout {
                IrLocalPropertyLayout::TopLevelStorage {
                    storage,
                    qualifier: None,
                    ..
                } => {
                    let declaration = self
                        .ir
                        .statics
                        .get(*storage as usize)
                        .ok_or_else(|| "an invalid top-level property storage".to_string())?;
                    let global = self
                        .globals
                        .get(*storage as usize)
                        .copied()
                        .ok_or_else(|| "an invalid top-level property global".to_string())?;
                    Ok(Some(TopLevelProperty {
                        global,
                        ty: declaration.ty,
                        mutable: declaration.is_var,
                        direct_read: declaration.accessors.getter
                            != crate::ir::IrStaticAccessor::Declared,
                        direct_write: declaration.is_var
                            && declaration.accessors.setter
                                != crate::ir::IrStaticAccessor::Declared,
                    }))
                }
                _ => Ok(None),
            };
        }
        let Some(storage) = self.properties.get(target) else {
            return Ok(None);
        };
        let reference = self
            .ir
            .referenced_module_properties
            .get(&target)
            .ok_or_else(|| "a cross-file property with no module record".to_string())?;
        if reference.owner.is_some()
            || reference.extension_receiver.is_some()
            || !reference.context_parameters.is_empty()
        {
            return Err("a cross-file property storage with a receiver".to_string());
        }
        if reference.ty != storage.ty || reference.mutable != storage.mutable {
            return Err(
                "a cross-file property storage disagrees with its module record".to_string(),
            );
        }
        Ok(Some(storage))
    }

    /// `x as C`: the value, cast to class `C` of this file.
    pub(super) fn cast(&mut self, arg: u32, target: Ty, non_null: bool) -> Result<(), Unsupported> {
        let class = self.class_of_type(target).expect("checked by the caller");
        self.value(arg, any())?;
        if self.ir.classes[class as usize].is_interface {
            let value = self.local(REFERENCE);
            self.code.local_tee(value);
            self.instance_test(class, !non_null)?;
            self.code
                .if_(BlockType::Value(REFERENCE))
                .local_get(value)
                .else_()
                .unreachable()
                .end();
        } else {
            let structure = self.file_classes()?.structure(class);
            self.code.ref_cast(!non_null, HeapType::Concrete(structure));
        }
        Ok(())
    }

    /// `is`, `!is` and `as?`.
    pub(super) fn type_test(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        target: Ty,
    ) -> Result<(), Unsupported> {
        if !matches!(self.carrier_of(arg)?, Some(ValType::Ref { .. }) | None) {
            return Err(format!(
                "a type test of a `{}` value",
                super::describe(self.type_of(arg)?.unwrap_or(Ty::Unit))
            ));
        }
        let nullable = matches!(target.kind(), TyKind::Nullable(_)) && op != IrTypeOp::SafeCast;
        let tested = target.non_null();
        let value = self.local(REFERENCE);
        self.value(arg, any())?;
        self.code.local_tee(value);
        match tested.kind() {
            TyKind::STRING => {
                self.code.ref_test(nullable, STRING_HEAP);
            }
            TyKind::Obj(name, _) if name == crate::types::wk::any() => {
                self.code.ref_test(nullable, HeapType::Eq);
            }
            _ => match self.class_of_type(tested) {
                Some(class) => self.instance_test(class, nullable)?,
                None => {
                    return Err(format!("a type test against `{}`", super::describe(tested)));
                }
            },
        }
        match op {
            IrTypeOp::NotInstanceOf => {
                self.code.op(EQZ32);
            }
            IrTypeOp::SafeCast => {
                let (result, cast) = if tested == Ty::String {
                    (STRING, Some(STRING_HEAP))
                } else {
                    (REFERENCE, None)
                };
                self.code.if_(BlockType::Value(result)).local_get(value);
                if let Some(heap) = cast {
                    self.code.ref_cast(true, heap);
                }
                self.code.else_().ref_null(HeapType::None).end();
            }
            _ => {}
        }
        Ok(())
    }

    /// Replace the reference on the stack with whether it is an instance of `class` — or null,
    /// when `nullable`.
    fn instance_test(&mut self, class: ClassId, nullable: bool) -> Result<(), Unsupported> {
        let classes = self.file_classes()?;
        if !self.ir.classes[class as usize].is_interface {
            let structure = classes.structure(class);
            self.code.ref_test(nullable, HeapType::Concrete(structure));
            return Ok(());
        }
        // An interface has no struct of its own: an instance of it is an instance of one of the
        // file's classes implementing it. A class from another file cannot implement it, since
        // such a class declines.
        let implementors: Vec<u32> = self
            .ir
            .classes
            .iter()
            .enumerate()
            .filter(|(id, candidate)| {
                !candidate.is_interface && classes.tables.interfaces[*id].contains(&class)
            })
            .map(|(id, _)| classes.structure(id as ClassId))
            .collect();
        let value = self.local(REFERENCE);
        self.code.local_set(value);
        if nullable {
            self.code.local_get(value).ref_is_null();
        } else {
            self.code.i32_const(0);
        }
        for structure in implementors {
            self.code
                .local_get(value)
                .ref_test(false, HeapType::Concrete(structure))
                .op(OR32);
        }
        Ok(())
    }

    /// Whether a call through `member`'s slot on a value statically of `class` could reach
    /// `kotlin.Any`'s own member, which this module has no body for.
    fn reaches_any_default(&self, class: ClassId, member: AnyMember) -> Result<bool, Unsupported> {
        let classes = self.file_classes()?;
        Ok(self.ir.classes.iter().enumerate().any(|(id, candidate)| {
            let id = id as ClassId;
            if candidate.is_interface {
                return false;
            }
            let mut ancestor = Some(id);
            let mut related = classes.tables.interfaces[id as usize].contains(&class);
            while let Some(current) = ancestor {
                related |= current == class;
                ancestor = classes.tables.table(current).superclass;
            }
            related
                && classes.tables.table(id).vtable[member.slot() as usize]
                    == Slot::AnyMember(member)
        }))
    }

    /// `lhs == rhs` on objects: `lhs?.equals(rhs) ?: (rhs === null)`.
    pub(super) fn object_equality(&mut self, lhs: u32, rhs: u32) -> Result<(), Unsupported> {
        // Both operands run once, left first, even when the left value is null. The static
        // type can be Any: the GC representation, not that type, selects the equality body.
        let (left, right) = (self.local(REFERENCE), self.local(REFERENCE));
        self.value(lhs, any())?;
        self.code.local_set(left);
        self.value(rhs, any())?;
        self.code.local_set(right);
        let any_table = self.runtime.objects.any_table;
        let equals = self.runtime.objects.any_slot_types[0];
        self.code
            .local_get(left)
            .ref_is_null()
            .if_(BlockType::Value(ValType::I32))
            .local_get(right)
            .ref_is_null()
            .else_()
            .local_get(left)
            .ref_test(false, STRING_HEAP)
            .if_(BlockType::Value(ValType::I32))
            .local_get(right)
            .ref_test(false, STRING_HEAP)
            .if_(BlockType::Value(ValType::I32))
            .local_get(left)
            .ref_cast(false, STRING_HEAP)
            .local_get(right)
            .ref_cast(false, STRING_HEAP)
            .call(self.runtime.string_equals)
            .else_()
            .i32_const(0)
            .end()
            .else_()
            .local_get(left)
            .ref_test(false, HeapType::Concrete(self.runtime.objects.object))
            .if_(BlockType::Value(ValType::I32))
            .local_get(left)
            .local_get(right)
            .local_get(left);
        self.table_of_object();
        self.code
            .struct_get(any_table, AnyMember::Equals.slot())
            .call_ref(equals)
            .else_()
            // Non-object eq references have no virtual override: their default is identity.
            .local_get(left)
            .local_get(right)
            .ref_eq()
            .end()
            .end()
            .end();
        Ok(())
    }

    /// Replace the object on the stack, statically of type `ty`, with its `toString()`, or
    /// `"null"`.
    pub(super) fn object_to_string(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let class = self.class_of_type(ty).expect("checked by the caller");
        if self.reaches_any_default(class, AnyMember::ToString)? {
            return Err("`kotlin.Any.toString`'s own rendering".to_string());
        }
        let value = self.local(REFERENCE);
        self.code.local_set(value);
        let any_table = self.runtime.objects.any_table;
        let to_string = self.runtime.objects.any_slot_types[2];
        self.code
            .local_get(value)
            .ref_is_null()
            .if_(BlockType::Value(STRING));
        self.runtime.string_constant(
            self.module,
            &mut self.code,
            &"null".encode_utf16().collect::<Vec<_>>(),
        );
        self.code.else_().local_get(value).local_get(value);
        self.table_of_object();
        self.code
            .struct_get(any_table, AnyMember::ToString.slot())
            .call_ref(to_string)
            .end()
            .ref_as_non_null();
        Ok(())
    }
}

/// `kotlin.Any`, the type every class value is read as before its class is tested.
fn any() -> Ty {
    Ty::obj_name(crate::types::wk::any())
}

/// Push the default value of a `value` field.
fn default_value(code: &mut Code, value: ValType) {
    match value {
        ValType::I32 => code.i32_const(0),
        ValType::I64 => code.i64_const(0),
        ValType::F32 => code.f32_const(0.0),
        ValType::F64 => code.f64_const(0.0),
        ValType::Ref { .. } => code.ref_null(HeapType::None),
    };
}

/// Define the functions the file's classes need beyond their methods.
pub(super) fn define_class_functions(
    context: &FileContext<'_>,
    classes: &FileClasses,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    let ir = context.ir;
    for (id, class) in ir.classes.iter().enumerate() {
        let id = id as ClassId;
        let form = &classes.forms[id as usize];
        if let Some(constructor) = form.constructor {
            let mut parameters = vec![Ty::obj_name(class.fq_name)];
            parameters.extend(class.ctor_args.iter().map(|arg| arg.ty));
            let mut declared = crate::fx_hash::HashMap::default();
            for &root in class
                .super_arg_prelude
                .iter()
                .chain(&class.super_args)
                .chain(class.init_body.iter())
            {
                declared.extend(ir.declared_value_slot_types(root));
            }
            let mut body = Body::new(context, module, runtime, &parameters, declared, Ty::Unit);
            body.primary_constructor(id)?;
            let lowered = body.finish();
            define(module, constructor, &parameters, lowered)?;
        }
        for (ordinal, &constructor) in form.secondaries.iter().enumerate() {
            let secondary = &class.secondary_ctors[ordinal];
            let mut parameters = vec![Ty::obj_name(class.fq_name)];
            parameters.extend(secondary.prefix_params.iter().copied());
            parameters.extend(secondary.params.iter().copied());
            let mut declared = crate::fx_hash::HashMap::default();
            for &root in secondary
                .delegate_prelude
                .iter()
                .chain(&secondary.delegate_args)
                .chain(secondary.body.iter())
            {
                declared.extend(ir.declared_value_slot_types(root));
            }
            let mut body = Body::new(context, module, runtime, &parameters, declared, Ty::Unit);
            body.secondary_constructor(id, ordinal)?;
            let lowered = body.finish();
            define(module, constructor, &parameters, lowered)?;
        }
        if let Some(singleton) = form.singleton {
            let constructor = match (form.constructor, class.ctor_args.is_empty()) {
                (Some(constructor), true) => constructor,
                _ => {
                    return Err(format!(
                        "an object declaration with constructor parameters (`{}`)",
                        class.fq_name()
                    ))
                }
            };
            let mut body = Body::new(
                context,
                module,
                runtime,
                &[],
                crate::fx_hash::HashMap::default(),
                Ty::Unit,
            );
            body.code
                .global_get(singleton.global)
                .ref_is_null()
                .if_(BlockType::Empty);
            body.allocate(id)?;
            body.code
                .global_set(singleton.global)
                .global_get(singleton.global)
                .call(constructor)
                .end()
                .global_get(singleton.global);
            let (code, locals) = body.finish();
            let ty = module.func_type(Vec::new(), vec![REFERENCE]);
            module.define(singleton.getter, Function { ty, locals, code });
        }
    }
    for &(accessor, function, ty) in &classes.accessors {
        let Accessor {
            class,
            field,
            setter,
        } = accessor;
        let structure = classes.structure(class);
        let index = classes.field(class, field);
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(structure));
        if setter {
            code.local_get(1).struct_set(structure, index);
        } else {
            code.struct_get(structure, index);
        }
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
    Ok(())
}

/// Define the program-wide entries reserved for ordinary member properties of this source file.
/// A using file calls only these indices; this defining file alone knows the class layout and
/// realizes the property through its virtual slot, declared accessor or exact backing field.
pub(super) fn define_module_property_functions(
    context: &FileContext<'_>,
    classes: &FileClasses,
    definitions: &[MemberPropertyDefinition],
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    for definition in definitions {
        let layout = context
            .ir
            .local_property_layouts
            .get(&definition.property)
            .ok_or_else(|| "a module member property with no local layout".to_string())?;
        let IrLocalPropertyLayout::Member {
            class,
            owner,
            property,
            ty,
            mutable,
            context_parameters,
            ..
        } = layout
        else {
            return Err("a module member property with a non-member layout".to_string());
        };
        if !context_parameters.is_empty()
            || *owner != definition.entry.owner
            || *ty != definition.entry.ty
            || *mutable != definition.entry.setter.is_some()
        {
            return Err("a module member property entry disagrees with its local layout".into());
        }
        let declaration = context
            .ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.properties.get(*property as usize))
            .ok_or_else(|| "a module member property with an invalid class index".to_string())?;
        if declaration
            .storage_ty
            .is_some_and(|storage| storage != declaration.ty)
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                declaration.name
            ));
        }

        let value = carrier(definition.entry.ty)?;
        let getter_ty = module.func_type(vec![REFERENCE], value.into_iter().collect());
        let mut getter = Code::default();
        emit_module_property_getter(context, classes, runtime, *class, *property, &mut getter)?;
        module.define(
            definition.entry.getter,
            Function {
                ty: getter_ty,
                locals: Vec::new(),
                code: getter,
            },
        );

        if let Some(setter) = definition.entry.setter {
            let value = value
                .ok_or_else(|| "a mutable module member property with no Wasm value".to_string())?;
            let setter_ty = module.func_type(vec![REFERENCE, value], Vec::new());
            let mut body = Code::default();
            emit_module_property_setter(context, classes, runtime, *class, *property, &mut body)?;
            module.define(
                setter,
                Function {
                    ty: setter_ty,
                    locals: Vec::new(),
                    code: body,
                },
            );
        }
    }
    Ok(())
}

fn emit_module_property_getter(
    context: &FileContext<'_>,
    classes: &FileClasses,
    runtime: &Runtime,
    class: ClassId,
    property: u32,
    code: &mut Code,
) -> Result<(), Unsupported> {
    let declaration = &context.ir.classes[class as usize].properties[property as usize];
    let slot = local_property_target(context.ir, class, property as usize)
        .and_then(|target| classes.tables.slot(class, &SlotKey::Getter(target)));
    if let Some(slot) = slot {
        code.local_get(0);
        emit_module_property_dispatch(classes, runtime, class, slot, code)?;
        return Ok(());
    }
    if let Some(getter) = declaration.getter {
        let function = context
            .functions
            .get(getter as usize)
            .copied()
            .flatten()
            .ok_or_else(|| "a module property getter with no emitted function".to_string())?;
        code.local_get(0).call(function);
        return Ok(());
    }
    let field = declaration
        .backing_field
        .ok_or_else(|| "a module property with neither storage nor a getter".to_string())?;
    let structure = classes.structure(class);
    code.local_get(0)
        .ref_cast(false, HeapType::Concrete(structure))
        .struct_get(structure, classes.field(class, field));
    Ok(())
}

fn emit_module_property_setter(
    context: &FileContext<'_>,
    classes: &FileClasses,
    runtime: &Runtime,
    class: ClassId,
    property: u32,
    code: &mut Code,
) -> Result<(), Unsupported> {
    let declaration = &context.ir.classes[class as usize].properties[property as usize];
    let slot = local_property_target(context.ir, class, property as usize)
        .and_then(|target| classes.tables.slot(class, &SlotKey::Setter(target)));
    if let Some(slot) = slot {
        code.local_get(0).local_get(1);
        emit_module_property_dispatch(classes, runtime, class, slot, code)?;
        return Ok(());
    }
    if let Some(setter) = declaration.setter {
        let function = context
            .functions
            .get(setter as usize)
            .copied()
            .flatten()
            .ok_or_else(|| "a module property setter with no emitted function".to_string())?;
        code.local_get(0).local_get(1).call(function);
        return Ok(());
    }
    let field = declaration
        .backing_field
        .ok_or_else(|| "a module property with neither storage nor a setter".to_string())?;
    let structure = classes.structure(class);
    code.local_get(0)
        .ref_cast(false, HeapType::Concrete(structure))
        .local_get(1)
        .struct_set(structure, classes.field(class, field));
    Ok(())
}

/// Append the table reference and indirect call after the wrapper's receiver and value operands.
fn emit_module_property_dispatch(
    classes: &FileClasses,
    runtime: &Runtime,
    class: ClassId,
    slot: u32,
    code: &mut Code,
) -> Result<(), Unsupported> {
    let ty = classes
        .slot_type(class, slot)
        .ok_or_else(|| "a module property slot with no signature".to_string())?;
    let table_type = classes.forms[class as usize]
        .table_type
        .unwrap_or(classes.file_table);
    let object = runtime.objects.object;
    code.local_get(0)
        .ref_cast(false, HeapType::Concrete(object))
        .struct_get(object, 0)
        .ref_cast(false, HeapType::Concrete(table_type))
        .struct_get(table_type, slot)
        .call_ref(ty);
    Ok(())
}

/// Define `function`, taking `parameters` and answering nothing, as `(code, locals)`.
fn define(
    module: &mut Module,
    function: u32,
    parameters: &[Ty],
    (code, locals): (Code, Vec<ValType>),
) -> Result<(), Unsupported> {
    let mut params = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        params.push(carrier(*parameter)?.ok_or("a `Unit` constructor parameter")?);
    }
    let ty = module.func_type(params, Vec::new());
    module.define(function, Function { ty, locals, code });
    Ok(())
}

impl Body<'_> {
    fn finish(self) -> (Code, Vec<ValType>) {
        (self.code, self.locals)
    }

    /// The constructor a delegation to `class`'s constructor `ordinal` (0 for the primary) reaches,
    /// and the parameters it takes.
    fn constructor_of(&self, class: ClassId, ordinal: u32) -> Result<(u32, Vec<Ty>), Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        let form = &self.file_classes()?.forms[class as usize];
        if ordinal == 0 {
            let constructor = form.constructor.ok_or_else(|| {
                format!(
                    "a delegation to a primary constructor `{}` lacks",
                    declaration.fq_name()
                )
            })?;
            return Ok((
                constructor,
                declaration.ctor_args.iter().map(|arg| arg.ty).collect(),
            ));
        }
        let index = ordinal as usize - 1;
        let secondary = declaration
            .secondary_ctors
            .get(index)
            .ok_or("a delegation to an unknown constructor")?;
        let mut params = secondary.prefix_params.clone();
        params.extend(secondary.params.iter().copied());
        Ok((form.secondaries[index], params))
    }

    /// Call `constructor`, taking `params`, on `this` with `args`.
    fn delegate(
        &mut self,
        constructor: u32,
        params: &[Ty],
        args: &[u32],
    ) -> Result<(), Unsupported> {
        if params.len() != args.len() {
            return Err("a constructor delegation with omitted arguments".to_string());
        }
        self.code.local_get(0);
        for (arg, param) in args.iter().zip(params) {
            self.value(*arg, *param)?;
        }
        self.code.call(constructor);
        Ok(())
    }

    fn primary_constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let name = declaration.fq_name();
        if self
            .ir
            .super_constructor_default_arguments
            .get(&declaration.fq_name_id())
            .is_some_and(|omitted| !omitted.is_empty())
        {
            return Err(format!(
                "a superclass constructor call with omitted arguments (`{name}`)"
            ));
        }
        for &statement in &declaration.super_arg_prelude {
            self.statement(statement)?;
        }
        // These exact parameter-to-field pairs are semantic constructor-order metadata. In
        // particular, an inner class must expose its enclosing instance before a superclass
        // constructor can call an override which reads it.
        let structure = self.file_classes()?.structure(class);
        for &(parameter, field) in &declaration.pre_super_param_fields {
            let argument = declaration
                .ctor_args
                .get(parameter as usize)
                .ok_or_else(|| format!("a pre-super store from an unknown parameter (`{name}`)"))?;
            let stored = declaration
                .fields
                .get(field as usize)
                .ok_or_else(|| format!("a pre-super store to an unknown field (`{name}`)"))?;
            if argument.field_index != Some(field) || carrier(argument.ty)? != carrier(stored.ty)? {
                return Err(format!(
                    "a pre-super constructor parameter stored at another field (`{name}`)"
                ));
            }
            let slot = self.file_classes()?.field(class, field);
            self.code
                .local_get(0)
                .ref_cast(false, HeapType::Concrete(structure))
                .local_get(parameter + 1)
                .struct_set(structure, slot);
        }
        match self.file_classes()?.tables.table(class).superclass {
            Some(parent) => {
                let (constructor, params) =
                    self.constructor_of(parent, declaration.super_ctor.ordinal)?;
                self.delegate(constructor, &params, &declaration.super_args)?;
            }
            None if !declaration.super_args.is_empty() => {
                return Err(format!("a superclass constructor call (`{name}`)"));
            }
            None => {}
        }
        if !declaration.explicit_param_stores {
            let mut next_field = 0;
            for (index, argument) in declaration.ctor_args.iter().enumerate() {
                if !argument.is_field {
                    continue;
                }
                let field = argument.field_index.unwrap_or(next_field);
                next_field = field + 1;
                if declaration
                    .pre_super_param_fields
                    .contains(&(index as u32, field))
                {
                    continue;
                }
                if carrier(argument.ty)? != carrier(declaration.fields[field as usize].ty)? {
                    return Err(format!(
                        "a constructor parameter stored at another type (`{name}`)"
                    ));
                }
                let slot = self.file_classes()?.field(class, field);
                self.code
                    .local_get(0)
                    .ref_cast(false, HeapType::Concrete(structure))
                    .local_get(index as u32 + 1)
                    .struct_set(structure, slot);
            }
        }
        if let Some(init) = declaration.init_body {
            self.statement(init)?;
        }
        Ok(())
    }

    fn secondary_constructor(&mut self, class: ClassId, ordinal: usize) -> Result<(), Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        let name = declaration.fq_name();
        let secondary = declaration.secondary_ctors[ordinal].clone();
        if !secondary.default_parameters.is_empty() {
            return Err(format!(
                "a constructor delegating with omitted arguments (`{name}`)"
            ));
        }
        for &statement in &secondary.delegate_prelude {
            self.statement(statement)?;
        }
        match &secondary.delegate {
            CtorDelegateTarget::This { target, .. } => {
                let (constructor, params) = self.constructor_of(class, target.ordinal)?;
                self.delegate(constructor, &params, &secondary.delegate_args)?;
            }
            CtorDelegateTarget::Super { target, .. } => {
                match self.file_classes()?.tables.table(class).superclass {
                    Some(parent) => {
                        let (constructor, params) = self.constructor_of(parent, target.ordinal)?;
                        self.delegate(constructor, &params, &secondary.delegate_args)?;
                    }
                    None if !secondary.delegate_args.is_empty() => {
                        return Err(format!("a superclass constructor call (`{name}`)"));
                    }
                    None => {}
                }
            }
            CtorDelegateTarget::ImplicitEnumBase => {
                return Err(format!("an enum constructor (`{name}`)"));
            }
        }
        if let Some(body) = secondary.body {
            self.statement(body)?;
        }
        Ok(())
    }
}
