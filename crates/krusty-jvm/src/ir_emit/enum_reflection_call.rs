//! JVM realization of reified enum reflection calls.
//!
//! A concrete classifier dispatches to that enum's static helper. A reified type parameter left in
//! an inline template keeps the stdlib marker and placeholder that call-site specialization later
//! replaces.

use super::*;
use krusty::types::TyKind;

impl Emitter<'_> {
    /// Kotlin's mode-5 reified marker for the enum operation that follows it. Call-site
    /// specialization replaces the erased placeholder after it with the enum's own member.
    fn emit_reified_enum_marker(&mut self, identity: &str, code: &mut CodeBuilder) {
        code.push_int(5, self.cw);
        code.push_string(krusty::types::type_parameter_source_name(identity), self.cw);
        let marker = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "reifiedOperationMarker",
            "(ILjava/lang/String;)V",
        );
        code.invokestatic(marker, 2, 0);
    }

    /// `enumValues<E>()`: the enum's own `values()`, or, for a reified parameter of an inline
    /// template, the marker and the empty erased array kotlinc writes in its place.
    pub(super) fn emit_enum_values(&mut self, classifier: Ty, code: &mut CodeBuilder) {
        let classifier = classifier.non_null();
        match classifier.kind() {
            TyKind::Obj(classifier, _) => {
                let owner = classifier.render();
                let descriptor = format!("()[L{owner};");
                let method = self.cw.methodref(&owner, "values", &descriptor);
                code.invokestatic(method, 0, 1);
            }
            TyKind::TyParam(identity, _) => {
                self.emit_reified_enum_marker(identity.as_str(), code);
                code.push_int(0, self.cw);
                let class = self.cw.class_ref("java/lang/Enum");
                code.anewarray(class);
            }
            _ => unreachable!("checked enumValues classifier is enum or reified: {classifier:?}"),
        }
    }

    pub(super) fn emit_enum_entries(
        &mut self,
        call: ExprId,
        classifier: Ty,
        code: &mut CodeBuilder,
    ) {
        {
            let classifier = classifier.non_null();
            match classifier.kind() {
                TyKind::Obj(classifier, _) => {
                    let owner = classifier.render();
                    let method =
                        self.cw
                            .methodref(&owner, "getEntries", "()Lkotlin/enums/EnumEntries;");
                    code.invokestatic(method, 0, 1);
                }
                TyKind::TyParam(identity, _) => {
                    // The reified read is the stdlib's inline template, so kotlinc marks the call
                    // site before the marker the call site's copy later specializes.
                    self.mark_inline_call_site_line(call, code);
                    self.emit_reified_enum_marker(identity.as_str(), code);
                    code.aconst_null();
                    let class = self.cw.class_ref("kotlin/enums/EnumEntries");
                    code.checkcast(class);
                }
                _ => {
                    unreachable!(
                        "checked enumEntries classifier is enum or reified: {classifier:?}"
                    )
                }
            }
        }
    }

    pub(super) fn emit_enum_value_of(
        &mut self,
        call: ExprId,
        classifier: Ty,
        argument: ExprId,
        code: &mut CodeBuilder,
    ) {
        // `enumValueOf<E>` is the stdlib's reified INLINE template, so what follows is an
        // expansion rather than a call on this line. kotlinc marks the call site here and lets the
        // argument's own line join it at the same offset; the `Enum.valueOf` this ends with belongs
        // to the expansion.
        self.mark_inline_call_site_line(call, code);
        {
            let classifier = classifier.non_null();
            match classifier.kind() {
                TyKind::TyParam(identity, _) => {
                    // `enumValueOf` is not `@InlineOnly`, so kotlinc's inliner stores its argument once,
                    // into the parameter's slot, before the body runs; the body then reads that slot.
                    self.emit_value(argument, code);
                    let argument = self.frame.enter_temp(TempRole::InlineArgument, Ty::String);
                    let name = argument.slot();
                    store(Ty::String, name, code);
                    let lease = self.lease_frame_temporary(argument, Ty::String);
                    // Kotlin's public inline template keeps the reified classifier as the standard
                    // mode-5 marker plus a null Class placeholder. A consuming compiler replaces that
                    // placeholder at the call site.
                    self.emit_reified_enum_marker(identity.as_str(), code);
                    code.aconst_null();
                    load(Ty::String, name, code);
                    self.release_temporary(lease);
                    let method = self.cw.methodref(
                        "java/lang/Enum",
                        "valueOf",
                        "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
                    );
                    code.invokestatic(method, 2, 1);
                }
                TyKind::Obj(classifier, _) => {
                    self.emit_value(argument, code);
                    let owner = classifier.render();
                    let descriptor = format!("(Ljava/lang/String;)L{owner};");
                    let method = self.cw.methodref(&owner, "valueOf", &descriptor);
                    code.invokestatic(method, 1, 1);
                }
                _ => {
                    unreachable!(
                        "checked enumValueOf classifier is enum or reified: {classifier:?}"
                    )
                }
            }
        }
    }
}
