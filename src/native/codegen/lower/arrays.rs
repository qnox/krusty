//! Arrays: one object shape, a stride from the element type, and a bounds check at every access.
//!
//! Every array is the same object — a header, a length, then the elements — and what distinguishes
//! an `IntArray` from an `Array<String>` is two numbers in its type descriptor: how wide an element
//! is, and whether the collector should look inside. That is why the array types are the RUNTIME's
//! rather than something each file emits: an array's descriptor depends on the element width, not
//! on the element type a program wrote, so `Array<String>` and `Array<Foo>` are the same type here
//! and there is nothing per-program to declare.
//!
//! **Every access is bounds-checked.** Kotlin throws `IndexOutOfBoundsException`; there are no
//! exceptions yet, so an out-of-range index is a diagnosable exit, as a failed cast already is. The
//! check is a comparison against the array's own length, and comparing it unsigned is what makes a
//! negative index safe too: as an unsigned number, -1 is larger than any length.

use super::objects::trusted;
use super::*;

/// `sizeof(KArray)`: the object header, the length, and the padding that keeps an 8-byte element
/// aligned. Elements begin here, which is exactly what the descriptor tells the collector.
pub(super) const ELEMENTS: i64 = 16;
/// Where the length sits inside that header.
const LENGTH: i32 = 8;

/// The runtime type and element stride for an array type, or the decline naming it.
fn array_type(array: Ty) -> Result<(&'static str, u32), Unsupported> {
    if array.non_null().is_reference_array() {
        return Ok(("kt_type_array", 8));
    }
    let Some(element) = array.non_null().array_elem() else {
        return Err(format!("an array of `{array:?}`"));
    };
    Ok(match element {
        Ty::Byte => ("kt_type_byte_array", 1),
        Ty::Short => ("kt_type_short_array", 2),
        Ty::Int => ("kt_type_int_array", 4),
        Ty::Long => ("kt_type_long_array", 8),
        Ty::Char => ("kt_type_char_array", 2),
        Ty::Boolean => ("kt_type_boolean_array", 1),
        Ty::Float => ("kt_type_float_array", 4),
        Ty::Double => ("kt_type_double_array", 8),
        other => return Err(format!("an array of `{other:?}`")),
    })
}

/// What an array holds, which is not always what its element type says. An `Array<Int>` STORES
/// references — boxed `Int`s — while an `IntArray` stores the `Int`s themselves, and the two have
/// different strides and different carriers at every load and store. Conflating them reads four
/// bytes out of an eight-byte slot.
pub(super) struct Shape {
    pub(super) descriptor: DataId,
    pub(super) stride: u32,
    /// The type a slot actually holds: `Any?` for a reference array, the element type otherwise.
    pub(super) stored: Ty,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    pub(super) fn array_shape(&mut self, array: Ty) -> Result<Shape, Unsupported> {
        let (symbol, stride) = array_type(array)?;
        let element = array
            .non_null()
            .array_read_elem()
            .ok_or_else(|| format!("an array of `{array:?}`"))?;
        let stored = if array.non_null().is_reference_array() {
            any()
        } else {
            element
        };
        let descriptor = self.file.import_data(symbol)?;
        Ok(Shape {
            descriptor,
            stride,
            stored,
        })
    }

    /// The array an access names, checked non-null.
    fn array_of(&mut self, receiver: u32) -> Result<(Value, Shape), Unsupported> {
        let ty = self
            .type_of(receiver)
            .ok_or_else(|| "an array access on an undetermined type".to_string())?;
        let shape = self.array_shape(ty)?;
        let array = self.reference(receiver)?;
        self.null_check(array)?;
        Ok((array, shape))
    }

    /// The address of one element, after proving the index is inside the array.
    fn element_address(
        &mut self,
        array: Value,
        index: Value,
        stride: u32,
    ) -> Result<Value, Unsupported> {
        let length = self
            .builder
            .ins()
            .load(types::I32, trusted(), array, LENGTH);
        // Unsigned: a negative index is a very large one, so one comparison covers both ends.
        let inside = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, index, length);
        let fail = self.builder.create_block();
        let proceed = self.builder.create_block();
        self.builder.ins().brif(inside, proceed, &[], fail, &[]);

        self.continue_in(fail);
        self.runtime_call(
            "kt_index_out_of_bounds",
            &[Ty::Int, Ty::Int],
            Ty::Unit,
            &[index, length],
        )?;
        self.builder.ins().trap(TrapCode::unwrap_user(3));

        self.continue_in(proceed);
        let stride = self.builder.ins().iconst(types::I32, i64::from(stride));
        let offset = self.builder.ins().imul(index, stride);
        // The index is inside the array, so it is non-negative: widening it unsigned is exact.
        let offset = self.builder.ins().uextend(types::I64, offset);
        let start = self.builder.ins().iconst(types::I64, ELEMENTS);
        let base = self.builder.ins().iadd(array, start);
        Ok(self.builder.ins().iadd(base, offset))
    }

    /// `arrayOfNulls<T>(n)` and the sized `Array<T>(n)` constructor before its initializer runs.
    /// Zero is the right initial element for every kind Kotlin has here: zero, `false`, the NUL
    /// character, `0.0` and `null` are all zero bits, which is what the collector hands back.
    pub(super) fn new_array(&mut self, array: Ty, size: u32) -> Result<Option<Value>, Unsupported> {
        let descriptor = self.array_shape(array)?.descriptor;
        let length = self.coerce(size, Ty::Int)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(length) = length else {
            return Err("an array size of no value".to_string());
        };
        let descriptor = self.data_address(descriptor);
        self.runtime_call(
            "kt_array_new",
            &[any(), Ty::Int],
            any(),
            &[descriptor, length],
        )
    }

    /// An array written out element by element — `arrayOf(a, b)`, and the array a `vararg`
    /// parameter is passed as.
    pub(super) fn vararg(
        &mut self,
        array: Ty,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let shape = self.array_shape(array)?;
        let spread = spreads.iter().any(|spread| *spread);

        // Elements first, then the allocation: an element that allocates must not leave a
        // half-built array for a collection to find. A SPREAD element is the array it spreads,
        // which crosses as a reference whatever the elements are.
        let mut values = Vec::with_capacity(elements.len());
        for (value, spread) in elements.iter().zip(spreads) {
            let stored = if *spread { any() } else { shape.stored };
            let Some(value) = self.coerce(*value, stored)? else {
                return Err("a `Unit` array element".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            values.push(value);
        }
        let descriptor = self.data_address(shape.descriptor);
        // With no spread the length is the element count, and every element has a known offset.
        // With one it is not: `f(a, *xs, b)` is as long as `xs` is, which only run time knows, so
        // the length is summed and the elements are placed at a running index rather than at a
        // constant.
        let length = match spread {
            false => self.builder.ins().iconst(types::I32, values.len() as i64),
            true => {
                let mut length = self.builder.ins().iconst(
                    types::I32,
                    spreads.iter().filter(|spread| !**spread).count() as i64,
                );
                for (value, spread) in values.iter().zip(spreads) {
                    if !*spread {
                        continue;
                    }
                    let spread = self
                        .builder
                        .ins()
                        .load(types::I32, trusted(), *value, LENGTH);
                    length = self.builder.ins().iadd(length, spread);
                }
                length
            }
        };
        let array = self
            .runtime_call(
                "kt_array_new",
                &[any(), Ty::Int],
                any(),
                &[descriptor, length],
            )?
            .expect("`kt_array_new` returns the array");
        if !spread {
            for (index, value) in values.into_iter().enumerate() {
                let offset = ELEMENTS + (index as i64) * i64::from(shape.stride);
                self.builder
                    .ins()
                    .store(trusted(), value, array, offset as i32);
            }
            return Ok(Some(array));
        }
        let mut at = self.builder.ins().iconst(types::I32, 0);
        for (value, spread) in values.into_iter().zip(spreads) {
            if *spread {
                at = self
                    .runtime_call(
                        "kt_array_copy_into",
                        &[any(), Ty::Int, any()],
                        Ty::Int,
                        &[array, at, value],
                    )?
                    .expect("`kt_array_copy_into` answers the next index");
                continue;
            }
            let offset = self.builder.ins().uextend(types::I64, at);
            let offset = self
                .builder
                .ins()
                .imul_imm_s(offset, i64::from(shape.stride));
            let address = self.builder.ins().iadd_imm_s(array, ELEMENTS);
            let address = self.builder.ins().iadd(address, offset);
            self.builder.ins().store(trusted(), value, address, 0);
            at = self.builder.ins().iadd_imm_s(at, 1);
        }
        Ok(Some(array))
    }

    /// Read one element. What comes out of memory is the ARRAY's element carrier, which is not
    /// always what the read produces: `Array<Int>` stores boxed elements, and a `for (i in a)` over
    /// it wants an `Int`. So the load is converted to the type the operation declares — the same
    /// boundary the JVM crosses with a `checkcast` and an `intValue()`.
    pub(super) fn array_get(
        &mut self,
        receiver: u32,
        index: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let (array, shape) = self.array_of(receiver)?;
        let index = self.coerce(index, Ty::Int)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(index) = index else {
            return Err("an array index of no value".to_string());
        };
        let address = self.element_address(array, index, shape.stride)?;
        let clif = carrier(shape.stored)
            .clif()
            .expect("an element is never `Unit`");
        let value = self.builder.ins().load(clif, trusted(), address, 0);
        self.convert(value, Some(shape.stored), ret)
    }

    pub(super) fn array_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let (array, shape) = self.array_of(receiver)?;
        let index = self.coerce(index, Ty::Int)?;
        let value = self.coerce(value, shape.stored)?;
        if self.terminated {
            return Ok(None);
        }
        let (Some(index), Some(value)) = (index, value) else {
            return Err("an array store of no value".to_string());
        };
        let address = self.element_address(array, index, shape.stride)?;
        self.builder.ins().store(trusted(), value, address, 0);
        Ok(None)
    }

    pub(super) fn array_size(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let array = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.null_check(array)?;
        Ok(Some(self.builder.ins().load(
            types::I32,
            trusted(),
            array,
            LENGTH,
        )))
    }
}
