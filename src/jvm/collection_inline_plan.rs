//! JVM-provider dependencies for collection inline expansion.

use crate::libraries::{InlineBodyPlan, InlineCollectionLocalNames, LibraryMember};
use crate::types::{type_name, Ty};

/// Structural dependencies of the exact stdlib collection `map`/`flatMap` inline declarations.
/// They are registered as opaque external identities before checked FIR is built; common lowering
/// never sees these JVM owners or descriptors.
pub(super) fn collection_transform(flatten: bool) -> InlineBodyPlan {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let mut factory = LibraryMember::new(
        "<init>".to_string(),
        Vec::new(),
        Ty::Unit,
        "()V".to_string(),
    );
    factory.owner = Some(type_name("java/util/ArrayList"));

    let append_parameter = if flatten {
        Ty::obj_args("kotlin/collections/Collection", &[any])
    } else {
        any
    };
    let mut append = LibraryMember::new(
        if flatten { "addAll" } else { "add" }.to_string(),
        vec![append_parameter],
        Ty::Boolean,
        if flatten {
            "(Ljava/util/Collection;)Z".to_string()
        } else {
            "(Ljava/lang/Object;)Z".to_string()
        },
    );
    append.owner = Some(type_name("java/util/List"));
    append.physical_params = vec![if flatten {
        Ty::obj("kotlin/collections/Collection")
    } else {
        any
    }];
    append.set_is_interface(true);

    InlineBodyPlan::CollectionTransform {
        lambda_parameter: 1,
        flatten,
        local_names: InlineCollectionLocalNames {
            outer_receiver: (if flatten { "flatMap" } else { "map" }).into(),
            inner_receiver: (if flatten { "flatMapTo" } else { "mapTo" }).into(),
            destination: "destination".into(),
            element: (if flatten { "element" } else { "item" }).into(),
        },
        factory: Box::new(factory),
        append: Box::new(append),
    }
}
