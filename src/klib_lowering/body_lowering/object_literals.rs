//! Prepared object literals.
//!
//! kotlinc serializes an object expression inside an inline function as a block whose origin is
//! `OBJECT_LITERAL`: the local class, then a call of its constructor. The class's members still
//! read the inline function's parameters. Checked FIR stores each of those reads as a constructor
//! field and reads the field, which is the class an inlined call copies. This module lowers that
//! shape. A local class written as its own statement stays declined.

use super::{mismatch, unsupported, BodyLowering, KlibBodyDeclineReason, Lowered};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    ClassId, ExprId, FunId, IrClass, IrConstructorCapture, IrConstructorTarget, IrCtorArg, IrCtorParameterProvenance,
    IrExpr, IrField, IrFunction, IrFunctionOverride, IrProperty, IrPropertyOverride,
};
use crate::metadata::klib_ir::tree::{
    KlibIrClass, KlibIrExprKind, KlibIrField, KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrStatement,
};
use crate::metadata::klib_ir::KlibIrSymbol;
use crate::types::{ContextParameterKind, Ty, TypeName};

/// One enclosing value stored in an object literal's constructor.
#[derive(Clone, Copy)]
pub(super) struct CaptureField {
    pub(super) class: ClassId,
    pub(super) index: u32,
    pub(super) class_name: TypeName,
}

/// An object literal's delegation to a superclass constructor: the operands evaluated where the
/// object is constructed, the constructor's parameters and the constructor itself.
struct SuperclassForwards {
    operands: Vec<(ExprId, Ty)>,
    params: Vec<Ty>,
    target: IrConstructorTarget,
    /// The provider's constructor, when the target lays the superclass out itself.
    external: Option<crate::ir::IrExternalConstructorTarget>,
}

impl SuperclassForwards {
    /// A delegation to `Any`'s constructor, which forwards nothing.
    fn none() -> Self {
        Self {
            operands: Vec::new(),
            params: Vec::new(),
            target: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            external: None,
        }
    }
}

/// The fields an object literal's properties with storage add after its captured values.
struct ObjectStorage<'a> {
    first_index: u32,
    fields: Vec<(&'a KlibIrSymbol, Ty, bool)>,
}

struct Capture {
    symbol: KlibIrSymbol,
    name: String,
    ty: Ty,
    crossinline: Option<u32>,
}

impl<'a> BodyLowering<'a> {
    pub(super) fn object_literal(&mut self, statements: &[KlibIrStatement]) -> Lowered<ExprId> {
        let [KlibIrStatement::Class(class_id), KlibIrStatement::Expression(construction)] =
            statements
        else {
            return unsupported("an object literal of another shape");
        };
        match &self.arena.expr(*construction).kind {
            KlibIrExprKind::ConstructorCall { access, .. }
                if matches!(
                    access.arguments,
                    crate::metadata::klib_ir::tree::KlibIrArguments::Flat(ref values)
                        if values.is_empty()
                ) || matches!(
                    access.arguments,
                    crate::metadata::klib_ir::tree::KlibIrArguments::Split {
                        dispatch_receiver: None,
                        extension_receiver: None,
                        ref values,
                    } if values.is_empty()
                ) => {}
            KlibIrExprKind::ConstructorCall { .. } => {
                return unsupported("an object literal constructor passes arguments")
            }
            _ => return unsupported("an object literal of another shape"),
        }
        let arena = self.arena;
        let class = arena.class(*class_id);
        let delegation = self.prepared_constructor(class)?;
        let captures = self.captures_of(class)?;
        let (superclass, interfaces, supertypes) = self.supertypes(class)?;
        let forwarded = match delegation {
            Some(access) => self.superclass_forwards(&access, superclass, &supertypes)?,
            None => SuperclassForwards::none(),
        };
        let class_name = self
            .linker
            .local_class(&class.base.symbol.signature)?
            .ok_or_else(|| mismatch("an object literal names no class in its library"))?;
        let mut declared = IrClass::synthetic(class_name);
        declared.declaration_context = Some(crate::ir::IrClassDeclarationContext::Executable);
        declared.enclosure = Some(crate::ir::IrEnclosure::Function(self.function.function));
        declared.is_source_declared = true;
        declared.is_anonymous_object = true;
        declared.is_local_class = true;
        declared.superclass = superclass;
        declared.interfaces = interfaces.into();
        declared.supertypes = supertypes.clone();
        let capture_count = u32::try_from(captures.len()).expect("a capture list fits u32");
        let forward_count = u32::try_from(forwarded.operands.len()).expect("forwards fit u32");
        let prefix_count = capture_count + forward_count;
        declared.constructor_prefix_count = prefix_count;
        declared.ctor_param_count = prefix_count;
        declared.super_ctor = forwarded.target;
        // Each captured value is stored before the superclass constructor runs, as the anonymous
        // class checked FIR builds stores it.
        declared.pre_super_param_fields = (0..capture_count).map(|index| (index, index)).collect();
        declared.fields = captures
            .iter()
            .map(|capture| IrField::new(capture.name.clone(), capture.ty).with_is_final(true))
            .collect();
        // A property with storage keeps its value in a field after the captured values, stored
        // by the object's initialization.
        let mut stored = Vec::new();
        for member in &class.members {
            let KlibIrMember::Property(property) = member else {
                continue;
            };
            let Some(field) = &property.backing_field else {
                continue;
            };
            let ty = self.semantic(field.ty)?;
            let index = u32::try_from(declared.fields.len()).expect("a field index fits u32");
            let is_final = property.setter.is_none();
            declared
                .fields
                .push(IrField::new(field.name.clone(), ty).with_is_final(is_final));
            stored.push((field, ty, index, is_final));
        }
        declared.ctor_args = captures
            .iter()
            .enumerate()
            .map(|(index, capture)| IrCtorArg {
                name: None,
                context_kind: ContextParameterKind::None,
                ty: capture.ty,
                declared_ty: None,
                is_field: true,
                field_index: Some(u32::try_from(index).expect("a field index fits u32")),
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: Some(IrConstructorCapture {
                    source_name: capture.name.clone().into_boxed_str(),
                    receiver: None,
                    crossinline_parameter: capture.crossinline,
                    inlined: false,
                }),
                provenance: IrCtorParameterProvenance::Capture,
                capture_identity: None,
            })
            .collect();
        // A superclass constructor argument is evaluated where the object is constructed, as the
        // constructor of kotlinc's anonymous class receives it, and read back from its parameter.
        for (ordinal, (_, ty)) in forwarded.operands.iter().enumerate() {
            declared.ctor_args.push(IrCtorArg {
                name: None,
                context_kind: ContextParameterKind::None,
                ty: *ty,
                declared_ty: None,
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: Some(
                    u32::try_from(ordinal + 1).expect("a forward ordinal fits u32"),
                ),
                capture: None,
                provenance: IrCtorParameterProvenance::Value,
                capture_identity: None,
            });
        }
        let class_id = self.ir.add_class(declared);
        let owner = self.ir.classes[class_id as usize].fq_name_id();
        let mut capture_fields = HashMap::default();
        for (index, capture) in captures.iter().enumerate() {
            capture_fields.insert(
                capture.symbol.clone(),
                CaptureField {
                    class: class_id,
                    index: u32::try_from(index).expect("a field index fits u32"),
                    class_name: owner,
                },
            );
        }
        let storage = stored
            .iter()
            .map(|(field, ty, _, is_final)| (&field.base.symbol, *ty, *is_final))
            .collect::<Vec<_>>();
        self.lower_members(
            class,
            class_id,
            owner,
            &supertypes,
            &capture_fields,
            &ObjectStorage {
                first_index: capture_count,
                fields: storage,
            },
        )?;
        self.initialize_object(class_id, owner, &capture_fields, &stored)?;
        let mut args = Vec::with_capacity(captures.len());
        let mut params = Vec::with_capacity(captures.len());
        for capture in &captures {
            args.push(self.read(&capture.symbol)?);
            params.push(capture.ty);
        }
        let mut forwarded_reads = Vec::with_capacity(forwarded.operands.len());
        for (operand, ty) in &forwarded.operands {
            args.push(*operand);
            params.push(*ty);
            forwarded_reads.push(*ty);
        }
        {
            let reads = (0..forwarded_reads.len())
                .map(|ordinal| {
                    let slot = 1 + capture_count + u32::try_from(ordinal).expect("fits u32");
                    self.ir.add_expr(IrExpr::GetValue(slot))
                })
                .collect();
            let declaration = &mut self.ir.classes[class_id as usize];
            declaration.super_args = reads;
            declaration.super_ctor_params = forwarded.params;
            if let Some(external) = forwarded.external {
                let name = declaration.fq_name;
                self.ir.external_super_constructors.insert(name, external);
            }
        }
        let constructed = self.ir.add_expr(IrExpr::New {
            internal: owner,
            args,
            ctor_params: Some(params),
            declared_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: prefix_count,
        });
        Ok(self.typed(constructed, Ty::obj_name(owner)))
    }

    /// Checks the members an object literal may declare, and returns the constructor's delegation
    /// to its superclass constructor unless that is `Any`'s.
    fn prepared_constructor(&self, class: &KlibIrClass) -> Lowered<Option<KlibIrMemberAccess>> {
        let mut delegation = None;
        for member in &class.members {
            match member {
                KlibIrMember::Function(id) if self.arena.function(*id).constructor => {
                    delegation = self.constructor_body(self.arena.function(*id))?;
                }
                KlibIrMember::Function(_) | KlibIrMember::Property(_) => {}
                KlibIrMember::AnonymousInitializer { statements, .. } if statements.is_empty() => {}
                KlibIrMember::AnonymousInitializer { .. } => {
                    return unsupported("an object literal initializer")
                }
                KlibIrMember::Class(_) => {
                    return unsupported("a nested class in an object literal")
                }
                KlibIrMember::Field(_) => return unsupported("an object literal field"),
                KlibIrMember::EnumEntry(_) => return unsupported("an object literal enum entry"),
                KlibIrMember::TypeAlias(_) => {
                    return unsupported("a local type alias in an object literal")
                }
            }
        }
        Ok(delegation)
    }

    fn constructor_body(&self, function: &KlibIrFunction) -> Lowered<Option<KlibIrMemberAccess>> {
        let Some(crate::metadata::klib_ir::tree::KlibIrBody::Block(statements)) = &function.body
        else {
            return unsupported("an object literal constructor without a body");
        };
        let [KlibIrStatement::Expression(delegating), KlibIrStatement::Expression(initializer)] =
            statements.as_slice()
        else {
            return unsupported("an object literal constructor with its own statements");
        };
        let KlibIrExprKind::DelegatingConstructorCall(access) = &self.arena.expr(*delegating).kind
        else {
            return unsupported("an object literal constructor with its own statements");
        };
        let crate::metadata::klib_ir::KlibIrSignature::Public(callee) = &access.symbol.signature
        else {
            return unsupported("an object literal constructor with its own statements");
        };
        let package = callee.package().segments();
        let declaration = callee.declaration().segments();
        let any = package.len() == 1
            && package.first().map(String::as_str) == Some("kotlin")
            && declaration.first().map(String::as_str) == Some("Any");
        if !matches!(
            self.arena.expr(*initializer).kind,
            KlibIrExprKind::InstanceInitializerCall(_)
        ) {
            return unsupported("an object literal constructor with its own statements");
        }
        Ok((!any).then(|| access.clone()))
    }

    /// Lowers the arguments of an object literal's superclass constructor call where the object is
    /// constructed. kotlinc's anonymous class receives each as a constructor parameter, and its
    /// `super(...)` reads the parameter back.
    fn superclass_forwards(
        &mut self,
        access: &KlibIrMemberAccess,
        superclass: TypeName,
        supertypes: &[Ty],
    ) -> Lowered<SuperclassForwards> {
        let ty = supertypes
            .iter()
            .copied()
            .find(|ty| ty.non_null().kotlin_class_internal() == Some(superclass))
            .ok_or_else(|| mismatch("an object literal delegates to a class it does not extend"))?;
        let operands = self.constructor_operands(access, 0, ty)?;
        if operands.declaration.classifier != superclass || operands.local_access.is_some() {
            return Err(mismatch(
                "an object literal delegates to another class than its superclass",
            ));
        }
        if !operands.defaults.is_empty() {
            return unsupported("an object literal omitting a superclass constructor argument");
        }
        // A superclass this library realizes is reached by its constructor's ordinal; one the
        // target lays out itself, by the provider's constructor, as a source class extending it.
        let (ordinal, external) = match self.ir.class_id_by_name(superclass) {
            Some(parent) => (
                super::super::body_unit::superclass_constructor_ordinal(
                    Some(&self.ir.classes[parent as usize]),
                    &operands.params,
                )?,
                None,
            ),
            None => (
                0,
                operands
                    .external_target
                    .map(crate::ir::IrExternalConstructorTarget::unresolved),
            ),
        };
        let pairs = operands.args.into_iter().zip(operands.params.iter().copied());
        Ok(SuperclassForwards {
            operands: pairs.collect(),
            params: operands.params,
            target: IrConstructorTarget {
                ordinal,
                access: crate::ir::IrConstructorAccess::Unrestricted,
            },
            external,
        })
    }

    fn captures_of(&self, class: &KlibIrClass) -> Lowered<Vec<Capture>> {
        let mut seen = HashSet::default();
        let mut captures = Vec::new();
        for member in &class.members {
            let mut symbols = Vec::new();
            match member {
                KlibIrMember::Function(id) => {
                    symbols.extend(self.arena.lexical_value_symbols(self.arena.function(*id)));
                }
                KlibIrMember::Property(property) => {
                    // The stored value is read through its initializer, as the getter reads it.
                    if let Some(initializer) =
                        property.backing_field.as_ref().and_then(|field| field.initializer)
                    {
                        symbols.extend(self.arena.lexical_value_symbols_of_expression(initializer));
                    }
                    for accessor in property.getter.iter().chain(&property.setter) {
                        symbols.extend(self.arena.lexical_value_symbols(self.arena.function(*accessor)));
                    }
                }
                _ => {}
            }
            for symbol in symbols {
                if !seen.insert((*symbol).clone()) {
                    continue;
                }
                let Some(capture) = self.capture(symbol)? else {
                    continue;
                };
                captures.push(capture);
            }
        }
        Ok(captures)
    }

    fn capture(&self, symbol: &KlibIrSymbol) -> Lowered<Option<Capture>> {
        if let Some((slot, identity)) = self.header.parameter_identity(symbol) {
            // An extension receiver has no source name; kotlinc names its captured copy after the
            // function it extends.
            let name = identity.source_name.clone().unwrap_or_else(|| {
                format!(
                    "$this_{}",
                    self.ir.functions[self.function.function as usize].name
                )
            });
            let crossinline = self.crossinline_parameter(slot);
            return Ok(Some(Capture {
                symbol: (*symbol).clone(),
                name,
                ty: self.header.slot_type(slot),
                crossinline,
            }));
        }
        if let Some(local) = self.locals.get(symbol) {
            if local.mutable {
                return unsupported("an object literal capturing a mutable local");
            }
            return Ok(Some(Capture {
                symbol: (*symbol).clone(),
                name: self
                    .locals
                    .name(symbol)
                    .unwrap_or_else(|| "capture".to_owned()),
                ty: local.ty,
                crossinline: None,
            }));
        }
        Ok(None)
    }

    fn crossinline_parameter(&self, slot: u32) -> Option<u32> {
        let info = self.ir.fn_params.get(&self.function.function)?;
        let offset = self.header.parameter_count() - self.header.params().len();
        let index = usize::try_from(slot).ok()?.checked_sub(offset)?;
        matches!(
            info.inline_modifiers.get(index).copied(),
            Some(crate::types::InlineParameterModifier::Crossinline)
        )
        .then_some(slot)
    }

    fn supertypes(&self, class: &KlibIrClass) -> Lowered<(TypeName, Vec<TypeName>, Vec<Ty>)> {
        let mut superclass = crate::types::wk::any();
        let mut interfaces = Vec::new();
        let mut supertypes = Vec::new();
        for supertype in &class.supertypes {
            let ty = self.semantic(*supertype)?;
            let Some(name) = ty.non_null().kotlin_class_internal() else {
                return Err(mismatch(
                    "an object literal's supertype is not a classifier",
                ));
            };
            if name == crate::types::wk::any() {
                continue;
            }
            if self.linker.is_interface(name)? {
                interfaces.push(name);
            } else if superclass == crate::types::wk::any() {
                superclass = name;
            } else {
                return unsupported("an object literal with two superclass types");
            }
            supertypes.push(ty);
        }
        Ok((superclass, interfaces, supertypes))
    }

    fn lower_members(
        &mut self,
        class: &KlibIrClass,
        class_id: ClassId,
        owner: TypeName,
        supertypes: &[Ty],
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
    ) -> Lowered<()> {
        for member in &class.members {
            match member {
                KlibIrMember::Function(id) if !self.arena.function(*id).constructor => {
                    let function = self.arena.function(*id).clone();
                    let (fun, ret, params) = self.lower_member(&function, class_id, owner, captures, storage)?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    self.record_function_override(&function, fun, owner, supertypes, &params, ret)?;
                }
                KlibIrMember::Property(property) => {
                    let Some(getter_id) = property.getter else {
                        return unsupported("an object literal property without a getter");
                    };
                    let name = property.name.clone();
                    let getter = self.arena.function(getter_id).clone();
                    let (fun, ret, _) =
                        self.lower_member(&getter, class_id, owner, captures, storage)?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    let setter = match property.setter {
                        Some(setter_id) => {
                            let setter = self.arena.function(setter_id).clone();
                            let (setter, _, _) =
                                self.lower_member(&setter, class_id, owner, captures, storage)?;
                            self.ir.classes[class_id as usize].methods.push(setter);
                            Some(setter)
                        }
                        None => None,
                    };
                    self.ir.classes[class_id as usize]
                        .properties
                        .push(property_declaration(&name, ret, fun, setter));
                    self.record_property_override(&name, &getter, fun, owner, supertypes, ret)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn lower_member(
        &mut self,
        function: &KlibIrFunction,
        class: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
    ) -> Lowered<(FunId, Ty, Vec<Ty>)> {
        let mut params = Vec::new();
        let mut identities = Vec::new();
        for parameter in &function.regular_parameters {
            let ty = self.semantic(parameter.ty)?;
            params.push(ty);
            identities.push(crate::fir::ResolvedParameterIdentity::Source(
                parameter.name.clone().into_boxed_str(),
            ));
        }
        let ret = self.semantic(function.return_type)?;
        let fun = self.ir.add_fun(IrFunction {
            name: function.name.clone(),
            params: params.clone(),
            ret,
            body: None,
            is_static: false,
            dispatch_receiver: Some(owner),
            param_checks: vec![None; params.len()],
        });
        self.ir.fn_source_names.insert(fun, function.name.clone());
        self.ir.fn_params.insert(
            fun,
            crate::ir::FnParamInfo::identities(
                identities
                    .iter()
                    .map(crate::ir::IrParameterIdentity::resolved)
                    .collect(),
            ),
        );
        let statements = match &function.body {
            Some(crate::metadata::klib_ir::tree::KlibIrBody::Block(statements)) => statements,
            Some(_) => return Err(KlibBodyDeclineReason::SyntheticBody),
            None => return Err(KlibBodyDeclineReason::NoBody),
        };
        let frame = self.enter_member(function, class, owner, captures, storage, ret)?;
        let lowered = self.block(statements);
        self.exit_member(frame);
        let source = lowered?;
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![source],
            value: None,
        });
        self.ir.callable_scopes.insert(source);
        self.ir.callable_scopes.insert(body);
        self.ir.functions[fun as usize].body = Some(body);
        Ok((fun, ret, params))
    }

    fn enter_member(
        &mut self,
        function: &KlibIrFunction,
        class: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
        ret: Ty,
    ) -> Lowered<MemberFrame<'a>> {
        let mut locals = super::super::local_values::LocalValues::after(0);
        if let Some(receiver) = &function.dispatch_receiver {
            locals.temporary(&receiver.base.symbol, Ty::obj_name(owner))?;
        } else {
            locals.anonymous_slot();
        }
        for parameter in &function.regular_parameters {
            let ty = self.semantic(parameter.ty)?;
            locals.temporary(&parameter.base.symbol, ty)?;
        }
        let fields = super::instance_storage::InstanceStorageFrame::object_member(
            class,
            storage.first_index,
            storage.fields.iter().copied(),
            Ty::obj_name(owner),
        );
        Ok(MemberFrame {
            instance_storage: self.instance_storage.replace(fields),
            locals: std::mem::replace(&mut self.locals, locals),
            captures: std::mem::replace(&mut self.captures, captures.clone()),
            member_return: self.member_return.replace(function.base.symbol.clone()),
            member_result: self.member_result.replace(ret),
            compiler_aliases: std::mem::take(&mut self.compiler_aliases),
            returnable_targets: std::mem::take(&mut self.returnable_targets),
            returnable_frames: std::mem::take(&mut self.returnable_frames),
            null_proven: std::mem::take(&mut self.null_proven),
        })
    }

    /// The constructor frame an object's field initializers run in: the object is value 0 and the
    /// captured values are its fields.
    fn enter_initializer(
        &mut self,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
    ) -> MemberFrame<'a> {
        let mut locals = super::super::local_values::LocalValues::after(0);
        locals.anonymous_slot();
        MemberFrame {
            instance_storage: self.instance_storage.take(),
            locals: std::mem::replace(&mut self.locals, locals),
            captures: std::mem::replace(&mut self.captures, captures.clone()),
            member_return: self.member_return.take(),
            member_result: self.member_result.take(),
            compiler_aliases: std::mem::take(&mut self.compiler_aliases),
            returnable_targets: std::mem::take(&mut self.returnable_targets),
            returnable_frames: std::mem::take(&mut self.returnable_frames),
            null_proven: std::mem::take(&mut self.null_proven),
        }
    }

    /// Stores each property with storage's initializer into its field, in declaration order.
    fn initialize_object(
        &mut self,
        class_id: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        stored: &[(&KlibIrField, Ty, u32, bool)],
    ) -> Lowered<()> {
        if stored.iter().all(|(field, ..)| field.initializer.is_none()) {
            return Ok(());
        }
        let frame = self.enter_initializer(captures);
        let lowered = self.object_initializers(class_id, owner, stored);
        self.exit_member(frame);
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: lowered?,
            value: None,
        });
        self.ir.callable_scopes.insert(body);
        self.ir.classes[class_id as usize].init_body = Some(body);
        Ok(())
    }

    fn object_initializers(
        &mut self,
        class_id: ClassId,
        owner: TypeName,
        stored: &[(&KlibIrField, Ty, u32, bool)],
    ) -> Lowered<Vec<ExprId>> {
        let mut stores = Vec::new();
        for (field, ty, index, _) in stored {
            let Some(initializer) = field.initializer else {
                continue;
            };
            let value = self.operand(initializer, *ty)?;
            let receiver = self.ir.add_expr(IrExpr::GetValue(0));
            self.ir.set_logical_type(receiver, Ty::obj_name(owner));
            let store = self.ir.add_expr(IrExpr::SetField {
                receiver,
                class: class_id,
                index: *index,
                value,
            });
            stores.push(self.typed(store, Ty::Unit));
        }
        Ok(stores)
    }

    fn exit_member(&mut self, frame: MemberFrame<'a>) {
        self.instance_storage = frame.instance_storage;
        self.locals = frame.locals;
        self.captures = frame.captures;
        self.member_return = frame.member_return;
        self.member_result = frame.member_result;
        self.compiler_aliases = frame.compiler_aliases;
        self.returnable_targets = frame.returnable_targets;
        self.returnable_frames = frame.returnable_frames;
        self.null_proven = frame.null_proven;
    }

    fn record_function_override(
        &mut self,
        function: &KlibIrFunction,
        fun: FunId,
        owner: TypeName,
        supertypes: &[Ty],
        params: &[Ty],
        ret: Ty,
    ) -> Lowered<()> {
        use crate::fir::ResolvedFunctionOverrideTarget;
        for symbol in &function.overridden {
            let Some(record) = self.linker.dependency_override(symbol, supertypes, false)? else {
                continue;
            };
            let target = ResolvedFunctionOverrideTarget::External(
                self.linker.external_override(&record.identity)?,
            );
            let interface = self.linker.is_interface(record.owner)?;
            self.ir
                .function_overrides
                .entry(owner)
                .or_default()
                .push(IrFunctionOverride {
                    implementation: target,
                    implementation_function: Some(fun),
                    implementation_owner: owner,
                    overridden: target,
                    overridden_owner: record.owner,
                    overridden_semantic_role: None,
                    collection_barrier: None,
                    overridden_is_interface: interface,
                    name: function.name.clone(),
                    declared_parameters: record.declared_parameters,
                    declared_result: record.declared_result,
                    applied_parameters: record.applied_parameters,
                    applied_result: record.applied_result,
                    implementation_parameters: params.to_vec(),
                    implementation_parameter_identities: Vec::new(),
                    overridden_parameter_identities: Vec::new(),
                    implementation_result: ret,
                    suspend: false,
                    has_kotlin_superclass_override: false,
                    depth: 1,
                });
        }
        Ok(())
    }

    fn record_property_override(
        &mut self,
        name: &str,
        getter: &KlibIrFunction,
        fun: FunId,
        owner: TypeName,
        supertypes: &[Ty],
        ty: Ty,
    ) -> Lowered<()> {
        use crate::fir::ResolvedPropertyOverrideTarget;
        for symbol in &getter.overridden {
            let Some(record) = self.linker.dependency_override(symbol, supertypes, true)? else {
                continue;
            };
            let target = ResolvedPropertyOverrideTarget::External(
                self.linker.external_override(&record.identity)?,
            );
            let interface = self.linker.is_interface(record.owner)?;
            self.ir
                .property_overrides
                .entry(owner)
                .or_default()
                .push(IrPropertyOverride {
                    implementation: target,
                    implementation_getter: Some(fun),
                    implementation_setter: None,
                    implementation_owner: owner,
                    overridden: target,
                    overridden_owner: record.owner,
                    overridden_is_interface: interface,
                    name: name.to_owned(),
                    declared_type: record.declared_result,
                    applied_type: record.applied_result,
                    implementation_type: ty,
                    declared_receiver: None,
                    implementation_receiver: None,
                    overridden_setter_value: None,
                    implementation_mutable: false,
                    has_kotlin_superclass_override: false,
                    depth: 1,
                });
        }
        Ok(())
    }
}

struct MemberFrame<'a> {
    instance_storage: Option<super::instance_storage::InstanceStorageFrame<'a>>,
    locals: super::super::local_values::LocalValues,
    captures: HashMap<KlibIrSymbol, CaptureField>,
    member_return: Option<KlibIrSymbol>,
    member_result: Option<Ty>,
    compiler_aliases: HashMap<KlibIrSymbol, crate::metadata::klib_ir::tree::KlibIrExprId>,
    returnable_targets: Vec<KlibIrSymbol>,
    returnable_frames: Vec<super::compiler_blocks::ReturnableFrame>,
    null_proven: HashSet<KlibIrSymbol>,
}

fn property_declaration(name: &str, ty: Ty, getter: FunId, setter: Option<FunId>) -> IrProperty {
    IrProperty {
        name: name.to_owned(),
        origin: crate::ir::IrPropertyOrigin::Declaration,
        diagnostic_span: None,
        getter_diagnostic_span: None,
        setter_diagnostic_span: None,
        context_params: Vec::new(),
        source_order: u32::MAX,
        decl_line: 0,
        accessor_lines: crate::ir::IrAccessorLines::default(),
        ty,
        type_params: Vec::new(),
        visibility: crate::types::Visibility::Public,
        return_value_status: Default::default(),
        annotations: Box::new([]),
        initializer: None,
        storage_ty: None,
        backing_field: None,
        is_var: setter.is_some(),
        is_open: true,
        modifiers: crate::ir::IrPropertyModifiers {
            modality: crate::ir::IrPropertyModality::Open,
            declared_getter: true,
            ..crate::ir::IrPropertyModifiers::default()
        },
        delegate_field: None,
        has_constant_initializer: false,
        is_private: false,
        setter_visibility: crate::types::Visibility::Public,
        getter: Some(getter),
        setter,
        needs_access_bridge: false,
        accessor_annotations: Default::default(),
    }
}
