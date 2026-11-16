//! `Double.toString()` and `Float.toString()`, as the Kotlin/Wasm standard library writes them.
//!
//! The library's `dtoa` (`kotlin.wasm.internal`, after AssemblyScript's) generates the digits with
//! Grisu2 and then lays them out as JavaScript's `Number.prototype.toString` does: plain notation
//! for a decimal exponent up to 21 and down to -6 (`10000000.0`, `0.000001`), and `1.0e+21` /
//! `1.0e-7` beyond. A `Float` is widened to a `Double` but generates its digits from its own
//! `Float` bits, so `0.1f` prints as `0.1`. The functions below are that code, instruction for
//! instruction, so every value prints the same digits the library's own module prints. A module
//! carries them only once a conversion needs them.

use super::*;

const I64_EQZ: u8 = 0x50;
const I64_LT_U: u8 = 0x54;
const I64_GT_U: u8 = 0x56;
const I64_LE_U: u8 = 0x58;
const I64_GE_U: u8 = 0x5a;
const I64_CLZ: u8 = 0x79;
const I32_GT_U: u8 = 0x4b;
const F64_EQ: u8 = 0x61;
const F64_NE: u8 = 0x62;
const F64_LT: u8 = 0x63;
const F64_NEG: u8 = 0x9a;
const F64_ADD: u8 = 0xa0;
const F64_MUL: u8 = 0xa2;
const F64_CONVERT_I32_S: u8 = 0xb7;
const F32_DEMOTE_F64: u8 = 0xb6;
const F64_PROMOTE_F32: u8 = 0xbb;
const I32_REINTERPRET_F32: u8 = 0xbc;
const I64_REINTERPRET_F64: u8 = 0xbd;
/// `i32.trunc_sat_f64_s`, Kotlin's `Double.toInt()`.
const I32_TRUNC_SAT_F64_S: u8 = 2;

/// The longest text `dtoaCore` writes: `MAX_DOUBLE_LENGTH`.
const MAX_DOUBLE_LENGTH: i32 = 28;

/// The library's `EXP_POWERS`: the binary exponents of the cached powers of ten.
const EXP_POWERS: [i16; 87] = [
    -1220, -1193, -1166, -1140, -1113, -1087, -1060, -1034, -1007, -980, -954, -927, -901, -874,
    -847, -821, -794, -768, -741, -715, -688, -661, -635, -608, -582, -555, -529, -502, -475, -449,
    -422, -396, -369, -343, -316, -289, -263, -236, -210, -183, -157, -130, -103, -77, -50, -24, 3,
    30, 56, 83, 109, 136, 162, 189, 216, 242, 269, 295, 322, 348, 375, 402, 428, 455, 481, 508,
    534, 561, 588, 614, 641, 667, 694, 720, 747, 774, 800, 827, 853, 880, 907, 933, 960, 986, 1013,
    1039, 1066,
];

/// The library's `FRC_POWERS`: the significands of `1e-348, 1e-340, ..., 1e340`.
const FRC_POWERS: [u64; 87] = [
    0xFA8FD5A0081C0288,
    0xBAAEE17FA23EBF76,
    0x8B16FB203055AC76,
    0xCF42894A5DCE35EA,
    0x9A6BB0AA55653B2D,
    0xE61ACF033D1A45DF,
    0xAB70FE17C79AC6CA,
    0xFF77B1FCBEBCDC4F,
    0xBE5691EF416BD60C,
    0x8DD01FAD907FFC3C,
    0xD3515C2831559A83,
    0x9D71AC8FADA6C9B5,
    0xEA9C227723EE8BCB,
    0xAECC49914078536D,
    0x823C12795DB6CE57,
    0xC21094364DFB5637,
    0x9096EA6F3848984F,
    0xD77485CB25823AC7,
    0xA086CFCD97BF97F4,
    0xEF340A98172AACE5,
    0xB23867FB2A35B28E,
    0x84C8D4DFD2C63F3B,
    0xC5DD44271AD3CDBA,
    0x936B9FCEBB25C996,
    0xDBAC6C247D62A584,
    0xA3AB66580D5FDAF6,
    0xF3E2F893DEC3F126,
    0xB5B5ADA8AAFF80B8,
    0x87625F056C7C4A8B,
    0xC9BCFF6034C13053,
    0x964E858C91BA2655,
    0xDFF9772470297EBD,
    0xA6DFBD9FB8E5B88F,
    0xF8A95FCF88747D94,
    0xB94470938FA89BCF,
    0x8A08F0F8BF0F156B,
    0xCDB02555653131B6,
    0x993FE2C6D07B7FAC,
    0xE45C10C42A2B3B06,
    0xAA242499697392D3,
    0xFD87B5F28300CA0E,
    0xBCE5086492111AEB,
    0x8CBCCC096F5088CC,
    0xD1B71758E219652C,
    0x9C40000000000000,
    0xE8D4A51000000000,
    0xAD78EBC5AC620000,
    0x813F3978F8940984,
    0xC097CE7BC90715B3,
    0x8F7E32CE7BEA5C70,
    0xD5D238A4ABE98068,
    0x9F4F2726179A2245,
    0xED63A231D4C4FB27,
    0xB0DE65388CC8ADA8,
    0x83C7088E1AAB65DB,
    0xC45D1DF942711D9A,
    0x924D692CA61BE758,
    0xDA01EE641A708DEA,
    0xA26DA3999AEF774A,
    0xF209787BB47D6B85,
    0xB454E4A179DD1877,
    0x865B86925B9BC5C2,
    0xC83553C5C8965D3D,
    0x952AB45CFA97A0B3,
    0xDE469FBD99A05FE3,
    0xA59BC234DB398C25,
    0xF6C69A72A3989F5C,
    0xB7DCBF5354E9BECE,
    0x88FCF317F22241E2,
    0xCC20CE9BD35C78A5,
    0x98165AF37B2153DF,
    0xE2A0B5DC971F303A,
    0xA8D9D1535CE3B396,
    0xFB9B7CD9A4A7443C,
    0xBB764C4CA7A44410,
    0x8BAB8EEFB6409C1A,
    0xD01FEF10A657842C,
    0x9B10A4E5E9913129,
    0xE7109BFBA19C0C9D,
    0xAC2820D9623BF429,
    0x80444B5E7AA7CF85,
    0xBF21E44003ACDD2D,
    0x8E679C2F5E44FF8F,
    0xD433179D9C8CB841,
    0x9E19DB92B4E31BA9,
    0xEB96BF6EBADF77D9,
    0xAF87023B9BF0EE6B,
];

/// The floating conversions a module has defined so far.
#[derive(Default)]
pub(super) struct FloatingStrings {
    /// `(value: Double) -> String`.
    double: Option<u32>,
    /// `(value: Float) -> String`.
    float: Option<u32>,
    /// `dtoa(value: Double, isSinglePrecision: Boolean) -> String`.
    dtoa: Option<u32>,
}

/// The functions `dtoa` calls, and the global holding the library's `_K`: the decimal exponent
/// of the digits `grisu2` wrote.
#[derive(Clone, Copy)]
struct Grisu {
    decimal_exponent: u32,
    grisu2: u32,
    umul64f: u32,
    gen_digits: u32,
    grisu_round: u32,
    prettify: u32,
    gen_exponent: u32,
}

impl Runtime {
    /// `Double.toString()`: `(value: Double) -> String`, defined on first use.
    pub(crate) fn string_of_double(&mut self, module: &mut Module) -> u32 {
        if let Some(function) = self.floating_strings.double {
            return function;
        }
        let dtoa = self.dtoa(module);
        let ty = module.func_type(vec![ValType::F64], vec![self.string_type()]);
        let mut code = Code::default();
        code.local_get(0).i32_const(0).call(dtoa);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        self.floating_strings.double = Some(function);
        function
    }

    /// `Float.toString()`: `(value: Float) -> String`, which is `dtoa(this.toDouble(), true)`.
    pub(crate) fn string_of_float(&mut self, module: &mut Module) -> u32 {
        if let Some(function) = self.floating_strings.float {
            return function;
        }
        let dtoa = self.dtoa(module);
        let ty = module.func_type(vec![ValType::F32], vec![self.string_type()]);
        let mut code = Code::default();
        code.local_get(0)
            .op(F64_PROMOTE_F32)
            .i32_const(1)
            .call(dtoa);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        self.floating_strings.float = Some(function);
        function
    }

    /// `dtoa`: the special values by name, every other value through `dtoaCore` into a buffer of
    /// `MAX_DOUBLE_LENGTH` units, of which the string keeps the ones written.
    fn dtoa(&mut self, module: &mut Module) -> u32 {
        if let Some(function) = self.floating_strings.dtoa {
            return function;
        }
        let grisu = self.define_grisu(module);
        let string = self.string_type();
        let ty = module.func_type(vec![ValType::F64, ValType::I32], vec![string]);
        let (value, single, buffer, sign, length, result) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        code.local_get(value).f64_const(0.0).op(F64_EQ);
        code.if_(BlockType::Empty);
        code.local_get(value)
            .op(I64_REINTERPRET_F64)
            .op(I64_EQZ)
            .if_(BlockType::Value(string));
        self.ascii_constant(module, &mut code, "0.0");
        code.else_();
        self.ascii_constant(module, &mut code, "-0.0");
        code.end().return_().end();
        code.local_get(value).local_get(value).op(F64_NE);
        code.if_(BlockType::Empty);
        self.ascii_constant(module, &mut code, "NaN");
        code.return_().end();
        code.local_get(value).f64_const(f64::INFINITY).op(F64_EQ);
        code.if_(BlockType::Empty);
        self.ascii_constant(module, &mut code, "Infinity");
        code.return_().end();
        code.local_get(value)
            .f64_const(f64::NEG_INFINITY)
            .op(F64_EQ);
        code.if_(BlockType::Empty);
        self.ascii_constant(module, &mut code, "-Infinity");
        code.return_().end();
        code.i32_const(MAX_DOUBLE_LENGTH)
            .array_new_default(self.chars)
            .local_set(buffer);
        // dtoaCore: a negative value writes its sign and generates the digits of its magnitude.
        code.local_get(value)
            .f64_const(0.0)
            .op(F64_LT)
            .local_tee(sign)
            .if_(BlockType::Empty)
            .local_get(value)
            .op(F64_NEG)
            .local_set(value)
            .local_get(buffer)
            .i32_const(0)
            .i32_const(i32::from(b'-'))
            .array_set(self.chars)
            .end();
        code.local_get(value)
            .local_get(buffer)
            .local_get(sign)
            .local_get(single)
            .call(grisu.grisu2)
            .local_set(length);
        code.local_get(buffer)
            .local_get(sign)
            .local_get(length)
            .local_get(sign)
            .op(SUB32)
            .global_get(grisu.decimal_exponent)
            .call(grisu.prettify)
            .local_get(sign)
            .op(ADD32)
            .local_set(length);
        code.local_get(length)
            .array_new_default(self.chars)
            .local_tee(result)
            .i32_const(0)
            .local_get(buffer)
            .i32_const(0)
            .local_get(length)
            .array_copy(self.chars, self.chars);
        code.local_get(result).struct_new(self.string);
        let chars = ValType::reference(self.chars);
        let locals = vec![chars, ValType::I32, ValType::I32, chars];
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.floating_strings.dtoa = Some(function);
        function
    }

    /// Declare and define the functions `dtoa` calls.
    fn define_grisu(&mut self, module: &mut Module) -> Grisu {
        let grisu = Grisu {
            decimal_exponent: module.global(ValType::I32),
            grisu2: module.declare(),
            umul64f: module.declare(),
            gen_digits: module.declare(),
            grisu_round: module.declare(),
            prettify: module.declare(),
            gen_exponent: module.declare(),
        };
        self.define_grisu2(module, grisu);
        define_umul64f(module, grisu);
        self.define_gen_digits(module, grisu);
        self.define_grisu_round(module, grisu);
        self.define_prettify(module, grisu);
        self.define_gen_exponent(module, grisu);
        grisu
    }

    /// `grisu2(value, buffer, sign, isSinglePrecision) -> length`, with `normalizedBoundaries` and
    /// `getCachedPower` written in place: their results are this function's locals rather than
    /// the library's private globals, which nothing else reads.
    fn define_grisu2(&self, module: &mut Module, grisu: Grisu) {
        let chars = ValType::reference(self.chars);
        let ty = module.func_type(
            vec![ValType::F64, chars, ValType::I32, ValType::I32],
            vec![ValType::I32],
        );
        let powers = module.ty(CompositeType::Array(FieldType {
            storage: StorageType::Val(ValType::I64),
            mutable: false,
        }));
        let frc_powers = module.data(
            &FRC_POWERS
                .iter()
                .flat_map(|power| power.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        let exp_powers = module.data(
            &EXP_POWERS
                .iter()
                .flat_map(|power| power.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        let (value, buffer, sign, single) = (0, 1, 2, 3);
        // i64 locals.
        let (frc, bits, boundary, frc_plus, frc_minus, frc_pow) = (4, 5, 6, 7, 8, 9);
        // i32 locals.
        let (exp, single_bits, boundary_exp, normalized_exp, shift, k, index, exp_pow) =
            (10, 11, 12, 13, 14, 15, 16, 17);
        // f64 local.
        let dk = 18;
        let mut code = Code::default();
        // frexp.
        code.local_get(single).if_(BlockType::Empty);
        code.local_get(value)
            .op(F32_DEMOTE_F64)
            .op(I32_REINTERPRET_F32)
            .local_tee(single_bits)
            .i32_const(0x7F80_0000)
            .op(AND32)
            .i32_const(23)
            .op(SHR_U32)
            .local_set(exp);
        code.local_get(exp)
            .i32_const(0)
            .op(NE32)
            .op(EXTEND_I32_U)
            .i64_const(23)
            .op(SHL64)
            .local_get(single_bits)
            .i32_const(0x007F_FFFF)
            .op(AND32)
            .op(EXTEND_I32_U)
            .op(ADD64)
            .local_set(frc);
        biased_exponent(&mut code, exp, 0x7F + 23);
        code.else_();
        code.local_get(value)
            .op(I64_REINTERPRET_F64)
            .local_tee(bits)
            .i64_const(0x7FF0_0000_0000_0000)
            .op(AND64)
            .i64_const(52)
            .op(SHR_U64)
            .op(WRAP_I64)
            .local_set(exp);
        code.local_get(exp)
            .i32_const(0)
            .op(NE32)
            .op(EXTEND_I32_U)
            .i64_const(52)
            .op(SHL64)
            .local_get(bits)
            .i64_const(0x000F_FFFF_FFFF_FFFF)
            .op(AND64)
            .op(ADD64)
            .local_set(frc);
        biased_exponent(&mut code, exp, 0x3FF + 52);
        code.end();
        // normalizedBoundaries(frc, exp, isSinglePrecision).
        code.local_get(frc)
            .i64_const(1)
            .op(SHL64)
            .i64_const(1)
            .op(ADD64)
            .local_tee(boundary)
            .op(I64_CLZ)
            .op(WRAP_I64)
            .local_set(shift);
        code.local_get(boundary)
            .local_get(shift)
            .op(EXTEND_I32_U)
            .op(SHL64)
            .local_set(frc_plus);
        code.local_get(exp)
            .i32_const(1)
            .op(SUB32)
            .local_get(shift)
            .op(SUB32)
            .local_set(normalized_exp);
        // `m` is 2 for the smallest normalized significand, whose lower neighbour is closer.
        code.i32_const(1)
            .local_get(frc)
            .local_get(single)
            .if_(BlockType::Value(ValType::I64))
            .i64_const(0x0080_0000)
            .else_()
            .i64_const(0x0010_0000_0000_0000)
            .end()
            .op(EQ64)
            .op(ADD32)
            .local_set(boundary_exp);
        code.local_get(frc)
            .local_get(boundary_exp)
            .op(EXTEND_I32_U)
            .op(SHL64)
            .i64_const(1)
            .op(SUB64)
            .local_get(exp)
            .local_get(boundary_exp)
            .op(SUB32)
            .local_get(normalized_exp)
            .op(SUB32)
            .op(EXTEND_I32_S)
            .op(SHL64)
            .local_set(frc_minus);
        // getCachedPower(_exp).
        code.i32_const(-61)
            .local_get(normalized_exp)
            .op(SUB32)
            .op(F64_CONVERT_I32_S)
            .f64_const(f64::from_bits(0x3FD3_4413_509F_79FE))
            .op(F64_MUL)
            .f64_const(347.0)
            .op(F64_ADD)
            .local_tee(dk)
            .trunc_sat(I32_TRUNC_SAT_F64_S)
            .local_tee(k)
            .local_get(k)
            .op(F64_CONVERT_I32_S)
            .local_get(dk)
            .op(F64_NE)
            .op(ADD32)
            .i32_const(3)
            .op(SHR_S32)
            .i32_const(1)
            .op(ADD32)
            .local_set(index);
        code.i32_const(348)
            .local_get(index)
            .i32_const(3)
            .op(SHL32)
            .op(SUB32)
            .global_set(grisu.decimal_exponent);
        // `FRC_POWERS[index]` and `EXP_POWERS[index]`, each read as a one-element array.
        code.i32_const(frc_powers as i32)
            .local_get(index)
            .i32_const(8)
            .op(MUL32)
            .op(ADD32)
            .i32_const(1)
            .array_new_data(powers, 0)
            .i32_const(0)
            .array_get(powers)
            .local_set(frc_pow);
        code.i32_const(exp_powers as i32)
            .local_get(index)
            .i32_const(2)
            .op(MUL32)
            .op(ADD32)
            .i32_const(1)
            .array_new_data(self.chars, 0)
            .i32_const(0)
            .array_get_s(self.chars)
            .local_set(exp_pow);
        // Normalize the value's own significand; its exponent is not read again.
        code.local_get(frc)
            .local_get(frc)
            .op(I64_CLZ)
            .op(SHL64)
            .local_set(frc);
        // genDigits(buffer, w_frc, wp_frc, wp_exp, wp_frc - wm_frc, sign).
        code.local_get(buffer);
        code.local_get(frc).local_get(frc_pow).call(grisu.umul64f);
        code.local_get(frc_plus)
            .local_get(frc_pow)
            .call(grisu.umul64f)
            .i64_const(1)
            .op(SUB64)
            .local_tee(boundary);
        code.local_get(normalized_exp)
            .local_get(exp_pow)
            .op(ADD32)
            .i32_const(64)
            .op(ADD32);
        code.local_get(boundary)
            .local_get(frc_minus)
            .local_get(frc_pow)
            .call(grisu.umul64f)
            .i64_const(1)
            .op(ADD64)
            .op(SUB64);
        code.local_get(sign).call(grisu.gen_digits);
        let mut locals = vec![ValType::I64; 6];
        locals.extend([ValType::I32; 8]);
        locals.push(ValType::F64);
        module.define(grisu.grisu2, Function { ty, locals, code });
    }

    /// `genDigits(buffer, w_frc, mp_frc, mp_exp, delta, sign) -> length`: the digits of the
    /// integral part, then of the fraction, until the rest is inside the boundaries. The integral
    /// part's divisions are the library's signed `Int` ones; its `when (kappa)` has no `10` arm,
    /// so a ten-digit part takes the `else` arm.
    fn define_gen_digits(&self, module: &mut Module, grisu: Grisu) {
        let chars = ValType::reference(self.chars);
        let ty = module.func_type(
            vec![
                chars,
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I32,
            ],
            vec![ValType::I32],
        );
        let (buffer, w_frc, mp_frc, mp_exp, delta, sign) = (0, 1, 2, 3, 4, 5);
        // i64 locals.
        let (one_frc, mask, wp_w_frc, p2, pow10, rest, unit, digit_bits) =
            (6, 7, 8, 9, 10, 11, 12, 13);
        // i32 locals.
        let (one_exp, p1, kappa, length, digit, divisor, count) = (14, 15, 16, 17, 18, 19, 20);
        let mut code = Code::default();
        code.i32_const(0)
            .local_get(mp_exp)
            .op(SUB32)
            .local_set(one_exp);
        code.i64_const(1)
            .local_get(one_exp)
            .op(EXTEND_I32_S)
            .op(SHL64)
            .local_tee(one_frc)
            .i64_const(1)
            .op(SUB64)
            .local_set(mask);
        code.local_get(mp_frc)
            .local_get(w_frc)
            .op(SUB64)
            .local_set(wp_w_frc);
        code.local_get(mp_frc)
            .local_get(one_exp)
            .op(EXTEND_I32_S)
            .op(SHR_U64)
            .op(WRAP_I64)
            .local_set(p1);
        code.local_get(mp_frc)
            .local_get(mask)
            .op(AND64)
            .local_set(p2);
        // decimalCount32(p1.toUInt()): one more digit for every power of ten the value reaches.
        code.i32_const(1)
            .local_set(kappa)
            .i32_const(10)
            .local_set(divisor);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(kappa).i32_const(10).op(GE_S32).br_if(1);
        code.local_get(p1).local_get(divisor).op(LT_U32).br_if(1);
        code.local_get(kappa)
            .i32_const(1)
            .op(ADD32)
            .local_set(kappa)
            .local_get(divisor)
            .i32_const(10)
            .op(MUL32)
            .local_set(divisor)
            .br(0);
        code.end().end();
        code.local_get(sign).local_set(length);
        // The integral part.
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(kappa).i32_const(0).op(LE_S32).br_if(1);
        code.local_get(kappa).i32_const(9).op(I32_GT_U);
        code.if_(BlockType::Empty)
            .i32_const(0)
            .local_set(digit)
            .i64_const(1)
            .local_set(pow10);
        code.else_();
        // `10^(kappa - 1)`.
        code.i32_const(1)
            .local_set(divisor)
            .i32_const(1)
            .local_set(count);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(count).local_get(kappa).op(GE_S32).br_if(1);
        code.local_get(divisor)
            .i32_const(10)
            .op(MUL32)
            .local_set(divisor)
            .local_get(count)
            .i32_const(1)
            .op(ADD32)
            .local_set(count)
            .br(0);
        code.end().end();
        code.local_get(p1)
            .local_get(divisor)
            .op(DIV_S32)
            .local_set(digit);
        code.local_get(p1)
            .local_get(divisor)
            .op(REM_S32)
            .local_set(p1);
        code.local_get(divisor).op(EXTEND_I32_U).local_set(pow10);
        code.end();
        code.local_get(digit).local_get(length).op(OR32);
        code.if_(BlockType::Empty);
        self.write_digit(&mut code, buffer, length, digit);
        code.end();
        code.local_get(kappa)
            .i32_const(1)
            .op(SUB32)
            .local_set(kappa);
        // `p1.toULong()` widens with the sign.
        code.local_get(p1)
            .op(EXTEND_I32_S)
            .local_get(one_exp)
            .op(EXTEND_I32_S)
            .op(SHL64)
            .local_get(p2)
            .op(ADD64)
            .local_tee(rest)
            .local_get(delta)
            .op(I64_LE_U);
        code.if_(BlockType::Empty);
        add_to_decimal_exponent(&mut code, grisu, kappa);
        code.local_get(buffer)
            .local_get(length)
            .local_get(delta)
            .local_get(rest)
            .local_get(pow10)
            .local_get(one_exp)
            .op(EXTEND_I32_S)
            .op(SHL64)
            .local_get(wp_w_frc)
            .call(grisu.grisu_round)
            .local_get(length)
            .return_();
        code.end();
        code.br(0).end().end();
        // The fraction.
        code.i64_const(1).local_set(unit);
        code.loop_(BlockType::Empty);
        for scaled in [p2, delta, unit] {
            code.local_get(scaled)
                .i64_const(10)
                .op(MUL64)
                .local_set(scaled);
        }
        code.local_get(p2)
            .local_get(one_exp)
            .op(EXTEND_I32_S)
            .op(SHR_U64)
            .local_tee(digit_bits)
            .local_get(length)
            .op(EXTEND_I32_S)
            .op(OR64)
            .i64_const(0)
            .op(NE64);
        code.if_(BlockType::Empty);
        code.local_get(digit_bits).op(WRAP_I64).local_set(digit);
        self.write_digit(&mut code, buffer, length, digit);
        code.end();
        code.local_get(p2).local_get(mask).op(AND64).local_set(p2);
        code.local_get(kappa)
            .i32_const(1)
            .op(SUB32)
            .local_set(kappa);
        code.local_get(p2).local_get(delta).op(I64_LT_U);
        code.if_(BlockType::Empty);
        add_to_decimal_exponent(&mut code, grisu, kappa);
        code.local_get(buffer)
            .local_get(length)
            .local_get(delta)
            .local_get(p2)
            .local_get(one_frc)
            .local_get(wp_w_frc)
            .local_get(unit)
            .op(MUL64)
            .call(grisu.grisu_round)
            .local_get(length)
            .return_();
        code.end();
        code.br(0).end().unreachable();
        let mut locals = vec![ValType::I64; 8];
        locals.extend([ValType::I32; 7]);
        module.define(grisu.gen_digits, Function { ty, locals, code });
    }

    /// `buffer[length++] = digitToChar(digit)`.
    fn write_digit(&self, code: &mut Code, buffer: u32, length: u32, digit: u32) {
        code.local_get(buffer)
            .local_get(length)
            .local_get(digit)
            .i32_const(i32::from(b'0'))
            .op(ADD32)
            .array_set(self.chars);
        code.local_get(length)
            .i32_const(1)
            .op(ADD32)
            .local_set(length);
    }

    /// `grisuRound(buffer, len, delta, rest, ten_kappa, wp_w)`: lower the last digit while that
    /// brings the value closer to the middle of the boundaries.
    fn define_grisu_round(&self, module: &mut Module, grisu: Grisu) {
        let chars = ValType::reference(self.chars);
        let ty = module.func_type(
            vec![
                chars,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
            ],
            Vec::new(),
        );
        let (buffer, length, delta, rest, ten_kappa, wp_w) = (0, 1, 2, 3, 4, 5);
        let (last, digit) = (6, 7);
        let mut code = Code::default();
        code.local_get(length)
            .i32_const(1)
            .op(SUB32)
            .local_set(last);
        code.local_get(buffer)
            .local_get(last)
            .array_get_u(self.chars)
            .local_set(digit);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(rest).local_get(wp_w).op(I64_GE_U).br_if(1);
        code.local_get(delta)
            .local_get(rest)
            .op(SUB64)
            .local_get(ten_kappa)
            .op(I64_LT_U)
            .br_if(1);
        code.local_get(rest)
            .local_get(ten_kappa)
            .op(ADD64)
            .local_get(wp_w)
            .op(I64_LT_U);
        code.local_get(wp_w)
            .local_get(rest)
            .op(SUB64)
            .local_get(rest)
            .local_get(ten_kappa)
            .op(ADD64)
            .local_get(wp_w)
            .op(SUB64)
            .op(I64_GT_U);
        code.op(OR32).op(EQZ32).br_if(1);
        code.local_get(digit)
            .i32_const(1)
            .op(SUB32)
            .local_set(digit);
        code.local_get(rest)
            .local_get(ten_kappa)
            .op(ADD64)
            .local_set(rest);
        code.br(0).end().end();
        code.local_get(buffer)
            .local_get(last)
            .local_get(digit)
            .array_set(self.chars);
        module.define(
            grisu.grisu_round,
            Function {
                ty,
                locals: vec![ValType::I32; 2],
                code,
            },
        );
    }

    /// `prettify(BufferWithOffset(buffer, offset), length, k) -> length`: lay the `length` digits
    /// at `offset` out for the decimal exponent `k`.
    fn define_prettify(&self, module: &mut Module, grisu: Grisu) {
        let chars = ValType::reference(self.chars);
        let ty = module.func_type(
            vec![chars, ValType::I32, ValType::I32, ValType::I32],
            vec![ValType::I32],
        );
        let (buffer, offset, length, k) = (0, 1, 2, 3);
        let (kk, shifted) = (4, 5);
        let at = Offset { buffer, offset };
        let mut code = Code::default();
        code.local_get(k).op(EQZ32).if_(BlockType::Empty);
        self.put(&mut code, at, |code| code.local_get(length), b'.');
        self.put(
            &mut code,
            at,
            |code| code.local_get(length).i32_const(1).op(ADD32),
            b'0',
        );
        code.local_get(length)
            .i32_const(2)
            .op(ADD32)
            .return_()
            .end();
        code.local_get(length).local_get(k).op(ADD32).local_set(kk);
        // 1234e7 -> 12340000000
        code.local_get(length)
            .local_get(kk)
            .op(LE_S32)
            .local_get(kk)
            .i32_const(21)
            .op(LE_S32)
            .op(AND32);
        code.if_(BlockType::Empty);
        self.zeros(&mut code, at, length, kk);
        self.put(&mut code, at, |code| code.local_get(kk), b'.');
        self.put(
            &mut code,
            at,
            |code| code.local_get(kk).i32_const(1).op(ADD32),
            b'0',
        );
        code.local_get(kk).i32_const(2).op(ADD32).return_().end();
        // 1234e-2 -> 12.34
        code.local_get(kk)
            .i32_const(0)
            .op(GT_S32)
            .local_get(kk)
            .i32_const(21)
            .op(LE_S32)
            .op(AND32);
        code.if_(BlockType::Empty);
        self.shift(
            &mut code,
            at,
            |code| code.local_get(kk).i32_const(1).op(ADD32),
            |code| code.local_get(kk),
            |code| code.i32_const(0).local_get(k).op(SUB32),
        );
        self.put(&mut code, at, |code| code.local_get(kk), b'.');
        code.local_get(length)
            .i32_const(1)
            .op(ADD32)
            .return_()
            .end();
        // 1234e-6 -> 0.001234
        code.i32_const(-6)
            .local_get(kk)
            .op(LT_S32)
            .local_get(kk)
            .i32_const(0)
            .op(LE_S32)
            .op(AND32);
        code.if_(BlockType::Empty);
        code.i32_const(2).local_get(kk).op(SUB32).local_set(shifted);
        self.shift(
            &mut code,
            at,
            |code| code.local_get(shifted),
            |code| code.i32_const(0),
            |code| code.local_get(length),
        );
        self.put(&mut code, at, |code| code.i32_const(0), b'0');
        self.put(&mut code, at, |code| code.i32_const(1), b'.');
        code.i32_const(2).local_set(kk);
        self.zeros(&mut code, at, kk, shifted);
        code.local_get(length)
            .local_get(shifted)
            .op(ADD32)
            .return_()
            .end();
        // 1e30
        code.local_get(length).i32_const(1).op(EQ32);
        code.if_(BlockType::Empty);
        self.put(&mut code, at, |code| code.i32_const(1), b'e');
        code.local_get(buffer)
            .local_get(offset)
            .i32_const(2)
            .op(ADD32)
            .local_get(kk)
            .i32_const(1)
            .op(SUB32)
            .call(grisu.gen_exponent)
            .i32_const(2)
            .op(ADD32)
            .return_()
            .end();
        // 1234e30 -> 1.234e+33
        self.shift(
            &mut code,
            at,
            |code| code.i32_const(2),
            |code| code.i32_const(1),
            |code| code.local_get(length).i32_const(1).op(SUB32),
        );
        self.put(&mut code, at, |code| code.i32_const(1), b'.');
        self.put(
            &mut code,
            at,
            |code| code.local_get(length).i32_const(1).op(ADD32),
            b'e',
        );
        code.local_get(length);
        code.local_get(buffer)
            .local_get(offset)
            .local_get(length)
            .op(ADD32)
            .i32_const(2)
            .op(ADD32)
            .local_get(kk)
            .i32_const(1)
            .op(SUB32)
            .call(grisu.gen_exponent);
        code.op(ADD32).i32_const(2).op(ADD32);
        module.define(
            grisu.prettify,
            Function {
                ty,
                locals: vec![ValType::I32; 2],
                code,
            },
        );
    }

    /// `buffer[address] = unit`, at the buffer's offset.
    fn put(
        &self,
        code: &mut Code,
        at: Offset,
        address: impl FnOnce(&mut Code) -> &mut Code,
        unit: u8,
    ) {
        code.local_get(at.buffer);
        address(code).local_get(at.offset).op(ADD32);
        code.i32_const(i32::from(unit)).array_set(self.chars);
    }

    /// `for (i in from until until) buffer[i] = '0'`, counting in the local `from`.
    fn zeros(&self, code: &mut Code, at: Offset, from: u32, until: u32) {
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(from).local_get(until).op(GE_S32).br_if(1);
        self.put(code, at, |code| code.local_get(from), b'0');
        code.local_get(from)
            .i32_const(1)
            .op(ADD32)
            .local_set(from)
            .br(0);
        code.end().end();
    }

    /// `buffer.memoryCopy(destination, source, length)`: `array.copy` moves overlapping units as
    /// the library's copy loop does.
    fn shift(
        &self,
        code: &mut Code,
        at: Offset,
        destination: impl FnOnce(&mut Code) -> &mut Code,
        source: impl FnOnce(&mut Code) -> &mut Code,
        length: impl FnOnce(&mut Code) -> &mut Code,
    ) {
        code.local_get(at.buffer);
        destination(code).local_get(at.offset).op(ADD32);
        code.local_get(at.buffer);
        source(code).local_get(at.offset).op(ADD32);
        length(code);
        code.array_copy(self.chars, self.chars);
    }

    /// `genExponent(BufferWithOffset(buffer, offset), k) -> length`: the exponent's sign, then
    /// its magnitude as `Int.toString()` writes it.
    fn define_gen_exponent(&self, module: &mut Module, grisu: Grisu) {
        let chars = ValType::reference(self.chars);
        let ty = module.func_type(vec![chars, ValType::I32, ValType::I32], vec![ValType::I32]);
        let (buffer, offset, k) = (0, 1, 2);
        let (negative, digits) = (3, 4);
        let mut code = Code::default();
        code.local_get(k)
            .i32_const(0)
            .op(LT_S32)
            .local_tee(negative)
            .if_(BlockType::Empty)
            .i32_const(0)
            .local_get(k)
            .op(SUB32)
            .local_set(k)
            .end();
        code.local_get(k)
            .call(self.string_of_i32)
            .struct_get(self.string, 0)
            .local_set(digits);
        code.local_get(buffer)
            .local_get(offset)
            .i32_const(1)
            .op(ADD32)
            .local_get(digits)
            .i32_const(0)
            .local_get(digits)
            .array_len()
            .array_copy(self.chars, self.chars);
        code.local_get(buffer)
            .local_get(offset)
            .i32_const(i32::from(b'-'))
            .i32_const(i32::from(b'+'))
            .local_get(negative)
            .select()
            .array_set(self.chars);
        code.local_get(digits).array_len().i32_const(1).op(ADD32);
        module.define(
            grisu.gen_exponent,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::reference(self.chars)],
                code,
            },
        );
    }
}

/// A `BufferWithOffset`: the buffer and offset locals of the function writing through it.
#[derive(Clone, Copy)]
struct Offset {
    buffer: u32,
    offset: u32,
}

/// `exp = (if (exp != 0) exp else 1) - bias`.
fn biased_exponent(code: &mut Code, exp: u32, bias: i32) {
    code.local_get(exp)
        .i32_const(1)
        .local_get(exp)
        .select()
        .i32_const(bias)
        .op(SUB32)
        .local_set(exp);
}

/// `_K += kappa`.
fn add_to_decimal_exponent(code: &mut Code, grisu: Grisu, kappa: u32) {
    code.global_get(grisu.decimal_exponent)
        .local_get(kappa)
        .op(ADD32)
        .global_set(grisu.decimal_exponent);
}

/// `umul64f(u, v)`: the high 64 bits of the 128-bit product, rounded.
fn define_umul64f(module: &mut Module, grisu: Grisu) {
    let ty = module.func_type(vec![ValType::I64, ValType::I64], vec![ValType::I64]);
    let (u, v) = (0, 1);
    let (u0, v0, u1, v1, t, w) = (2, 3, 4, 5, 6, 7);
    let mut code = Code::default();
    for (whole, low, high) in [(u, u0, u1), (v, v0, v1)] {
        code.local_get(whole)
            .i64_const(0xFFFF_FFFF)
            .op(AND64)
            .local_set(low);
        code.local_get(whole)
            .i64_const(32)
            .op(SHR_U64)
            .local_set(high);
    }
    // t = u1 * v0 + (u0 * v0 shr 32)
    code.local_get(u1)
        .local_get(v0)
        .op(MUL64)
        .local_get(u0)
        .local_get(v0)
        .op(MUL64)
        .i64_const(32)
        .op(SHR_U64)
        .op(ADD64)
        .local_set(t);
    // w = u0 * v1 + (t and 0xFFFFFFFF) + 0x7FFFFFFF
    code.local_get(u0)
        .local_get(v1)
        .op(MUL64)
        .local_get(t)
        .i64_const(0xFFFF_FFFF)
        .op(AND64)
        .op(ADD64)
        .i64_const(0x7FFF_FFFF)
        .op(ADD64)
        .local_set(w);
    // u1 * v1 + (t shr 32) + (w shr 32)
    code.local_get(u1)
        .local_get(v1)
        .op(MUL64)
        .local_get(t)
        .i64_const(32)
        .op(SHR_U64)
        .op(ADD64)
        .local_get(w)
        .i64_const(32)
        .op(SHR_U64)
        .op(ADD64);
    module.define(
        grisu.umul64f,
        Function {
            ty,
            locals: vec![ValType::I64; 6],
            code,
        },
    );
}
