//! `X::class`, `x::class` and the `KClass` members a class token answers itself.
//!
//! A literal over a TYPE names its classifier statically; a literal over a VALUE reads the type
//! information the object's table carries, so `val x: CharSequence = ""` answers `String::class`.
//! A scalar is boxed first, which is why `(n++)::class` answers `Int::class`. Either way the result
//! is a new class token over the classifier's one record (`crate::class_tokens`).
//!
//! The record's names are declaration facts: a classifier this file declares is named from the
//! facts common IR recorded for it, any other from the declaration name its provider published.

use krusty::backend::class_names::{declared_class, published_classifier};
use krusty::libraries::{ClassNameKind, CompilerIntrinsic};
use krusty::types::{wk, TypeName};

use crate::class_tokens::{emit_runtime_info, QUALIFIED_NAME_FIELD, SIMPLE_NAME_FIELD};

use super::*;

impl Body<'_> {
    /// `::class` over the type `classifier` or the value `value`.
    pub(super) fn class_literal(
        &mut self,
        literal: u32,
        classifier: Option<Ty>,
        value: Option<u32>,
    ) -> Result<(), Unsupported> {
        let info = match (classifier, value) {
            (_, Some(value)) => {
                // The receiver's own class, which its table names: the static type is only an
                // upper bound of it.
                self.value(value, objects::any())?;
                let string_info = self.type_info(wk::string())?;
                let scratch = self.local(REFERENCE);
                emit_runtime_info(
                    &mut self.code,
                    &self.runtime.objects,
                    self.runtime.string,
                    string_info,
                    scratch,
                );
                self.class_token();
                return Ok(());
            }
            (Some(ty), None) => {
                let target = ty.non_null();
                if target.is_ty_param() {
                    if self
                        .ir
                        .is_reified_template_operation(self.scope, literal, target)
                    {
                        // Only a reified declaration's own template names its unsubstituted
                        // parameter; every executed expansion names the call's type argument.
                        self.code.unreachable();
                        return Ok(());
                    }
                    return Err(format!(
                        "a class literal of the type parameter `{}`",
                        describe(target)
                    ));
                }
                let classifier = target
                    .kotlin_class_internal()
                    .ok_or_else(|| format!("a class literal of `{}`", describe(target)))?;
                self.type_info(classifier)?
            }
            (None, None) => return Err("a class literal naming neither a type nor a value".into()),
        };
        // Named just above, so never null.
        self.code.global_get(info).ref_as_non_null();
        self.class_token();
        Ok(())
    }

    /// Replace the record on the stack with a new class token for it.
    fn class_token(&mut self) {
        let scratch = self.local(ValType::reference(self.runtime.objects.type_infos.ty));
        self.runtime
            .class_tokens
            .emit_token(&mut self.code, scratch);
    }

    /// The global holding `classifier`'s record, named here: a classifier this file declares
    /// from the facts common IR recorded for it, any other from its published declaration name.
    fn type_info(&mut self, classifier: TypeName) -> Result<u32, Unsupported> {
        let names = match self.ir.class_id_by_name(classifier) {
            Some(class) => declared_class(self.ir, class),
            None => published_classifier(self.classifiers, classifier).ok_or_else(|| {
                format!(
                    "a class literal of `{}`, whose declaration name is not published",
                    classifier.render().replace('/', ".")
                )
            })?,
        };
        let infos = &mut self.runtime.objects.type_infos;
        let global = infos.global(self.module, classifier);
        infos.name(self.module, classifier, &names);
        Ok(global)
    }

    /// Which name the external property `target` reads, when its getter is the platform stdlib's
    /// own `KClass` name getter.
    pub(super) fn class_name_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> Option<ClassNameKind> {
        let property = self.callables.property(target)?;
        match self
            .callables
            .callable(property.getter)?
            .compiler_operation()?
        {
            CompilerIntrinsic::ClassName(kind) => Some(kind),
            _ => None,
        }
    }

    /// `k.simpleName` or `k.qualifiedName`.
    pub(super) fn class_name(
        &mut self,
        kind: ClassNameKind,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        self.value(receiver, Ty::obj_name(wk::kclass()))?;
        let field = match kind {
            ClassNameKind::Simple => SIMPLE_NAME_FIELD,
            ClassNameKind::Qualified => QUALIFIED_NAME_FIELD,
        };
        let info = self.runtime.objects.type_infos.ty;
        self.runtime
            .class_tokens
            .emit_name(&mut self.code, info, field);
        Ok(())
    }
}

/// Name every record a runtime table reserved without naming it (a box's, an exception's), from
/// its classifier's published declaration name. One these facts do not name stays null for a
/// later file's facts to name; a literal never reads an unnamed record, since it names its own.
pub(super) fn name_type_infos(
    module: &mut Module,
    runtime: &mut Runtime,
    classifiers: &dyn krusty::backend::BackendClassifierSource,
) {
    for classifier in runtime.objects.type_infos.unnamed().to_vec() {
        if let Some(names) = published_classifier(classifiers, classifier) {
            runtime.objects.type_infos.name(module, classifier, &names);
        }
    }
}
