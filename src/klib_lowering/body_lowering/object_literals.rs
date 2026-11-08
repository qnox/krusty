//! Prepared object literals.
//!
//! kotlinc serializes an object expression inside an inline function as a block whose origin is
//! `OBJECT_LITERAL`: the local class, then a call of its constructor. The class's members still
//! read the inline function's parameters. Checked FIR stores each of those reads as a constructor
//! field and reads the field, which is the class an inlined call copies. This module lowers that
//! shape. A local class written as its own statement stays declined.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    ClassId, ExprId, FunId, IrClass, IrConstructorCapture, IrCtorArg, IrCtorParameterProvenance,
    IrExpr, IrField, IrFunction, IrFunctionOverride, IrProperty, IrPropertyOverride,
};
use crate::metadata::klib_ir::tree::{
    KlibIrClass, KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrStatement,
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

/// The dependency declaration an object member overrides.
///
/// `owner` is the classifier that declares the member. `declared_*` is that declaration's own
/// signature, and `applied_*` is the same signature through the object's supertype. A property
/// edge names the getter's callable, which is the identity a property slot records; its type is
/// the signature's result.
pub(in crate::klib_lowering) struct DependencyOverride {
    pub(in crate::klib_lowering) target: crate::fir::ExternalCallableId,
    pub(in crate::klib_lowering) kind: DependencyOverrideKind,
    pub(in crate::klib_lowering) owner: TypeName,
    pub(in crate::klib_lowering) interface: bool,
    pub(in crate::klib_lowering) declared_parameters: Vec<Ty>,
    pub(in crate::klib_lowering) declared_result: Ty,
    pub(in crate::klib_lowering) applied_parameters: Vec<Ty>,
    pub(in crate::klib_lowering) applied_result: Ty,
}

/// Whether an override edge publishes a function or the getter of a property.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::klib_lowering) enum DependencyOverrideKind {
    Function,
    Property,
}

/// The authenticated override `realization` names, viewed through one of `supertypes`.
///
/// The declaring classifier comes from the realization. `interface` is that classifier's kind.
/// The applied signature substitutes the classifier's own type parameters with the arguments of
/// the supertype whose hierarchy reaches it. A supertype is never selected by its position.
pub(in crate::klib_lowering) fn authenticated_override(
    realization: &crate::libraries::ExternalCallableRealization,
    target: crate::fir::ExternalCallableId,
    kind: DependencyOverrideKind,
    interface: bool,
    supertypes: &[Ty],
    applied_supertype: impl Fn(Ty, TypeName) -> Option<Ty>,
) -> Lowered<DependencyOverride> {
    let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
        realization.declaration_owner
    else {
        return Err(mismatch(
            "an override's declaration has no classifier owner",
        ));
    };
    if realization
        .klib_body_class_contract
        .as_ref()
        .is_some_and(|contract| contract.classifier != owner)
    {
        return Err(mismatch("an override's class contract names another owner"));
    }
    let (declared_parameters, declared_result) = declared_signature(realization);
    if kind == DependencyOverrideKind::Property && !declared_parameters.is_empty() {
        return Err(mismatch(
            "an object literal overrides a property with parameters",
        ));
    }
    let applied = applied_owner(supertypes, owner, &applied_supertype)?;
    let (applied_parameters, applied_result) =
        applied_signature(realization, applied, &declared_parameters, declared_result)?;
    Ok(DependencyOverride {
        target,
        kind,
        owner,
        interface,
        declared_parameters,
        declared_result,
        applied_parameters,
        applied_result,
    })
}

/// The declaration's own parameters and result, before the object's type arguments: its
/// value-class spelling, else its generic signature, else the shape the provider published.
fn declared_signature(
    realization: &crate::libraries::ExternalCallableRealization,
) -> (Vec<Ty>, Ty) {
    let callable = &realization.callable;
    let parameters = callable
        .declared_params
        .clone()
        .map(|parameters| parameters.to_vec())
        .or_else(|| {
            callable.generic_sig.as_ref().map(|signature| {
                if realization.kind == crate::libraries::ExternalCallableKind::Extension {
                    signature
                        .parameters_with_receiver(callable.context_count)
                        .to_vec()
                } else {
                    signature.params.clone()
                }
            })
        })
        .unwrap_or_else(|| callable.params.clone());
    let result = callable
        .declared_ret
        .or_else(|| callable.generic_sig.as_ref().map(|signature| signature.ret))
        .unwrap_or(callable.ret);
    (parameters, result)
}

fn applied_owner(
    supertypes: &[Ty],
    owner: TypeName,
    applied_supertype: &impl Fn(Ty, TypeName) -> Option<Ty>,
) -> Lowered<Ty> {
    let mut found = None;
    for supertype in supertypes.iter().copied() {
        let candidate = supertype.non_null();
        let view = if candidate.kotlin_class_internal() == Some(owner) {
            Some(candidate)
        } else {
            applied_supertype(candidate, owner)
        };
        let Some(view) = view else {
            continue;
        };
        if view.non_null().kotlin_class_internal() != Some(owner) {
            return Err(mismatch("an applied supertype names another classifier"));
        }
        if found.replace(view.non_null()).is_some() {
            return Err(mismatch("an object repeats one overridden supertype"));
        }
    }
    found.ok_or_else(|| mismatch("an override's declaring type is not a supertype of the object"))
}

fn applied_signature(
    realization: &crate::libraries::ExternalCallableRealization,
    applied: Ty,
    parameters: &[Ty],
    result: Ty,
) -> Lowered<(Vec<Ty>, Ty)> {
    let arguments = applied.type_args();
    let Some(contract) = realization.klib_body_class_contract.as_ref() else {
        if arguments.is_empty() {
            return Ok((parameters.to_vec(), result));
        }
        return Err(mismatch(
            "an override's declaring class has no type-parameter contract",
        ));
    };
    let formals = &contract.parameters.formals;
    let own = contract.parameters.own_count;
    if own > formals.len() || arguments.len() != own {
        return Err(mismatch(
            "an override's declaring type has another number of type arguments",
        ));
    }
    if own == 0 {
        return Ok((parameters.to_vec(), result));
    }
    let mut bindings = crate::fx_hash::HashMap::default();
    for (formal, argument) in formals[..own]
        .iter()
        .copied()
        .zip(arguments.iter().copied())
    {
        let Some(name) = formal.ty_param_name() else {
            return Err(mismatch(
                "a declaring type parameter is not a type parameter",
            ));
        };
        if bindings
            .insert(crate::types::TyParamName::new(name), argument)
            .is_some()
        {
            return Err(mismatch("a declaring type repeats a type parameter"));
        }
    }
    Ok((
        parameters
            .iter()
            .copied()
            .map(|parameter| crate::types::ty_subst_keep_unbound(parameter, &bindings))
            .collect(),
        crate::types::ty_subst_keep_unbound(result, &bindings),
    ))
}

struct Capture {
    symbol: KlibIrSymbol,
    name: String,
    ty: Ty,
    crossinline: Option<u32>,
}

impl BodyLowering<'_> {
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
        let class = self.arena.class(*class_id).clone();
        self.prepared_constructor(&class)?;
        let captures = self.captures_of(&class)?;
        let (superclass, interfaces, supertypes) = self.supertypes(&class)?;
        let class_name = crate::types::type_name(&format!(
            "__krusty_object_{}_{}",
            self.function.function,
            self.ir.classes.len()
        ));
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
        declared.constructor_prefix_count = capture_count;
        declared.ctor_param_count = capture_count;
        declared.fields = captures
            .iter()
            .map(|capture| IrField::new(capture.name.clone(), capture.ty).with_is_final(true))
            .collect();
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
                }),
                provenance: IrCtorParameterProvenance::Capture,
                capture_identity: None,
            })
            .collect();
        let class_id = self.ir.add_class(declared);
        let owner = self.ir.classes[class_id as usize].fq_name_id();
        self.local_classes.insert(class.base.symbol.clone(), owner);
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
        self.lower_members(&class, class_id, owner, &supertypes, &capture_fields)?;
        let mut args = Vec::with_capacity(captures.len());
        let mut params = Vec::with_capacity(captures.len());
        for capture in &captures {
            args.push(self.read(&capture.symbol)?);
            params.push(capture.ty);
        }
        let constructed = self.ir.add_expr(IrExpr::New {
            internal: owner,
            args,
            ctor_params: Some(params),
            declared_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: u32::try_from(captures.len()).expect("a capture list fits u32"),
        });
        Ok(self.typed(constructed, Ty::obj_name(owner)))
    }

    fn prepared_constructor(&self, class: &KlibIrClass) -> Lowered<()> {
        for member in &class.members {
            match member {
                KlibIrMember::Function(id) if self.arena.function(*id).constructor => {
                    self.constructor_body(self.arena.function(*id))?;
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
        Ok(())
    }

    fn constructor_body(&self, function: &KlibIrFunction) -> Lowered<()> {
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
        if !any {
            return unsupported("an object literal with a superclass constructor argument");
        }
        if !matches!(
            self.arena.expr(*initializer).kind,
            KlibIrExprKind::InstanceInitializerCall(_)
        ) {
            return unsupported("an object literal constructor with its own statements");
        }
        Ok(())
    }

    fn captures_of(&self, class: &KlibIrClass) -> Lowered<Vec<Capture>> {
        let mut seen = HashSet::default();
        let mut captures = Vec::new();
        for member in &class.members {
            let function = match member {
                KlibIrMember::Function(id) => Some(self.arena.function(*id)),
                KlibIrMember::Property(property) => {
                    property.getter.map(|getter| self.arena.function(getter))
                }
                _ => None,
            };
            let Some(function) = function else {
                continue;
            };
            for symbol in self.arena.lexical_value_symbols(function) {
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
            let name = identity
                .source_name
                .clone()
                .ok_or_else(|| mismatch("an object literal captures an unnamed parameter"))?;
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
            let ty = self.resolve_type(*supertype)?;
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
    ) -> Lowered<()> {
        for member in &class.members {
            match member {
                KlibIrMember::Function(id) if !self.arena.function(*id).constructor => {
                    let function = self.arena.function(*id).clone();
                    let (fun, ret, params) = self.lower_member(&function, owner, captures)?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    self.record_function_override(&function, fun, owner, supertypes, &params, ret)?;
                }
                KlibIrMember::Property(property) => {
                    let Some(getter_id) = property.getter else {
                        return unsupported("an object literal property without a getter");
                    };
                    if property.setter.is_some() || property.backing_field.is_some() {
                        return unsupported("an object literal property with storage");
                    }
                    let name = property.name.clone();
                    let getter = self.arena.function(getter_id).clone();
                    let (fun, ret, _) = self.lower_member(&getter, owner, captures)?;
                    self.ir.classes[class_id as usize].methods.push(fun);
                    self.ir.classes[class_id as usize]
                        .properties
                        .push(property_declaration(&name, ret, fun));
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
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
    ) -> Lowered<(FunId, Ty, Vec<Ty>)> {
        let mut params = Vec::new();
        let mut identities = Vec::new();
        for parameter in &function.regular_parameters {
            let ty = self.resolve_type(parameter.ty)?;
            params.push(ty);
            identities.push(crate::fir::ResolvedParameterIdentity::Source(
                parameter.name.clone().into_boxed_str(),
            ));
        }
        let ret = self.resolve_type(function.return_type)?;
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
            Some(_) => {
                return Err(crate::klib_lowering::decline::KlibBodyDeclineReason::SyntheticBody)
            }
            None => return Err(crate::klib_lowering::decline::KlibBodyDeclineReason::NoBody),
        };
        let frame = self.enter_member(function, owner, captures, ret)?;
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
        owner: TypeName,
        captures: &HashMap<KlibIrSymbol, CaptureField>,
        ret: Ty,
    ) -> Lowered<MemberFrame> {
        let mut locals = super::super::local_values::LocalValues::after(0);
        if let Some(receiver) = &function.dispatch_receiver {
            locals.temporary(&receiver.base.symbol, Ty::obj_name(owner))?;
        } else {
            locals.anonymous_slot();
        }
        for parameter in &function.regular_parameters {
            let ty = self.resolve_type(parameter.ty)?;
            locals.temporary(&parameter.base.symbol, ty)?;
        }
        Ok(MemberFrame {
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

    fn exit_member(&mut self, frame: MemberFrame) {
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
            let Some(record) = self.linker.dependency_override(symbol, supertypes)? else {
                continue;
            };
            if record.kind != DependencyOverrideKind::Function {
                return Err(mismatch("a function overrides a property"));
            }
            let target = ResolvedFunctionOverrideTarget::External(record.target);
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
                    overridden_is_interface: record.interface,
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
            let Some(record) = self.linker.dependency_override(symbol, supertypes)? else {
                continue;
            };
            if record.kind != DependencyOverrideKind::Property {
                return Err(mismatch("a property overrides a function"));
            }
            let target = ResolvedPropertyOverrideTarget::External(record.target);
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
                    overridden_is_interface: record.interface,
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

struct MemberFrame {
    locals: super::super::local_values::LocalValues,
    captures: HashMap<KlibIrSymbol, CaptureField>,
    member_return: Option<KlibIrSymbol>,
    member_result: Option<Ty>,
    compiler_aliases: HashMap<KlibIrSymbol, crate::metadata::klib_ir::tree::KlibIrExprId>,
    returnable_targets: Vec<KlibIrSymbol>,
    returnable_frames: Vec<super::compiler_blocks::ReturnableFrame>,
    null_proven: HashSet<KlibIrSymbol>,
}

fn property_declaration(name: &str, ty: Ty, getter: FunId) -> IrProperty {
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
        is_var: false,
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
        setter: None,
        needs_access_bridge: false,
        accessor_annotations: Default::default(),
    }
}
