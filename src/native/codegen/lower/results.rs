//! `kotlin.Result`: a value class of the stdlib, carried as its value.
//!
//! The checked declaration says `Result` wraps an `Any?`, so this target's representation policy
//! carries a `Result<T>` as that reference, exactly as it carries any value class over a
//! reference: a SUCCESS is the value itself and a FAILURE is a marker object holding the
//! exception — which is Kotlin's own representation, the one its stdlib builds. The runtime
//! answers every member as a question about that one reference (`kt_result_*`).
//!
//! Every member here is recognized by the classifier's IDENTITY, and only where the inventory
//! (`native::value_classes`) holds `Result` as the value class its declaration says it is. Nothing
//! is inferred from how a provider spells an owner.

use super::*;

/// The classifier every member here belongs to.
const RESULT: &str = "kotlin/Result";
/// Its companion, which holds no state: `Result.success(x)` is the value's own constructor.
const RESULT_COMPANION: &str = "kotlin/Result$Companion";

impl BodyLowering<'_, '_, '_> {
    /// Whether `classifier` is `kotlin.Result`, carried as the value class it is declared to be.
    pub(super) fn is_result(&self, classifier: TypeName) -> bool {
        classifier.matches(RESULT) && self.file.values.is_value_class(classifier)
    }

    /// Whether `classifier` is `Result`'s companion, which has no instance here: every member of
    /// it this target realizes takes its arguments alone.
    pub(super) fn is_result_companion(&self, classifier: TypeName) -> bool {
        classifier.matches(RESULT_COMPANION)
            && self
                .file
                .values
                .is_value_class(crate::types::type_name(RESULT))
    }

    /// A member of `Result` — or `getOrThrow`, the stdlib's extension on it — called on
    /// `receiver`, or `None` when this is not one.
    pub(super) fn result_member(
        &mut self,
        owner: TypeName,
        name: &str,
        params: &[Ty],
        receiver: u32,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let symbol = if self.is_result(owner) {
            match (name, params) {
                ("getOrNull", []) => "kt_result_get_or_null",
                ("exceptionOrNull", []) => "kt_result_exception_or_null",
                ("toString", []) => "kt_result_to_string",
                _ => return None,
            }
        } else {
            // `getOrThrow` is a top-level extension, declared beside `Result` rather than on it:
            // what identifies it is the RECEIVER's type.
            let on_result = self
                .type_of(receiver)
                .and_then(|ty| ty.non_null().obj_internal())
                .is_some_and(|classifier| self.is_result(classifier));
            if !(on_result && name == "getOrThrow" && params.is_empty()) {
                return None;
            }
            "kt_result_get_or_throw"
        };
        let result = Ty::Obj(crate::types::type_name(RESULT), &[]);
        Some(self.result_call(symbol, receiver, result, ret))
    }

    /// `Result.success(x)` / `Result.failure(e)`, or `None` for any other companion member.
    pub(super) fn result_companion_member(
        &mut self,
        owner: TypeName,
        name: &str,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !self.is_result_companion(owner) {
            return None;
        }
        let symbol = match (name, args) {
            ("success", [_]) => "kt_result_success",
            ("failure", [_]) => "kt_result_failure",
            _ => return None,
        };
        // What a success holds, or a failure's exception: any value, a boxed one included.
        Some(self.result_call(symbol, args[0], Ty::nullable(any()), ret))
    }

    /// `r.isSuccess` / `r.isFailure`, as the runtime function answering it, or `None`.
    pub(super) fn result_predicate(&self, owner: TypeName, name: &str) -> Option<&'static str> {
        if !self.is_result(owner) {
            return None;
        }
        match name {
            "isSuccess" => Some("kt_result_is_success"),
            "isFailure" => Some("kt_result_is_failure"),
            _ => None,
        }
    }

    /// The runtime function answering a `Result` PROPERTY read, or `None` for any other.
    pub(super) fn result_property(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        self.result_predicate(getter.callable.owner, &property.name)
    }

    /// One runtime question about the reference a `Result` is — which is its value as the
    /// representation policy carries it, so `operand` is taken at `operand_ty` and handed over as
    /// it is. The runtime answers a reference whatever the site's own type — `getOrThrow` on a
    /// `Result<Int>` answers the box the value is held in — and reconciling the two is this
    /// boundary's job, between the PROJECTED types: a `Result` answered is that same reference.
    fn result_call(
        &mut self,
        symbol: &str,
        operand: u32,
        operand_ty: Ty,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(operand) = self.coerce(operand, operand_ty)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(produced) = self.runtime_call(symbol, &[any()], any(), &[operand])? else {
            return Ok(None);
        };
        let ret = self.file.values.project(ret);
        self.convert_carriers(produced, Some(Ty::nullable(any())), ret)
    }
}
