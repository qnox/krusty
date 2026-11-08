//! A non-public class used by a body of the library that declares it.
//!
//! A file-local signature names a declaration only inside the KLIB that serialized it. The same
//! file and slot in another library are a different declaration and stay refused. The class is
//! not published, so lowering materializes it in the unit that lowers the body, and the
//! constructor call builds that class instead of an external dependency constructor.

use super::class_storage::member_field;
use super::constructor_calls::{require_constructor_kind, ConstructorDeclaration};
use super::decline::KlibBodyDeclineReason;
use super::klib_types::{semantic_type_in, KlibTypeParameters};
use crate::fx_hash::HashMap;
use crate::ir::{
    IrClass, IrConstructorAccess, IrConstructorTarget, IrCtorArg, IrCtorParameterProvenance,
    IrField, IrFile,
};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::KlibBodyClassParameters;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrFunction,
    KlibIrMember, KlibIrStatement, KlibIrTypeParameter, KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::semantic::{
    class_is_value, class_kind, declaration_modality, declaration_visibility,
};
use crate::types::{Ty, TyKind, TypeName, TypeVariance, Visibility};

/// The constructor a same-library file-local construction invokes.
pub(super) struct LocalConstructor {
    pub(super) declaration: ConstructorDeclaration,
    pub(super) access: IrConstructorAccess,
}

/// One file-local class materialized in the unit that lowers its library's bodies.
pub(super) struct MaterializedLocalClass {
    pub(super) class: crate::ir::ClassId,
    classifier: TypeName,
    constructor: KlibIrSignature,
    declaration: ConstructorDeclaration,
    access: IrConstructorAccess,
    formals: Vec<Ty>,
    supertypes: Vec<Ty>,
}

/// Where a lowering reads the library that owns the body, and how it checks a root delegation.
pub(super) struct LocalClassSource<'a> {
    pub(super) bodies: &'a KlibDeclarationBodies,
    pub(super) module: &'a str,
    pub(super) validate_root:
        &'a dyn Fn(&KlibPublicIdSignature) -> Result<(), KlibBodyDeclineReason>,
}

/// The classifier identity of a file-local class in `module`. The file and slot are the identity;
/// the source spelling is not.
pub(super) fn classifier_name(module: &str, signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::FileLocal { file, slot } = signature else {
        return None;
    };
    Some(
        TypeName::ROOT
            .nested_child("klib-local")
            .nested_child(module)
            .nested_child(&file.to_string())
            .nested_child(&slot.to_string()),
    )
}

/// The classifier `module` itself declares at `signature`, when that declaration is a class.
pub(super) fn classifier(
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &KlibIrSignature,
) -> Result<Option<TypeName>, KlibBodyDeclineReason> {
    let Some(name) = classifier_name(module, signature) else {
        return Ok(None);
    };
    let Some((_, class)) = bodies.module_local_class(module, signature) else {
        return Ok(None);
    };
    if class.base.symbol.kind != KlibIrSymbolKind::Class
        || class.base.symbol.signature != *signature
    {
        return Ok(None);
    }
    if class_is_value(class.base.flags) || class.value_class_representation.is_some() {
        return Err(unsupported("a non-public value class"));
    }
    match class_kind(class.base.flags) {
        crate::libraries::TypeKind::Class => Ok(Some(name)),
        _ => Err(unsupported("a non-public classifier other than a class")),
    }
}

/// Whether a same-library class materialized in `classes` makes `sub` a subtype of `sup`.
/// A class from another library is absent here, so it stays outside this relation.
pub(super) fn is_subtype(
    classes: &HashMap<KlibIrSignature, MaterializedLocalClass>,
    sub: Ty,
    sup: Ty,
    public_subtype: &dyn Fn(Ty, Ty) -> bool,
) -> bool {
    public_subtype(sub, sup) || proves_local(classes, sub, sup, public_subtype, 0)
}

fn proves_local(
    classes: &HashMap<KlibIrSignature, MaterializedLocalClass>,
    sub: Ty,
    sup: Ty,
    public_subtype: &dyn Fn(Ty, Ty) -> bool,
    depth: u8,
) -> bool {
    if depth == 8 {
        return false;
    }
    if let TyKind::Nullable(inner) = sub.kind() {
        return match sup.kind() {
            TyKind::Nullable(target) => {
                proves_local(classes, inner, sup, public_subtype, depth + 1)
                    || proves_local(classes, inner, target, public_subtype, depth + 1)
            }
            _ => false,
        };
    }
    let TyKind::Obj(name, arguments) = sub.kind() else {
        return false;
    };
    let Some(local) = classes.values().find(|class| class.classifier == name) else {
        return false;
    };
    if arguments.len() != local.formals.len() {
        return false;
    }
    let mut bindings = HashMap::default();
    for (formal, argument) in local.formals.iter().zip(arguments.iter()) {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return false;
        };
        bindings.insert(identity.as_str().to_owned(), *argument);
    }
    for declared in &local.supertypes {
        let applied = crate::types::ty_subst_applied_arguments(*declared, &bindings);
        if applied == sub {
            continue;
        }
        if applied == sup
            || public_subtype(applied, sup)
            || proves_local(classes, applied, sup, public_subtype, depth + 1)
        {
            return true;
        }
    }
    false
}

/// The file-local constructor `signature` of the body module, materialized once per class.
pub(super) fn link(
    source: LocalClassSource<'_>,
    signature: &KlibIrSignature,
    ir: &mut IrFile,
    cache: &mut HashMap<KlibIrSignature, MaterializedLocalClass>,
) -> Result<LocalConstructor, KlibBodyDeclineReason> {
    let Some((arena, function)) = source
        .bodies
        .module_local_function(source.module, signature)
    else {
        return Err(unsupported("a call of a file-private constructor"));
    };
    require_constructor_kind(function)?;
    if function.dispatch_receiver.is_some()
        || function.extension_receiver.is_some()
        || !function.context_parameters.is_empty()
        || function.companion_extension_class.is_some()
        || !function.overridden.is_empty()
    {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a constructor receiver or context",
        ));
    }
    if !function.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "constructor-owned type parameters",
        ));
    }
    let class_signature = constructor_class(arena, function)?;
    if let Some(known) = cache.get(&class_signature) {
        if known.constructor != *signature {
            return Err(unsupported("a non-public class with another constructor"));
        }
        return Ok(LocalConstructor {
            declaration: known.declaration.clone(),
            access: known.access,
        });
    }
    let Some((class_arena, class)) = source
        .bodies
        .module_local_class(source.module, &class_signature)
    else {
        return Err(non_public_type());
    };
    if !std::ptr::eq(arena, class_arena) {
        return Err(mismatch("a constructor is not a member of its class"));
    }
    let materialized = materialize(source, signature, arena, class, function, ir)?;
    let realized = LocalConstructor {
        declaration: materialized.declaration.clone(),
        access: materialized.access,
    };
    cache.insert(class_signature, materialized);
    Ok(realized)
}

fn constructor_class(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<KlibIrSignature, KlibBodyDeclineReason> {
    let crate::metadata::klib_ir::tree::KlibIrType::Simple {
        classifier,
        arguments,
        ..
    } = arena.ty(function.return_type)
    else {
        return Err(mismatch("a constructor result names another class"));
    };
    if classifier.kind != KlibIrSymbolKind::Class {
        return Err(mismatch("a constructor result names another class"));
    }
    let _ = arguments;
    match &classifier.signature {
        KlibIrSignature::FileLocal { .. } => Ok(classifier.signature.clone()),
        KlibIrSignature::Public(_) | KlibIrSignature::Accessor(_) => Err(non_public_type()),
    }
}

fn materialize(
    source: LocalClassSource<'_>,
    signature: &KlibIrSignature,
    arena: &KlibIrArena,
    class: &KlibIrClass,
    function: &KlibIrFunction,
    ir: &mut IrFile,
) -> Result<MaterializedLocalClass, KlibBodyDeclineReason> {
    let class_name = classifier(source.bodies, source.module, &class.base.symbol.signature)?
        .ok_or_else(non_public_type)?;
    if declaration_modality(class.base.flags).is_abstract() {
        return Err(unsupported("a non-public abstract class"));
    }
    if !class.sealed_subclasses.is_empty() {
        return Err(unsupported("a non-public sealed class"));
    }
    if class
        .members
        .iter()
        .any(|member| matches!(member, KlibIrMember::EnumEntry(_)))
    {
        return Err(unsupported("a non-public enum"));
    }
    let constructors = class
        .members
        .iter()
        .filter_map(|member| match member {
            KlibIrMember::Function(id) => {
                let member = arena.function(*id);
                member.constructor.then_some(member)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if constructors.len() != 1 || constructors[0].base.symbol.signature != *signature {
        return Err(unsupported("a non-public class with another constructor"));
    }
    delegates_to_any(arena, &class.base.symbol, function, source.validate_root)?;
    let resolve = |signature: &KlibIrSignature| classifier(source.bodies, source.module, signature);
    let (types, formals, variances, bounds) = class_formals(arena, class, &resolve)?;
    let supertypes = class
        .supertypes
        .iter()
        .map(|supertype| semantic_type_in(arena, &types, *supertype, &resolve))
        .collect::<Result<Vec<_>, _>>()?;
    let self_type = Ty::obj_args_name(class_name, &formals);
    let result = semantic_type_in(arena, &types, function.return_type, &resolve)?;
    if result != self_type {
        return Err(mismatch(
            "a constructor result is not its declared class application",
        ));
    }
    if let Some(receiver) = &class.this_receiver {
        if semantic_type_in(arena, &types, receiver.ty, &resolve)? != self_type {
            return Err(mismatch(
                "a class instance receiver has another declared type",
            ));
        }
    }
    let (fields, field_names) = instance_fields(arena, class, &types, &resolve)?;
    let (params, defaults, ctor_args) =
        constructor_parameters(arena, function, &types, &resolve, &fields, &field_names)?;
    for index in 0..fields.len() {
        if !ctor_args.iter().any(|argument| {
            argument.field_index == Some(u32::try_from(index).expect("field index"))
        }) {
            return Err(unsupported(
                "a non-public class field without a constructor parameter",
            ));
        }
    }
    let mut declaration = IrClass::synthetic(class_name);
    declaration.fields = fields;
    declaration.ctor_param_count = u32::try_from(
        ctor_args
            .iter()
            .filter(|argument| argument.is_field)
            .count(),
    )
    .expect("constructor arity fits u32");
    declaration.ctor_args = ctor_args;
    declaration.explicit_param_stores = false;
    let class_id = ir.add_class(declaration);
    ir.record_declared_class_self_type(class_name, self_type);
    let access = match declaration_visibility(function.base.flags) {
        Visibility::Private => IrConstructorAccess::Private,
        _ => IrConstructorAccess::Unrestricted,
    };
    Ok(MaterializedLocalClass {
        class: class_id,
        classifier: class_name,
        constructor: signature.clone(),
        declaration: ConstructorDeclaration::local(
            class_name,
            params,
            KlibBodyClassParameters {
                formals: formals.clone().into_boxed_slice(),
                bounds: bounds.into_boxed_slice(),
                variances: variances.into_boxed_slice(),
                own_count: formals.len(),
            },
            defaults.into_iter().collect(),
        ),
        access,
        formals,
        supertypes,
    })
}

fn class_formals(
    arena: &KlibIrArena,
    class: &KlibIrClass,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<(KlibTypeParameters, Vec<Ty>, Vec<TypeVariance>, Vec<Vec<Ty>>), KlibBodyDeclineReason> {
    let mut types = KlibTypeParameters::default();
    let mut formals = Vec::with_capacity(class.type_parameters.len());
    let mut variances = Vec::with_capacity(class.type_parameters.len());
    for parameter in &class.type_parameters {
        if parameter.is_reified() {
            return Err(unsupported(
                "a reified type parameter of a non-public class",
            ));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter {
            return Err(mismatch("a non-public class type parameter has no symbol"));
        }
        let variance = variance(parameter)?;
        let formal = Ty::ty_param(
            crate::types::metadata_type_parameter(&parameter.name),
            Ty::nullable(Ty::obj("kotlin/Any")),
        );
        if !types.bind(&parameter.base.symbol, formal) {
            return Err(mismatch("a non-public class repeats a type parameter"));
        }
        formals.push(formal);
        variances.push(variance);
    }
    let mut bounds = Vec::with_capacity(class.type_parameters.len());
    for parameter in &class.type_parameters {
        let mut parameter_bounds = Vec::with_capacity(parameter.supertypes.len());
        for bound in &parameter.supertypes {
            parameter_bounds.push(semantic_type_in(arena, &types, *bound, resolve)?);
        }
        bounds.push(parameter_bounds);
    }
    Ok((types, formals, variances, bounds))
}

fn variance(parameter: &KlibIrTypeParameter) -> Result<TypeVariance, KlibBodyDeclineReason> {
    match parameter.variance() {
        Some(KlibIrVariance::Invariant) => Ok(TypeVariance::Invariant),
        Some(KlibIrVariance::In) => Ok(TypeVariance::In),
        Some(KlibIrVariance::Out) => Ok(TypeVariance::Out),
        None => Err(mismatch(
            "a non-public class type parameter has no variance",
        )),
    }
}

fn instance_fields(
    arena: &KlibIrArena,
    class: &KlibIrClass,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<(Vec<IrField>, HashMap<String, usize>), KlibBodyDeclineReason> {
    let mut fields = Vec::new();
    let mut names = HashMap::default();
    for member in &class.members {
        match member {
            KlibIrMember::AnonymousInitializer { statements, .. } if !statements.is_empty() => {
                return Err(unsupported("an initializer of a non-public class"));
            }
            _ => {}
        }
        let Some(field) = member_field(member) else {
            continue;
        };
        if field.initializer.is_some() {
            return Err(unsupported("a non-public class field initializer"));
        }
        if !matches!(
            field.base.symbol.kind,
            KlibIrSymbolKind::Field | KlibIrSymbolKind::StandaloneField
        ) {
            return Err(mismatch("a class field has an invalid storage identity"));
        }
        let flags = field
            .instance_storage_flags()
            .ok_or_else(|| unsupported("an external, static or unknown field storage flag"))?;
        let ty = semantic_type_in(arena, types, field.ty, resolve)?;
        if names.insert(field.name.clone(), fields.len()).is_some() {
            return Err(mismatch("a non-public class repeats a field"));
        }
        fields.push(
            IrField::new(field.name.clone(), ty)
                .with_is_final(flags.is_final)
                .with_is_private(flags.visibility == Visibility::Private),
        );
    }
    Ok((fields, names))
}

fn constructor_parameters(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    types: &KlibTypeParameters,
    resolve: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
    fields: &[IrField],
    field_names: &HashMap<String, usize>,
) -> Result<(Vec<Ty>, Vec<bool>, Vec<IrCtorArg>), KlibBodyDeclineReason> {
    let mut params = Vec::with_capacity(function.regular_parameters.len());
    let mut defaults = Vec::with_capacity(function.regular_parameters.len());
    let mut arguments = Vec::with_capacity(function.regular_parameters.len());
    for parameter in &function.regular_parameters {
        if parameter.vararg_element_type.is_some() {
            return Err(unsupported("a vararg constructor of a non-public class"));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::ValueParameter {
            return Err(mismatch("a constructor parameter has no value symbol"));
        }
        let ty = semantic_type_in(arena, types, parameter.ty, resolve)?;
        let field_index = match field_names.get(&parameter.name) {
            Some(&index) => {
                if fields[index].ty != ty {
                    return Err(mismatch("a constructor parameter disagrees with its field"));
                }
                Some(u32::try_from(index).expect("field index"))
            }
            None => None,
        };
        defaults.push(parameter.default_value.is_some());
        params.push(ty);
        arguments.push(IrCtorArg {
            name: Some(parameter.name.clone()),
            context_kind: crate::types::ContextParameterKind::None,
            ty,
            declared_ty: Some(ty),
            is_field: field_index.is_some(),
            field_index,
            has_default: parameter.default_value.is_some(),
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: IrCtorParameterProvenance::Value,
            capture_identity: None,
        });
    }
    Ok((params, defaults, arguments))
}

fn delegates_to_any(
    arena: &KlibIrArena,
    class_symbol: &KlibIrSymbol,
    function: &KlibIrFunction,
    validate_root: &dyn Fn(&KlibPublicIdSignature) -> Result<(), KlibBodyDeclineReason>,
) -> Result<(), KlibBodyDeclineReason> {
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return Err(unsupported("a nontrivial primary constructor body"));
    };
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        return Err(unsupported("a nontrivial primary constructor body"));
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return Err(mismatch(
            "a primary constructor does not delegate before initialization",
        ));
    };
    let no_operands = match &access.arguments {
        KlibIrArguments::Flat(values) => values.is_empty(),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => dispatch_receiver.is_none() && extension_receiver.is_none() && values.is_empty(),
    };
    let root = match &access.symbol.signature {
        KlibIrSignature::Public(signature) => {
            super::ir_builtins::root_constructor(signature, || validate_root(signature))?
        }
        _ => false,
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || !no_operands
        || !access.type_arguments.is_empty()
        || !root
        || !matches!(
            &arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(class) if class == class_symbol
        )
    {
        return Err(mismatch(
            "an ordinary class does not delegate to Any and initialize its own instance",
        ));
    }
    Ok(())
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

fn unsupported(form: &'static str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedOperation(form)
}

fn non_public_type() -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedType("a type of a non-public class")
}

/// The construction target recorded for a same-library primary constructor.
pub(super) fn construction_target(access: IrConstructorAccess) -> IrConstructorTarget {
    IrConstructorTarget { ordinal: 0, access }
}
