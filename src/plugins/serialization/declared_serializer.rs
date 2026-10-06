//! The serializer instance a declaration names for itself with `@Serializable(with = X::class)`.

use crate::ir::{ExprId, IrExpr, IrFile, Spelled};
use crate::plugins::PluginContext;
use crate::types::{Ty, TypeName};

use super::type_parameter_serializers::TypeParameterSerializers;

/// An instance of the explicit serializer `X` a property (or its declared type) names: an
/// `object` serializer is its `INSTANCE`, whether this compilation declares it or the provider
/// confirms a classpath object; a class serializer is constructed through the exact primary
/// constructor selected by checked FIR.
pub(super) fn build_field_serializer_instance(
    ir: &mut IrFile,
    ctx: &PluginContext,
    classifier: TypeName,
    served: Ty,
    scope: TypeParameterSerializers<'_>,
    annotated: &Spelled,
) -> ExprId {
    if let Some(object) = ir
        .classes
        .iter()
        .position(|class| class.fq_name_id() == classifier && class.is_object)
    {
        ir.add_expr(IrExpr::StaticInstance {
            owner: object as u32,
            ty: object as u32,
            field: "INSTANCE",
        })
    } else if ctx.external_serializer_is_object(classifier) == Some(true) {
        ir.add_expr(IrExpr::ExternalStaticInstance {
            owner: classifier,
            ty: classifier,
            field: "INSTANCE".to_string(),
        })
    } else {
        let type_args = served.non_null().type_args();
        let Some(construction) = u32::try_from(type_args.len())
            .ok()
            .and_then(|arity| {
                ir.type_use_serializer_constructions
                    .get(&(classifier, arity))
            })
            .cloned()
        else {
            return super::element_serializer::unsupported_element_serializer(ir, served);
        };
        let Some(arguments) = construction
            .operands
            .iter()
            .map(|&ordinal| {
                let ordinal = usize::try_from(ordinal).ok()?;
                super::element_serializer::element_serializer_plan_in(
                    ir,
                    ctx,
                    type_args.get(ordinal)?,
                    scope,
                    annotated.arg(ordinal),
                )
            })
            .collect::<Option<Vec<_>>>()
        else {
            return super::element_serializer::unsupported_element_serializer(ir, served);
        };
        let arguments = arguments
            .into_iter()
            .map(|argument| super::element_serializer::emit_narrowed_argument(ir, argument))
            .collect();
        emit_selected_construction(ir, construction, arguments)
    }
}

pub(super) fn emit_selected_construction(
    ir: &mut IrFile,
    construction: crate::ir::IrCustomSerializerConstruction,
    arguments: Vec<ExprId>,
) -> ExprId {
    let declared_here = ir.class_id_by_name(construction.serializer).is_some();
    let (target, external_target) = match construction.target {
        crate::ir::IrCustomSerializerConstructorTarget::Module(target) => (Some(target), None),
        crate::ir::IrCustomSerializerConstructorTarget::External(target) => {
            (None, Some(target.declaration))
        }
    };
    let new = ir.add_expr(IrExpr::New {
        internal: construction.serializer,
        args: arguments,
        ctor_params: (!declared_here).then(|| construction.parameters.to_vec()),
        ctor_desc: None,
        external_target,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    ir.construction_declared_params
        .insert(new, construction.parameters);
    if let Some(target) = target {
        ir.construction_targets.insert(new, target);
    }
    new
}
