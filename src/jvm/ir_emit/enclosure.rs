//! The scope a local, anonymous or generated class is declared in, realized as the JVM class and
//! method its `EnclosingMethod` names.

use crate::ir::IrFile;
use crate::types::{Ty, TypeName};

use super::{
    class_ctor_jvm_tys, function_descriptor, is_jvm_interface, jvm_tys, method_descriptor,
    static_storage,
};

/// A local, anonymous or generated class's `EnclosingMethod`: the JVM class its scope belongs to,
/// and the method when that scope is one, as `(name, descriptor)`. kotlinc's rule: a function (a
/// local function being its own, a suspend lambda's class its `invokeSuspend`) names itself; a
/// top-level property initializer names the file
/// facade with no method; a classifier's initializer names its primary `<init>`, or no method when
/// the classifier's storage is static (an object, or a companion whose fields its outer class
/// holds, and so its outer class).
pub(in crate::jvm) fn class_enclosure(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    c: &crate::ir::IrClass,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    scope_enclosure(ir, override_results, c.enclosure?, facade)
}

/// Resolve a source property's accessor role to the exact finalized IR function the JVM emits.
///
/// Common IR keeps the semantic property identity and side. The finalized layout is the JVM
/// boundary that chooses its physical accessor function; consumers must share that decision rather
/// than reconstructing an accessor from its spelling.
pub(in crate::jvm) fn property_accessor_function(
    ir: &IrFile,
    property: crate::fir::PropertyId,
    is_setter: bool,
) -> crate::ir::FunId {
    assert!(
        ir.local_property_layouts.contains_key(&property),
        "property accessor enclosure has no finalized property realization"
    );
    realized_property_accessor(ir, property, is_setter)
        .unwrap_or_else(|| panic!("source accessor enclosure has no emitted accessor"))
}

/// [`property_accessor_function`] for a caller that has no use for a property its file does not
/// realize, or an accessor that is not emitted.
pub(in crate::jvm) fn realized_property_accessor(
    ir: &IrFile,
    property: crate::fir::PropertyId,
    is_setter: bool,
) -> Option<crate::ir::FunId> {
    match ir.local_property_layouts.get(&property)? {
        crate::ir::IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. }
        | crate::ir::IrLocalPropertyLayout::Member { getter, setter, .. } => {
            if is_setter {
                *setter
            } else {
                *getter
            }
        }
        crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. }
        | crate::ir::IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
            if is_setter {
                *setter
            } else {
                Some(*getter)
            }
        }
    }
}

fn scope_enclosure(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    enclosure: crate::ir::IrEnclosure,
    facade: &str,
) -> Option<(String, Option<(String, String)>)> {
    match enclosure {
        crate::ir::IrEnclosure::Function(function) => {
            function_enclosure(ir, override_results, function, facade)
        }
        // A suspend lambda realized as a class of its own became `Function` when its class was
        // declared; any other lambda belongs to the scope it is written in.
        crate::ir::IrEnclosure::Lambda(function) => scope_enclosure(
            ir,
            override_results,
            *ir.lambda_enclosures.get(&function)?,
            facade,
        ),
        crate::ir::IrEnclosure::PropertyAccessor {
            property,
            setter: is_setter,
        } => function_enclosure(
            ir,
            override_results,
            property_accessor_function(ir, property, is_setter),
            facade,
        ),
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
        crate::ir::IrEnclosure::Classifier(class) => {
            Some((ir.classes[class as usize].fq_name(), None))
        }
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
            // An enum entry's class initializes its body through its `(String, int)V` constructor,
            // which it declares with no primary constructor of its own.
            if class.is_enum_entry {
                let descriptor = method_descriptor(&[Ty::String, Ty::Int], Ty::Unit);
                return Some((class.fq_name(), Some(("<init>".to_string(), descriptor))));
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
    // A suspend member whose body moved to its static `$suspendImpl` keeps the member's class.
    // A value class's static `constructor-impl` has no receiver; its owner is the class that
    // declares the member.
    let owner = declaration
        .dispatch_receiver
        .or_else(|| {
            ir.jvm_suspend_impl_bodies
                .get(&function)
                .map(|&(owner, _)| owner)
        })
        .or_else(|| {
            ir.classes
                .iter()
                .find(|class| class.methods.contains(&function))
                .map(|class| class.fq_name)
        })
        .map(TypeName::render)
        .unwrap_or_else(|| facade.to_string());
    let descriptor = function_descriptor(ir, override_results, function);
    Some((owner, Some((declaration.name.clone(), descriptor))))
}
