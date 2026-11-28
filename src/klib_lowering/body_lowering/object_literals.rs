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
    ClassId, ExprId, FunId, IrClass, IrConstructorCapture, IrConstructorTarget, IrCtorArg,
    IrCtorParameterProvenance, IrExpr, IrField, IrFunction, IrFunctionOverride, IrProperty,
    IrPropertyOverride,
};
use crate::metadata::klib_ir::tree::{
    KlibIrClass, KlibIrExprKind, KlibIrField, KlibIrFunction, KlibIrMember, KlibIrMemberAccess,
    KlibIrStatement,
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
/// object is constructed, the constructor's declared parameters and the constructor itself.
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
    /// The serialized declaration of the object's class.
    declaring: &'a KlibIrSymbol,
    first_index: u32,
    fields: Vec<(&'a KlibIrSymbol, Ty, bool)>,
    /// The object's properties with a backing field, each with that field.
    properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
}

/// A member an object literal declares, which its own bodies call.
#[derive(Clone)]
pub(super) struct ObjectMember {
    fun: FunId,
    owner: TypeName,
    params: Vec<Ty>,
    ret: Ty,
}

impl ObjectMember {
    /// The call shape of the member, as of any function this unit lowers.
    pub(super) fn linked(&self) -> super::LinkedCallee {
        super::LinkedCallee {
            callee: crate::ir::Callee::Local(self.fun),
            receiver: Some(Ty::obj_name(self.owner)),
            params: self.params.clone(),
            ret: self.ret,
            context_count: 0,
            singleton_dispatch: None,
            type_parameters: Vec::new(),
            type_parameter_bounds: Some(Vec::new()),
            class_type_parameters: Vec::new(),
            parameter_defaults: Vec::new(),
            edge: super::super::CallEdge::default(),
        }
    }
}

struct Capture {
    symbol: KlibIrSymbol,
    name: String,
    ty: Ty,
    crossinline: Option<u32>,
    /// What the capture is when it holds a receiver rather than a value.
    receiver: Option<crate::ir::IrCapturedReceiver>,
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
        declared.site_name =
            self.linker
                .site_container()
                .map(|container| crate::ir::IrSiteClassName {
                    container,
                    name: class.name.as_str().into(),
                });
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
                    receiver: capture.receiver.clone(),
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
        if let Some(provenance) = self.anonymous_object_provenance() {
            self.ir
                .local_class_name_provenance
                .insert(class_id, provenance);
        }
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
        let storage = ObjectStorage {
            declaring: &class.base.symbol,
            first_index: capture_count,
            fields: storage,
            properties: super::super::class_storage::backed_properties(&class.members),
        };
        self.lower_members(
            class,
            class_id,
            owner,
            &supertypes,
            &capture_fields,
            &storage,
        )?;
        // Dispatch through an interface member the object inherits a body for reaches that body.
        self.linker
            .implement_inherited_members(self.ir, &class.base.symbol.signature, owner)?;
        self.initialize_object(class, class_id, owner, &capture_fields, &storage, &stored)?;
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

    /// How Kotlin names an object literal the body of a published function declares: in the
    /// function's class, or its package for a top-level one, below the function, numbered after
    /// the ones it already declared. A body of any other declaration names none here.
    fn anonymous_object_provenance(&self) -> Option<crate::ir::IrLocalClassNameProvenance> {
        let crate::metadata::klib_ir::KlibIrSignature::Public(signature) =
            &self.function.symbol.signature
        else {
            return None;
        };
        let (function, owner) = signature.declaration().segments().split_last()?;
        let package = signature
            .package()
            .segments()
            .iter()
            .fold(TypeName::ROOT, |parent, segment| {
                crate::types::type_name_child(parent, segment)
            });
        let lexical_owner = match owner.split_first() {
            None => None,
            Some((first, nested)) => {
                let owner = nested.iter().fold(
                    crate::types::type_name_child(package, first),
                    |parent, segment| crate::types::type_name_nested_child(parent, segment),
                );
                Some(match self.ir.class_id_by_name(owner) {
                    Some(class) => crate::ir::IrLocalClassOwner::Class(class),
                    None => crate::ir::IrLocalClassOwner::External(owner),
                })
            }
        };
        let source = crate::ir::IrLocalClassNameRoot::Dependency { package };
        let segments: Box<[String]> = Box::new([function.clone()]);
        let ordinal = self
            .ir
            .local_class_name_provenance
            .values()
            .filter(|known| {
                known.source == source
                    && known.lexical_owner == lexical_owner
                    && known.segments == segments
            })
            .filter_map(|known| known.ordinal)
            .max()
            .map_or(1, |ordinal| ordinal.saturating_add(1));
        Some(crate::ir::IrLocalClassNameProvenance {
            source,
            lexical_owner,
            segments,
            ordinal: Some(ordinal),
            parents: Box::new([]),
        })
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
        let operands =
            self.constructor_operands(access, 0, ty, super::ConstructorRole::Delegation)?;
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
                    operands.declaration.declared_parameters(),
                )?,
                None,
            ),
            None => {
                // The target lays out only a class it represents; any other superclass must be
                // one a support unit materializes for the object to extend.
                if !operands.declaration.class_emits_members
                    && !self.linker.target_represents(superclass)
                {
                    return unsupported(
                        "an object literal extending a dependency class no unit materializes",
                    );
                }
                (
                    0,
                    operands
                        .external_target
                        .map(crate::ir::IrExternalConstructorTarget::unresolved),
                )
            }
        };
        let pairs = operands
            .args
            .into_iter()
            .zip(operands.params.iter().copied());
        Ok(SuperclassForwards {
            operands: pairs.collect(),
            // The superclass constructor's own declared frame, as a source class records it: a
            // backend couples it to the constructor's physical frame, while each operand above is
            // carried at its type in this object's instantiation.
            params: operands.declaration.declared_parameters().to_vec(),
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
                    if let Some(initializer) = property
                        .backing_field
                        .as_ref()
                        .and_then(|field| field.initializer)
                    {
                        symbols.extend(self.arena.lexical_value_symbols_of_expression(initializer));
                    }
                    for accessor in property.getter.iter().chain(&property.setter) {
                        symbols.extend(
                            self.arena
                                .lexical_value_symbols(self.arena.function(*accessor)),
                        );
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
            // A receiver has no source name; its declaration fact names the capture, and a target
            // spells it.
            let receiver = match identity.source_name {
                Some(_) => None,
                None => self
                    .header
                    .capture_parameter(
                        symbol,
                        0,
                        crate::ir::IrCapturingCallable::LocalFunction,
                        &self
                            .ir
                            .fn_params
                            .get(&self.function.function)
                            .expect("a body publishes its parameters")
                            .captured_receivers,
                        false,
                    )?
                    .and_then(|parameter| parameter.receiver),
            };
            let name = match (&identity.source_name, &receiver) {
                (Some(name), _) => name.clone(),
                (None, Some(crate::ir::IrCapturedReceiver::Callable { label, .. })) => {
                    label.to_string()
                }
                (None, _) => {
                    return Err(mismatch("an object literal captures an unnamed parameter"))
                }
            };
            let crossinline = self.crossinline_parameter(slot);
            return Ok(Some(Capture {
                symbol: (*symbol).clone(),
                name,
                ty: self.header.slot_type(slot),
                crossinline,
                receiver,
            }));
        }
        // The instance a member's body reads as `this` is no declared parameter, yet an object
        // literal written in the member reads it as the enclosing instance.
        if let Some(value) = self.header.value(symbol) {
            let receiver = self
                .header
                .capture_parameter(
                    symbol,
                    0,
                    crate::ir::IrCapturingCallable::LocalFunction,
                    &self
                        .ir
                        .fn_params
                        .get(&self.function.function)
                        .expect("a body publishes its parameters")
                        .captured_receivers,
                    false,
                )?
                .and_then(|parameter| parameter.receiver);
            if receiver.is_none() {
                return Err(mismatch("an object literal captures an unnamed receiver"));
            }
            return Ok(Some(Capture {
                symbol: (*symbol).clone(),
                name: "this$0".to_owned(),
                ty: self.header.slot_type(value.index),
                crossinline: None,
                receiver,
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
                receiver: None,
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
        // Every member is declared before any body is lowered, so a body calls the members the
        // object declares, its properties' accessors included, as it calls any function.
        let mut declared = HashMap::default();
        for member in &class.members {
            let functions: Vec<_> = match member {
                KlibIrMember::Function(id) if !self.arena.function(*id).constructor => vec![*id],
                KlibIrMember::Property(property) => property
                    .getter
                    .iter()
                    .chain(&property.setter)
                    .copied()
                    .collect(),
                _ => Vec::new(),
            };
            for id in functions {
                let function = self.arena.function(id);
                let member = self.declare_member(function, owner)?;
                self.object_members
                    .insert(function.base.symbol.signature.clone(), member.clone());
                declared.insert(id, member);
            }
        }
        for member in &class.members {
            match member {
                KlibIrMember::Function(id) if !self.arena.function(*id).constructor => {
                    let function = self.arena.function(*id).clone();
                    let (fun, ret, params) = self.lower_member(
                        &function,
                        &declared[id],
                        class_id,
                        owner,
                        captures,
                        storage,
                    )?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    self.record_function_override(&function, fun, owner, supertypes, &params, ret)?;
                }
                KlibIrMember::Property(property) => {
                    let Some(getter_id) = property.getter else {
                        return unsupported("an object literal property without a getter");
                    };
                    let name = property.name.clone();
                    let getter = self.arena.function(getter_id).clone();
                    let (fun, ret, _) = self.lower_member(
                        &getter,
                        &declared[&getter_id],
                        class_id,
                        owner,
                        captures,
                        storage,
                    )?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    let setter = match property.setter {
                        Some(setter_id) => {
                            let setter = self.arena.function(setter_id).clone();
                            let (setter, _, _) = self.lower_member(
                                &setter,
                                &declared[&setter_id],
                                class_id,
                                owner,
                                captures,
                                storage,
                            )?;
                            self.ir.classes[class_id as usize].methods.push(setter);
                            Some(setter)
                        }
                        None => None,
                    };
                    let dispatched =
                        self.record_property_override(&name, &getter, fun, owner, supertypes, ret)?;
                    self.ir.classes[class_id as usize]
                        .properties
                        .push(property_declaration(
                            &name,
                            property.base.flags,
                            ret,
                            fun,
                            setter,
                            dispatched,
                        )?);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn declare_member(
        &mut self,
        function: &KlibIrFunction,
        owner: TypeName,
    ) -> Lowered<ObjectMember> {
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
        Ok(ObjectMember {
            fun,
            owner,
            params,
            ret,
        })
    }

    fn lower_member(
        &mut self,
        function: &KlibIrFunction,
        declared: &ObjectMember,
        class: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
    ) -> Lowered<(FunId, Ty, Vec<Ty>)> {
        let (fun, ret, params) = (declared.fun, declared.ret, declared.params.clone());
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
            locals.temporary(&receiver.base.symbol, Ty::obj_name(owner), false)?;
        } else {
            locals.anonymous_slot();
        }
        for parameter in &function.regular_parameters {
            let ty = self.semantic(parameter.ty)?;
            locals.temporary(&parameter.base.symbol, ty, false)?;
        }
        let fields = super::instance_storage::InstanceStorageFrame::object_member(
            class,
            storage.declaring,
            storage.first_index,
            storage.fields.iter().copied(),
            storage.properties.clone(),
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
        class: &KlibIrClass,
        class_id: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
    ) -> Lowered<MemberFrame<'a>> {
        let mut locals = super::super::local_values::LocalValues::after(0);
        // An initializer reads the object itself, and the properties it stores, through the
        // class's own receiver, as a member does through its dispatch receiver.
        match &class.this_receiver {
            Some(receiver) => {
                locals.temporary(&receiver.base.symbol, Ty::obj_name(owner), false)?;
            }
            None => {
                locals.anonymous_slot();
            }
        }
        let fields = super::instance_storage::InstanceStorageFrame::object_member(
            class_id,
            storage.declaring,
            storage.first_index,
            storage.fields.iter().copied(),
            storage.properties.clone(),
            Ty::obj_name(owner),
        );
        Ok(MemberFrame {
            instance_storage: self.instance_storage.replace(fields),
            locals: std::mem::replace(&mut self.locals, locals),
            captures: std::mem::replace(&mut self.captures, captures.clone()),
            member_return: self.member_return.take(),
            member_result: self.member_result.take(),
            compiler_aliases: std::mem::take(&mut self.compiler_aliases),
            returnable_targets: std::mem::take(&mut self.returnable_targets),
            returnable_frames: std::mem::take(&mut self.returnable_frames),
            null_proven: std::mem::take(&mut self.null_proven),
        })
    }

    /// Stores each property with storage's initializer into its field, in declaration order.
    fn initialize_object(
        &mut self,
        class: &KlibIrClass,
        class_id: ClassId,
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        storage: &ObjectStorage<'a>,
        stored: &[(&KlibIrField, Ty, u32, bool)],
    ) -> Lowered<()> {
        if stored.iter().all(|(field, ..)| field.initializer.is_none()) {
            return Ok(());
        }
        let frame = self.enter_initializer(class, class_id, owner, captures, storage)?;
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

    /// An override of the accessor `record` names, which this unit emits as a method of the
    /// class it materializes: the override edge of a function without parameters.
    fn record_accessor_method_override(
        &mut self,
        getter: &KlibIrFunction,
        fun: FunId,
        owner: TypeName,
        record: super::super::same_library_class::MemberOverride,
        interface: bool,
        ty: Ty,
    ) -> Lowered<()> {
        let target = crate::fir::ResolvedFunctionOverrideTarget::External(
            self.linker.external_override(&record.identity)?,
        );
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
                overridden_semantic_role: record.semantic_role,
                collection_barrier: None,
                overridden_is_interface: interface,
                name: getter.name.clone(),
                declared_parameters: record.declared_parameters,
                declared_result: record.declared_result,
                applied_parameters: record.applied_parameters,
                applied_result: record.applied_result,
                implementation_parameters: Vec::new(),
                implementation_parameter_identities: Vec::new(),
                overridden_parameter_identities: Vec::new(),
                implementation_result: ty,
                suspend: false,
                has_kotlin_superclass_override: false,
                depth: 1,
            });
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
    ) -> Lowered<bool> {
        use crate::fir::ResolvedPropertyOverrideTarget;
        let mut dispatched = false;
        for symbol in &getter.overridden {
            let Some(record) = self.linker.dependency_override(symbol, supertypes, true)? else {
                continue;
            };
            let external = self.linker.external_override(&record.identity)?;
            let interface = self.linker.is_interface(record.owner)?;
            // An accessor a class this unit materializes declares is a method of that class,
            // whose slot a function override edge reaches.
            if self.ir.dependency_methods.contains_key(&external) {
                self.record_accessor_method_override(getter, fun, owner, record, interface, ty)?;
                continue;
            }
            let target = ResolvedPropertyOverrideTarget::External(external);
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
                    declared_contexts: Vec::new(),
                    implementation_contexts: Vec::new(),
                    declared_context_identities: Vec::new(),
                    overridden_setter_value: None,
                    implementation_mutable: false,
                    has_kotlin_superclass_override: false,
                    depth: 1,
                });
            dispatched = true;
        }
        Ok(dispatched)
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
    null_proven: HashSet<super::safe_calls::NullProvenRead>,
}

/// An object literal property, with the modality its serialized declaration flags state: only an
/// `open` or `abstract` one (an `override` not marked `final` included) dispatches, and only when it
/// overrides a property of a supertype the unit does not materialize; an accessor of a class the
/// unit materializes is overridden as that class's method, so the property itself stays final.
fn property_declaration(
    name: &str,
    flags: u64,
    ty: Ty,
    getter: FunId,
    setter: Option<FunId>,
    dispatched: bool,
) -> Lowered<IrProperty> {
    use crate::metadata::semantic::KotlinModality;
    let modality = match crate::metadata::semantic::declaration_modality(flags) {
        KotlinModality::Sealed => return Err(mismatch("an object literal property is sealed")),
        _ if !dispatched => crate::ir::IrPropertyModality::Final,
        KotlinModality::Final => crate::ir::IrPropertyModality::Final,
        KotlinModality::Open => crate::ir::IrPropertyModality::Open,
        KotlinModality::Abstract => crate::ir::IrPropertyModality::Abstract,
    };
    Ok(IrProperty {
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
        is_open: modality != crate::ir::IrPropertyModality::Final,
        modifiers: crate::ir::IrPropertyModifiers {
            modality,
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
    })
}
