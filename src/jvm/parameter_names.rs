//! JVM projections of backend-neutral parameter identity and provenance.
//!
//! Kotlin metadata, local-variable tables, null assertions, and Java reflection do not share one
//! spelling contract. Keeping their projections separate prevents a physical JVM name from becoming
//! common-IR identity or leaking onto another class-file surface.

use crate::ir::{
    IrCapturedDeclaration, IrCapturingCallable, IrFile, IrGeneratedParameterRole, IrLambdaForm,
    IrParameterIdentity, IrParameterRole,
};
use crate::jvm::anonymous_context_labels;

/// Name of a JVM `LocalVariableTable` entry, or `None` for a genuinely unnamed parameter.
pub(super) fn local_variable(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    // kotlinc lifts a value a local function captures into a leading parameter named after it,
    // prefixed with `$`.
    if let (Some(name), IrParameterRole::CapturedValue { .. }) =
        (&identity.source_name, identity.role)
    {
        return Some(format!("${name}"));
    }
    // A local delegated property's storage parameter keeps its source spelling (`x$delegate`) and
    // kotlinc's local-variable `$` prefix. That role is checked before the plain source-name
    // return: the storage identity publishes a name, and the prefix is still part of the JVM name.
    if let IrParameterRole::Generated(IrGeneratedParameterRole::LocalDelegateStorage) =
        identity.role
    {
        return identity.source_name.as_ref().map(|name| format!("${name}"));
    }
    if let Some(name) = &identity.source_name {
        return Some(name.clone());
    }
    match identity.role {
        IrParameterRole::Value | IrParameterRole::ContextValue => None,
        // kotlinc writes no local-variable row for a parameter with a special name.
        IrParameterRole::UnusedValue | IrParameterRole::DestructuredValue => None,
        IrParameterRole::AnonymousContextParameter { .. } => None,
        IrParameterRole::ContextReceiver { ordinal } => {
            Some(format!("$context_receiver_{ordinal}"))
        }
        IrParameterRole::ExtensionReceiver => Some(format!("$this${function_name}")),
        IrParameterRole::CapturedValue { ordinal, .. } => Some(format!("$capture{ordinal}")),
        IrParameterRole::CapturedReceiver { ordinal } => Some(format!("$this${ordinal}")),
        IrParameterRole::PropertySetterValue => Some("<set-?>".to_string()),
        IrParameterRole::Generated(role) => match role {
            IrGeneratedParameterRole::Positional { .. } => None,
            IrGeneratedParameterRole::Continuation => Some("$completion".to_string()),
            IrGeneratedParameterRole::ContinuationDispatchReceiver => Some("this$0".to_string()),
            IrGeneratedParameterRole::HolderReceiver => Some("$this".to_string()),
            // kotlinc names the parameter like the field it initializes.
            IrGeneratedParameterRole::OuterInstance => Some("this$0".to_string()),
            IrGeneratedParameterRole::ValueClassCarrier => Some("arg0".to_string()),
            IrGeneratedParameterRole::ValueClassEqualsOperand { ordinal } => {
                Some(value_class_equals_operand(ordinal).to_string())
            }
            IrGeneratedParameterRole::AccessorValue { ordinal } => Some(format!("value{ordinal}")),
            IrGeneratedParameterRole::ReferenceInvokeValue { ordinal } => {
                Some(format!("p{ordinal}"))
            }
            // kotlinc's built-in `FunctionN.invoke` numbers its parameters from one.
            IrGeneratedParameterRole::FunctionInvokeValue { ordinal } => {
                Some(format!("p{}", ordinal + 1))
            }
            IrGeneratedParameterRole::SuspendLambdaCreateValue => Some("value".to_string()),
            IrGeneratedParameterRole::InterfaceDelegationValue { ordinal } => {
                Some(format!("p{ordinal}"))
            }
            IrGeneratedParameterRole::LocalDelegateStorage => {
                identity.source_name.as_ref().map(|name| format!("${name}"))
            }
            IrGeneratedParameterRole::LocalDelegateDispatch => Some("this$0".to_string()),
        },
    }
}

/// A value parameter of the generic `invoke` bridge kotlinc writes for a callable-reference class.
/// It numbers them from one, unlike the specialized `invoke` it bridges to; a suspend reference's
/// trailing continuation keeps its `$completion` name.
pub(super) fn reference_invoke_bridge_parameter(ordinal: u16, continuation: bool) -> String {
    if continuation {
        return "$completion".to_string();
    }
    format!("p{}", ordinal + 1)
}

/// The local created when an inline callable-reference invocation binds one adapter parameter.
/// Constructing the common identity here keeps the JVM spelling in the one projection boundary.
pub(super) fn inline_callable_reference_parameter(ordinal: u32) -> String {
    let identity = IrParameterIdentity::generated(
        IrGeneratedParameterRole::ReferenceInvokeValue { ordinal },
        None,
    );
    local_variable(&identity, "")
        .expect("an inline callable-reference parameter has a generated JVM name")
}

/// `LocalVariableTable` names of a suspend lambda class's generated `member`, formatted from the
/// parameter identities its realization recorded and checked against `physical_parameters`: a
/// captured value or receiver as its constructor parameter is spelled (`$receiver` where kotlinc's
/// receiver convention applies), the generated roles as they are spelled everywhere.
pub(super) fn suspend_lambda_member(
    ir: &IrFile,
    parameters: &crate::jvm::suspend::cps::SuspendLambdaParameters,
    member: crate::jvm::suspend::cps::SuspendLambdaMember,
    physical_parameters: &[crate::types::Ty],
) -> Vec<String> {
    parameters
        .physical(member, physical_parameters.len())
        .iter()
        .map(|identity| match identity.role {
            IrParameterRole::CapturedValue { .. } | IrParameterRole::CapturedReceiver { .. } => {
                super::capture_names::suspend_lambda_capture(ir, parameters, identity).parameter
            }
            IrParameterRole::Generated(
                IrGeneratedParameterRole::Continuation
                | IrGeneratedParameterRole::SuspendLambdaCreateValue
                | IrGeneratedParameterRole::FunctionInvokeValue { .. },
            ) => local_variable(identity, "")
                .expect("a suspend lambda's generated parameter role has a JVM name"),
            role => panic!("a suspend lambda's {member:?} declares no {role:?} parameter"),
        })
        .collect()
}

pub(super) fn value_class_equals_operand(ordinal: u8) -> &'static str {
    match ordinal {
        1 => "p1",
        2 => "p2",
        _ => panic!("a value-class equality operand is first or second"),
    }
}

/// Local-variable spellings for one complete physical parameter identity list. An entry may be
/// unnamed; that absence is preserved for class-file surfaces that support `name_index = 0` and
/// omitted LVT rows.
pub(super) fn function_locals(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let anonymous = if anonymous_context_locals(ir, function) {
        let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
        disambiguated_anonymous_context_labels(identities, &semantic_types)
    } else {
        vec![None; identities.len()]
    };
    Some(
        identities
            .iter()
            .zip(anonymous)
            .map(|(identity, anonymous)| {
                anonymous.or_else(|| function_local_variable(ir, function, identity))
            })
            .collect(),
    )
}

/// Kotlin 2.4.20 names an anonymous context parameter by its generated label in the IR itself, so
/// the label is also its local-variable name; earlier releases left it unnamed there, publishing the
/// label only to reflection and null assertions. A lambda literal's own context-function parameter
/// stays unnamed in the method's `LocalVariableTable` at every version: the label is only the
/// null-check message.
fn anonymous_context_locals(ir: &IrFile, function: u32) -> bool {
    anonymous_context_parameters_are_locals()
        && ir
            .lambda_origins
            .get(&function)
            .is_none_or(|origin| origin.form != IrLambdaForm::Literal)
}

fn anonymous_context_parameters_are_locals() -> bool {
    crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20)
}

/// kotlinc's spelling of the extension receiver of a value class's interface entry. The entry is a
/// fresh member on the box under its own JVM name (`f-<hash>`, `getC`) that keeps its receiver as
/// an extension receiver, so the receiver is labeled after that name with its non-identifier
/// characters escaped (`$this$f_u2d<hash>`), never after the source declaration or its property.
/// Its local-variable row and its `MethodParameters` name agree.
pub(super) fn value_class_interface_entry_receiver(entry_name: &str) -> String {
    format!(
        "$this${}",
        crate::jvm::debug_local_names::escaped(entry_name)
    )
}

/// kotlinc's spelling of a source lambda's extension receiver: `$this$<label>` after the lambda's
/// label, and `<this>` for a lambda without one.
pub(super) fn lambda_receiver(origin: &crate::ir::IrLambdaOrigin) -> String {
    match origin.label.as_deref() {
        Some(label) => format!("$this${}", super::debug_local_names::escaped(label)),
        None => "<this>".to_string(),
    }
}

/// JVM local-table spelling for a parameter of one exact common-IR function, apart from the
/// anonymous context labels [`function_locals`] adds for the whole parameter list.
///
/// A source extension declaration uses kotlinc's `$this$<function>` spelling. A receiver lambda's
/// static implementation instead uses [`lambda_receiver`]'s spelling after its checked label; the
/// typed lambda edge selects that ABI surface, without making common IR encode either JVM spelling.
fn function_local_variable(
    ir: &IrFile,
    function: u32,
    identity: &IrParameterIdentity,
) -> Option<String> {
    if let IrParameterRole::CapturedReceiver { ordinal } = identity.role {
        let (name, _) = super::capture_names::lifted_receiver(ir, function, ordinal as usize)
            .expect("a lifted callable publishes the origin of each captured receiver");
        return Some(name);
    }
    if matches!(identity.role, IrParameterRole::ExtensionReceiver) {
        // Only a source lambda's implementation takes an extension receiver among the functions
        // with lambda parameters; a generated adapter passes its receiver as a value.
        if ir.lambda_own_params_from.contains_key(&function) {
            let origin = ir
                .lambda_origins
                .get(&function)
                .expect("a lambda's extension receiver belongs to a source lambda");
            return Some(lambda_receiver(origin));
        }
        // kotlinc names the receiver after the function's IR name when it writes the method. A
        // local function has its lifted name by then (`top$wrap`), mangled for a JVM local.
        if let Some(lifted) = ir.lifted_names.get(&function) {
            return Some(format!(
                "$this${}",
                super::debug_local_names::escaped(lifted)
            ));
        }
        let source_name = ir
            .fn_source_names
            .get(&function)
            .expect("an extension receiver retains its declaration source name");
        return Some(format!("$this${source_name}"));
    }
    // kotlinc's `LocalDeclarationsLowering` keeps a captured variable's own name when a named
    // local function or an anonymous function captures it (a shared cell included), and
    // `$`-prefixes a captured parameter and whatever a lambda literal captures.
    if let (Some(name), IrParameterRole::CapturedValue { capture, .. }) =
        (&identity.source_name, identity.role)
    {
        if capture.declaration == IrCapturedDeclaration::Variable
            && capture.capturer != IrCapturingCallable::Lambda
        {
            return Some(name.clone());
        }
    }
    local_variable(identity, "")
}

/// The name source wrote for a setter's value parameter, which Kotlin metadata records. The final
/// semantic parameter is the setter value by the accessor contract; its typed IR identity
/// distinguishes a written name from the compiler-generated implicit setter value.
pub(super) fn explicit_setter(ir: &IrFile, setter: Option<u32>) -> Option<String> {
    let identity = ir.function_parameter_identities(setter?)?.last()?;
    (identity.role == IrParameterRole::Value
        && identity.provenance == crate::ir::IrParameterProvenance::SourceDeclared)
        .then(|| metadata(identity).map(str::to_owned))
        .flatten()
}

/// Kotlin metadata accepts only a declaration/producer-published semantic name. A lambda's `_`
/// parameter, which declares no name, is kotlinc's `<unused var>` like an anonymous context
/// parameter; a destructuring one is `<destruct>`.
pub(super) fn metadata(identity: &IrParameterIdentity) -> Option<&str> {
    match identity.role {
        IrParameterRole::AnonymousContextParameter { .. } | IrParameterRole::UnusedValue => {
            Some("<unused var>")
        }
        IrParameterRole::DestructuredValue => Some(DESTRUCTURED),
        _ => identity.source_name.as_deref(),
    }
}

/// Kotlin metadata name for a parameter. Source names remain borrowed; a compiler-generated
/// delegation parameter is formatted here, at the JVM boundary, from its recorded semantic role.
pub(super) fn metadata_owned(identity: &IrParameterIdentity) -> Option<String> {
    match identity.role {
        IrParameterRole::Generated(IrGeneratedParameterRole::InterfaceDelegationValue {
            ordinal,
        }) => Some(format!("p{ordinal}")),
        _ => metadata(identity).map(str::to_owned),
    }
}

/// Kotlin's special name for a parameter written as a destructuring declaration. A suspend lambda's
/// body also reads the parameter back into a local of this name.
pub(super) const DESTRUCTURED: &str = "<destruct>";

pub(super) fn metadata_context_kind(
    identity: &IrParameterIdentity,
) -> crate::types::ContextParameterKind {
    match identity.role {
        IrParameterRole::ContextValue => crate::types::ContextParameterKind::Named,
        IrParameterRole::AnonymousContextParameter { .. } => {
            crate::types::ContextParameterKind::Anonymous
        }
        IrParameterRole::ContextReceiver { .. } => {
            crate::types::ContextParameterKind::LegacyReceiver
        }
        _ => panic!("a metadata context prefix must retain its semantic role"),
    }
}

/// Name of one Java-reflection `MethodParameters` entry.
pub(super) fn method_parameter(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    local_variable(identity, function_name)
}

fn disambiguated_anonymous_context_labels(
    identities: &[IrParameterIdentity],
    types: &[crate::types::Ty],
) -> Vec<Option<String>> {
    assert_eq!(identities.len(), types.len());
    anonymous_context_labels::disambiguated(
        identities
            .iter()
            .zip(types)
            .map(|(identity, ty)| {
                matches!(
                    identity.role,
                    IrParameterRole::AnonymousContextParameter { .. }
                )
                .then_some(*ty)
            })
            .collect(),
    )
}

fn function_semantic_parameter_types(
    ir: &IrFile,
    function: u32,
    identities: &[IrParameterIdentity],
    physical_types: &[crate::types::Ty],
) -> Vec<crate::types::Ty> {
    let declared = ir
        .vc_declared_sigs
        .get(&function)
        .map(|(_, params, _)| params.as_slice())
        .or_else(|| {
            ir.suspend_declared_sigs
                .get(&function)
                .map(|(params, _)| params.as_slice())
        })
        .or_else(|| {
            ir.signatures
                .get(&function)
                .map(|signature| signature.params.as_slice())
        })
        .or_else(|| {
            ir.member_semantic_sigs
                .get(&function)
                .map(|(params, _)| params.as_slice())
        });
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| match identity.role {
            IrParameterRole::AnonymousContextParameter { ordinal } => match declared {
                Some(params) => *params
                    .get(ordinal as usize)
                    .expect("an anonymous context parameter retains its declared semantic type"),
                // These maps are installed only when a representation pass changes a declaration
                // parameter. Without one, the function's physical list is still the checked common-
                // IR semantic list; there is no second spelling or descriptor path to consult.
                None => physical_types[index],
            },
            _ => physical_types[index],
        })
        .collect()
}

/// The identities of a constructor's physical parameters: the one projection its
/// `LocalVariableTable` and `MethodParameters` names and flags are derived from.
pub(super) fn constructor_identities(
    arguments: &[crate::ir::IrCtorArg],
) -> Vec<IrParameterIdentity> {
    let mut context_ordinal = 0u32;
    arguments
        .iter()
        .enumerate()
        .map(|(physical_ordinal, argument)| {
            let identity = match argument.context_kind {
                crate::types::ContextParameterKind::Named => IrParameterIdentity::context_value(
                    argument
                        .name
                        .as_deref()
                        .expect("a named classifier context parameter retains its source name"),
                ),
                crate::types::ContextParameterKind::Anonymous => {
                    IrParameterIdentity::anonymous_context_parameter(context_ordinal)
                }
                crate::types::ContextParameterKind::LegacyReceiver => {
                    IrParameterIdentity::context_receiver(context_ordinal)
                }
                crate::types::ContextParameterKind::None
                    if argument.provenance
                        == crate::ir::IrCtorParameterProvenance::ContinuationDispatchReceiver =>
                {
                    IrParameterIdentity::generated(
                        IrGeneratedParameterRole::ContinuationDispatchReceiver,
                        None,
                    )
                }
                crate::types::ContextParameterKind::None
                    if argument.provenance
                        == crate::ir::IrCtorParameterProvenance::Continuation =>
                {
                    IrParameterIdentity::generated(IrGeneratedParameterRole::Continuation, None)
                }
                crate::types::ContextParameterKind::None
                    if argument.provenance
                        == crate::ir::IrCtorParameterProvenance::EnclosingInstance =>
                {
                    assert!(
                        argument.name.is_none() && argument.capture.is_none(),
                        "a generated constructor parameter has no source or capture identity"
                    );
                    assert_eq!(
                        physical_ordinal, 0,
                        "an outer instance is its constructor's first parameter"
                    );
                    IrParameterIdentity::generated(IrGeneratedParameterRole::OuterInstance, None)
                }
                crate::types::ContextParameterKind::None => match argument.name.as_deref() {
                    Some(name) => IrParameterIdentity::source(name),
                    None => IrParameterIdentity::generated(
                        IrGeneratedParameterRole::Positional {
                            ordinal: physical_ordinal as u32,
                        },
                        None,
                    ),
                },
            };
            if argument.context_kind != crate::types::ContextParameterKind::None {
                context_ordinal += 1;
            }
            identity
        })
        .collect()
}

fn constructor_anonymous_labels(
    arguments: &[crate::ir::IrCtorArg],
    identities: &[IrParameterIdentity],
) -> Vec<Option<String>> {
    let semantic_types = arguments
        .iter()
        .map(|argument| argument.declared_ty.unwrap_or(argument.ty))
        .collect::<Vec<_>>();
    disambiguated_anonymous_context_labels(identities, &semantic_types)
}

pub(super) fn constructor_method_parameters(
    arguments: &[crate::ir::IrCtorArg],
) -> Vec<Option<String>> {
    let identities = constructor_identities(arguments);
    let anonymous = constructor_anonymous_labels(arguments, &identities);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index]
                .clone()
                .or_else(|| method_parameter(identity, "<init>"))
        })
        .collect()
}

/// A primary constructor's LocalVariableTable names. kotlinc names a local or anonymous class's
/// captured-value parameter like the field it stores into, and a captured receiver as
/// [`super::capture_names::class_capture`] says.
pub(super) fn constructor_local_variables(
    ir: &IrFile,
    class: &crate::ir::IrClass,
) -> Vec<Option<String>> {
    let arguments = &class.ctor_args;
    let identities = constructor_identities(arguments);
    let anonymous = if anonymous_context_parameters_are_locals() {
        constructor_anonymous_labels(arguments, &identities)
    } else {
        vec![None; identities.len()]
    };
    identities
        .iter()
        .zip(anonymous)
        .zip(arguments)
        .map(|((identity, anonymous), argument)| {
            argument
                .capture
                .as_ref()
                .map(|capture| super::capture_names::class_capture(ir, class, capture).parameter)
                .or(anonymous)
                .or_else(|| local_variable(identity, "<init>"))
                .or_else(|| {
                    argument
                        .anonymous_super_forward
                        .map(|ordinal| format!("$super_call_param${ordinal}"))
                })
        })
        .collect()
}

pub(super) fn constructor_assertions(arguments: &[crate::ir::IrCtorArg]) -> Vec<Option<String>> {
    let identities = constructor_identities(arguments);
    let anonymous = constructor_anonymous_labels(arguments, &identities);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| anonymous[index].clone().or_else(|| assertion(identity)))
        .collect()
}

pub(super) fn function_method_parameters(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
    let anonymous = disambiguated_anonymous_context_labels(identities, &semantic_types);
    Some(
        identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                anonymous[index]
                    .clone()
                    .or_else(|| function_local_variable(ir, function, identity))
            })
            .collect(),
    )
}

pub(super) fn function_assertions(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
    let anonymous = disambiguated_anonymous_context_labels(identities, &semantic_types);
    // kotlinc replaces a function whose signature the value-class ABI mangles (or moves to a static
    // `-impl`) with a copy whose extension receiver is an ordinary parameter, named as the
    // receiver's local variable. Its guard then quotes that name rather than `<this>`. A lambda's
    // implementation takes its receiver as such a parameter too (`$this$<label>`).
    let replaced = ir.lambda_own_params_from.contains_key(&function)
        || ir
            .vc_declared_sigs
            .get(&function)
            .zip(ir.functions.get(function as usize))
            .is_some_and(|((declared, ..), physical)| *declared != physical.name);
    Some(
        identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                anonymous[index].clone().or_else(|| match identity.role {
                    IrParameterRole::ExtensionReceiver if replaced => {
                        function_local_variable(ir, function, identity)
                    }
                    // A lambda's guard quotes its receiver's local name, `$this$<label>`.
                    IrParameterRole::ExtensionReceiver => ir
                        .lambda_origins
                        .get(&function)
                        .map(lambda_receiver)
                        .or_else(|| assertion(identity)),
                    _ => assertion(identity),
                })
            })
            .collect(),
    )
}

/// kotlinc's name for an extension receiver that a static taking the interface instance first
/// (`DefaultImpls`, `access$<name>$jd`) receives as an ordinary parameter.
const HOLDER_EXTENSION_RECEIVER: &str = "$receiver";

/// One surface's spellings of `function`'s parameters where its body is emitted. kotlinc writes a
/// static that takes a member's dispatch receiver first — an interface member's body on
/// `DefaultImpls` (`holder_receiver`) or `access$<name>$jd`, a suspend member's body moved to
/// `<name>$suspendImpl` (a recorded holder receiver) — with the interface or class instance as
/// `$this` and the extension receiver moved into an ordinary parameter named `$receiver`, which its
/// local-variable row, null check and reflection name all read. Elsewhere `names` stand.
pub(super) fn placed(
    ir: &IrFile,
    function: u32,
    holder_receiver: Option<crate::types::TypeName>,
    names: Option<Vec<Option<String>>>,
) -> Option<Vec<Option<String>>> {
    let mut names = names?;
    if !takes_dispatch_receiver_first(ir, function, holder_receiver) {
        return Some(names);
    }
    let identities = ir
        .function_parameter_identities(function)
        .expect("parameter spellings are projected from recorded parameter identities");
    assert_eq!(
        identities.len(),
        names.len(),
        "one spelling per recorded parameter identity"
    );
    for (name, identity) in names.iter_mut().zip(identities) {
        if identity.role == IrParameterRole::ExtensionReceiver {
            *name = Some(HOLDER_EXTENSION_RECEIVER.to_string());
        }
    }
    Some(names)
}

/// Whether `function` is emitted as a static taking its member's dispatch receiver as `$this`:
/// written onto an interface's holder, or recorded with a holder receiver of its own.
pub(super) fn takes_dispatch_receiver_first(
    ir: &IrFile,
    function: u32,
    holder_receiver: Option<crate::types::TypeName>,
) -> bool {
    holder_receiver.is_some()
        || ir
            .function_parameter_identities(function)
            .and_then(<[IrParameterIdentity]>::first)
            .is_some_and(|identity| {
                identity.role
                    == IrParameterRole::Generated(IrGeneratedParameterRole::HolderReceiver)
            })
}

/// [`placed`] for a provider-published parameter list republished on a `DefaultImpls` holder.
pub(super) fn resolved_on_holder(
    identities: &[crate::fir::ResolvedParameterIdentity],
    names: &mut [Option<String>],
) {
    assert_eq!(
        identities.len(),
        names.len(),
        "one spelling per provider-published parameter identity"
    );
    for (name, identity) in names.iter_mut().zip(identities) {
        if matches!(
            identity,
            crate::fir::ResolvedParameterIdentity::ExtensionReceiver
        ) {
            *name = Some(HOLDER_EXTENSION_RECEIVER.to_string());
        }
    }
}

/// Text passed to `Intrinsics.checkNotNullParameter` for one checked parameter.
pub(super) fn assertion(identity: &IrParameterIdentity) -> Option<String> {
    match identity.role {
        IrParameterRole::ExtensionReceiver => Some("<this>".to_string()),
        IrParameterRole::UnusedValue | IrParameterRole::DestructuredValue => {
            metadata(identity).map(str::to_owned)
        }
        _ => local_variable(identity, ""),
    }
}

/// Parameter identity written to coroutine `@DebugMetadata`.
pub(super) fn debug_metadata(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    local_variable(identity, function_name)
}

/// Debug name for an override/bridge parameter published before common IR is built.
pub(super) fn resolved_local_variable(
    identity: &crate::fir::ResolvedParameterIdentity,
    function_name: &str,
) -> Option<String> {
    match identity {
        crate::fir::ResolvedParameterIdentity::Source(name) => Some(name.to_string()),
        crate::fir::ResolvedParameterIdentity::Unnamed { .. } => None,
        crate::fir::ResolvedParameterIdentity::InterfaceDelegationValue { ordinal } => {
            Some(format!("p{ordinal}"))
        }
        crate::fir::ResolvedParameterIdentity::ContextValue { source_name, .. } => {
            Some(source_name.to_string())
        }
        crate::fir::ResolvedParameterIdentity::AnonymousContextParameter { .. } => None,
        crate::fir::ResolvedParameterIdentity::LegacyContextReceiver { ordinal } => {
            Some(format!("$context_receiver_{ordinal}"))
        }
        crate::fir::ResolvedParameterIdentity::ExtensionReceiver => {
            Some(format!("$this${function_name}"))
        }
        crate::fir::ResolvedParameterIdentity::PropertySetterValue => Some("<set-?>".to_string()),
        crate::fir::ResolvedParameterIdentity::SuspendCompletion => Some("$completion".to_string()),
    }
}

/// Local-variable spellings of a bridge. A named overridden parameter keeps that declaration's
/// spelling. An unnamed parameter — a Java binary declaration publishes none — is kotlinc's
/// positional `pN` in this table only. The ordinal is the recorded identity, not a descriptor
/// reconstruction, and it does not become a source or reflection name.
pub(super) fn bridge_local_variables(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    resolved_local_variables(identities, semantic_types, function_name)
        .into_iter()
        .zip(identities)
        .map(|(name, identity)| {
            name.or_else(|| match identity {
                crate::fir::ResolvedParameterIdentity::Unnamed { ordinal } => {
                    Some(format!("p{ordinal}"))
                }
                _ => None,
            })
        })
        .collect()
}

/// Local-variable spellings for a provider-published parameter list. Anonymous context labels are
/// derived from the declaration's semantic types, never from erased bridge descriptors.
pub(super) fn resolved_local_variables(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    assert_eq!(
        identities.len(),
        semantic_types.len(),
        "one semantic type per provider-published parameter identity"
    );
    let anonymous = if anonymous_context_parameters_are_locals() {
        resolved_anonymous_context_labels(identities, semantic_types)
    } else {
        vec![None; identities.len()]
    };
    identities
        .iter()
        .zip(anonymous)
        .map(|(identity, anonymous)| {
            anonymous.or_else(|| resolved_local_variable(identity, function_name))
        })
        .collect()
}

/// Local-variable spellings for a synthetic accessor to one dependency declaration. Source and
/// context identities keep their declaration spelling. An unnamed Java parameter takes its
/// provider-published ordinal (`p0`, ...), and an extension receiver uses the accessor convention
/// rather than the target method's `$this$<name>` spelling.
pub(super) fn dependency_access_bridge_local_variables(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    physical_types: &[crate::types::Ty],
    physical_plan: Option<&[crate::libraries::PhysicalParameterSlot]>,
    function_name: &str,
) -> Vec<Option<String>> {
    let source = resolved_local_variables(identities, semantic_types, function_name)
        .into_iter()
        .zip(identities)
        .map(|(name, identity)| match identity {
            crate::fir::ResolvedParameterIdentity::Unnamed { ordinal } => {
                Some(format!("p{ordinal}"))
            }
            crate::fir::ResolvedParameterIdentity::ExtensionReceiver => {
                Some("$receiver".to_string())
            }
            _ => name,
        })
        .collect::<Vec<_>>();
    let physical_plan = physical_plan
        .expect("a protected dependency accessor target carries a physical parameter plan");
    assert_eq!(
        physical_plan.len(),
        physical_types.len(),
        "a protected dependency accessor target names every physical parameter slot"
    );
    physical_plan
        .iter()
        .map(|slot| match *slot {
            crate::libraries::PhysicalParameterSlot::Source(ordinal) => source
                .get(ordinal as usize)
                .cloned()
                .expect("a physical source slot names an existing semantic parameter"),
            crate::libraries::PhysicalParameterSlot::Continuation => {
                Some("$completion".to_string())
            }
            crate::libraries::PhysicalParameterSlot::Dispatch => {
                panic!("a virtual protected dependency target cannot carry a dispatch ABI slot")
            }
        })
        .collect()
}

fn resolved_anonymous_context_labels(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
) -> Vec<Option<String>> {
    assert_eq!(identities.len(), semantic_types.len());
    let projected = identities
        .iter()
        .map(|identity| match identity {
            crate::fir::ResolvedParameterIdentity::AnonymousContextParameter { ordinal } => {
                IrParameterIdentity::anonymous_context_parameter(*ordinal)
            }
            _ => IrParameterIdentity::generated(
                IrGeneratedParameterRole::Positional { ordinal: 0 },
                None,
            ),
        })
        .collect::<Vec<_>>();
    disambiguated_anonymous_context_labels(&projected, semantic_types)
}

pub(super) fn resolved_method_parameters(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    let anonymous = resolved_anonymous_context_labels(identities, semantic_types);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index]
                .clone()
                .or_else(|| resolved_local_variable(identity, function_name))
        })
        .collect()
}

pub(super) fn resolved_assertions(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    let anonymous = resolved_anonymous_context_labels(identities, semantic_types);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index].clone().or_else(|| match identity {
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver => {
                    Some("<this>".to_string())
                }
                _ => resolved_local_variable(identity, function_name),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        FnParamInfo, IrFunction, IrGeneratedParameterRole, IrParameterCheck, IrParameterIdentity,
    };

    #[test]
    fn each_jvm_surface_projects_the_receiver_for_its_own_contract() {
        let receiver = IrParameterIdentity::extension_receiver();
        assert_eq!(
            local_variable(&receiver, "inspect"),
            Some("$this$inspect".to_string())
        );
        assert_eq!(assertion(&receiver), Some("<this>".to_string()));
        assert_eq!(metadata(&receiver), None);
    }

    #[test]
    fn a_lambda_parameter_with_a_special_name_has_no_local_variable_row() {
        for (role, name) in [
            (IrParameterRole::UnusedValue, "<unused var>"),
            (IrParameterRole::DestructuredValue, "<destruct>"),
        ] {
            let parameter = IrParameterIdentity {
                source_name: None,
                role,
                provenance: crate::ir::IrParameterProvenance::SourceDeclared,
            };
            assert_eq!(local_variable(&parameter, "box$lambda$0"), None);
            assert_eq!(metadata(&parameter), Some(name));
            assert_eq!(assertion(&parameter), Some(name.to_string()));
        }
    }

    #[test]
    fn a_lambda_literal_quotes_its_context_parameter_without_a_local_row() {
        let receiver = crate::types::Ty::obj("Receiver");
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "functionType$lambda$0".to_string(),
            params: vec![receiver],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::anonymous_context_parameter(0)]),
        );
        ir.lambda_origins.insert(
            function,
            crate::ir::IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "functionType".into(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "functionType".into(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: IrLambdaForm::Literal,
                class_provenance: None,
            },
        );
        assert_eq!(
            function_locals(&ir, function, &[receiver]),
            Some(vec![None])
        );
        assert_eq!(
            function_assertions(&ir, function, &[receiver]),
            Some(vec![Some("$context-Receiver".to_string())])
        );

        let mut declared = IrFile::default();
        let declared_function = declared.add_fun(IrFunction {
            name: "anonymous".to_string(),
            params: vec![receiver],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        declared.fn_params.insert(
            declared_function,
            FnParamInfo::identities(vec![IrParameterIdentity::anonymous_context_parameter(0)]),
        );
        let declared_local = if anonymous_context_parameters_are_locals() {
            Some("$context-Receiver".to_string())
        } else {
            None
        };
        assert_eq!(
            function_locals(&declared, declared_function, &[receiver]),
            Some(vec![declared_local])
        );
    }

    #[test]
    fn a_local_delegate_storage_parameter_keeps_kotlincs_dollar_prefix() {
        let storage = IrParameterIdentity::generated(
            IrGeneratedParameterRole::LocalDelegateStorage,
            Some("delegated$delegate".to_string()),
        );
        assert_eq!(
            local_variable(&storage, "one$lambda$0$0"),
            Some("$delegated$delegate".to_string())
        );
        assert_eq!(
            method_parameter(&storage, "one$lambda$0$0"),
            Some("$delegated$delegate".to_string())
        );
        assert_eq!(metadata(&storage), Some("delegated$delegate"));
        let dispatch =
            IrParameterIdentity::generated(IrGeneratedParameterRole::LocalDelegateDispatch, None);
        assert_eq!(
            local_variable(&dispatch, "read$lambda$0"),
            Some("this$0".to_string())
        );
    }

    #[test]
    fn an_unnamed_generated_parameter_never_acquires_a_positional_name() {
        let generated = IrParameterIdentity::generated(
            IrGeneratedParameterRole::Positional { ordinal: 3 },
            None,
        );
        assert_eq!(local_variable(&generated, "inspect"), None);
        assert_eq!(method_parameter(&generated, "inspect"), None);
        assert_eq!(metadata(&generated), None);
    }

    #[test]
    fn interface_delegation_parameter_is_formatted_only_at_the_jvm_boundary() {
        let generated = IrParameterIdentity::generated(
            IrGeneratedParameterRole::InterfaceDelegationValue { ordinal: 2 },
            None,
        );
        assert_eq!(generated.source_name, None);
        assert_eq!(
            local_variable(&generated, "forward"),
            Some("p2".to_string())
        );
        assert_eq!(
            method_parameter(&generated, "forward"),
            Some("p2".to_string())
        );
        assert_eq!(metadata(&generated), None);
        assert_eq!(metadata_owned(&generated), Some("p2".to_string()));
    }

    #[test]
    fn receiver_lambda_uses_its_backend_debug_surface() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "transform$lambda$0".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );
        ir.lambda_own_params_from.insert(function, 0);
        let origin = |label: Option<&str>| crate::ir::IrLambdaOrigin {
            identity: 0,
            lexical_owner: None,
            enclosing_name: "transform".into(),
            binding_name: None,
            ordinal: 0,
            implementation_name: "transform".into(),
            implementation_ordinal: 0,
            receiver_parameter: Some(0),
            label: label.map(str::to_owned),
            form: crate::ir::IrLambdaForm::Literal,
            class_provenance: None,
        };

        ir.lambda_origins.insert(function, origin(None));
        assert_eq!(
            function_locals(&ir, function, &[crate::types::Ty::String]),
            Some(vec![Some("<this>".to_string())])
        );
        ir.lambda_origins.insert(function, origin(Some("build")));
        assert_eq!(
            function_locals(&ir, function, &[crate::types::Ty::String]),
            Some(vec![Some("$this$build".to_string())])
        );
    }

    #[test]
    #[should_panic(expected = "a lambda's extension receiver belongs to a source lambda")]
    fn a_lambda_receiver_without_a_source_lambda_is_rejected() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "adapter".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );
        ir.lambda_own_params_from.insert(function, 0);

        let _ = function_locals(&ir, function, &[crate::types::Ty::String]);
    }

    #[test]
    fn declared_extension_receiver_uses_only_its_published_source_name() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "renamed-physical".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );
        ir.fn_source_names.insert(function, "transform".to_string());

        assert_eq!(
            function_locals(&ir, function, &[crate::types::Ty::String]),
            Some(vec![Some("$this$transform".to_string())])
        );
    }

    #[test]
    #[should_panic(expected = "an extension receiver retains its declaration source name")]
    fn declared_extension_receiver_never_uses_a_physical_function_name() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "renamed-physical".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );

        let _ = function_locals(&ir, function, &[crate::types::Ty::String]);
    }

    #[test]
    fn anonymous_context_label_uses_the_declared_value_class_not_its_carrier() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "inspect-erased".to_string(),
            params: vec![crate::types::Ty::String, crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None::<IrParameterCheck>; 2],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![
                IrParameterIdentity::anonymous_context_parameter(0),
                IrParameterIdentity::source("value"),
            ]),
        );
        ir.vc_declared_sigs.insert(
            function,
            (
                "inspect".to_string(),
                vec![
                    crate::types::Ty::obj("demo/Wrapped"),
                    crate::types::Ty::String,
                ],
                crate::types::Ty::Unit,
            ),
        );

        assert_eq!(
            function_method_parameters(
                &ir,
                function,
                &[crate::types::Ty::String, crate::types::Ty::String],
            ),
            Some(vec![
                Some("$context-Wrapped".to_string()),
                Some("value".to_string()),
            ])
        );
        assert_eq!(
            function_locals(
                &ir,
                function,
                &[crate::types::Ty::String, crate::types::Ty::String],
            ),
            Some(vec![
                anonymous_context_parameters_are_locals()
                    .then(|| Some("$context-Wrapped".to_string()))
                    .flatten(),
                Some("value".to_string()),
            ])
        );
    }

    #[test]
    fn dependency_bridge_names_follow_the_provider_physical_slot_plan() {
        use crate::fir::ResolvedParameterIdentity;
        use crate::libraries::PhysicalParameterSlot;
        use crate::types::Ty;

        assert_eq!(
            dependency_access_bridge_local_variables(
                &[
                    ResolvedParameterIdentity::ExtensionReceiver,
                    ResolvedParameterIdentity::Source("named".into()),
                    ResolvedParameterIdentity::Unnamed { ordinal: 2 },
                ],
                &[Ty::String, Ty::Int, Ty::Long],
                &[
                    Ty::String,
                    Ty::Int,
                    Ty::Long,
                    Ty::obj("kotlin/coroutines/Continuation"),
                ],
                Some(&[
                    PhysicalParameterSlot::Source(0),
                    PhysicalParameterSlot::Source(1),
                    PhysicalParameterSlot::Source(2),
                    PhysicalParameterSlot::Continuation,
                ]),
                "renamed-physical",
            ),
            vec![
                Some("$receiver".to_string()),
                Some("named".to_string()),
                Some("p2".to_string()),
                Some("$completion".to_string()),
            ]
        );
    }

    #[test]
    fn bridge_locals_format_only_an_unnamed_identity_positionally() {
        use crate::fir::ResolvedParameterIdentity;
        use crate::types::Ty;

        assert_eq!(
            bridge_local_variables(
                &[
                    ResolvedParameterIdentity::Source("named".into()),
                    ResolvedParameterIdentity::Unnamed { ordinal: 1 },
                ],
                &[Ty::String, Ty::String],
                "map",
            ),
            vec![Some("named".to_string()), Some("p1".to_string())]
        );
        assert_eq!(
            resolved_local_variable(&ResolvedParameterIdentity::Unnamed { ordinal: 1 }, "map"),
            None,
            "the generic provider projection remains unnamed"
        );
    }

    #[test]
    #[should_panic(expected = "one semantic type per provider-published parameter identity")]
    fn resolved_provider_parameter_names_reject_a_mismatched_semantic_arity() {
        resolved_local_variables(
            &[crate::fir::ResolvedParameterIdentity::Source(
                "value".into(),
            )],
            &[],
            "inspect",
        );
    }
}

#[cfg(test)]
mod suspend_lambda_member_tests {
    use super::suspend_lambda_member as local_variables;
    use crate::ir::{
        IrCapturedDeclaration, IrCapturedReceiver, IrCapturingCallable, IrFile,
        IrParameterIdentity, IrValueCapture,
    };
    use crate::jvm::method_parameters::suspend_lambda_member as method_parameters;
    use crate::jvm::suspend::cps::{SuspendLambdaMember, SuspendLambdaParameters};
    use crate::types::Ty;

    const SYNTHETIC: u16 = 0x1000;

    fn named(names: &[&str], flags: &[u16]) -> Vec<(Option<String>, u16)> {
        assert_eq!(names.len(), flags.len());
        names
            .iter()
            .zip(flags)
            .map(|(name, &flags)| (Some((*name).to_string()), flags))
            .collect()
    }

    /// A suspend lambda class's generated members are named on both class-file surfaces from the
    /// identities its realization recorded, and only from them: neither planner takes the other's
    /// rows or a descriptor position. They agree where kotlinc spells a parameter alike and differ
    /// exactly where it does not: a captured enclosing instance is `$receiver` in the
    /// constructor's local-variable table and `this$0`, its field, in `MethodParameters`.
    #[test]
    fn both_surfaces_format_the_recorded_suspend_lambda_identities() {
        let ir = IrFile::default();
        let captured = IrParameterIdentity::captured_value(
            Some("x".to_string()),
            0,
            IrValueCapture {
                declaration: IrCapturedDeclaration::Variable,
                capturer: IrCapturingCallable::Lambda,
            },
        );
        let lambda = SuspendLambdaParameters::new(
            vec![captured, IrParameterIdentity::captured_receiver(0)],
            1,
            vec![IrCapturedReceiver::Enclosing {
                classifier: crate::types::type_name("Outer"),
            }],
            None,
        );
        let constructor = [Ty::Long, Ty::obj("Outer"), Ty::obj("Continuation")];
        assert_eq!(
            method_parameters(&ir, &lambda, SuspendLambdaMember::Constructor, &constructor),
            named(&["$x", "this$0", "$completion"], &[SYNTHETIC, SYNTHETIC, 0])
        );
        assert_eq!(
            local_variables(&ir, &lambda, SuspendLambdaMember::Constructor, &constructor),
            ["$x", "$receiver", "$completion"]
        );
        let create = [Ty::obj("Object"), Ty::obj("Continuation")];
        assert_eq!(
            method_parameters(&ir, &lambda, SuspendLambdaMember::Create, &create),
            named(&["value", "$completion"], &[0, 0])
        );
        assert_eq!(
            local_variables(&ir, &lambda, SuspendLambdaMember::Create, &create),
            ["value", "$completion"]
        );
        let invoke = [Ty::Int, Ty::obj("Continuation")];
        assert_eq!(
            method_parameters(&ir, &lambda, SuspendLambdaMember::Invoke, &invoke),
            named(&["p1", "p2"], &[0, 0])
        );
        assert_eq!(
            local_variables(&ir, &lambda, SuspendLambdaMember::Invoke, &invoke),
            ["p1", "p2"]
        );
    }

    /// The typed `invoke`'s continuation is the overridden `FunctionN.invoke`'s last parameter, so
    /// it takes that role's spelling, not the completion's.
    #[test]
    fn typed_invoke_names_its_function_invoke_values_from_one() {
        let lambda = SuspendLambdaParameters::new(Vec::new(), 2, Vec::new(), None);
        let invoke = [Ty::Int, Ty::String, Ty::obj("Continuation")];
        assert_eq!(
            method_parameters(
                &IrFile::default(),
                &lambda,
                SuspendLambdaMember::Invoke,
                &invoke
            ),
            named(&["p1", "p2", "p3"], &[0, 0, 0])
        );
    }
}
