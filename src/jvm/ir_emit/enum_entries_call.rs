//! JVM realization of the zero-argument `enumEntries<T>()`.
//!
//! A concrete classifier is that enum's `getEntries()`. A reified type parameter left in an inline
//! template is the mode-5 marker plus a null `EnumEntries` placeholder; call-site specialization
//! replaces the classifier before emission of the expanded call.

use super::*;

impl Emitter<'_> {
    pub(super) fn emit_enum_entries(&mut self, classifier: Ty, code: &mut CodeBuilder) {
        match classifier.non_null() {
            Ty::Obj(classifier, _) => {
                let owner = classifier.render();
                let method =
                    self.cw
                        .methodref(&owner, "getEntries", "()Lkotlin/enums/EnumEntries;");
                code.invokestatic(method, 0, 1);
            }
            Ty::TyParam(identity, _) => {
                code.push_int(5, self.cw);
                code.push_string(crate::types::type_parameter_source_name(identity), self.cw);
                let marker = self.cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(marker, 2, 0);
                code.aconst_null();
                let class = self.cw.class_ref("kotlin/enums/EnumEntries");
                code.checkcast(class);
            }
            classifier => {
                unreachable!("checked enumEntries classifier is enum or reified: {classifier:?}")
            }
        }
    }
}
