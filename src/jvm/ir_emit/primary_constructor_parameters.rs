//! The primary constructor's physical parameters as every consumer of the `<init>` sees them: the
//! backing field each one stores into, and the nullability and user annotations its source
//! parameter carries. A plain parameter (`class C(p: String)`) is an argument only, so it has no
//! field but is annotated like a property-backed one.

use super::*;

/// The backing field each physical constructor parameter stores into, `None` for a plain parameter.
/// A synthesized class (no `ctor_args`) stores every parameter into its leading fields.
pub(super) fn primary_ctor_parameter_fields(
    c: &IrClass,
    parameter_count: usize,
) -> Vec<Option<usize>> {
    if c.ctor_args.is_empty() {
        (0..parameter_count).map(Some).collect()
    } else if c
        .ctor_args
        .iter()
        .any(|argument| argument.field_index.is_some())
    {
        c.ctor_args
            .iter()
            .map(|argument| argument.field_index.map(|field| field as usize))
            .collect()
    } else {
        let mut field = 0usize;
        c.ctor_args
            .iter()
            .map(|argument| {
                argument.is_field.then(|| {
                    let current = field;
                    field += 1;
                    current
                })
            })
            .collect()
    }
}

/// One SOURCE parameter of the primary constructor: the compiler's prefix (enclosing instance,
/// lexical captures) is left out, as kotlinc sizes the parameter-annotation tables by the source
/// parameters.
pub(super) struct SourceCtorParameter<'a> {
    /// 0 = primitive, platform or nullable-bounded type parameter (no annotation), 1 = non-null
    /// reference (`@NotNull`), 2 = nullable reference (`@Nullable`).
    pub(super) nullability: u8,
    pub(super) annotations: Option<&'a crate::ir::DeclarationAnnotations>,
}

/// Whether the primary constructor's parameters carry `@NotNull`/`@Nullable`. kotlinc annotates no
/// private declaration: a declared `private constructor`, a value class's synthetic primary, and a
/// primary hidden behind a marker accessor are all invisible from outside.
fn annotates_parameter_nullability(ir: &IrFile, c: &IrClass) -> bool {
    !c.is_value
        && !ir.has_value_param_ctor(&c.fq_name())
        && ir.ctor_visibilities.get(&c.fq_name_id()) != Some(&crate::types::Visibility::Private)
}

pub(super) fn primary_ctor_source_parameters<'a>(
    ir: &IrFile,
    c: &'a IrClass,
) -> Vec<SourceCtorParameter<'a>> {
    let mut parameters = source_parameters(ir, c);
    if !annotates_parameter_nullability(ir, c) {
        for parameter in &mut parameters {
            parameter.nullability = 0;
        }
    }
    parameters
}

fn source_parameters<'a>(ir: &IrFile, c: &'a IrClass) -> Vec<SourceCtorParameter<'a>> {
    let fq_name = c.fq_name();
    let prefix = c.constructor_prefix_count as usize;
    if c.ctor_args.is_empty() {
        return c.fields[..c.ctor_param_count as usize]
            .iter()
            .skip(prefix)
            .map(|field| SourceCtorParameter {
                nullability: field_nullability_kind(ir, &fq_name, &field.name, field.ty),
                annotations: None,
            })
            .collect();
    }
    let fields = primary_ctor_parameter_fields(c, c.ctor_args.len());
    c.ctor_args
        .iter()
        .zip(fields)
        .enumerate()
        .skip(prefix)
        .map(|(index, (argument, field))| SourceCtorParameter {
            nullability: match field {
                Some(field) => {
                    let field = &c.fields[field];
                    field_nullability_kind(ir, &fq_name, &field.name, field.ty)
                }
                None => plain_parameter_nullability(ir, c, &fq_name, argument),
            },
            annotations: c.ctor_param_annotations.get(index),
        })
        .collect()
}

fn plain_parameter_nullability(
    ir: &IrFile,
    c: &IrClass,
    fq_name: &str,
    argument: &crate::ir::IrCtorArg,
) -> u8 {
    let descriptor = crate::jvm::names::type_descriptor(argument.ty);
    if !(descriptor.starts_with('L') || descriptor.starts_with('[')) {
        return 0;
    }
    if matches!(argument.ty, Ty::PlatformNullable(_)) {
        0
    } else if let Some(parameter) = argument.type_param {
        let name = c
            .type_params
            .get(parameter as usize)
            .expect("a constructor parameter's type parameter is declared by its class");
        u8::from(!ir.class_type_param_admits_null(fq_name, name))
    } else if matches!(argument.ty, Ty::Nullable(_)) {
        2
    } else {
        1
    }
}
