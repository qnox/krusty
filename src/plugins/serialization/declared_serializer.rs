//! The serializer instance a declaration names for itself with `@Serializable(with = X::class)`.

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::plugins::PluginContext;
use crate::types::TypeName;

/// An instance of the explicit serializer `X` a property (or its declared type) names: an
/// `object` serializer is its `INSTANCE`, whether this compilation declares it or the provider
/// confirms a classpath object; a class serializer is `new X()` (the no-argument constructor
/// user-defined property serializers have).
pub(super) fn build_field_serializer_instance(
    ir: &mut IrFile,
    ctx: &PluginContext,
    classifier: TypeName,
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
        ir.new_external(&classifier.render(), "()V", vec![])
    }
}
