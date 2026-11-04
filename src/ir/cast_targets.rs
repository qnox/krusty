//! A cast target as kotlinc's IR renderer spells it in `null cannot be cast to non-null type …`.
//!
//! Every backend that raises that NullPointerException renders the same way. They differ only on
//! where a top-level function lives: the JVM places one in a file facade, while a target without
//! facades keeps it in its package. So the caller supplies that owner. The JVM message is
//! `IrType.render()`, which writes the root package as `<root>`, so a root-package classifier or
//! type-parameter owner reads `<root>.Name` there; the native runtime message omits that prefix.

use super::IrFile;
use crate::types::{Ty, TyKind, TypeName};

impl IrFile {
    /// Render `ty` for a null-cast message. `top_level_owner` names the owner of a function that
    /// has no dispatch receiver, and `None` leaves such a function unqualified.
    /// `qualify_root_classifier` selects the IR renderer's `<root>.Type` spelling.
    pub fn rendered_cast_target(
        &self,
        ty: Ty,
        top_level_owner: &dyn Fn(u32) -> Option<TypeName>,
        qualify_root_classifier: bool,
    ) -> String {
        let arguments = |arguments: &mut dyn Iterator<Item = Ty>| {
            let rendered: Vec<String> = arguments
                .map(|argument| {
                    self.rendered_cast_target(argument, top_level_owner, qualify_root_classifier)
                })
                .collect();
            if rendered.is_empty() {
                String::new()
            } else {
                format!("<{}>", rendered.join(", "))
            }
        };
        match ty.kind() {
            TyKind::Unit => "kotlin.Unit".to_string(),
            TyKind::Nothing => "kotlin.Nothing".to_string(),
            TyKind::Null => "kotlin.Nothing?".to_string(),
            TyKind::Error => "<error>".to_string(),
            TyKind::Pending => "<pending>".to_string(),
            TyKind::Obj(name, types) => format!(
                "{}{}",
                rendered_owner(name, qualify_root_classifier),
                arguments(&mut types.iter().copied())
            ),
            TyKind::Nullable(inner) => format!(
                "{}?",
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            ),
            TyKind::PlatformNullable(inner) | TyKind::MutabilityFlexible(inner) => {
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            }
            TyKind::InProjection(inner) => format!(
                "in {}",
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            ),
            TyKind::OutProjection(inner) => format!(
                "out {}",
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            ),
            TyKind::StarProjection(_) => "*".to_string(),
            // IR spells a raw Java occurrence by its lower bound, whose argument is the erasure.
            TyKind::RawProjection(inner) => {
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            }
            TyKind::DefinitelyNotNull(inner) => format!(
                "{} & Any",
                self.rendered_cast_target(inner, top_level_owner, qualify_root_classifier)
            ),
            TyKind::Intersection(parts) => parts
                .iter()
                .map(|part| {
                    self.rendered_cast_target(*part, top_level_owner, qualify_root_classifier)
                })
                .collect::<Vec<_>>()
                .join(" & "),
            TyKind::TyParam(name, _) => self.rendered_type_parameter(
                name.as_str(),
                top_level_owner,
                qualify_root_classifier,
            ),
            TyKind::Fun(signature) => format!(
                "{}{}{}",
                if signature.suspend {
                    "kotlin.coroutines.SuspendFunction"
                } else {
                    "kotlin.Function"
                },
                signature.params.len(),
                arguments(&mut signature.params.iter().copied().chain([signature.ret]))
            ),
        }
    }

    /// Render one declaration-owned type parameter through its recorded semantic identity. The
    /// opaque identity is only compared; its coordinates are never parsed back into an owner.
    fn rendered_type_parameter(
        &self,
        identity: &str,
        top_level_owner: &dyn Fn(u32) -> Option<TypeName>,
        qualify_root: bool,
    ) -> String {
        let source = crate::types::type_parameter_source_name(identity);
        if let Some((&function, _)) = self
            .signatures
            .iter()
            .filter(|(_, signature)| {
                signature
                    .type_params
                    .iter()
                    .any(|parameter| parameter.semantic_name == identity)
            })
            .min_by_key(|(function, _)| *function)
        {
            let declaration = &self.functions[function as usize];
            let name = self
                .vc_declared_sigs
                .get(&function)
                .map_or(declaration.name.as_str(), |(name, _, _)| name.as_str());
            return match declaration
                .dispatch_receiver
                .or_else(|| top_level_owner(function))
            {
                Some(owner) => {
                    format!("{source} of {}.{name}", rendered_owner(owner, qualify_root))
                }
                None => format!("{source} of {name}"),
            };
        }
        if let Some((owner, _)) = self.class_signatures().find(|(_, signature)| {
            signature
                .type_params
                .iter()
                .any(|parameter| parameter.semantic_name == identity)
        }) {
            return format!("{source} of {}", rendered_owner(owner, qualify_root));
        }
        source.to_string()
    }
}

fn rendered_owner(owner: TypeName, qualify_root: bool) -> String {
    let rendered = owner.render().replace(['/', '$'], ".");
    if qualify_root && owner.package_matches("") {
        format!("<root>.{rendered}")
    } else {
        rendered
    }
}
