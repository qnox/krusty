//! Physical JVM method collisions on one package facade.

use crate::diag::Span;
use crate::ir::{FunId, IrFile};

use super::UndeterminedDiagnosticType;

type ClashRows = Vec<(Span, String)>;

#[derive(Clone, Copy)]
enum Declaration {
    Function(usize),
    PropertyAccessor {
        property: usize,
        function: Option<FunId>,
        setter: bool,
    },
    PropertyAnnotationMarker(FunId),
    InPlaceDelegate {
        function: FunId,
        source_order: u32,
    },
}

type SignatureGroups = Vec<((String, String), Vec<Declaration>)>;

pub(super) fn report(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> Result<ClashRows, ClashRows> {
    report_groups(ir, signature_groups(ir, override_results)?)
}

/// Every physical method the package facade declares, grouped by its JVM name and descriptor.
fn signature_groups(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> Result<SignatureGroups, ClashRows> {
    let mut groups: SignatureGroups = Vec::new();
    for (index, declaration) in ir.package_functions.iter().enumerate() {
        if declaration.companion {
            continue;
        }
        let Some(function) = ir.functions.get(declaration.function as usize) else {
            return Err(vec![(
                declaration.signature_span,
                "internal error: package function platform-clash validation has no IR function"
                    .to_string(),
            )]);
        };
        if let Err(kind) = super::determined_function_signature(function) {
            return Err(vec![(
                declaration.signature_span,
                super::undetermined_type_message(kind),
            )]);
        }
        push(
            &mut groups,
            (
                function.name.clone(),
                crate::jvm::ir_emit::function_descriptor(
                    ir,
                    override_results,
                    declaration.function,
                ),
            ),
            Declaration::Function(index),
        );
    }
    for (index, property) in ir.package_properties.iter().enumerate() {
        let Some(layout) = ir.local_property_layouts.get(&property.property) else {
            continue;
        };
        let (storage, getter, setter) = match layout {
            crate::ir::IrLocalPropertyLayout::TopLevelStorage {
                storage,
                getter,
                setter,
                ..
            } => (Some(*storage), *getter, *setter),
            crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. } => {
                (None, Some(*getter), *setter)
            }
            _ => continue,
        };
        for (setter, function) in [(false, getter), (true, setter)] {
            let signature = if let Some(function) = function {
                let Some(declaration) = ir.functions.get(function as usize) else {
                    return Err(vec![(
                        property_accessor_span(ir, index, setter),
                        "internal error: package property accessor has no IR function".to_string(),
                    )]);
                };
                if let Err(kind) = super::determined_function_signature(declaration) {
                    return Err(vec![(
                        property_accessor_span(ir, index, setter),
                        super::undetermined_type_message(kind),
                    )]);
                }
                Some((
                    declaration.name.clone(),
                    crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
                ))
            } else {
                let side = if setter {
                    crate::jvm::ir_emit::PropertyAccessorSide::Setter
                } else {
                    crate::jvm::ir_emit::PropertyAccessorSide::Getter
                };
                storage
                    .and_then(|storage| {
                        crate::jvm::ir_emit::declared_default_static_accessor_signature(
                            ir, storage, side,
                        )
                    })
                    .map(|signature| (signature.name, signature.descriptor))
            };
            let Some(signature) = signature else { continue };
            push(
                &mut groups,
                signature,
                Declaration::PropertyAccessor {
                    property: index,
                    function,
                    setter,
                },
            );
        }
    }
    for property in &ir.package_properties {
        let Some(marker) = ir.jvm_package_property_annotation_marker(property.property) else {
            continue;
        };
        let Some(function) = ir.functions.get(marker as usize) else {
            return Err(vec![(
                Span::new(0, 0),
                "internal error: package property annotation marker has no IR function".to_string(),
            )]);
        };
        if let Err(kind) = super::determined_function_signature(function) {
            return Err(vec![(
                Span::new(0, 0),
                super::undetermined_type_message(kind),
            )]);
        }
        push(
            &mut groups,
            (
                function.name.clone(),
                crate::jvm::ir_emit::function_descriptor(ir, override_results, marker),
            ),
            Declaration::PropertyAnnotationMarker(marker),
        );
    }
    for property in &ir.package_properties {
        let Some(function) = ir.jvm_package_delegate_method(property.property) else {
            continue;
        };
        let Some(declaration) = ir.functions.get(function as usize) else {
            return Err(vec![(
                Span::new(0, 0),
                "internal error: package in-place delegate has no IR function".to_string(),
            )]);
        };
        if let Err(kind) = super::determined_function_signature(declaration) {
            return Err(vec![(
                Span::new(0, 0),
                super::undetermined_type_message(kind),
            )]);
        }
        push(
            &mut groups,
            (
                declaration.name.clone(),
                crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
            ),
            Declaration::InPlaceDelegate {
                function,
                source_order: property.source_order,
            },
        );
    }
    Ok(groups)
}

fn report_groups(ir: &IrFile, groups: SignatureGroups) -> Result<ClashRows, ClashRows> {
    let mut clashes = Vec::new();
    let mut reported_delegate_header = false;
    for ((name, descriptor), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut rendered = Vec::with_capacity(members.len());
        for &declaration in &members {
            let key = declaration_order(ir, declaration).map_err(|kind| {
                vec![(
                    declaration_span(ir, declaration),
                    super::undetermined_type_message(kind),
                )]
            })?;
            rendered.push((declaration, key));
        }
        if members
            .iter()
            .any(|declaration| matches!(declaration, Declaration::InPlaceDelegate { .. }))
        {
            rendered.sort_by_key(|(declaration, _)| delegate_group_order(ir, *declaration));
        } else {
            rendered.sort_by(|(_, left), (_, right)| left.cmp(right));
        }
        let listed = rendered
            .iter()
            .map(|(declaration, _)| render(ir, *declaration))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|kind| {
                vec![(
                    declaration_span(ir, rendered[0].0),
                    super::undetermined_type_message(kind),
                )]
            })?;
        let message = super::clash_message(&name, &descriptor, &listed);
        let mut located = Vec::new();
        for declaration in members {
            if matches!(declaration, Declaration::InPlaceDelegate { .. }) {
                if reported_delegate_header {
                    continue;
                }
                reported_delegate_header = true;
            }
            located.push((declaration_span(ir, declaration), message.clone()));
        }
        located.sort_by_key(|(span, _)| span.lo);
        clashes.extend(located);
    }
    clashes.sort_by_key(|(span, _)| span.lo);
    Ok(clashes)
}

/// A method a multifile part's facade forwards to, with the declaration it realizes: its JVM
/// slot, kotlinc's ordering key for a clash listing, and kotlinc's rendering of the declaration.
#[derive(Clone, Debug)]
pub(crate) struct FacadeSlot {
    pub(super) name: String,
    pub(super) descriptor: String,
    pub(super) order: (String, String, Vec<String>, usize),
    pub(super) rendered: String,
    /// A `$default` stub, which kotlinc reports only beside a method that is not one.
    pub(super) default_stub: bool,
}

/// The package facade methods a multifile facade forwards: the non-private functions, their
/// `$default` stubs and the non-private property accessors.
pub(super) fn facade_slots(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> Result<Vec<FacadeSlot>, ClashRows> {
    let undetermined = |declaration: Declaration, kind| {
        vec![(
            declaration_span(ir, declaration),
            super::undetermined_type_message(kind),
        )]
    };
    let mut slots = Vec::new();
    for ((name, descriptor), members) in signature_groups(ir, override_results)? {
        for declaration in members {
            let forwarded = match declaration {
                Declaration::Function(index) => {
                    !ir.package_functions[index].visibility.is_private()
                }
                Declaration::PropertyAccessor {
                    property, setter, ..
                } => {
                    let property = &ir.package_properties[property];
                    let visibility = if setter {
                        property.setter_visibility
                    } else {
                        property.visibility
                    };
                    !visibility.is_private()
                }
                Declaration::PropertyAnnotationMarker(_) | Declaration::InPlaceDelegate { .. } => {
                    false
                }
            };
            if !forwarded {
                continue;
            }
            slots.push(FacadeSlot {
                name: name.clone(),
                descriptor: descriptor.clone(),
                order: declaration_order(ir, declaration)
                    .map_err(|kind| undetermined(declaration, kind))?,
                rendered: render(ir, declaration)
                    .map_err(|kind| undetermined(declaration, kind))?,
                default_stub: false,
            });
            if let Declaration::Function(index) = declaration {
                let function = &ir.package_functions[index];
                if function.param_defaults.iter().any(|&default| default) {
                    let masks = crate::jvm::ir_emit::default_stub_mask_count(ir, function.function);
                    let mut order =
                        function_order(function).map_err(|kind| undetermined(declaration, kind))?;
                    order.0.push_str("$default");
                    slots.push(FacadeSlot {
                        name: format!("{name}$default"),
                        descriptor: crate::jvm::ir_emit::default_stub_descriptor(
                            ir,
                            function.function,
                            crate::types::Ty::obj("java/lang/Object"),
                        ),
                        order,
                        rendered: render_default_stub(function, ir, masks)
                            .map_err(|kind| undetermined(declaration, kind))?,
                        default_stub: true,
                    });
                }
            }
        }
    }
    Ok(slots)
}

fn push(
    groups: &mut Vec<((String, String), Vec<Declaration>)>,
    signature: (String, String),
    declaration: Declaration,
) {
    if let Some((_, members)) = groups.iter_mut().find(|(key, _)| *key == signature) {
        members.push(declaration);
    } else {
        groups.push((signature, vec![declaration]));
    }
}

fn declaration_span(ir: &IrFile, declaration: Declaration) -> Span {
    match declaration {
        Declaration::Function(index) => ir.package_functions[index].signature_span,
        Declaration::PropertyAccessor {
            property, setter, ..
        } => property_accessor_span(ir, property, setter),
        Declaration::PropertyAnnotationMarker(_) | Declaration::InPlaceDelegate { .. } => {
            Span::new(0, 0)
        }
    }
}

fn property_accessor_span(ir: &IrFile, property: usize, setter: bool) -> Span {
    let declaration = &ir.package_properties[property];
    let checked = ir
        .checked_properties
        .get(&declaration.property)
        .expect("a package property accessor retains its checked declaration");
    (if setter {
        checked.setter_diagnostic_span
    } else {
        checked.getter_diagnostic_span
    })
    .or(checked.diagnostic_span)
    .expect("a source package property retains its diagnostic span")
}

fn delegate_group_order(ir: &IrFile, declaration: Declaration) -> (u8, u32, FunId) {
    match declaration {
        Declaration::InPlaceDelegate {
            function,
            source_order,
        } => (0, source_order, function),
        Declaration::Function(index) => {
            let declaration = &ir.package_functions[index];
            (1, declaration.source_order, declaration.function)
        }
        Declaration::PropertyAccessor {
            property,
            function,
            setter,
        } => (
            1 + u8::from(setter),
            ir.package_properties[property].source_order,
            function.unwrap_or(u32::MAX),
        ),
        Declaration::PropertyAnnotationMarker(function) => (3, u32::MAX, function),
    }
}

fn declaration_order(
    ir: &IrFile,
    declaration: Declaration,
) -> Result<(String, String, Vec<String>, usize), UndeterminedDiagnosticType> {
    match declaration {
        Declaration::Function(index) => function_order(&ir.package_functions[index]),
        Declaration::PropertyAccessor {
            property,
            function: None,
            setter,
        } => Ok((
            ir.package_properties[property].name.clone(),
            String::new(),
            Vec::new(),
            usize::from(setter),
        )),
        Declaration::PropertyAccessor {
            function: Some(function),
            ..
        }
        | Declaration::InPlaceDelegate { function, .. }
        | Declaration::PropertyAnnotationMarker(function) => {
            let function = &ir.functions[function as usize];
            Ok((function.name.clone(), String::new(), Vec::new(), 0))
        }
    }
}

fn render(ir: &IrFile, declaration: Declaration) -> Result<String, UndeterminedDiagnosticType> {
    match declaration {
        Declaration::Function(index) => render_function(&ir.package_functions[index], ir),
        Declaration::PropertyAccessor {
            property,
            function,
            setter,
        } => {
            let property = &ir.package_properties[property];
            let callable = if setter {
                format!("<set-{}>", property.name)
            } else {
                format!("<get-{}>", property.name)
            };
            if let Some(function) = function {
                let mut type_context = super::semantic_function_parameters(ir, function).to_vec();
                type_context.push(ir.functions[function as usize].ret);
                Ok(super::render_source_member_method(
                    ir,
                    function,
                    &callable,
                    owner(ir),
                    &type_context,
                ))
            } else {
                render_default_property_accessor(ir, property, setter, &callable)
            }
        }
        Declaration::PropertyAnnotationMarker(function) => {
            let function = &ir.functions[function as usize];
            Ok(format!(
                "fun {}(): Unit defined in {}",
                super::rendered_callable_name(&function.name),
                owner(ir)
            ))
        }
        Declaration::InPlaceDelegate { function, .. } => super::render_physical_function(
            &ir.functions[function as usize],
            (0..ir.functions[function as usize].params.len()).map(|_| "<this>"),
            owner(ir),
        ),
    }
}

fn render_default_property_accessor(
    ir: &IrFile,
    property: &crate::ir::IrPackageProperty,
    setter: bool,
    callable: &str,
) -> Result<String, UndeterminedDiagnosticType> {
    debug_assert!(property.type_params.is_empty());
    debug_assert!(property.receiver.is_none() || property.is_companion_extension());
    debug_assert!(property.context_parameters.is_empty());
    let parameters = if setter {
        format!(
            "{}: {}",
            super::rendered_callable_name("<set-?>"),
            super::source_type_name(property.ty)?
        )
    } else {
        String::new()
    };
    let result = if setter {
        crate::types::Ty::Unit
    } else {
        property.ty
    };
    Ok(format!(
        "fun {}({parameters}): {} defined in {}",
        super::rendered_callable_name(callable),
        super::source_type_name(result)?,
        owner(ir)
    ))
}

fn render_function(
    function: &crate::ir::IrPackageFunction,
    ir: &IrFile,
) -> Result<String, UndeterminedDiagnosticType> {
    let mut rendered = String::from("fun ");
    if !function.type_params.is_empty() {
        rendered.push('<');
        for (index, parameter) in function.type_params.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&parameter.name);
            let bounds = parameter
                .bounds
                .iter()
                .copied()
                .filter(|bound| !super::is_default_bound(*bound))
                .collect::<Vec<_>>();
            if !bounds.is_empty() {
                rendered.push_str(" : ");
                for (bound_index, bound) in bounds.iter().enumerate() {
                    if bound_index > 0 {
                        rendered.push_str(" & ");
                    }
                    rendered.push_str(&super::source_type_name(*bound)?);
                }
            }
        }
        rendered.push_str("> ");
    }
    if let Some(receiver) = function.receiver {
        rendered.push_str(&super::source_type_name(receiver)?);
        rendered.push('.');
    }
    rendered.push_str(&super::rendered_callable_name(&function.name));
    rendered.push('(');
    rendered.push_str(&rendered_value_parameters(function)?.join(", "));
    rendered.push_str("): ");
    rendered.push_str(&super::source_type_name(function.ret)?);
    rendered.push_str(" defined in ");
    rendered.push_str(owner(ir));
    Ok(rendered)
}

/// A function's value parameters as kotlinc renders them: `name: Type`, and ` = ...` after one
/// that declares a default value.
fn rendered_value_parameters(
    function: &crate::ir::IrPackageFunction,
) -> Result<Vec<String>, UndeterminedDiagnosticType> {
    value_parameters(function)?
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            let mut rendered = format!("{name}: {}", super::source_type_name(*ty)?);
            if function
                .param_defaults
                .get(function.context_count + index)
                .copied()
                .unwrap_or(false)
            {
                rendered.push_str(" = ...");
            }
            Ok(rendered)
        })
        .collect()
}

/// kotlinc's rendering of a function's `$default` stub: the function's parameters, one `$maskN`
/// word per 32 of them and the `$handler` marker.
fn render_default_stub(
    function: &crate::ir::IrPackageFunction,
    ir: &IrFile,
    masks: usize,
) -> Result<String, UndeterminedDiagnosticType> {
    let mut parameters = rendered_value_parameters(function)?;
    parameters.extend((0..masks).map(|mask| {
        format!(
            "{}: Int",
            super::rendered_callable_name(&format!("$mask{mask}"))
        )
    }));
    parameters.push(format!(
        "{}: Any?",
        super::rendered_callable_name("$handler")
    ));
    let mut rendered = String::from("fun ");
    if let Some(receiver) = function.receiver {
        rendered.push_str(&super::source_type_name(receiver)?);
        rendered.push('.');
    }
    rendered.push_str(&super::rendered_callable_name(&format!(
        "{}$default",
        function.name
    )));
    rendered.push('(');
    rendered.push_str(&parameters.join(", "));
    rendered.push_str("): ");
    rendered.push_str(&super::source_type_name(function.ret)?);
    rendered.push_str(" defined in ");
    rendered.push_str(owner(ir));
    Ok(rendered)
}

fn function_order(
    function: &crate::ir::IrPackageFunction,
) -> Result<(String, String, Vec<String>, usize), UndeterminedDiagnosticType> {
    Ok((
        function.name.clone(),
        function
            .receiver
            .map(super::qualified_type_name)
            .transpose()?
            .unwrap_or_default(),
        value_parameters(function)?
            .iter()
            .map(|(_, ty)| super::qualified_type_name(*ty))
            .collect::<Result<Vec<_>, _>>()?,
        function.type_params.len(),
    ))
}

fn value_parameters(
    function: &crate::ir::IrPackageFunction,
) -> Result<&[(String, crate::types::Ty)], UndeterminedDiagnosticType> {
    function
        .params
        .get(function.context_count..)
        .ok_or(UndeterminedDiagnosticType::InvalidContextParameterCount)
}

fn owner(ir: &IrFile) -> &str {
    ir.package
        .as_deref()
        .filter(|package| !package.is_empty())
        .unwrap_or("root package")
}
