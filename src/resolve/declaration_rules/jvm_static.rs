//! Declaration rules for `@JvmStatic` functions, read from the resolved `kotlin/jvm/JvmStatic`
//! identity. A function passing them is a member the JVM backend can make static: a named
//! object's (as a static method) or a companion's (through a static bridge on its outer class).

use crate::ast::{ClassDecl, FunDecl};
use crate::diag::Span;
use crate::resolve::{Checker, CheckerScope};
use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

/// The declaration that holds a function, as far as `@JvmStatic` is concerned.
#[derive(Clone, Copy)]
pub(in crate::resolve) enum JvmStaticHost {
    NamedObject,
    Companion {
        of_interface: bool,
    },
    /// Top level, a local scope, an anonymous object, an enum entry, or an ordinary classifier.
    Elsewhere,
}

impl Checker<'_> {
    pub(super) fn jvm_static_host(
        &self,
        class: &ClassDecl,
        owner: Option<TypeName>,
        anonymous: bool,
    ) -> JvmStaticHost {
        if !class.is_singleton() || anonymous {
            return JvmStaticHost::Elsewhere;
        }
        let Some(owner) = owner else {
            return JvmStaticHost::Elsewhere;
        };
        let outer = owner
            .nested_owner()
            .and_then(|outer| self.fed_source().classifier(outer))
            .filter(|outer| {
                outer
                    .companion_object
                    .as_ref()
                    .is_some_and(|(_, companion)| *companion == owner)
            });
        match outer {
            Some(outer) => JvmStaticHost::Companion {
                of_interface: outer.is_interface(),
            },
            None => JvmStaticHost::NamedObject,
        }
    }

    /// Reject `@JvmStatic` on `function` where `host` cannot make it static, at the annotation.
    pub(in crate::resolve) fn check_jvm_static_function(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
        host: JvmStaticHost,
    ) {
        let Some(annotation) =
            self.resolved_annotation_in_scope(scope, &function.annotations, "kotlin/jvm/JvmStatic")
        else {
            return;
        };
        let message = match host {
            JvmStaticHost::Elsewhere => {
                "only members in named objects and companion objects can be annotated with \
                 '@JvmStatic'."
            }
            JvmStaticHost::Companion { of_interface: true } if !function.visibility.is_public() => {
                "only public members in interface companion objects can be annotated with \
                 '@JvmStatic'."
            }
            JvmStaticHost::NamedObject if function.is_override() => {
                "override member cannot be '@JvmStatic' in an object."
            }
            JvmStaticHost::NamedObject | JvmStaticHost::Companion { .. } => return,
        };
        self.diags.error(
            Span::new(annotation.at, annotation.span.hi),
            message.to_string(),
        );
    }
}
