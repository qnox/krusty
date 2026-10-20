//! Checked types as kotlinc's IR spells them in a [`TypeIdentity`].

use krusty_plugin_ir::{ClassName, Classifier, TypeArgument, TypeIdentity, TypeParameterOwner};

use crate::types::{Ty, TypeName};

/// The type parameters a type may name, by their declared names: a dependency callable's own, in
/// declaration order. A module function's body names none.
#[derive(Clone, Copy)]
pub(super) struct TypeParameters<'a>(pub(super) &'a [String]);

impl TypeParameters<'_> {
    pub(super) const NONE: TypeParameters<'static> = TypeParameters(&[]);
}

/// `ty` as a [`TypeIdentity`], or what it is when the plugin IR cannot spell it yet.
pub(super) fn identity(ty: Ty, parameters: TypeParameters<'_>) -> Result<TypeIdentity, String> {
    let (ty, nullable) = match ty {
        Ty::Nullable(inner) => (*inner, true),
        // The type of the `null` literal is `Nothing?`.
        Ty::Null => (Ty::Nothing, true),
        other => (other, false),
    };
    let classifier = match ty {
        Ty::Unit => kotlin_class("Unit"),
        Ty::Nothing => kotlin_class("Nothing"),
        Ty::Obj(name, arguments) => Classifier::Class {
            class: class_name(name),
            arguments: arguments
                .iter()
                .map(|argument| type_argument(*argument, parameters))
                .collect::<Result<_, _>>()?,
        },
        Ty::TyParam(name, _) => match parameters.0.iter().position(|formal| formal == name) {
            Some(index) => Classifier::TypeParameter {
                owner: TypeParameterOwner::Callable,
                index: index as u32,
            },
            None => return Err(format!("the type parameter `{name}` of a class")),
        },
        Ty::PlatformNullable(_) => return Err("a platform type".into()),
        Ty::Fun(_) => return Err("a function type".into()),
        Ty::DefinitelyNotNull(_) => return Err("a definitely non-null type".into()),
        Ty::Intersection(_) => return Err("an intersection type".into()),
        Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_) => {
            return Err("a projection outside a type argument".into())
        }
        Ty::Null | Ty::Nullable(_) | Ty::Error | Ty::Pending => {
            return Err(format!("the unchecked type {ty:?}"))
        }
    };
    Ok(TypeIdentity {
        classifier,
        nullable,
        annotations: Vec::new(),
    })
}

fn type_argument(ty: Ty, parameters: TypeParameters<'_>) -> Result<TypeArgument, String> {
    Ok(match ty {
        Ty::StarProjection(_) => TypeArgument::Star,
        Ty::InProjection(inner) => TypeArgument::In(identity(*inner, parameters)?),
        Ty::OutProjection(inner) => TypeArgument::Out(identity(*inner, parameters)?),
        other => TypeArgument::Invariant(identity(other, parameters)?),
    })
}

fn kotlin_class(name: &str) -> Classifier {
    Classifier::Class {
        class: ClassName {
            package: "kotlin".into(),
            classes: vec![name.into()],
        },
        arguments: Vec::new(),
    }
}

/// `name`'s package and its classes from the outermost in, walked through the name tree.
pub(super) fn class_name(name: TypeName) -> ClassName {
    let mut classes = Vec::new();
    let mut class = name;
    while let Some(owner) = class.nested_owner() {
        classes.push(class.nested_segment_ref().to_string());
        class = owner;
    }
    classes.push(class.segment_ref().to_string());
    classes.reverse();
    ClassName {
        package: package_name(class.namespace()),
        classes,
    }
}

/// A package's dot-separated name, empty for the root package.
pub(super) fn package_name(package: TypeName) -> String {
    let mut segments = Vec::new();
    let mut current = package;
    while current != TypeName::ROOT {
        segments.push(current.segment_ref());
        current = current.namespace();
    }
    segments.reverse();
    segments.join(".")
}
