//! Compiler-selected built-in operations realized directly by Native lowering.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// A compiler-selected operation on built-in types, realized by the runtime.
    pub(super) fn intrinsic(
        &mut self,
        operation: IrIntrinsic,
        ret: Ty,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match operation {
            // `relational_operator` records that the call came from a `ComparisonCall`, which
            // lets a backend emit a branch instead of materializing the -1/0/1. This one answers
            // the same VALUE either way and leaves that to Cranelift, so the hint is ignored
            // rather than acted on.
            IrIntrinsic::PrimitiveCompare { operand, .. } => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Err("a malformed `compareTo`".into());
                };
                // The operand type named here is the erased one, which for an unsigned value is
                // the signed number sharing its bits — and ordering is exactly the question that
                // reads those bits differently. The receiver's own checked type decides.
                if let Some(element) = self
                    .checked_type(receiver)
                    .map(Ty::non_null)
                    .filter(|ty| ty.is_unsigned())
                {
                    return self.unsigned_compare(element, receiver, *argument, ret);
                }
                let Some(suffix) = scalar_suffix(operand) else {
                    return Err("`compareTo` on a non-scalar operand".into());
                };
                let left = self.coerce(receiver, operand)?;
                let right = self.coerce(*argument, operand)?;
                if self.terminated {
                    return Ok(None);
                }
                let (Some(left), Some(right)) = (left, right) else {
                    return Err("`compareTo` on `Unit`".into());
                };
                self.runtime_call(
                    &format!("kt_compare_{suffix}"),
                    &[operand, operand],
                    ret,
                    &[left, right],
                )
            }
            // `s[i]`, and the read a `for (c in s)` loop is lowered into: common lowering turns
            // that loop into a counted one over `StringLength` and this, so the two arrive
            // together and only one of them was answered.
            //
            // The runtime answers a `Char`; a site that asked for a boxed one — `for (c: Char? in
            // s)` — gets the conversion, which is why the answer is not handed straight back.
            IrIntrinsic::StringGet => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Err("a malformed string read".into());
                };
                self.string_get(receiver, *index, ret)
            }
            IrIntrinsic::Ieee754Equals { operand } => {
                let [left, right] = args else {
                    return Err("a malformed IEEE floating-point equality".into());
                };
                let operand = operand.canonical_semantic();
                if !matches!(operand, Ty::Float | Ty::Double) {
                    return Err(declined!(
                        "an IEEE equality whose operand is not floating-point (`{operand:?}`)"
                    ));
                }
                // Each argument's checked type carries its own nullability; the intrinsic carries
                // the common floating declaration type. Physical carriers cannot supply either
                // fact: a nullable float is a reference, and a property read through an interface
                // may have a representation type that is deliberately less specific.
                let left_ty = self.checked_type(*left);
                let right_ty = self.checked_type(*right);
                if [left_ty, right_ty]
                    .into_iter()
                    .any(|ty| ty.and_then(scalar_bound) != Some(operand))
                {
                    return Err(
                        "an IEEE equality whose checked operands do not match its floating type"
                            .into(),
                    );
                }
                self.ieee_equality(IrBinOp::Eq, *left, left_ty, *right, right_ty)
            }
            IrIntrinsic::ArrayGet => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Err("a malformed array read".into());
                };
                self.array_get(receiver, *index, ret)
            }
            IrIntrinsic::ArraySet => {
                let (Some(receiver), [index, value]) = (receiver, args) else {
                    return Err("a malformed array store".into());
                };
                self.array_set(receiver, *index, *value)
            }
            IrIntrinsic::ArraySize => {
                let Some(receiver) = receiver else {
                    return Err("a malformed array size".into());
                };
                self.array_size(receiver)
            }
            // `"$u"`: the frontend names the conversion rather than letting the template reach for
            // `Any.toString()`, because the value it would reach for is the signed number sharing
            // the bits.
            IrIntrinsic::UnsignedToString { source } => {
                let Some(receiver) = receiver else {
                    return Err("a malformed unsigned `toString`".into());
                };
                let Some(realized) = self.unsigned_intrinsic_to_string(source.non_null(), receiver)
                else {
                    return Err(declined!("an unsigned `toString` of `{source:?}`"));
                };
                realized
            }
            IrIntrinsic::StringLength => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `String.length`".into());
                };
                let value = self.reference(receiver)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_string_length", &[any()], ret, &[value])
            }
            // The frontend publishes the exact `kotlin.Enum.name` declaration as an intrinsic
            // after ordinary property selection. Native enum objects already carry that base
            // property's storage, so consume the selected identity through the same reader as an
            // external `Enum`-typed property access.
            IrIntrinsic::EnumName => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `Enum.name` read".into());
                };
                if !args.is_empty() {
                    return Err("a malformed `Enum.name` read".into());
                }
                self.enum_member("name", receiver)
            }
            IrIntrinsic::NullableAnyToString => {
                let Some(receiver) = receiver else {
                    return Err("a malformed `toString`".into());
                };
                let value = self.reference(receiver)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_to_string", &[any()], ret, &[value])
            }
            IrIntrinsic::GeneratedPropertyHash { ty } => {
                let [value] = args else {
                    return Err("a malformed data-class field hash".into());
                };
                self.field_hash(*value, ty)
            }
            IrIntrinsic::GeneratedPropertyEquals { ty } => {
                let [left, right] = args else {
                    return Err("a malformed data-class field comparison".into());
                };
                self.field_equals(*left, *right, ty)
            }
            // A data class rendering an ARRAY property shows its CONTENTS, not the identity an
            // array's own `toString` answers — `data class D(val xs: IntArray)` prints
            // `D(xs=[1, 2])`. That is the same rendering `xs.contentToString()` asks for, and the
            // runtime already answers it for every array it lays out.
            IrIntrinsic::DataClassArrayToString { .. } => {
                let [value] = args else {
                    return Err("a malformed data-class array rendering".into());
                };
                let array = self.reference(*value)?;
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call("kt_array_content_to_string", &[any()], any(), &[array])
            }
            IrIntrinsic::Assert { mode } => self.checked_assertion(mode, args),
            // Inline specialization has already replaced a reified parameter by the call site's
            // enum; one still standing here names no enum this generator can build.
            IrIntrinsic::EnumEntries { classifier } => match classifier.non_null().obj_internal() {
                Some(classifier) => self.enum_entries(classifier),
                None => Err(declined!(
                    "`enumEntries` of `{classifier:?}`, which is not an enum"
                )),
            },
            other => Err(declined!("the `{other:?}` intrinsic")),
        }
    }

    /// Kotlin's `assert(value)` / `assert(value) { message }`.
    ///
    /// The MODE decides before anything is evaluated, which is the whole of what makes this an
    /// intrinsic rather than a call: `always-disable` evaluates NEITHER child, so a condition with
    /// a side effect does not have it — the corpus asks that directly.
    ///
    /// Enabled, the condition is evaluated and branched on, and the message is computed only on
    /// the failing side: `lazyMessage` is lazy exactly there. It crosses as the FUNCTION it is,
    /// invoked by the runtime beside the failure it reports rather than here.
    ///
    /// The `Runtime` mode is the one this target does not answer. Whether assertions are on is a
    /// question about how the program was BUILT, and nothing in this generator can see that yet —
    /// answering it either way would be a guess a program can observe.
    fn checked_assertion(
        &mut self,
        mode: crate::types::AssertionMode,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match mode {
            crate::types::AssertionMode::AlwaysDisabled => Ok(None),
            crate::types::AssertionMode::Runtime => Err(
                "an `assert` whose enabling is decided at run time, which this target does not \
                 answer yet"
                    .into(),
            ),
            crate::types::AssertionMode::AlwaysEnabled => {
                let [condition, message @ ..] = args else {
                    return Err("a malformed `assert`".into());
                };
                if message.len() > 1 {
                    return Err("an `assert` with more than a condition and a message".into());
                }
                let Some(value) = self.coerce(*condition, Ty::Boolean)? else {
                    return Ok(None);
                };
                if self.terminated {
                    return Ok(None);
                }
                // The message ARGUMENT is an ordinary argument and is evaluated here, before the
                // branch: `assert(c, xs.filter { … }::message)` filters whether or not the
                // assertion holds, and the corpus asks exactly that. What `lazyMessage` makes lazy
                // is the INVOCATION, which happens beside the failure and nowhere else.
                let lazy = match message {
                    [function] => self.reference(*function)?,
                    _ => self.builder.ins().iconst(types::I64, 0),
                };
                if self.terminated {
                    return Ok(None);
                }
                let failed = self.builder.create_block();
                let passed = self.builder.create_block();
                self.builder.ins().brif(value, passed, &[], failed, &[]);

                self.continue_in(failed);
                self.builder.seal_block(failed);
                self.runtime_call("kt_assertion_failed", &[any()], Ty::Unit, &[lazy])?;
                if !self.terminated {
                    self.builder.ins().jump(passed, &[]);
                }

                self.continue_in(passed);
                self.builder.seal_block(passed);
                Ok(None)
            }
        }
    }
}
