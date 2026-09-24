//! JVM `MethodParameters` planning from checked declarations and backend-generated provenance.

use crate::ir::{
    IrClass, IrFile, IrGeneratedParameterRole, IrParameterIdentity, IrParameterRole,
    IrSecondaryCtor,
};
use crate::types::{Ty, TypeName};

pub(super) type MethodParameter = (Option<String>, u16);

const SYNTHETIC: u16 = 0x1000;
const MANDATED: u16 = 0x8000;

fn parameter(name: impl Into<String>, flags: u16) -> MethodParameter {
    (Some(name.into()), flags)
}

/// JVM storage spelling for a lexical capture. Common IR retains the source spelling; local and
/// anonymous classes prefix captured fields with `$`, while an inner-class receiver already has its
/// explicit `this$0` identity.
pub(super) fn capture_field_name(class: &IrClass, index: usize) -> Option<String> {
    if class.is_inner_class && index == 0 {
        return None;
    }
    (class.is_local_class && index < class.constructor_prefix_count as usize).then(|| {
        let source = &class.fields[index].name;
        if source.starts_with('$') {
            source.clone()
        } else {
            format!("${source}")
        }
    })
}

/// Record parameter 0 of a value-class member lowered to a static `-impl`: the former receiver,
/// compiler-generated and — like kotlinc's — without a nullability annotation.
pub(super) fn prepend_value_class_receiver(ir: &mut IrFile, function: u32, name: &str) {
    ir.jvm_value_class_receiver_impls.insert(function);
    let expected = ir.functions[function as usize].params.len();
    let info = ir
        .fn_params
        .entry(function)
        .or_insert_with(|| crate::ir::FnParamInfo::identities(Vec::new()));
    info.prepend_generated(IrParameterIdentity::generated(
        IrGeneratedParameterRole::ValueClassCarrier,
        Some(name.to_string()),
    ));
    assert_eq!(
        info.identities.len(),
        expected,
        "value-class carrier provenance must match the transformed function"
    );
}

pub(super) fn record_function(
    ir: &mut IrFile,
    function: u32,
    names: &[&str],
    compiler_generated: &[usize],
) {
    let expected = ir.functions[function as usize].params.len();
    assert_eq!(
        names.len(),
        expected,
        "generated parameter identities must match the function"
    );
    let names = names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let info = ir
        .fn_params
        .entry(function)
        .or_insert_with(|| crate::ir::FnParamInfo::source_names(names.clone()));
    assert_eq!(
        info.identities
            .iter()
            .map(|identity| identity.source_name.as_deref())
            .collect::<Vec<_>>(),
        names
            .iter()
            .map(String::as_str)
            .map(Some)
            .collect::<Vec<_>>(),
        "one function has one parameter identity list"
    );
    for &parameter in compiler_generated {
        let identity = info
            .identities
            .get_mut(parameter)
            .expect("generated MethodParameters provenance needs an identity");
        identity.role = IrParameterRole::Generated(IrGeneratedParameterRole::ValueClassCarrier);
        identity.provenance = crate::ir::IrParameterProvenance::CompilerGenerated;
    }
}

/// Parameters of one emitted common-IR function. Presence of `FnParamInfo` is the explicit contract
/// that the function has declaration/debug parameter identities; compiler-generated parameters are
/// flagged from their recorded provenance. A holder receiver is a JVM-generated synthetic prefix.
pub(super) fn function(
    ir: &IrFile,
    function: u32,
    physical_parameters: &[Ty],
    holder_receiver: Option<TypeName>,
) -> Vec<MethodParameter> {
    let generated = ir.generated_function_publication(function);
    if ir.synthetic_methods.contains(&function) && generated.is_none() {
        return Vec::new();
    }
    let Some(identities) = ir
        .function_parameter_identities(function)
        .map(<[IrParameterIdentity]>::to_vec)
    else {
        return Vec::new();
    };
    assert_eq!(
        identities.len(),
        physical_parameters.len(),
        "recorded function parameter identities must match the physical JVM parameters"
    );
    let projected_names =
        crate::jvm::parameter_names::function_method_parameters(ir, function, physical_parameters)
            .expect("published MethodParameters identities have JVM projections");
    let mut parameters = identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            (
                projected_names[index].clone(),
                u16::from(matches!(
                    identity.role,
                    IrParameterRole::Generated(
                        IrGeneratedParameterRole::HolderReceiver
                            | IrGeneratedParameterRole::ValueClassCarrier
                    )
                )) * SYNTHETIC,
            )
        })
        .collect::<Vec<_>>();
    if holder_receiver.is_some() {
        parameters.insert(0, parameter("$this", SYNTHETIC));
    }
    parameters
}

fn constructor_prefix(class: &IrClass, count: usize) -> Vec<MethodParameter> {
    assert!(
        count <= class.ctor_args.len(),
        "constructor prefix exceeds its arguments"
    );
    class
        .ctor_args
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, argument)| {
            if class.is_inner_class && index == 0 {
                return parameter("this$0", MANDATED);
            }
            let name = argument
                .field_index
                .and_then(|field| {
                    capture_field_name(class, field as usize).or_else(|| {
                        class
                            .fields
                            .get(field as usize)
                            .map(|field| field.name.clone())
                    })
                })
                .or_else(|| argument.name.clone())
                .expect("a captured constructor prefix needs an exact storage name");
            parameter(name, SYNTHETIC)
        })
        .collect()
}

pub(super) fn primary_constructor(
    class: &IrClass,
    physical_parameters: &[Ty],
) -> Vec<MethodParameter> {
    if class.is_value || physical_parameters.is_empty() {
        return Vec::new();
    }
    assert_eq!(
        class.ctor_args.len(),
        physical_parameters.len(),
        "primary constructor identities must match its physical JVM parameters"
    );
    let prefix = class.constructor_prefix_count as usize;
    let mut parameters = constructor_prefix(class, prefix);
    let projected = crate::jvm::parameter_names::constructor_method_parameters(&class.ctor_args);
    parameters.extend(projected[prefix..].iter().cloned().map(|name| (name, 0)));
    parameters
}

/// Exact source/compiler identities carried by a primary constructor, independent of whether the
/// JVM `MethodParameters` attribute was requested. Synthetic marker accessors still need these for
/// their `LocalVariableTable`; omitting that table must not be used as a substitute for identities.
pub(super) fn primary_constructor_identities(
    class: &IrClass,
    physical_parameters: &[Ty],
) -> Vec<Option<String>> {
    let identities = crate::jvm::parameter_names::constructor_local_variables(&class.ctor_args);
    assert_eq!(identities.len(), physical_parameters.len());
    identities
}

/// Everything a class KIND prepends to EVERY constructor it declares, ahead of what the
/// declaration wrote: an `enum class` carries `(String $enum$name, int $enum$ordinal)` on its
/// primary and on each secondary alike. Carried as ONE description — the physical types and the
/// reflected identities together — because a prefix that reaches the descriptor but not
/// `MethodParameters` (or the generated debug identities, or the default stub, or the `this(…)`
/// delegation) is exactly how a constructor comes to describe an arity it does not have.
#[derive(Clone, Default)]
pub(super) struct OwnerConstructorPrefix {
    pub(super) types: Vec<Ty>,
    parameters: Vec<MethodParameter>,
}

impl OwnerConstructorPrefix {
    /// An ordinary class prepends nothing.
    pub(super) fn none() -> Self {
        Self::default()
    }

    /// `java.lang.Enum` needs the constant's name and ordinal, so kotlinc threads them through
    /// every constructor of an `enum class` and marks both `ACC_SYNTHETIC` in `MethodParameters`.
    /// They are not value parameters: a body's value ids still start at the first declared one.
    pub(super) fn enum_class() -> Self {
        Self {
            types: vec![Ty::String, Ty::Int],
            parameters: vec![
                parameter("$enum$name", SYNTHETIC),
                parameter("$enum$ordinal", SYNTHETIC),
            ],
        }
    }

    pub(super) fn len(&self) -> usize {
        self.types.len()
    }
}

pub(super) fn secondary_constructor(
    class: &IrClass,
    constructor: &IrSecondaryCtor,
    owner_prefix: &OwnerConstructorPrefix,
    physical_parameters: &[Ty],
) -> Vec<MethodParameter> {
    if constructor.synthetic || physical_parameters.is_empty() {
        return Vec::new();
    }
    assert_eq!(
        owner_prefix.len() + constructor.prefix_params.len() + constructor.named_params.len(),
        physical_parameters.len(),
        "secondary constructor identities must match its physical JVM parameters"
    );
    let mut parameters = owner_prefix.parameters.clone();
    parameters.extend(constructor_prefix(class, constructor.prefix_params.len()));
    parameters.extend(
        constructor
            .named_params
            .iter()
            .map(|(name, _)| parameter(name.clone(), 0)),
    );
    parameters
}

/// Exact identities for a secondary constructor's physical parameters. Unlike
/// [`secondary_constructor`], this includes compiler-generated constructors: those do not publish
/// `MethodParameters`, but an emitted marker accessor must still use the identities common IR
/// recorded instead of inventing `pN` names or silently dropping its debug locals.
pub(super) fn secondary_constructor_identities(
    class: &IrClass,
    constructor: &IrSecondaryCtor,
    owner_prefix: &OwnerConstructorPrefix,
    physical_parameters: &[Ty],
) -> Vec<Option<String>> {
    assert_eq!(
        owner_prefix.len() + constructor.prefix_params.len() + constructor.named_params.len(),
        physical_parameters.len(),
        "secondary constructor identities must match its physical JVM parameters"
    );
    let mut parameters = owner_prefix.parameters.clone();
    parameters.extend(constructor_prefix(class, constructor.prefix_params.len()));
    parameters.extend(
        constructor
            .named_params
            .iter()
            .map(|(name, _)| parameter(name.clone(), 0)),
    );
    parameters.into_iter().map(|(name, _)| name).collect()
}

pub(super) fn enum_constructor(class: &IrClass) -> Vec<MethodParameter> {
    let mut parameters = OwnerConstructorPrefix::enum_class().parameters;
    parameters.extend(class.ctor_args.iter().map(|argument| {
        parameter(
            argument
                .name
                .clone()
                .expect("an enum constructor parameter needs its source name"),
            0,
        )
    }));
    parameters
}

pub(super) fn enum_value_of() -> [MethodParameter; 1] {
    [parameter("value", 0)]
}

/// Compatibility-holder forwards prepend a JVM-generated receiver to the declaration's parameters.
/// Metadata/provider boundaries must supply every source identity; inventing `pN` names would make
/// reflection succeed with a semantically false parameter list.
pub(super) fn holder_forward(
    names: &[Option<String>],
    physical_parameters: &[Ty],
) -> Vec<MethodParameter> {
    assert_eq!(
        names.len(),
        physical_parameters.len(),
        "compatibility-holder parameter identities must match its declaration"
    );
    std::iter::once(parameter("$this", SYNTHETIC))
        .chain(names.iter().cloned().map(|name| (name, 0)))
        .collect()
}

pub(super) fn continuation_constructor(has_outer_receiver: bool) -> Vec<MethodParameter> {
    let mut parameters = Vec::with_capacity(usize::from(has_outer_receiver) + 1);
    if has_outer_receiver {
        parameters.push(parameter("this$0", 0));
    }
    parameters.push(parameter("$completion", 0));
    parameters
}

pub(super) fn continuation_invoke_suspend() -> [MethodParameter; 1] {
    [parameter("$result", 0)]
}
