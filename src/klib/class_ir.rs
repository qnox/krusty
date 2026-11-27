//! The classes of a KLIB's ordinary IR component: each class declaration with its constructors,
//! properties and member functions, laid out as Kotlin's IR serializer writes them.
//!
//! A class is written only when every member is: its primary and secondary constructors with the
//! delegation each makes, the properties its primary constructor declares with their backing
//! fields and default accessors, and member functions whose bodies the body writer translates.
//! Any other class keeps the file's ordinary IR unwritten, since that component lists every
//! declaration of the file or none.

use super::body_encoding::BodyEncoder;
use super::body_tree::{
    constructor_body, expression_body, function_body, property_accessor, supported_type,
    BodyExprId, BodyKind, BodyScope, BodyStatement, CallTarget, FunctionBody, ValueRef,
};
use super::inline_ir::{
    prepared_function, BodyFacts, Component, DeclarationSignatures, FileWriter, PreparedFunction,
    PreparedParameter,
};
use super::ir_tables::{
    lattice, symbol, ANONYMOUS_INITIALIZER_SYMBOL, CLASS_SYMBOL, CONSTRUCTOR_SYMBOL, FIELD_SYMBOL,
    PROPERTY_SYMBOL, RECEIVER_PARAMETER_SYMBOL, VALUE_PARAMETER_SYMBOL,
};
use crate::fir::ResolvedPropertyOverrideTarget;
use crate::ir::{CtorDelegateTarget, IrClass, IrExpr, IrFile, IrLocalPropertyLayout, IrProperty};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::protobuf::Pb;
use crate::types::{Ty, Visibility};

/// Kotlin IR's flag for a primary constructor, beside the function flags a constructor takes.
const PRIMARY_CONSTRUCTOR: u64 = 1 << 15;

pub(super) struct PreparedClass {
    signature: KlibPublicIdSignature,
    source_order: u32,
    name: String,
    flags: u64,
    self_type: Ty,
    supertypes: Vec<Ty>,
    members: Vec<PreparedMember>,
}

enum PreparedMember {
    Constructor(PreparedConstructor),
    Property(Box<PreparedProperty>),
    Function(PreparedFunction),
    /// An `init { … }` block's statements, which run in the primary constructor with `this`
    /// and the primary constructor's parameters in scope.
    Initializer(FunctionBody),
}

struct PreparedConstructor {
    signature: KlibPublicIdSignature,
    flags: u64,
    parameters: Vec<(String, Ty)>,
    /// Whether this is the primary constructor, whose parameters the properties' backing fields
    /// are initialized from.
    primary: bool,
    body: FunctionBody,
}

struct PreparedProperty {
    signature: KlibPublicIdSignature,
    name: String,
    flags: u64,
    ty: Ty,
    field: PreparedField,
    getter: PreparedFunction,
    setter: Option<PreparedFunction>,
}

struct PreparedField {
    flags: u64,
    initializer: FieldInitializer,
}

enum FieldInitializer {
    /// The primary-constructor parameter a constructor-declared property is initialized from.
    Parameter(usize),
    /// A body property's initializer expression, evaluated with `this` and the primary
    /// constructor's parameters in scope.
    Expression(FunctionBody, BodyExprId),
}

/// Every class of `classes`, or `None` when one has a declaration or a body this writer does not
/// write yet.
pub(super) fn collect(
    ir: &IrFile,
    file_name: &str,
    classes: &[&IrClass],
    declarations: &DeclarationSignatures,
    facts: &BodyFacts<'_>,
) -> Option<Vec<PreparedClass>> {
    classes
        .iter()
        .map(|class| prepared_class(ir, file_name, class, declarations, facts))
        .collect()
}

fn prepared_class(
    ir: &IrFile,
    file_name: &str,
    class: &IrClass,
    declarations: &DeclarationSignatures,
    facts: &BodyFacts<'_>,
) -> Option<PreparedClass> {
    if !ordinary_class(class) {
        return None;
    }
    let class_id = ir.class_id_by_name(class.fq_name)?;
    let signature = facts.exports.classifier(class_id)?.signature.clone();
    let self_type = Ty::obj_name(class.fq_name);
    let record = ir.class_declarations.get(&class.fq_name_id())?.as_ref()?;

    // The primary constructor and the properties its parameters declare come first, then the
    // body's members in source order.
    let mut members = Vec::new();
    let primary_parameters = class
        .ctor_args
        .iter()
        .map(|argument| Some((argument.name.clone()?, argument.ty)))
        .collect::<Option<Vec<_>>>()?;
    if !primary_parameters
        .iter()
        .all(|(_, ty)| supported_type(*ty, &[]))
    {
        return None;
    }
    // Property initializers and `init` blocks see `this` and the primary constructor's
    // parameters, as the primary constructor's own body does.
    let slots = std::iter::once(self_type)
        .chain(primary_parameters.iter().map(|(_, ty)| *ty))
        .collect::<Vec<_>>();
    let scope = body_scope(ir, declarations, facts, &slots);
    if class.is_interface {
        if !primary_parameters.is_empty() || !class.secondary_ctors.is_empty() {
            return None;
        }
    } else {
        let super_target = superclass_constructor(&scope, class)?;
        // An object's constructor is private, so no other module names it; it is nullary.
        let signature = if class.is_object {
            nullary_constructor(&signature, &class_name(class)?)
        } else {
            public(super::body_tree::constructor_target(
                &scope,
                class.fq_name,
                crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
            )?)?
        };
        members.push(PreparedMember::Constructor(PreparedConstructor {
            signature,
            flags: (record.primary_constructor_flags() & VISIBILITY_BITS) | PRIMARY_CONSTRUCTOR,
            parameters: primary_parameters.clone(),
            primary: true,
            body: constructor_body(&scope, super_target, &class.super_args, Some(class.fq_name))?,
        }));
    }
    let mut body_members = Vec::new();
    for (index, property) in class.properties.iter().enumerate() {
        let prepared = prepared_property(ir, file_name, class, index, property, &scope, facts)?;
        let parameter = match prepared.field.initializer {
            FieldInitializer::Parameter(parameter) => Some(parameter),
            FieldInitializer::Expression(..) => None,
        };
        body_members.push((
            parameter.is_none().then_some(property.source_order),
            parameter,
            PreparedMember::Property(Box::new(prepared)),
        ));
    }
    for (order, body) in initializers(ir, class, &scope)? {
        body_members.push((Some(order), None, PreparedMember::Initializer(body)));
    }
    for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
        if !constructor.prefix_params.is_empty()
            || !constructor.delegate_prelude.is_empty()
            || !constructor.default_parameters.is_empty()
            || constructor.defaults.iter().any(Option::is_some)
            || constructor.vararg_index.is_some()
            || constructor.synthetic
            || constructor.vc_params
            || constructor
                .body
                .is_some_and(|body| !matches!(ir.expr(body), crate::ir::IrExpr::Block { stmts, value: None } if stmts.is_empty()))
        {
            return None;
        }
        let parameters = constructor.named_params.clone();
        if parameters.len() != constructor.params.len()
            || !parameters.iter().all(|(_, ty)| supported_type(*ty, &[]))
        {
            return None;
        }
        let slots = std::iter::once(self_type)
            .chain(parameters.iter().map(|(_, ty)| *ty))
            .collect::<Vec<_>>();
        let scope = body_scope(ir, declarations, facts, &slots);
        let target = match &constructor.delegate {
            CtorDelegateTarget::This {
                target,
                default_masks,
                ..
            } if default_masks.is_empty() => {
                super::body_tree::constructor_target(&scope, class.fq_name, *target)?
            }
            CtorDelegateTarget::Super { default_masks, .. } if default_masks.is_empty() => {
                superclass_constructor(&scope, class)?
            }
            _ => return None,
        };
        let flags = record.secondary_constructor_flags(ordinal)? & VISIBILITY_BITS;
        let initialized = matches!(constructor.delegate, CtorDelegateTarget::Super { .. })
            .then_some(class.fq_name);
        let prepared = PreparedConstructor {
            signature: public(super::body_tree::constructor_target(
                &scope,
                class.fq_name,
                crate::ir::IrConstructorTarget {
                    ordinal: u32::try_from(ordinal + 1).ok()?,
                    access: crate::ir::IrConstructorAccess::Unrestricted,
                },
            )?)?,
            flags,
            parameters,
            primary: false,
            body: constructor_body(&scope, target, &constructor.delegate_args, initialized)?,
        };
        body_members.push((
            Some(constructor.source_order),
            None,
            PreparedMember::Constructor(prepared),
        ));
    }
    for &function in &class.methods {
        let prepared = prepared_function(
            ir,
            file_name,
            declarations,
            facts,
            function,
            ir.inline_fns.contains(&function),
        )?;
        body_members.push((
            Some(*ir.fn_source_order.get(&function)?),
            None,
            PreparedMember::Function(prepared),
        ));
    }
    // Constructor-declared properties keep their parameter order ahead of the body's members.
    body_members.sort_by_key(|(order, parameter, _)| (order.is_some(), *parameter, *order));
    members.extend(body_members.into_iter().map(|(_, _, member)| member));
    Some(PreparedClass {
        signature,
        source_order: ir.class_source_order(class_id)?,
        name: class_name(class)?,
        flags: crate::metadata::declaration_flags::class_flags(ir, class),
        self_type,
        supertypes: supertypes(class)?,
        members,
    })
}

const VISIBILITY_BITS: u64 = 0b1110;

fn body_scope<'a>(
    ir: &'a IrFile,
    declarations: &'a DeclarationSignatures,
    facts: &BodyFacts<'a>,
    parameters: &'a [Ty],
) -> BodyScope<'a> {
    BodyScope {
        ir,
        declarations,
        callables: facts.callables,
        builtins: facts.builtins,
        exports: facts.exports,
        parameters,
        type_parameters: &[],
    }
}

/// A top-level, non-generic class with no annotations, nested classes or special kind.
fn ordinary_class(class: &IrClass) -> bool {
    class.is_source_declared
        && !class.is_compiler_generated
        && matches!(
            class.declaration_context,
            Some(crate::ir::IrClassDeclarationContext::TopLevel)
        )
        && !(class.is_companion
            || class.is_enum
            || class.is_enum_entry
            || class.is_annotation
            || class.is_data
            || class.is_value
            || class.is_full_value
            || class.is_inner_class
            || class.is_local_class
            || class.is_anonymous_object
            || class.is_script
            || class.is_sealed)
        && class.type_params.is_empty()
        && class.captured_type_params.is_empty()
        && class.companion_class.is_none()
        && class.published_nested_classifiers.is_empty()
        && !class.applied_annotations.declares_annotations()
        && !class.explicit_param_stores
        && class.pre_super_param_fields.is_empty()
        && class.super_arg_prelude.is_empty()
        && class.constructor_prefix_count == 0
        && (class.has_primary_ctor || class.is_interface)
        && class.bridges.is_empty()
        && class.installed_interface_overrides.is_empty()
        && class.lambda.is_none()
        && class.sam_wrapper.is_none()
        && class.func_ref.is_none()
        && class.prop_ref.is_none()
        && class.ctor_args.iter().all(|argument| {
            argument.is_field
                && !argument.has_default
                && !argument.is_vararg
                && argument.capture.is_none()
                && argument.type_param.is_none()
        })
        && class.fields.len() == class.properties.len()
}

/// The `init { … }` blocks of `class` with their source order, once each statement of its
/// initialization is either a body property's initializer or one of those blocks. `None` for any
/// other statement.
fn initializers(
    ir: &IrFile,
    class: &IrClass,
    scope: &BodyScope<'_>,
) -> Option<Vec<(u32, FunctionBody)>> {
    let Some(body) = class.init_body else {
        return class
            .properties
            .iter()
            .all(|property| property.initializer.is_none())
            .then(Vec::new);
    };
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    let class_id = ir.class_id_by_name(class.fq_name)?;
    let mut blocks = Vec::new();
    let mut initialized = 0;
    for &statement in stmts {
        let initializes_property = match ir.expr(statement) {
            IrExpr::SetField {
                receiver,
                class: owner,
                index,
                value,
            } => {
                *owner == class_id
                    && matches!(ir.expr(*receiver), IrExpr::GetValue(0))
                    && class.properties.iter().any(|property| {
                        property.initializer == Some(*value)
                            && property.backing_field == Some(*index)
                    })
            }
            _ => false,
        };
        if initializes_property {
            initialized += 1;
            continue;
        }
        let initializer = ir
            .checked_class_initializers
            .iter()
            .find(|initializer| initializer.class == class_id && initializer.body == statement)?;
        blocks.push((initializer.source_order, function_body(scope, statement)?));
    }
    let declared = class
        .properties
        .iter()
        .filter(|property| property.initializer.is_some())
        .count();
    (initialized == declared).then_some(blocks)
}

fn class_name(class: &IrClass) -> Option<String> {
    match class.declaration_name.as_ref()? {
        crate::types::ClassifierDeclarationName::TopLevel { name, .. } => Some(name.to_string()),
        _ => None,
    }
}

/// The supertypes Kotlin records: the ones the class names, or `kotlin.Any` when it names none.
/// `None` when it names both a superclass and an interface, whose source order common IR keeps
/// only in its supertype list.
fn supertypes(class: &IrClass) -> Option<Vec<Ty>> {
    let superclass = !class.superclass.matches("kotlin/Any");
    match (superclass, class.interfaces.is_empty()) {
        (_, true) => Some(vec![Ty::obj_name(class.superclass)]),
        (false, false) => Some(class.interfaces.iter().map(Ty::obj_name).collect()),
        (true, false) => None,
    }
}

/// The superclass constructor a constructor of `class` delegates to.
fn superclass_constructor(scope: &BodyScope<'_>, class: &IrClass) -> Option<CallTarget> {
    if class.superclass.matches("kotlin/Any") {
        if !class.super_args.is_empty() {
            return None;
        }
        return Some(CallTarget::Public(any_constructor()));
    }
    // A superclass of another module names its constructor by an identity common IR does not
    // carry on the class yet.
    super::body_tree::constructor_target(scope, class.superclass, class.super_ctor)
}

fn any_constructor() -> KlibPublicIdSignature {
    nullary_constructor(
        &KlibPublicIdSignature::classifier("kotlin/Any".into()),
        "Any",
    )
}

/// The nullary constructor of the top-level, non-generic class `class` named `name`.
fn nullary_constructor(class: &KlibPublicIdSignature, name: &str) -> KlibPublicIdSignature {
    use crate::metadata::id_signature::{
        callable_signature, CallableShape, ClassScope, DeclarationContainer, Placement,
    };
    use crate::metadata::semantic::KotlinType;

    let classes = [ClassScope::<KotlinType> {
        name,
        type_parameters: Vec::new(),
        expect: false,
    }];
    callable_signature(
        DeclarationContainer {
            package: class.package().segments(),
            classes: &classes,
            native_interop_library: false,
        },
        &CallableShape::<KotlinType> {
            name: "<init>",
            contexts: Vec::new(),
            receiver: None,
            params: Vec::new(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("a nullary constructor of a non-generic class is signable")
}

fn public(target: CallTarget) -> Option<KlibPublicIdSignature> {
    match target {
        CallTarget::Public(signature) => Some(signature),
        CallTarget::Accessor(_) => None,
    }
}

/// A property a primary-constructor parameter declares, with its backing field and default
/// accessors.
fn prepared_property(
    ir: &IrFile,
    file_name: &str,
    class: &IrClass,
    index: usize,
    property: &IrProperty,
    scope: &BodyScope<'_>,
    facts: &BodyFacts<'_>,
) -> Option<PreparedProperty> {
    let modifiers = &property.modifiers;
    if modifiers.declared_getter
        || modifiers.declared_setter
        || modifiers.delegated
        || modifiers.lateinit
        || property.getter.is_some()
        || property.setter.is_some()
        || property.storage_ty.is_some()
        || property.delegate_field.is_some()
        || property.has_constant_initializer
        || !property.context_params.is_empty()
        || !property.type_params.is_empty()
        || !property.annotations.is_empty()
        || property.needs_access_bridge
        || property.visibility != Visibility::Public
        || property.setter_visibility != Visibility::Public
        || !matches!(property.origin, crate::ir::IrPropertyOrigin::Declaration)
    {
        return None;
    }
    let field = property.backing_field?;
    let parameter = class
        .ctor_args
        .iter()
        .position(|argument| argument.field_index == Some(field));
    let initializer = match (parameter, property.initializer) {
        (Some(parameter), None) => FieldInitializer::Parameter(parameter),
        (None, Some(initializer)) => {
            let (body, expression) = expression_body(scope, initializer)?;
            FieldInitializer::Expression(body, expression)
        }
        _ => return None,
    };
    let id = ir
        .local_property_layouts
        .iter()
        .find_map(|(id, layout)| match layout {
            IrLocalPropertyLayout::Member {
                owner,
                property: at,
                ..
            } if *owner == class.fq_name && *at as usize == index => Some(*id),
            _ => None,
        })?;
    let exports = facts.exports;
    let accessor = |setter: bool| -> Option<KlibAccessorIdSignature> {
        let operation = if setter {
            crate::backend::ExportedPropertyAccessor::Setter(id)
        } else {
            crate::backend::ExportedPropertyAccessor::Getter(id)
        };
        let (_, identity) = exports
            .property_accessors()
            .iter()
            .find(|(exported, _)| *exported == operation)?;
        match &identity.declaration {
            KlibDeclarationSignature::Accessor(signature) => Some(signature.clone()),
            _ => None,
        }
    };
    let getter_signature = accessor(false)?;
    let signature = getter_signature.property().clone();
    let self_type = Ty::obj_name(class.fq_name);
    let ty = property.ty;
    if !supported_type(ty, &[]) {
        return None;
    }
    let (overridden_getters, overridden_setters) = overridden_accessors(ir, class, id, facts)?;
    let modality = match modifiers.modality {
        crate::ir::IrPropertyModality::Final => 0,
        crate::ir::IrPropertyModality::Open => 1,
        _ => return None,
    };
    let visibility =
        crate::metadata::declaration_flags::declaration_visibility_bits(property.visibility) << 1;
    let accessor_flags = visibility | (modality << 4);
    let receiver =
        |body: &mut FunctionBody| body.push(self_type, BodyKind::GetValue(ValueRef::Parameter(0)));
    let mut getter_body = FunctionBody::default();
    let this = receiver(&mut getter_body);
    let read = getter_body.push(
        ty,
        BodyKind::GetField {
            property: signature.clone(),
            receiver: Some(this),
        },
    );
    let returned = getter_body.push(Ty::Nothing, BodyKind::Return(read));
    getter_body
        .statements
        .push(BodyStatement::Expression(returned));
    let accessor_function = |signature: KlibAccessorIdSignature,
                             name: String,
                             parameters: Vec<PreparedParameter>,
                             result: Ty,
                             overridden: Vec<CallTarget>,
                             body: FunctionBody| PreparedFunction {
        signature: CallTarget::Accessor(signature),
        name,
        origin: "DEFAULT_PROPERTY_ACCESSOR",
        file_name: file_name.to_owned(),
        flags: accessor_flags,
        inline_only: false,
        inline: false,
        dispatch_receiver: Some(self_type),
        parameters,
        context_count: 0,
        extension_parameter: None,
        result,
        type_parameters: Vec::new(),
        overridden,
        source_order: None,
        body: Some(body),
    };
    let getter = accessor_function(
        getter_signature,
        format!("<get-{}>", property.name),
        Vec::new(),
        ty,
        overridden_getters,
        getter_body,
    );
    let setter = if property.is_var {
        let mut body = FunctionBody::default();
        let this = receiver(&mut body);
        let value = body.push(ty, BodyKind::GetValue(ValueRef::Parameter(1)));
        let write = body.push(
            Ty::Unit,
            BodyKind::SetField {
                property: signature.clone(),
                receiver: Some(this),
                value,
            },
        );
        body.statements.push(BodyStatement::Expression(write));
        Some(accessor_function(
            accessor(true)?,
            format!("<set-{}>", property.name),
            vec![PreparedParameter {
                name: "<set-?>".to_owned(),
                ty,
                default_null: false,
                vararg_element: None,
            }],
            Ty::Unit,
            overridden_setters,
            body,
        ))
    } else {
        None
    };
    let is_var = u64::from(property.is_var);
    Some(PreparedProperty {
        signature,
        name: property.name.clone(),
        // `isVar` (bit 8), `hasGetter` (bit 9) and `hasSetter` (bit 10).
        flags: visibility | (modality << 4) | (is_var << 8) | (1 << 9) | (is_var << 10),
        ty,
        field: PreparedField {
            // A backing field is private, and final unless its property is a `var`.
            flags: (1 << 1) | ((1 - is_var) << 4),
            initializer,
        },
        getter,
        setter,
    })
}

/// The accessors of the direct supertypes' properties that the getter and the setter of `property`
/// override, as kotlinc names them. A setter overrides only a `var`'s setter. `None` when an
/// overridden accessor has no identity this writer can name.
fn overridden_accessors(
    ir: &IrFile,
    class: &IrClass,
    property: crate::fir::PropertyId,
    facts: &BodyFacts<'_>,
) -> Option<(Vec<CallTarget>, Vec<CallTarget>)> {
    let mut getters = Vec::new();
    let mut setters = Vec::new();
    let edges = ir
        .property_overrides
        .values()
        .flatten()
        .filter(|edge| edge.implementation == ResolvedPropertyOverrideTarget::Module(property));
    for edge in edges {
        if edge.implementation_getter.is_some() || edge.implementation_setter.is_some() {
            return None;
        }
        let accessor = |setter: bool| -> Option<KlibAccessorIdSignature> {
            let target = match edge.overridden {
                ResolvedPropertyOverrideTarget::Module(overridden) => {
                    property_accessor(ir, facts.exports, overridden, setter)?
                }
                // An external edge names the overridden getter; its setter is not named yet.
                ResolvedPropertyOverrideTarget::External(getter) => {
                    if setter {
                        return None;
                    }
                    let accessor = getter;
                    match &facts
                        .callables
                        .callable(accessor)?
                        .dependency_callable
                        .as_ref()?
                        .declaration
                    {
                        KlibDeclarationSignature::Accessor(signature) => {
                            CallTarget::Accessor(signature.clone())
                        }
                        _ => return None,
                    }
                }
            };
            match target {
                CallTarget::Accessor(signature) => Some(signature),
                CallTarget::Public(_) => None,
            }
        };
        let names = super::inline_ir::inherited_names(ir, facts, class, edge.overridden_owner)?;
        let record = |signature: KlibAccessorIdSignature, into: &mut Vec<CallTarget>| {
            for name in &names {
                let named = CallTarget::Accessor(match name {
                    None => signature.clone(),
                    Some(supertype) => signature.as_member_of(supertype)?,
                });
                if !into.contains(&named) {
                    into.push(named);
                }
            }
            Some(())
        };
        record(accessor(false)?, &mut getters)?;
        if edge.overridden_setter_value.is_some() {
            record(accessor(true)?, &mut setters)?;
        }
    }
    Some((getters, setters))
}

impl PreparedClass {
    /// The class's place among its file's declarations, shared with functions.
    pub(super) fn source_order(&self) -> u32 {
        self.source_order
    }
}

/// Writes classes into one file's tables.
pub(super) struct ClassWriter<'w> {
    writer: &'w mut FileWriter,
}

impl<'w> ClassWriter<'w> {
    pub(super) fn new(writer: &'w mut FileWriter) -> Self {
        Self { writer }
    }

    /// The class's `IrDeclaration`, keyed by its signature's table index.
    pub(super) fn class(mut self, class: &PreparedClass) -> (u32, Vec<u8>) {
        let tables = &mut self.writer.tables;
        let signature = tables.public_signature(&class.signature);
        let mut base = Pb::new();
        base.field_varint(1, symbol(signature, CLASS_SYMBOL));
        base.field_varint(2, u64::from(tables.string("DEFINED")));
        base.field_varint(4, class.flags);
        let this_symbol = symbol(tables.local_signature(), RECEIVER_PARAMETER_SYMBOL);
        let this_receiver =
            self.parameter("<this>", class.self_type, this_symbol, "INSTANCE_RECEIVER");
        let tables = &mut self.writer.tables;
        let name = tables.string(&class.name);
        let mut encoded = Pb::new();
        encoded.field_message(1, &base);
        encoded.field_varint(2, u64::from(name));
        encoded.field_message(3, &this_receiver);
        let mut primary_parameters = Vec::new();
        for member in &class.members {
            let member = match member {
                PreparedMember::Constructor(constructor) => self.constructor(
                    constructor,
                    class.self_type,
                    this_symbol,
                    &mut primary_parameters,
                ),
                PreparedMember::Property(property) => {
                    self.property(property, this_symbol, &primary_parameters)
                }
                PreparedMember::Function(function) => {
                    let (_, declaration) = self.writer.function(function, Component::Main);
                    declaration
                }
                PreparedMember::Initializer(body) => {
                    self.initializer(body, this_symbol, &primary_parameters)
                }
            };
            encoded.field_bytes(5, &member);
        }
        let supertypes = class
            .supertypes
            .iter()
            .map(|ty| u64::from(self.writer.tables.ty(*ty, &[])))
            .collect::<Vec<_>>();
        encoded.field_packed_varints(6, &supertypes);
        let mut declaration = Pb::new();
        declaration.field_message(2, &encoded);
        (signature, declaration.canonical().into_bytes())
    }

    fn parameter(&mut self, name: &str, ty: Ty, symbol: u64, origin: &str) -> Pb {
        let tables = &mut self.writer.tables;
        let mut base = Pb::new();
        base.field_varint(1, symbol);
        base.field_varint(2, u64::from(tables.string(origin)));
        base.field_varint(4, 0);
        let ty = tables.ty(ty, &[]);
        let name = tables.string(name);
        let mut parameter = Pb::new();
        parameter.field_message(1, &base);
        parameter.field_varint(2, lattice(name, ty));
        parameter
    }

    fn constructor(
        &mut self,
        constructor: &PreparedConstructor,
        self_type: Ty,
        this_symbol: u64,
        primary_parameters: &mut Vec<(u64, Ty)>,
    ) -> Vec<u8> {
        let tables = &mut self.writer.tables;
        let signature = tables.public_signature(&constructor.signature);
        let declaration_symbol = symbol(signature, CONSTRUCTOR_SYMBOL);
        let mut base = Pb::new();
        base.field_varint(1, declaration_symbol);
        base.field_varint(2, u64::from(tables.string("DEFINED")));
        base.field_varint(4, constructor.flags);
        let name = tables.string("<init>");
        let result = tables.ty(self_type, &[]);
        let mut function_base = Pb::new();
        function_base.field_message(1, &base);
        function_base.field_varint(2, lattice(name, result));
        let mut slots = vec![this_symbol];
        for (name, ty) in &constructor.parameters {
            let parameter_symbol =
                symbol(self.writer.tables.local_signature(), VALUE_PARAMETER_SYMBOL);
            let parameter = self.parameter(name, *ty, parameter_symbol, "DEFINED");
            function_base.repeated_message(6, &parameter);
            slots.push(parameter_symbol);
            if constructor.primary {
                primary_parameters.push((parameter_symbol, *ty));
            }
        }
        let body = BodyEncoder::new(&mut self.writer.tables, declaration_symbol, &slots, &[])
            .block_body(&constructor.body);
        function_base.field_varint(7, u64::from(body));
        let mut encoded = Pb::new();
        encoded.field_message(1, &function_base);
        let mut declaration = Pb::new();
        declaration.field_message(3, &encoded);
        declaration.canonical().into_bytes()
    }

    fn property(
        &mut self,
        property: &PreparedProperty,
        this_symbol: u64,
        primary_parameters: &[(u64, Ty)],
    ) -> Vec<u8> {
        let tables = &mut self.writer.tables;
        let signature = tables.public_signature(&property.signature);
        let mut base = Pb::new();
        base.field_varint(1, symbol(signature, PROPERTY_SYMBOL));
        base.field_varint(2, u64::from(tables.string("DEFINED")));
        base.field_varint(4, property.flags);
        let name = tables.string(&property.name);
        let mut encoded = Pb::new();
        encoded.field_message(1, &base);
        encoded.field_varint(2, u64::from(name));

        let field_signature = tables.field_signature(&property.signature);
        let mut field_base = Pb::new();
        field_base.field_varint(1, symbol(field_signature, FIELD_SYMBOL));
        field_base.field_varint(2, u64::from(tables.string("PROPERTY_BACKING_FIELD")));
        field_base.field_varint(4, property.field.flags);
        let ty = tables.ty(property.ty, &[]);
        let initializer = match &property.field.initializer {
            FieldInitializer::Parameter(parameter) => {
                let (parameter_symbol, parameter_ty) = primary_parameters[*parameter];
                // The field's initializer reads the primary-constructor parameter that declares it.
                let mut read = Pb::new();
                read.field_varint(1, parameter_symbol);
                read.field_varint(
                    2,
                    u64::from(tables.string("INITIALIZE_PROPERTY_FROM_PARAMETER")),
                );
                let mut initializer = Pb::new();
                initializer.field_varint(2, u64::from(tables.ty(parameter_ty, &[])));
                initializer.field_message(6, &read);
                tables.body(initializer.canonical().into_bytes())
            }
            FieldInitializer::Expression(body, expression) => {
                let slots = initializer_slots(this_symbol, primary_parameters);
                BodyEncoder::new(tables, this_symbol, &slots, &[])
                    .expression_body(body, *expression)
            }
        };
        let mut field = Pb::new();
        field.field_message(1, &field_base);
        field.field_varint(2, lattice(name, ty));
        field.field_varint(3, u64::from(initializer));
        encoded.field_message(3, &field);

        let getter = self
            .writer
            .function_message(&property.getter, Component::Main);
        encoded.field_message(4, &getter);
        if let Some(setter) = &property.setter {
            let setter = self.writer.function_message(setter, Component::Main);
            encoded.field_message(5, &setter);
        }
        let mut declaration = Pb::new();
        declaration.field_message(7, &encoded);
        declaration.canonical().into_bytes()
    }

    /// An `init { … }` block as an `IrAnonymousInit`.
    fn initializer(
        &mut self,
        body: &FunctionBody,
        this_symbol: u64,
        primary_parameters: &[(u64, Ty)],
    ) -> Vec<u8> {
        let tables = &mut self.writer.tables;
        let declaration_symbol = symbol(tables.local_signature(), ANONYMOUS_INITIALIZER_SYMBOL);
        let mut base = Pb::new();
        base.field_varint(1, declaration_symbol);
        base.field_varint(2, u64::from(tables.string("DEFINED")));
        let slots = initializer_slots(this_symbol, primary_parameters);
        let body = BodyEncoder::new(tables, declaration_symbol, &slots, &[]).block_body(body);
        let mut encoded = Pb::new();
        encoded.field_message(1, &base);
        encoded.field_varint(2, u64::from(body));
        let mut declaration = Pb::new();
        declaration.field_message(1, &encoded);
        declaration.canonical().into_bytes()
    }
}

/// The value slots of a property initializer or an `init` block: the class's `this` receiver,
/// then the primary constructor's parameters.
fn initializer_slots(this_symbol: u64, primary_parameters: &[(u64, Ty)]) -> Vec<u64> {
    std::iter::once(this_symbol)
        .chain(primary_parameters.iter().map(|(symbol, _)| *symbol))
        .collect()
}
