#include "krusty_math.h"

/* From Arm's optimized-routines, as musl and glibc 2.28+ carry it. Copyright (c) 2017-2018, Arm
 * Limited. SPDX-License-Identifier: MIT. */

double __math_oflow(uint32_t sign)
{
	return __math_xflow(sign, 0x1p769);
}

double __math_uflow(uint32_t sign)
{
	return __math_xflow(sign, 0x1p-767);
}

double __math_invalid(double x)
{
	return (x - x) / (x - x);
}

double __math_divzero(uint32_t sign)
{
	return fp_barrier(sign ? -1.0 : 1.0) / 0.0;
}

double __math_xflow(uint32_t sign, double y)
{
	return eval_as_double(fp_barrier(sign ? -y : y) * y);
}

float __math_oflowf(uint32_t sign)
{
	return __math_xflowf(sign, 0x1p97f);
}

float __math_uflowf(uint32_t sign)
{
	return __math_xflowf(sign, 0x1p-95f);
}

float __math_invalidf(float x)
{
	return (x - x) / (x - x);
}

float __math_divzerof(uint32_t sign)
{
	return fp_barrierf(sign ? -1.0f : 1.0f) / 0.0f;
}

float __math_xflowf(uint32_t sign, float y)
{
	return eval_as_float(fp_barrierf(sign ? -y : y) * y);
}

/*
 * Shared data between exp, exp2 and pow.
 *
 * Copyright (c) 2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#undef N
#define N (1 << EXP_TABLE_BITS)

const struct exp_data __exp_data = {
// N/ln2
.invln2N = 0x1.71547652b82fep0 * N,
// -ln2/N
.negln2hiN = -0x1.62e42fefa0000p-8,
.negln2loN = -0x1.cf79abc9e3b3ap-47,
// Used for rounding when !TOINT_INTRINSICS
#if EXP_USE_TOINT_NARROW
.shift = 0x1800000000.8p0,
#else
.shift = 0x1.8p52,
#endif
// exp polynomial coefficients.
.poly = {
// abs error: 1.555*2^-66
// ulp error: 0.509 (0.511 without fma)
// if |x| < ln2/256+eps
// abs error if |x| < ln2/256+0x1p-15: 1.09*2^-65
// abs error if |x| < ln2/128: 1.7145*2^-56
0x1.ffffffffffdbdp-2,
0x1.555555555543cp-3,
0x1.55555cf172b91p-5,
0x1.1111167a4d017p-7,
},
.exp2_shift = 0x1.8p52 / N,
// exp2 polynomial coefficients.
.exp2_poly = {
// abs error: 1.2195*2^-65
// ulp error: 0.507 (0.511 without fma)
// if |x| < 1/256
// abs error if |x| < 1/128: 1.9941*2^-56
0x1.62e42fefa39efp-1,
0x1.ebfbdff82c424p-3,
0x1.c6b08d70cf4b5p-5,
0x1.3b2abd24650ccp-7,
0x1.5d7e09b4e3a84p-10,
},
// 2^(k/N) ~= H[k]*(1 + T[k]) for int k in [0,N)
// tab[2*k] = asuint64(T[k])
// tab[2*k+1] = asuint64(H[k]) - (k << 52)/N
.tab = {
0x0, 0x3ff0000000000000,
0x3c9b3b4f1a88bf6e, 0x3feff63da9fb3335,
0xbc7160139cd8dc5d, 0x3fefec9a3e778061,
0xbc905e7a108766d1, 0x3fefe315e86e7f85,
0x3c8cd2523567f613, 0x3fefd9b0d3158574,
0xbc8bce8023f98efa, 0x3fefd06b29ddf6de,
0x3c60f74e61e6c861, 0x3fefc74518759bc8,
0x3c90a3e45b33d399, 0x3fefbe3ecac6f383,
0x3c979aa65d837b6d, 0x3fefb5586cf9890f,
0x3c8eb51a92fdeffc, 0x3fefac922b7247f7,
0x3c3ebe3d702f9cd1, 0x3fefa3ec32d3d1a2,
0xbc6a033489906e0b, 0x3fef9b66affed31b,
0xbc9556522a2fbd0e, 0x3fef9301d0125b51,
0xbc5080ef8c4eea55, 0x3fef8abdc06c31cc,
0xbc91c923b9d5f416, 0x3fef829aaea92de0,
0x3c80d3e3e95c55af, 0x3fef7a98c8a58e51,
0xbc801b15eaa59348, 0x3fef72b83c7d517b,
0xbc8f1ff055de323d, 0x3fef6af9388c8dea,
0x3c8b898c3f1353bf, 0x3fef635beb6fcb75,
0xbc96d99c7611eb26, 0x3fef5be084045cd4,
0x3c9aecf73e3a2f60, 0x3fef54873168b9aa,
0xbc8fe782cb86389d, 0x3fef4d5022fcd91d,
0x3c8a6f4144a6c38d, 0x3fef463b88628cd6,
0x3c807a05b0e4047d, 0x3fef3f49917ddc96,
0x3c968efde3a8a894, 0x3fef387a6e756238,
0x3c875e18f274487d, 0x3fef31ce4fb2a63f,
0x3c80472b981fe7f2, 0x3fef2b4565e27cdd,
0xbc96b87b3f71085e, 0x3fef24dfe1f56381,
0x3c82f7e16d09ab31, 0x3fef1e9df51fdee1,
0xbc3d219b1a6fbffa, 0x3fef187fd0dad990,
0x3c8b3782720c0ab4, 0x3fef1285a6e4030b,
0x3c6e149289cecb8f, 0x3fef0cafa93e2f56,
0x3c834d754db0abb6, 0x3fef06fe0a31b715,
0x3c864201e2ac744c, 0x3fef0170fc4cd831,
0x3c8fdd395dd3f84a, 0x3feefc08b26416ff,
0xbc86a3803b8e5b04, 0x3feef6c55f929ff1,
0xbc924aedcc4b5068, 0x3feef1a7373aa9cb,
0xbc9907f81b512d8e, 0x3feeecae6d05d866,
0xbc71d1e83e9436d2, 0x3feee7db34e59ff7,
0xbc991919b3ce1b15, 0x3feee32dc313a8e5,
0x3c859f48a72a4c6d, 0x3feedea64c123422,
0xbc9312607a28698a, 0x3feeda4504ac801c,
0xbc58a78f4817895b, 0x3feed60a21f72e2a,
0xbc7c2c9b67499a1b, 0x3feed1f5d950a897,
0x3c4363ed60c2ac11, 0x3feece086061892d,
0x3c9666093b0664ef, 0x3feeca41ed1d0057,
0x3c6ecce1daa10379, 0x3feec6a2b5c13cd0,
0x3c93ff8e3f0f1230, 0x3feec32af0d7d3de,
0x3c7690cebb7aafb0, 0x3feebfdad5362a27,
0x3c931dbdeb54e077, 0x3feebcb299fddd0d,
0xbc8f94340071a38e, 0x3feeb9b2769d2ca7,
0xbc87deccdc93a349, 0x3feeb6daa2cf6642,
0xbc78dec6bd0f385f, 0x3feeb42b569d4f82,
0xbc861246ec7b5cf6, 0x3feeb1a4ca5d920f,
0x3c93350518fdd78e, 0x3feeaf4736b527da,
0x3c7b98b72f8a9b05, 0x3feead12d497c7fd,
0x3c9063e1e21c5409, 0x3feeab07dd485429,
0x3c34c7855019c6ea, 0x3feea9268a5946b7,
0x3c9432e62b64c035, 0x3feea76f15ad2148,
0xbc8ce44a6199769f, 0x3feea5e1b976dc09,
0xbc8c33c53bef4da8, 0x3feea47eb03a5585,
0xbc845378892be9ae, 0x3feea34634ccc320,
0xbc93cedd78565858, 0x3feea23882552225,
0x3c5710aa807e1964, 0x3feea155d44ca973,
0xbc93b3efbf5e2228, 0x3feea09e667f3bcd,
0xbc6a12ad8734b982, 0x3feea012750bdabf,
0xbc6367efb86da9ee, 0x3fee9fb23c651a2f,
0xbc80dc3d54e08851, 0x3fee9f7df9519484,
0xbc781f647e5a3ecf, 0x3fee9f75e8ec5f74,
0xbc86ee4ac08b7db0, 0x3fee9f9a48a58174,
0xbc8619321e55e68a, 0x3fee9feb564267c9,
0x3c909ccb5e09d4d3, 0x3feea0694fde5d3f,
0xbc7b32dcb94da51d, 0x3feea11473eb0187,
0x3c94ecfd5467c06b, 0x3feea1ed0130c132,
0x3c65ebe1abd66c55, 0x3feea2f336cf4e62,
0xbc88a1c52fb3cf42, 0x3feea427543e1a12,
0xbc9369b6f13b3734, 0x3feea589994cce13,
0xbc805e843a19ff1e, 0x3feea71a4623c7ad,
0xbc94d450d872576e, 0x3feea8d99b4492ed,
0x3c90ad675b0e8a00, 0x3feeaac7d98a6699,
0x3c8db72fc1f0eab4, 0x3feeace5422aa0db,
0xbc65b6609cc5e7ff, 0x3feeaf3216b5448c,
0x3c7bf68359f35f44, 0x3feeb1ae99157736,
0xbc93091fa71e3d83, 0x3feeb45b0b91ffc6,
0xbc5da9b88b6c1e29, 0x3feeb737b0cdc5e5,
0xbc6c23f97c90b959, 0x3feeba44cbc8520f,
0xbc92434322f4f9aa, 0x3feebd829fde4e50,
0xbc85ca6cd7668e4b, 0x3feec0f170ca07ba,
0x3c71affc2b91ce27, 0x3feec49182a3f090,
0x3c6dd235e10a73bb, 0x3feec86319e32323,
0xbc87c50422622263, 0x3feecc667b5de565,
0x3c8b1c86e3e231d5, 0x3feed09bec4a2d33,
0xbc91bbd1d3bcbb15, 0x3feed503b23e255d,
0x3c90cc319cee31d2, 0x3feed99e1330b358,
0x3c8469846e735ab3, 0x3feede6b5579fdbf,
0xbc82dfcd978e9db4, 0x3feee36bbfd3f37a,
0x3c8c1a7792cb3387, 0x3feee89f995ad3ad,
0xbc907b8f4ad1d9fa, 0x3feeee07298db666,
0xbc55c3d956dcaeba, 0x3feef3a2b84f15fb,
0xbc90a40e3da6f640, 0x3feef9728de5593a,
0xbc68d6f438ad9334, 0x3feeff76f2fb5e47,
0xbc91eee26b588a35, 0x3fef05b030a1064a,
0x3c74ffd70a5fddcd, 0x3fef0c1e904bc1d2,
0xbc91bdfbfa9298ac, 0x3fef12c25bd71e09,
0x3c736eae30af0cb3, 0x3fef199bdd85529c,
0x3c8ee3325c9ffd94, 0x3fef20ab5fffd07a,
0x3c84e08fd10959ac, 0x3fef27f12e57d14b,
0x3c63cdaf384e1a67, 0x3fef2f6d9406e7b5,
0x3c676b2c6c921968, 0x3fef3720dcef9069,
0xbc808a1883ccb5d2, 0x3fef3f0b555dc3fa,
0xbc8fad5d3ffffa6f, 0x3fef472d4a07897c,
0xbc900dae3875a949, 0x3fef4f87080d89f2,
0x3c74a385a63d07a7, 0x3fef5818dcfba487,
0xbc82919e2040220f, 0x3fef60e316c98398,
0x3c8e5a50d5c192ac, 0x3fef69e603db3285,
0x3c843a59ac016b4b, 0x3fef7321f301b460,
0xbc82d52107b43e1f, 0x3fef7c97337b9b5f,
0xbc892ab93b470dc9, 0x3fef864614f5a129,
0x3c74b604603a88d3, 0x3fef902ee78b3ff6,
0x3c83c5ec519d7271, 0x3fef9a51fbc74c83,
0xbc8ff7128fd391f0, 0x3fefa4afa2a490da,
0xbc8dae98e223747d, 0x3fefaf482d8e67f1,
0x3c8ec3bc41aa2008, 0x3fefba1bee615a27,
0x3c842b94c3a9eb32, 0x3fefc52b376bba97,
0x3c8a64a931d185ee, 0x3fefd0765b6e4540,
0xbc8e37bae43be3ed, 0x3fefdbfdad9cbe14,
0x3c77893b4d91cd9d, 0x3fefe7c1819e90d8,
0x3c5305c14160cc89, 0x3feff3c22b8f71f1,
},
};

/*
 * Data for the log part of pow.
 *
 * Copyright (c) 2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#undef N
#define N (1 << POW_LOG_TABLE_BITS)

const struct pow_log_data __pow_log_data = {
.ln2hi = 0x1.62e42fefa3800p-1,
.ln2lo = 0x1.ef35793c76730p-45,
.poly = {
// relative error: 0x1.11922ap-70
// in -0x1.6bp-8 0x1.6bp-8
// Coefficients are scaled to match the scaling during evaluation.
-0x1p-1,
0x1.555555555556p-2 * -2,
-0x1.0000000000006p-2 * -2,
0x1.999999959554ep-3 * 4,
-0x1.555555529a47ap-3 * 4,
0x1.2495b9b4845e9p-3 * -8,
-0x1.0002b8b263fc3p-3 * -8,
},
/* Algorithm:

	x = 2^k z
	log(x) = k ln2 + log(c) + log(z/c)
	log(z/c) = poly(z/c - 1)

where z is in [0x1.69555p-1; 0x1.69555p0] which is split into N subintervals
and z falls into the ith one, then table entries are computed as

	tab[i].invc = 1/c
	tab[i].logc = round(0x1p43*log(c))/0x1p43
	tab[i].logctail = (double)(log(c) - logc)

where c is chosen near the center of the subinterval such that 1/c has only a
few precision bits so z/c - 1 is exactly representible as double:

	1/c = center < 1 ? round(N/center)/N : round(2*N/center)/N/2

Note: |z/c - 1| < 1/N for the chosen c, |log(c) - logc - logctail| < 0x1p-97,
the last few bits of logc are rounded away so k*ln2hi + logc has no rounding
error and the interval for z is selected such that near x == 1, where log(x)
is tiny, large cancellation error is avoided in logc + poly(z/c - 1).  */
.tab = {
#define A(a, b, c) {a, 0, b, c},
A(0x1.6a00000000000p+0, -0x1.62c82f2b9c800p-2, 0x1.ab42428375680p-48)
A(0x1.6800000000000p+0, -0x1.5d1bdbf580800p-2, -0x1.ca508d8e0f720p-46)
A(0x1.6600000000000p+0, -0x1.5767717455800p-2, -0x1.362a4d5b6506dp-45)
A(0x1.6400000000000p+0, -0x1.51aad872df800p-2, -0x1.684e49eb067d5p-49)
A(0x1.6200000000000p+0, -0x1.4be5f95777800p-2, -0x1.41b6993293ee0p-47)
A(0x1.6000000000000p+0, -0x1.4618bc21c6000p-2, 0x1.3d82f484c84ccp-46)
A(0x1.5e00000000000p+0, -0x1.404308686a800p-2, 0x1.c42f3ed820b3ap-50)
A(0x1.5c00000000000p+0, -0x1.3a64c55694800p-2, 0x1.0b1c686519460p-45)
A(0x1.5a00000000000p+0, -0x1.347dd9a988000p-2, 0x1.5594dd4c58092p-45)
A(0x1.5800000000000p+0, -0x1.2e8e2bae12000p-2, 0x1.67b1e99b72bd8p-45)
A(0x1.5600000000000p+0, -0x1.2895a13de8800p-2, 0x1.5ca14b6cfb03fp-46)
A(0x1.5600000000000p+0, -0x1.2895a13de8800p-2, 0x1.5ca14b6cfb03fp-46)
A(0x1.5400000000000p+0, -0x1.22941fbcf7800p-2, -0x1.65a242853da76p-46)
A(0x1.5200000000000p+0, -0x1.1c898c1699800p-2, -0x1.fafbc68e75404p-46)
A(0x1.5000000000000p+0, -0x1.1675cababa800p-2, 0x1.f1fc63382a8f0p-46)
A(0x1.4e00000000000p+0, -0x1.1058bf9ae4800p-2, -0x1.6a8c4fd055a66p-45)
A(0x1.4c00000000000p+0, -0x1.0a324e2739000p-2, -0x1.c6bee7ef4030ep-47)
A(0x1.4a00000000000p+0, -0x1.0402594b4d000p-2, -0x1.036b89ef42d7fp-48)
A(0x1.4a00000000000p+0, -0x1.0402594b4d000p-2, -0x1.036b89ef42d7fp-48)
A(0x1.4800000000000p+0, -0x1.fb9186d5e4000p-3, 0x1.d572aab993c87p-47)
A(0x1.4600000000000p+0, -0x1.ef0adcbdc6000p-3, 0x1.b26b79c86af24p-45)
A(0x1.4400000000000p+0, -0x1.e27076e2af000p-3, -0x1.72f4f543fff10p-46)
A(0x1.4200000000000p+0, -0x1.d5c216b4fc000p-3, 0x1.1ba91bbca681bp-45)
A(0x1.4000000000000p+0, -0x1.c8ff7c79aa000p-3, 0x1.7794f689f8434p-45)
A(0x1.4000000000000p+0, -0x1.c8ff7c79aa000p-3, 0x1.7794f689f8434p-45)
A(0x1.3e00000000000p+0, -0x1.bc286742d9000p-3, 0x1.94eb0318bb78fp-46)
A(0x1.3c00000000000p+0, -0x1.af3c94e80c000p-3, 0x1.a4e633fcd9066p-52)
A(0x1.3a00000000000p+0, -0x1.a23bc1fe2b000p-3, -0x1.58c64dc46c1eap-45)
A(0x1.3a00000000000p+0, -0x1.a23bc1fe2b000p-3, -0x1.58c64dc46c1eap-45)
A(0x1.3800000000000p+0, -0x1.9525a9cf45000p-3, -0x1.ad1d904c1d4e3p-45)
A(0x1.3600000000000p+0, -0x1.87fa06520d000p-3, 0x1.bbdbf7fdbfa09p-45)
A(0x1.3400000000000p+0, -0x1.7ab890210e000p-3, 0x1.bdb9072534a58p-45)
A(0x1.3400000000000p+0, -0x1.7ab890210e000p-3, 0x1.bdb9072534a58p-45)
A(0x1.3200000000000p+0, -0x1.6d60fe719d000p-3, -0x1.0e46aa3b2e266p-46)
A(0x1.3000000000000p+0, -0x1.5ff3070a79000p-3, -0x1.e9e439f105039p-46)
A(0x1.3000000000000p+0, -0x1.5ff3070a79000p-3, -0x1.e9e439f105039p-46)
A(0x1.2e00000000000p+0, -0x1.526e5e3a1b000p-3, -0x1.0de8b90075b8fp-45)
A(0x1.2c00000000000p+0, -0x1.44d2b6ccb8000p-3, 0x1.70cc16135783cp-46)
A(0x1.2c00000000000p+0, -0x1.44d2b6ccb8000p-3, 0x1.70cc16135783cp-46)
A(0x1.2a00000000000p+0, -0x1.371fc201e9000p-3, 0x1.178864d27543ap-48)
A(0x1.2800000000000p+0, -0x1.29552f81ff000p-3, -0x1.48d301771c408p-45)
A(0x1.2600000000000p+0, -0x1.1b72ad52f6000p-3, -0x1.e80a41811a396p-45)
A(0x1.2600000000000p+0, -0x1.1b72ad52f6000p-3, -0x1.e80a41811a396p-45)
A(0x1.2400000000000p+0, -0x1.0d77e7cd09000p-3, 0x1.a699688e85bf4p-47)
A(0x1.2400000000000p+0, -0x1.0d77e7cd09000p-3, 0x1.a699688e85bf4p-47)
A(0x1.2200000000000p+0, -0x1.fec9131dbe000p-4, -0x1.575545ca333f2p-45)
A(0x1.2000000000000p+0, -0x1.e27076e2b0000p-4, 0x1.a342c2af0003cp-45)
A(0x1.2000000000000p+0, -0x1.e27076e2b0000p-4, 0x1.a342c2af0003cp-45)
A(0x1.1e00000000000p+0, -0x1.c5e548f5bc000p-4, -0x1.d0c57585fbe06p-46)
A(0x1.1c00000000000p+0, -0x1.a926d3a4ae000p-4, 0x1.53935e85baac8p-45)
A(0x1.1c00000000000p+0, -0x1.a926d3a4ae000p-4, 0x1.53935e85baac8p-45)
A(0x1.1a00000000000p+0, -0x1.8c345d631a000p-4, 0x1.37c294d2f5668p-46)
A(0x1.1a00000000000p+0, -0x1.8c345d631a000p-4, 0x1.37c294d2f5668p-46)
A(0x1.1800000000000p+0, -0x1.6f0d28ae56000p-4, -0x1.69737c93373dap-45)
A(0x1.1600000000000p+0, -0x1.51b073f062000p-4, 0x1.f025b61c65e57p-46)
A(0x1.1600000000000p+0, -0x1.51b073f062000p-4, 0x1.f025b61c65e57p-46)
A(0x1.1400000000000p+0, -0x1.341d7961be000p-4, 0x1.c5edaccf913dfp-45)
A(0x1.1400000000000p+0, -0x1.341d7961be000p-4, 0x1.c5edaccf913dfp-45)
A(0x1.1200000000000p+0, -0x1.16536eea38000p-4, 0x1.47c5e768fa309p-46)
A(0x1.1000000000000p+0, -0x1.f0a30c0118000p-5, 0x1.d599e83368e91p-45)
A(0x1.1000000000000p+0, -0x1.f0a30c0118000p-5, 0x1.d599e83368e91p-45)
A(0x1.0e00000000000p+0, -0x1.b42dd71198000p-5, 0x1.c827ae5d6704cp-46)
A(0x1.0e00000000000p+0, -0x1.b42dd71198000p-5, 0x1.c827ae5d6704cp-46)
A(0x1.0c00000000000p+0, -0x1.77458f632c000p-5, -0x1.cfc4634f2a1eep-45)
A(0x1.0c00000000000p+0, -0x1.77458f632c000p-5, -0x1.cfc4634f2a1eep-45)
A(0x1.0a00000000000p+0, -0x1.39e87b9fec000p-5, 0x1.502b7f526feaap-48)
A(0x1.0a00000000000p+0, -0x1.39e87b9fec000p-5, 0x1.502b7f526feaap-48)
A(0x1.0800000000000p+0, -0x1.f829b0e780000p-6, -0x1.980267c7e09e4p-45)
A(0x1.0800000000000p+0, -0x1.f829b0e780000p-6, -0x1.980267c7e09e4p-45)
A(0x1.0600000000000p+0, -0x1.7b91b07d58000p-6, -0x1.88d5493faa639p-45)
A(0x1.0400000000000p+0, -0x1.fc0a8b0fc0000p-7, -0x1.f1e7cf6d3a69cp-50)
A(0x1.0400000000000p+0, -0x1.fc0a8b0fc0000p-7, -0x1.f1e7cf6d3a69cp-50)
A(0x1.0200000000000p+0, -0x1.fe02a6b100000p-8, -0x1.9e23f0dda40e4p-46)
A(0x1.0200000000000p+0, -0x1.fe02a6b100000p-8, -0x1.9e23f0dda40e4p-46)
A(0x1.0000000000000p+0, 0x0.0000000000000p+0, 0x0.0000000000000p+0)
A(0x1.0000000000000p+0, 0x0.0000000000000p+0, 0x0.0000000000000p+0)
A(0x1.fc00000000000p-1, 0x1.0101575890000p-7, -0x1.0c76b999d2be8p-46)
A(0x1.f800000000000p-1, 0x1.0205658938000p-6, -0x1.3dc5b06e2f7d2p-45)
A(0x1.f400000000000p-1, 0x1.8492528c90000p-6, -0x1.aa0ba325a0c34p-45)
A(0x1.f000000000000p-1, 0x1.0415d89e74000p-5, 0x1.111c05cf1d753p-47)
A(0x1.ec00000000000p-1, 0x1.466aed42e0000p-5, -0x1.c167375bdfd28p-45)
A(0x1.e800000000000p-1, 0x1.894aa149fc000p-5, -0x1.97995d05a267dp-46)
A(0x1.e400000000000p-1, 0x1.ccb73cdddc000p-5, -0x1.a68f247d82807p-46)
A(0x1.e200000000000p-1, 0x1.eea31c006c000p-5, -0x1.e113e4fc93b7bp-47)
A(0x1.de00000000000p-1, 0x1.1973bd1466000p-4, -0x1.5325d560d9e9bp-45)
A(0x1.da00000000000p-1, 0x1.3bdf5a7d1e000p-4, 0x1.cc85ea5db4ed7p-45)
A(0x1.d600000000000p-1, 0x1.5e95a4d97a000p-4, -0x1.c69063c5d1d1ep-45)
A(0x1.d400000000000p-1, 0x1.700d30aeac000p-4, 0x1.c1e8da99ded32p-49)
A(0x1.d000000000000p-1, 0x1.9335e5d594000p-4, 0x1.3115c3abd47dap-45)
A(0x1.cc00000000000p-1, 0x1.b6ac88dad6000p-4, -0x1.390802bf768e5p-46)
A(0x1.ca00000000000p-1, 0x1.c885801bc4000p-4, 0x1.646d1c65aacd3p-45)
A(0x1.c600000000000p-1, 0x1.ec739830a2000p-4, -0x1.dc068afe645e0p-45)
A(0x1.c400000000000p-1, 0x1.fe89139dbe000p-4, -0x1.534d64fa10afdp-45)
A(0x1.c000000000000p-1, 0x1.1178e8227e000p-3, 0x1.1ef78ce2d07f2p-45)
A(0x1.be00000000000p-1, 0x1.1aa2b7e23f000p-3, 0x1.ca78e44389934p-45)
A(0x1.ba00000000000p-1, 0x1.2d1610c868000p-3, 0x1.39d6ccb81b4a1p-47)
A(0x1.b800000000000p-1, 0x1.365fcb0159000p-3, 0x1.62fa8234b7289p-51)
A(0x1.b400000000000p-1, 0x1.4913d8333b000p-3, 0x1.5837954fdb678p-45)
A(0x1.b200000000000p-1, 0x1.527e5e4a1b000p-3, 0x1.633e8e5697dc7p-45)
A(0x1.ae00000000000p-1, 0x1.6574ebe8c1000p-3, 0x1.9cf8b2c3c2e78p-46)
A(0x1.ac00000000000p-1, 0x1.6f0128b757000p-3, -0x1.5118de59c21e1p-45)
A(0x1.aa00000000000p-1, 0x1.7898d85445000p-3, -0x1.c661070914305p-46)
A(0x1.a600000000000p-1, 0x1.8beafeb390000p-3, -0x1.73d54aae92cd1p-47)
A(0x1.a400000000000p-1, 0x1.95a5adcf70000p-3, 0x1.7f22858a0ff6fp-47)
A(0x1.a000000000000p-1, 0x1.a93ed3c8ae000p-3, -0x1.8724350562169p-45)
A(0x1.9e00000000000p-1, 0x1.b31d8575bd000p-3, -0x1.c358d4eace1aap-47)
A(0x1.9c00000000000p-1, 0x1.bd087383be000p-3, -0x1.d4bc4595412b6p-45)
A(0x1.9a00000000000p-1, 0x1.c6ffbc6f01000p-3, -0x1.1ec72c5962bd2p-48)
A(0x1.9600000000000p-1, 0x1.db13db0d49000p-3, -0x1.aff2af715b035p-45)
A(0x1.9400000000000p-1, 0x1.e530effe71000p-3, 0x1.212276041f430p-51)
A(0x1.9200000000000p-1, 0x1.ef5ade4dd0000p-3, -0x1.a211565bb8e11p-51)
A(0x1.9000000000000p-1, 0x1.f991c6cb3b000p-3, 0x1.bcbecca0cdf30p-46)
A(0x1.8c00000000000p-1, 0x1.07138604d5800p-2, 0x1.89cdb16ed4e91p-48)
A(0x1.8a00000000000p-1, 0x1.0c42d67616000p-2, 0x1.7188b163ceae9p-45)
A(0x1.8800000000000p-1, 0x1.1178e8227e800p-2, -0x1.c210e63a5f01cp-45)
A(0x1.8600000000000p-1, 0x1.16b5ccbacf800p-2, 0x1.b9acdf7a51681p-45)
A(0x1.8400000000000p-1, 0x1.1bf99635a6800p-2, 0x1.ca6ed5147bdb7p-45)
A(0x1.8200000000000p-1, 0x1.214456d0eb800p-2, 0x1.a87deba46baeap-47)
A(0x1.7e00000000000p-1, 0x1.2bef07cdc9000p-2, 0x1.a9cfa4a5004f4p-45)
A(0x1.7c00000000000p-1, 0x1.314f1e1d36000p-2, -0x1.8e27ad3213cb8p-45)
A(0x1.7a00000000000p-1, 0x1.36b6776be1000p-2, 0x1.16ecdb0f177c8p-46)
A(0x1.7800000000000p-1, 0x1.3c25277333000p-2, 0x1.83b54b606bd5cp-46)
A(0x1.7600000000000p-1, 0x1.419b423d5e800p-2, 0x1.8e436ec90e09dp-47)
A(0x1.7400000000000p-1, 0x1.4718dc271c800p-2, -0x1.f27ce0967d675p-45)
A(0x1.7200000000000p-1, 0x1.4c9e09e173000p-2, -0x1.e20891b0ad8a4p-45)
A(0x1.7000000000000p-1, 0x1.522ae0738a000p-2, 0x1.ebe708164c759p-45)
A(0x1.6e00000000000p-1, 0x1.57bf753c8d000p-2, 0x1.fadedee5d40efp-46)
A(0x1.6c00000000000p-1, 0x1.5d5bddf596000p-2, -0x1.a0b2a08a465dcp-47)
},
};

/*
 * Shared data between expf, exp2f and powf.
 *
 * Copyright (c) 2017-2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#undef N
#define N (1 << EXP2F_TABLE_BITS)

const struct exp2f_data __exp2f_data = {
  /* tab[i] = uint(2^(i/N)) - (i << 52-BITS)
     used for computing 2^(k/N) for an int |k| < 150 N as
     double(tab[k%N] + (k << 52-BITS)) */
  .tab = {
0x3ff0000000000000, 0x3fefd9b0d3158574, 0x3fefb5586cf9890f, 0x3fef9301d0125b51,
0x3fef72b83c7d517b, 0x3fef54873168b9aa, 0x3fef387a6e756238, 0x3fef1e9df51fdee1,
0x3fef06fe0a31b715, 0x3feef1a7373aa9cb, 0x3feedea64c123422, 0x3feece086061892d,
0x3feebfdad5362a27, 0x3feeb42b569d4f82, 0x3feeab07dd485429, 0x3feea47eb03a5585,
0x3feea09e667f3bcd, 0x3fee9f75e8ec5f74, 0x3feea11473eb0187, 0x3feea589994cce13,
0x3feeace5422aa0db, 0x3feeb737b0cdc5e5, 0x3feec49182a3f090, 0x3feed503b23e255d,
0x3feee89f995ad3ad, 0x3feeff76f2fb5e47, 0x3fef199bdd85529c, 0x3fef3720dcef9069,
0x3fef5818dcfba487, 0x3fef7c97337b9b5f, 0x3fefa4afa2a490da, 0x3fefd0765b6e4540,
  },
  .shift_scaled = 0x1.8p+52 / N,
  .poly = {
  0x1.c6af84b912394p-5, 0x1.ebfce50fac4f3p-3, 0x1.62e42ff0c52d6p-1,
  },
  .shift = 0x1.8p+52,
  .invln2_scaled = 0x1.71547652b82fep+0 * N,
  .poly_scaled = {
  0x1.c6af84b912394p-5/N/N/N, 0x1.ebfce50fac4f3p-3/N/N, 0x1.62e42ff0c52d6p-1/N,
  },
};

/*
 * Data definition for powf.
 *
 * Copyright (c) 2017-2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


const struct powf_log2_data __powf_log2_data = {
  .tab = {
  { 0x1.661ec79f8f3bep+0, -0x1.efec65b963019p-2 * POWF_SCALE },
  { 0x1.571ed4aaf883dp+0, -0x1.b0b6832d4fca4p-2 * POWF_SCALE },
  { 0x1.49539f0f010bp+0, -0x1.7418b0a1fb77bp-2 * POWF_SCALE },
  { 0x1.3c995b0b80385p+0, -0x1.39de91a6dcf7bp-2 * POWF_SCALE },
  { 0x1.30d190c8864a5p+0, -0x1.01d9bf3f2b631p-2 * POWF_SCALE },
  { 0x1.25e227b0b8eap+0, -0x1.97c1d1b3b7afp-3 * POWF_SCALE },
  { 0x1.1bb4a4a1a343fp+0, -0x1.2f9e393af3c9fp-3 * POWF_SCALE },
  { 0x1.12358f08ae5bap+0, -0x1.960cbbf788d5cp-4 * POWF_SCALE },
  { 0x1.0953f419900a7p+0, -0x1.a6f9db6475fcep-5 * POWF_SCALE },
  { 0x1p+0, 0x0p+0 * POWF_SCALE },
  { 0x1.e608cfd9a47acp-1, 0x1.338ca9f24f53dp-4 * POWF_SCALE },
  { 0x1.ca4b31f026aap-1, 0x1.476a9543891bap-3 * POWF_SCALE },
  { 0x1.b2036576afce6p-1, 0x1.e840b4ac4e4d2p-3 * POWF_SCALE },
  { 0x1.9c2d163a1aa2dp-1, 0x1.40645f0c6651cp-2 * POWF_SCALE },
  { 0x1.886e6037841edp-1, 0x1.88e9c2c1b9ff8p-2 * POWF_SCALE },
  { 0x1.767dcf5534862p-1, 0x1.ce0a44eb17bccp-2 * POWF_SCALE },
  },
  .poly = {
  0x1.27616c9496e0bp-2 * POWF_SCALE, -0x1.71969a075c67ap-2 * POWF_SCALE,
  0x1.ec70a6ca7baddp-2 * POWF_SCALE, -0x1.7154748bef6c8p-1 * POWF_SCALE,
  0x1.71547652ab82bp0 * POWF_SCALE,
  }
};
