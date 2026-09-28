//! The `EnclosingMethod` of a local, anonymous or generated class.

use super::*;

/// A local, anonymous or generated class's `EnclosingMethod`: the JVM class its scope belongs to,
/// and the method when that scope is one, as `(name, descriptor)`. kotlinc's rule: a function (a
/// local function being its own) names itself; a top-level property initializer names the file
/// facade with no method; a classifier's initializer names its primary `<init>`, or no method when
/// the classifier's storage is static (an object, or a companion whose fields its outer class
/// holds, and so its outer class).
pub(super) fn class_enclosure(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    c: &crate::ir::IrClass,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    match c.enclosure? {
        crate::ir::IrEnclosure::Function(function) => {
            function_enclosure(ir, override_results, function, facade)
        }
        crate::ir::IrEnclosure::PropertyAccessor {
            property,
            setter: is_setter,
        } => {
            let layout = ir.local_property_layouts.get(&property).unwrap_or_else(|| {
                panic!("property accessor enclosure has no finalized property realization")
            });
            let function = match layout {
                crate::ir::IrLocalPropertyLayout::TopLevelStorage {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        *getter
                    }
                }
                crate::ir::IrLocalPropertyLayout::TopLevelAccessor {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        Some(*getter)
                    }
                }
                crate::ir::IrLocalPropertyLayout::Member {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        *getter
                    }
                }
                crate::ir::IrLocalPropertyLayout::MemberExtension {
                    getter,
                    setter: accessor_setter,
                    ..
                } => {
                    if is_setter {
                        *accessor_setter
                    } else {
                        Some(*getter)
                    }
                }
            }
            .unwrap_or_else(|| panic!("source accessor enclosure has no emitted accessor"));
            function_enclosure(ir, override_results, function, facade)
        }
        crate::ir::IrEnclosure::Constructor { class, ordinal } => {
            let declaration = &ir.classes[class as usize];
            let mut parameters = if ordinal == 0 {
                class_ctor_jvm_tys(declaration)
            } else {
                let constructor = declaration
                    .secondary_ctors
                    .get(ordinal.saturating_sub(1) as usize)
                    .unwrap_or_else(|| panic!("constructor enclosure has no declared constructor"));
                jvm_tys(
                    &constructor
                        .prefix_params
                        .iter()
                        .chain(&constructor.params)
                        .copied()
                        .collect::<Vec<_>>(),
                )
            };
            if declaration.is_enum {
                parameters.splice(0..0, [Ty::String, Ty::Int]);
            }
            Some((
                declaration.fq_name(),
                Some((
                    "<init>".to_string(),
                    method_descriptor(&parameters, Ty::Unit),
                )),
            ))
        }
        crate::ir::IrEnclosure::File => Some((facade.to_string(), None)),
        crate::ir::IrEnclosure::ClassInitializer(class) => {
            let class = &ir.classes[class as usize];
            if class.is_companion {
                let outer = ir
                    .classes
                    .iter()
                    .find(|outer| outer.companion_class == Some(class.fq_name))?;
                let holder = if is_jvm_interface(outer) {
                    class
                } else {
                    outer
                };
                return Some((holder.fq_name(), None));
            }
            if static_storage(ir, class) {
                return Some((class.fq_name(), None));
            }
            class.has_primary_ctor.then(|| {
                let descriptor = method_descriptor(&class_ctor_jvm_tys(class), Ty::Unit);
                (class.fq_name(), Some(("<init>".to_string(), descriptor)))
            })
        }
    }
}

fn function_enclosure(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    function: crate::ir::FunId,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    let declaration = &ir.functions[function as usize];
    // A static class member (a value class's `constructor-impl`) has no receiver but its class.
    let owner = (declaration.dispatch_receiver)
        .or_else(|| {
            ir.classes
                .iter()
                .find(|c| c.methods.contains(&function))
                .map(|c| c.fq_name)
        })
        .map_or_else(|| facade.to_string(), TypeName::render);
    let descriptor = function_descriptor(ir, override_results, function);
    Some((owner, Some((declaration.name.clone(), descriptor))))
}
