//! What the Parcelize transform needs from a file, recorded at the frontend/backend handoff: the
//! parcelized classes, the serializer of each of their properties, and the `Parcel` members the
//! serializers call. Every class the port does not cover yet is rejected here.

use crate::ir::{ClassId, ExprId, FunId, IrExpr, IrFile};
use crate::plugins::runtime_declarations::PluginRuntime;
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{Ty, TypeName};

use super::frontend::{declaration, DESCRIBE_CONTENTS, WRITE_TO_PARCEL};
use super::serializers::{ParcelRuntime, ParcelSerializer, ParcelValue};

/// The Parcelize facts of one file.
pub struct ParcelizeFileFacts {
    /// The file, which roots the local `Creator` classes' naming provenance.
    pub(super) source: crate::ir::IrModuleSource,
    pub(super) runtime: ParcelRuntime,
    pub(super) classes: Vec<ParcelizedClass>,
}

/// A class the transform completes.
pub(super) struct ParcelizedClass {
    pub(super) class: ClassId,
    pub(super) describe_contents: FunId,
    pub(super) write_to_parcel: FunId,
    /// The primary-constructor properties, in parameter order.
    pub(super) properties: Vec<ParcelableProperty>,
}

/// One primary-constructor property kotlinc's `parcelableProperties` lists.
pub(super) struct ParcelableProperty {
    /// The backing field's index in the class.
    pub(super) field: u32,
    pub(super) ty: Ty,
    pub(super) serializer: ParcelSerializer,
}

const PARCELABLE: &str = "android/os/Parcelable";

/// The plugin-owned body a generated member holds until [`super::generate`] completes it.
pub(super) const GENERATED_BODY: &str = "generated-body";

/// Record the parcelized classes of `ir`, or reject what the port does not cover.
pub(super) fn record(
    ir: &mut IrFile,
    annotations: &[TypeName],
    source: &PluginSourceView<'_>,
    runtime: &mut dyn PluginRuntime,
    rejections: &mut Vec<PluginRejection>,
) {
    let mut classes = Vec::new();
    for (index, class) in ir.classes.iter().enumerate() {
        let parcelized = class
            .applied_annotations
            .applications()
            .any(|annotation| annotations.contains(&annotation.internal));
        if !parcelized {
            continue;
        }
        match parcelized_class(ir, index as ClassId) {
            Ok(class) => classes.push(class),
            Err(what) => rejections.push(rejection(source, class, what)),
        }
    }
    if classes.is_empty() || !rejections.is_empty() {
        return;
    }
    let used = classes
        .iter()
        .flat_map(|class| &class.properties)
        .map(|property| property.serializer.value)
        .collect::<Vec<_>>();
    let runtime = match ParcelRuntime::bind(runtime, &used) {
        Ok(runtime) => runtime,
        Err(missing) => {
            let class = &ir.classes[classes[0].class as usize];
            rejections.push(PluginRejection {
                span: rejection(source, class, "").span,
                message: format!("krusty's Parcelize compiler plugin cannot run: {missing}"),
            });
            return;
        }
    };
    // Each generated member is implemented from here on, for every declaration record; its body
    // is realized with the rest of the transform.
    for class in &classes {
        for function in [class.describe_contents, class.write_to_parcel] {
            let placeholder = generated_body_placeholder(ir);
            assert!(
                ir.functions[function as usize]
                    .body
                    .replace(placeholder)
                    .is_none(),
                "a frontend-declared Parcelize member has no body of its own"
            );
        }
    }
    let mut top_level = ir.classes[classes[0].class as usize].fq_name_id();
    while let Some(owner) = top_level.nested_owner() {
        top_level = owner;
    }
    let source = crate::ir::IrModuleSource {
        source: source.source,
        package: top_level.namespace(),
    };
    ir.parcelize = Some(Box::new(ParcelizeFileFacts {
        source,
        runtime,
        classes,
    }));
}

fn generated_body_placeholder(ir: &mut IrFile) -> ExprId {
    ir.add_expr(IrExpr::PluginPlaceholder {
        plugin: "parcelize",
        kind: GENERATED_BODY,
        exprs: Vec::new(),
        data: Vec::new(),
        types: Vec::new(),
    })
}

/// The facts of a parcelized class, or the construct the port does not cover yet.
fn parcelized_class(ir: &IrFile, index: ClassId) -> Result<ParcelizedClass, &'static str> {
    let class = &ir.classes[index as usize];
    if class.is_object
        || class.is_enum
        || class.is_interface
        || class.is_annotation
        || class.is_value
    {
        return Err("a parcelized object, enum, interface or value class");
    }
    if class.is_abstract || class.is_sealed || class.is_open {
        return Err("a parcelized class that is not final");
    }
    if class.is_inner_class || class.is_local_class {
        return Err("a parcelized inner or local class");
    }
    if !class.superclass.matches("kotlin/Any") {
        return Err("a parcelized class with a superclass");
    }
    if class.companion_class.is_some() {
        return Err("a parcelized class with a companion object");
    }
    if !class
        .interfaces
        .iter()
        .any(|interface| interface.matches(PARCELABLE))
    {
        return Err("a parcelized class that does not implement Parcelable itself");
    }
    if !class.type_params.is_empty() {
        return Err("a generic parcelized class");
    }
    if class.applied_annotations.applications().any(|annotation| {
        super::is_runtime_annotation(annotation.internal) && !is_parcelize(annotation.internal)
    }) {
        return Err("a parcelized class with a Parcelize annotation other than @Parcelize");
    }
    let owner = class.fq_name_id();
    let generated = |operation| {
        ir.plugin_declaration_functions
            .get(&declaration(operation))
            .into_iter()
            .flatten()
            .copied()
            .find(|&function| ir.functions[function as usize].dispatch_receiver == Some(owner))
    };
    let (Some(describe_contents), Some(write_to_parcel)) =
        (generated(DESCRIBE_CONTENTS), generated(WRITE_TO_PARCEL))
    else {
        return Err("a parcelized class that declares describeContents or writeToParcel");
    };
    if class.ctor_args.is_empty() {
        return Err("a parcelized class without primary-constructor properties");
    }
    let mut properties = Vec::with_capacity(class.ctor_args.len());
    for parameter in &class.ctor_args {
        let (true, Some(name), Some(field)) =
            (parameter.is_field, &parameter.name, parameter.field_index)
        else {
            return Err("a primary-constructor parameter that is not a property");
        };
        let property = class
            .properties
            .iter()
            .find(|property| property.name == *name)
            .expect("a primary-constructor property is one of the class's properties");
        if property
            .annotations
            .iter()
            .any(|annotation| super::is_runtime_annotation(*annotation))
        {
            return Err("a property with a Parcelize annotation");
        }
        let Some((value, null_aware)) = ParcelValue::of(parameter.ty) else {
            return Err("a property of this type");
        };
        properties.push(ParcelableProperty {
            field,
            ty: parameter.ty,
            serializer: ParcelSerializer { value, null_aware },
        });
    }
    let stored_in_body = class.properties.iter().any(|property| {
        property.backing_field.is_some()
            && !class
                .ctor_args
                .iter()
                .any(|parameter| parameter.name.as_deref() == Some(property.name.as_str()))
    });
    if stored_in_body {
        return Err("a parcelized class with a property declared in its body");
    }
    Ok(ParcelizedClass {
        class: index,
        describe_contents,
        write_to_parcel,
        properties,
    })
}

fn is_parcelize(annotation: TypeName) -> bool {
    super::PARCELIZE_ANNOTATIONS
        .iter()
        .any(|parcelize| annotation.matches(parcelize))
}

fn rejection(
    source: &PluginSourceView<'_>,
    class: &crate::ir::IrClass,
    what: &str,
) -> PluginRejection {
    let start = crate::plugins::line_start(source.text, class.decl_line);
    PluginRejection {
        span: crate::diag::Span::new(start, start),
        message: format!(
            "krusty's Parcelize compiler plugin does not support {what} yet; compile this module \
             with kotlinc"
        ),
    }
}
