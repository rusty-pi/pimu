//! IEEE 754 arithmetic the way the ARM ARM pseudocode defines it (chapter J1,
//! `shared/functions/float`): `FPUnpack`, `FPRound`, `FPProcessNaNs` and the
//! operations built on them, for half, single and double precision.
//!
//! Host floats cannot report `FPSR`'s cumulative flags, honour `FPCR.RMode`,
//! flush denormals, substitute the default NaN or pick which NaN propagates the
//! way ARM does. So every operation computes the exact result as an integer
//! mantissa and exponent and rounds it once in [`Fp::round`], a transcription
//! of `FPRound`. Mantissas that do not fit keep a jam bit — bits shifted out
//! are ORed into bit 0 — which preserves round/sticky information.
//!
//! Values are raw bits in a `u64`, with the format given separately.

/// `FPSR` cumulative exception bits.
pub const IOC: u32 = 1 << 0;
pub const DZC: u32 = 1 << 1;
pub const OFC: u32 = 1 << 2;
pub const UFC: u32 = 1 << 3;
pub const IXC: u32 = 1 << 4;
pub const IDC: u32 = 1 << 7;

/// A floating-point format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    H,
    S,
    D,
}

impl Fmt {
    /// Exponent bits.
    pub const fn e(self) -> u32 {
        match self {
            Fmt::H => 5,
            Fmt::S => 8,
            Fmt::D => 11,
        }
    }
    /// Fraction bits.
    pub const fn f(self) -> u32 {
        match self {
            Fmt::H => 10,
            Fmt::S => 23,
            Fmt::D => 52,
        }
    }
    pub const fn bits(self) -> u32 {
        1 + self.e() + self.f()
    }
    const fn min_exp(self) -> i32 {
        2 - (1 << (self.e() - 1))
    }
    const fn bias(self) -> i32 {
        (1 << (self.e() - 1)) - 1
    }
    const fn exp_mask(self) -> u64 {
        (1 << self.e()) - 1
    }
    const fn frac_mask(self) -> u64 {
        (1 << self.f()) - 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    TieEven,
    PosInf,
    NegInf,
    Zero,
    TieAway,
    Odd,
}

impl Rounding {
    /// `FPDecodeRounding`: the `RMode` encoding.
    pub fn from_rmode(rmode: u32) -> Rounding {
        match rmode & 3 {
            0 => Rounding::TieEven,
            1 => Rounding::PosInf,
            2 => Rounding::NegInf,
            _ => Rounding::Zero,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zero,
    Inf,
    QNaN,
    SNaN,
    /// `|value| = mant * 2^exp`, `mant != 0`.
    Num,
}

#[derive(Debug, Clone, Copy)]
struct Unpacked {
    kind: Kind,
    sign: bool,
    mant: u64,
    exp: i32,
}

impl Unpacked {
    fn is_nan(&self) -> bool {
        matches!(self.kind, Kind::QNaN | Kind::SNaN)
    }
}

/// An exact (up to the jam bit) nonzero real: `mant * 2^exp`.
#[derive(Debug, Clone, Copy)]
struct Real {
    sign: bool,
    mant: u128,
    exp: i32,
}

/// How the bits below the rounding point compare with half an ulp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    Exact,
    Below,
    Half,
    Above,
}

/// `m >> s`, and how the discarded bits compare with half of the new ulp.
fn shr_rest(m: u128, s: u32) -> (u128, Rest) {
    if s == 0 {
        return (m, Rest::Exact);
    }
    if s > 128 {
        return (0, if m == 0 { Rest::Exact } else { Rest::Below });
    }
    let (q, rem, half) = if s == 128 {
        (0, m, 1u128 << 127)
    } else {
        (m >> s, m & ((1u128 << s) - 1), 1u128 << (s - 1))
    };
    let rest = if rem == 0 {
        Rest::Exact
    } else if rem < half {
        Rest::Below
    } else if rem == half {
        Rest::Half
    } else {
        Rest::Above
    };
    (q, rest)
}

/// Shift right, ORing every bit shifted out into bit 0.
fn shr_jam(m: u128, s: u32) -> u128 {
    if s == 0 {
        m
    } else if s >= 128 {
        (m != 0) as u128
    } else {
        (m >> s) | ((m & ((1u128 << s) - 1) != 0) as u128)
    }
}

/// `isqrt(n)` and whether it was exact.
fn isqrt(n: u128) -> (u128, bool) {
    let mut rem = n;
    let mut root = 0u128;
    let mut bit = 1u128 << 126;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    (root, rem == 0)
}

/// The floating-point environment of one instruction: `FPCR` in, the
/// exception flags it raised out (to be ORed into `FPSR`).
pub struct Fp {
    pub fpcr: u32,
    pub flags: u32,
}

impl Fp {
    pub fn new(fpcr: u32) -> Fp {
        Fp { fpcr, flags: 0 }
    }

    fn dn(&self) -> bool {
        self.fpcr & (1 << 25) != 0
    }
    fn fz(&self) -> bool {
        self.fpcr & (1 << 24) != 0
    }
    fn ahp(&self) -> bool {
        self.fpcr & (1 << 26) != 0
    }
    pub fn rounding(&self) -> Rounding {
        Rounding::from_rmode(self.fpcr >> 22)
    }

    // --- Packing ------------------------------------------------------------

    fn unpack_base(&mut self, fmt: Fmt, bits: u64, ahp: bool) -> Unpacked {
        let sign = (bits >> (fmt.bits() - 1)) & 1 != 0;
        let exp = (bits >> fmt.f()) & fmt.exp_mask();
        let frac = bits & fmt.frac_mask();
        let f = fmt.f() as i32;
        let mut u = Unpacked {
            kind: Kind::Num,
            sign,
            mant: 0,
            exp: 0,
        };
        if exp == 0 {
            if frac == 0 || (self.fz() && fmt != Fmt::H) {
                if frac != 0 {
                    self.flags |= IDC;
                }
                u.kind = Kind::Zero;
            } else {
                u.mant = frac;
                u.exp = fmt.min_exp() - f;
            }
        } else if exp == fmt.exp_mask() && !(fmt == Fmt::H && ahp) {
            u.kind = if frac == 0 {
                Kind::Inf
            } else if frac >> (fmt.f() - 1) != 0 {
                Kind::QNaN
            } else {
                Kind::SNaN
            };
        } else {
            u.mant = frac | (1 << fmt.f());
            u.exp = exp as i32 - fmt.bias() - f;
        }
        u
    }

    /// `FPUnpack`: arithmetic ignores `FPCR.AHP`.
    fn unpack(&mut self, fmt: Fmt, bits: u64) -> Unpacked {
        self.unpack_base(fmt, bits, false)
    }

    pub fn zero(fmt: Fmt, sign: bool) -> u64 {
        (sign as u64) << (fmt.bits() - 1)
    }
    pub fn inf(fmt: Fmt, sign: bool) -> u64 {
        Self::zero(fmt, sign) | (fmt.exp_mask() << fmt.f())
    }
    fn max_normal(fmt: Fmt, sign: bool) -> u64 {
        Self::zero(fmt, sign) | ((fmt.exp_mask() - 1) << fmt.f()) | fmt.frac_mask()
    }
    pub fn default_nan(fmt: Fmt) -> u64 {
        (fmt.exp_mask() << fmt.f()) | (1 << (fmt.f() - 1))
    }
    fn two(fmt: Fmt, sign: bool) -> u64 {
        Self::zero(fmt, sign) | (((fmt.bias() + 1) as u64) << fmt.f())
    }
    fn one_point_five(fmt: Fmt, sign: bool) -> u64 {
        Self::zero(fmt, sign) | ((fmt.bias() as u64) << fmt.f()) | (1 << (fmt.f() - 1))
    }

    /// `FPRound`.
    fn round(&mut self, fmt: Fmt, r: Real, rounding: Rounding) -> u64 {
        debug_assert!(r.mant != 0);
        let f = fmt.f() as i32;
        let msb = 127 - r.mant.leading_zeros() as i32;
        let exponent = r.exp + msb;
        if self.fz() && fmt != Fmt::H && exponent < fmt.min_exp() {
            self.flags |= UFC;
            return Self::zero(fmt, r.sign);
        }
        let mut biased = (exponent - fmt.min_exp() + 1).max(0);
        let unit = if biased == 0 { fmt.min_exp() } else { exponent } - f;
        let shift = unit - r.exp;
        let (int_mant, rest) = if shift <= 0 {
            (r.mant << -shift, Rest::Exact)
        } else {
            shr_rest(r.mant, shift as u32)
        };
        let mut int_mant = int_mant as u64;
        if biased == 0 && rest != Rest::Exact {
            self.flags |= UFC;
        }
        let (round_up, overflow_to_inf) = match rounding {
            Rounding::TieEven => (
                rest == Rest::Above || (rest == Rest::Half && int_mant & 1 != 0),
                true,
            ),
            Rounding::PosInf => (rest != Rest::Exact && !r.sign, !r.sign),
            Rounding::NegInf => (rest != Rest::Exact && r.sign, r.sign),
            Rounding::TieAway => (rest == Rest::Above || rest == Rest::Half, true),
            Rounding::Zero | Rounding::Odd => (false, false),
        };
        if round_up {
            int_mant += 1;
            if int_mant == 1 << f {
                biased = 1;
            }
            if int_mant == 2 << f {
                biased += 1;
                int_mant >>= 1;
            }
        }
        let mut inexact = rest != Rest::Exact;
        if inexact && rounding == Rounding::Odd {
            int_mant |= 1;
        }
        let result = if fmt != Fmt::H || !self.ahp() {
            if biased >= fmt.exp_mask() as i32 {
                self.flags |= OFC;
                inexact = true;
                if overflow_to_inf {
                    Self::inf(fmt, r.sign)
                } else {
                    Self::max_normal(fmt, r.sign)
                }
            } else {
                Self::zero(fmt, r.sign) | ((biased as u64) << f) | (int_mant & fmt.frac_mask())
            }
        } else if biased > fmt.exp_mask() as i32 {
            self.flags |= IOC;
            inexact = false;
            Self::zero(fmt, r.sign) | ((1 << (fmt.bits() - 1)) - 1)
        } else {
            Self::zero(fmt, r.sign) | ((biased as u64) << f) | (int_mant & fmt.frac_mask())
        };
        if inexact {
            self.flags |= IXC;
        }
        result
    }

    fn round_mode(&mut self, fmt: Fmt, r: Real) -> u64 {
        let rounding = self.rounding();
        self.round(fmt, r, rounding)
    }

    // --- NaNs -----------------------------------------------------------------

    /// `FPProcessNaN`.
    fn process_nan(&mut self, fmt: Fmt, u: &Unpacked, bits: u64) -> u64 {
        let mut result = bits;
        if u.kind == Kind::SNaN {
            result |= 1 << (fmt.f() - 1);
            self.flags |= IOC;
        }
        if self.dn() {
            result = Self::default_nan(fmt);
        }
        result
    }

    /// `FPProcessNaNs` / `FPProcessNaNs3`: signalling NaNs first, then quiet
    /// ones, each in operand order.
    fn process_nans(&mut self, fmt: Fmt, ops: &[(Unpacked, u64)]) -> Option<u64> {
        for kind in [Kind::SNaN, Kind::QNaN] {
            if let Some((u, bits)) = ops.iter().find(|(u, _)| u.kind == kind) {
                return Some(self.process_nan(fmt, u, *bits));
            }
        }
        None
    }

    fn invalid(&mut self, fmt: Fmt) -> u64 {
        self.flags |= IOC;
        Self::default_nan(fmt)
    }

    // --- Arithmetic -------------------------------------------------------------

    fn real(u: &Unpacked) -> Real {
        Real {
            sign: u.sign,
            mant: u.mant as u128,
            exp: u.exp,
        }
    }

    /// Exact `a + b` of two nonzero reals, `None` if it is exactly zero.
    fn add_real(a: Real, b: Real) -> Option<Real> {
        // Put each with its top bit at 125, then align on the larger exponent.
        let norm = |r: Real| {
            let lz = r.mant.leading_zeros() as i32;
            let s = lz - 2;
            if s >= 0 {
                Real {
                    mant: r.mant << s,
                    exp: r.exp - s,
                    ..r
                }
            } else {
                Real {
                    mant: shr_jam(r.mant, (-s) as u32),
                    exp: r.exp - s,
                    ..r
                }
            }
        };
        let (a, b) = (norm(a), norm(b));
        let (big, small) = if a.exp >= b.exp { (a, b) } else { (b, a) };
        let d = (big.exp - small.exp) as u32;
        let sm = shr_jam(small.mant, d);
        if big.sign == small.sign {
            Some(Real {
                sign: big.sign,
                mant: big.mant + sm,
                exp: big.exp,
            })
        } else if big.mant > sm {
            Some(Real {
                sign: big.sign,
                mant: big.mant - sm,
                exp: big.exp,
            })
        } else if big.mant < sm {
            Some(Real {
                sign: small.sign,
                mant: sm - big.mant,
                exp: big.exp,
            })
        } else {
            None
        }
    }

    fn exact_zero(&self, fmt: Fmt) -> u64 {
        Self::zero(fmt, self.rounding() == Rounding::NegInf)
    }

    /// `FPAdd` / `FPSub`.
    pub fn add(&mut self, fmt: Fmt, a: u64, b: u64, sub: bool) -> u64 {
        let ua = self.unpack(fmt, a);
        let mut ub = self.unpack(fmt, b);
        if let Some(n) = self.process_nans(fmt, &[(ua, a), (ub, b)]) {
            return n;
        }
        ub.sign ^= sub;
        match (ua.kind, ub.kind) {
            (Kind::Inf, Kind::Inf) if ua.sign != ub.sign => self.invalid(fmt),
            (Kind::Inf, _) => Self::inf(fmt, ua.sign),
            (_, Kind::Inf) => Self::inf(fmt, ub.sign),
            (Kind::Zero, Kind::Zero) if ua.sign == ub.sign => Self::zero(fmt, ua.sign),
            (Kind::Zero, Kind::Zero) => self.exact_zero(fmt),
            (Kind::Zero, _) => self.round_mode(fmt, Self::real(&ub)),
            (_, Kind::Zero) => self.round_mode(fmt, Self::real(&ua)),
            _ => match Self::add_real(Self::real(&ua), Self::real(&ub)) {
                Some(r) => self.round_mode(fmt, r),
                None => self.exact_zero(fmt),
            },
        }
    }

    /// `FPMul`; `mulx` is `FMULX`, where infinity times zero is 2.
    pub fn mul(&mut self, fmt: Fmt, a: u64, b: u64, mulx: bool) -> u64 {
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if let Some(n) = self.process_nans(fmt, &[(ua, a), (ub, b)]) {
            return n;
        }
        let sign = ua.sign ^ ub.sign;
        match (ua.kind, ub.kind) {
            (Kind::Inf, Kind::Zero) | (Kind::Zero, Kind::Inf) => {
                if mulx {
                    Self::two(fmt, sign)
                } else {
                    self.invalid(fmt)
                }
            }
            (Kind::Inf, _) | (_, Kind::Inf) => Self::inf(fmt, sign),
            (Kind::Zero, _) | (_, Kind::Zero) => Self::zero(fmt, sign),
            _ => self.round_mode(
                fmt,
                Real {
                    sign,
                    mant: ua.mant as u128 * ub.mant as u128,
                    exp: ua.exp + ub.exp,
                },
            ),
        }
    }

    /// `FPMulAdd`: `addend + a * b`, rounded once. The instructions negate
    /// operands before calling this, the way the pseudocode does.
    pub fn mul_add(&mut self, fmt: Fmt, addend: u64, a: u64, b: u64) -> u64 {
        let uc = self.unpack(fmt, addend);
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        let inf_times_zero = matches!(
            (ua.kind, ub.kind),
            (Kind::Inf, Kind::Zero) | (Kind::Zero, Kind::Inf)
        );
        if let Some(n) = self.process_nans(fmt, &[(uc, addend), (ua, a), (ub, b)]) {
            if uc.kind == Kind::QNaN && inf_times_zero {
                return self.invalid(fmt);
            }
            return n;
        }
        let sign_p = ua.sign ^ ub.sign;
        let inf_p = ua.kind == Kind::Inf || ub.kind == Kind::Inf;
        let zero_p = ua.kind == Kind::Zero || ub.kind == Kind::Zero;
        let inf_c = uc.kind == Kind::Inf;
        if inf_times_zero || (inf_c && inf_p && uc.sign != sign_p) {
            return self.invalid(fmt);
        }
        if inf_c {
            return Self::inf(fmt, uc.sign);
        }
        if inf_p {
            return Self::inf(fmt, sign_p);
        }
        let zero_c = uc.kind == Kind::Zero;
        if zero_c && zero_p && uc.sign == sign_p {
            return Self::zero(fmt, uc.sign);
        }
        let prod = (!zero_p).then(|| Real {
            sign: sign_p,
            mant: ua.mant as u128 * ub.mant as u128,
            exp: ua.exp + ub.exp,
        });
        let sum = match (zero_c, prod) {
            (true, None) => None,
            (true, Some(p)) => Some(p),
            (false, None) => Some(Self::real(&uc)),
            (false, Some(p)) => Self::add_real(Self::real(&uc), p),
        };
        match sum {
            Some(r) => self.round_mode(fmt, r),
            None => self.exact_zero(fmt),
        }
    }

    /// `FPDiv`.
    pub fn div(&mut self, fmt: Fmt, a: u64, b: u64) -> u64 {
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if let Some(n) = self.process_nans(fmt, &[(ua, a), (ub, b)]) {
            return n;
        }
        let sign = ua.sign ^ ub.sign;
        match (ua.kind, ub.kind) {
            (Kind::Inf, Kind::Inf) | (Kind::Zero, Kind::Zero) => self.invalid(fmt),
            (Kind::Inf, _) | (_, Kind::Zero) => {
                if ub.kind == Kind::Zero && ua.kind != Kind::Inf {
                    self.flags |= DZC;
                }
                Self::inf(fmt, sign)
            }
            (Kind::Zero, _) | (_, Kind::Inf) => Self::zero(fmt, sign),
            _ => {
                // Normalise both to 64-bit mantissas, then a 128/64 divide
                // leaves at least 63 quotient bits.
                let (sa, sb) = (ua.mant.leading_zeros(), ub.mant.leading_zeros());
                let ma = (ua.mant << sa) as u128;
                let mb = (ub.mant << sb) as u128;
                let num = ma << 64;
                let q = num / mb;
                let rem = num % mb;
                self.round_mode(
                    fmt,
                    Real {
                        sign,
                        mant: (q << 1) | (rem != 0) as u128,
                        exp: ua.exp - sa as i32 - (ub.exp - sb as i32) - 65,
                    },
                )
            }
        }
    }

    /// `FPSqrt`.
    pub fn sqrt(&mut self, fmt: Fmt, a: u64) -> u64 {
        let ua = self.unpack(fmt, a);
        if ua.is_nan() {
            return self.process_nan(fmt, &ua, a);
        }
        match ua.kind {
            Kind::Zero => Self::zero(fmt, ua.sign),
            _ if ua.sign => self.invalid(fmt),
            Kind::Inf => Self::inf(fmt, false),
            _ => {
                // mant * 2^exp with an even exponent and the mantissa near
                // the top of 128 bits, so the root has 60+ bits.
                let mut m = ua.mant as u128;
                let mut e = ua.exp;
                let s = m.leading_zeros() - 2;
                m <<= s;
                e -= s as i32;
                if e & 1 != 0 {
                    m >>= 1;
                    e += 1;
                }
                let (root, exact) = isqrt(m);
                self.round_mode(
                    fmt,
                    Real {
                        sign: false,
                        mant: (root << 1) | (!exact) as u128,
                        exp: e / 2 - 1,
                    },
                )
            }
        }
    }

    /// `FPMax` / `FPMin`, and with `num` the `FPMaxNum` / `FPMinNum` forms
    /// where a single quiet NaN loses to a number.
    pub fn max_min(&mut self, fmt: Fmt, a: u64, b: u64, max: bool, num: bool) -> u64 {
        let (mut a, mut b) = (a, b);
        if num {
            // A single quiet NaN becomes -Inf for max, +Inf for min.
            let qnan = |v: u64| {
                (v >> fmt.f()) & fmt.exp_mask() == fmt.exp_mask() && (v >> (fmt.f() - 1)) & 1 != 0
            };
            if qnan(a) && !qnan(b) {
                a = Self::inf(fmt, max);
            } else if !qnan(a) && qnan(b) {
                b = Self::inf(fmt, max);
            }
        }
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if let Some(n) = self.process_nans(fmt, &[(ua, a), (ub, b)]) {
            return n;
        }
        let gt = Self::cmp_values(&ua, &ub) == std::cmp::Ordering::Greater;
        let pick_a = gt == max;
        let u = if pick_a { ua } else { ub };
        match u.kind {
            Kind::Inf => Self::inf(fmt, u.sign),
            Kind::Zero => {
                let sign = if max {
                    ua.sign && ub.sign
                } else {
                    ua.sign || ub.sign
                };
                // A zero picked against a nonzero means the other was the
                // larger/smaller one; only two zeros reach here.
                if ua.kind == Kind::Zero && ub.kind == Kind::Zero {
                    Self::zero(fmt, sign)
                } else {
                    Self::zero(fmt, u.sign)
                }
            }
            _ => self.round_mode(fmt, Self::real(&u)),
        }
    }

    /// Compare two non-NaN unpacked values.
    fn cmp_values(a: &Unpacked, b: &Unpacked) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let key = |u: &Unpacked| -> (i32, i64, u128) {
            // (class, exponent of msb, mantissa) for magnitude ordering.
            match u.kind {
                Kind::Zero => (0, 0, 0),
                Kind::Inf => (2, 0, 0),
                _ => {
                    let msb = 63 - u.mant.leading_zeros() as i64;
                    (1, u.exp as i64 + msb, (u.mant as u128) << (127 - msb))
                }
            }
        };
        let mag = key(a).cmp(&key(b));
        let (za, zb) = (a.kind == Kind::Zero, b.kind == Kind::Zero);
        match (a.sign && !za, b.sign && !zb) {
            (false, false) => mag,
            (true, true) => mag.reverse(),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }

    /// `FPCompare`: the `NZCV` result. `signal` is `FCMPE`.
    pub fn compare(&mut self, fmt: Fmt, a: u64, b: u64, signal: bool) -> u32 {
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if ua.is_nan() || ub.is_nan() {
            if signal || ua.kind == Kind::SNaN || ub.kind == Kind::SNaN {
                self.flags |= IOC;
            }
            return 0b0011 << 28;
        }
        match Self::cmp_values(&ua, &ub) {
            std::cmp::Ordering::Equal => 0b0110 << 28,
            std::cmp::Ordering::Less => 0b1000 << 28,
            std::cmp::Ordering::Greater => 0b0010 << 28,
        }
    }

    /// `FPCompareEQ` / `GE` / `GT` for the vector compares.
    pub fn compare_op(&mut self, fmt: Fmt, a: u64, b: u64, op: CmpOp) -> bool {
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if ua.is_nan() || ub.is_nan() {
            if op != CmpOp::Eq || ua.kind == Kind::SNaN || ub.kind == Kind::SNaN {
                self.flags |= IOC;
            }
            return false;
        }
        let o = Self::cmp_values(&ua, &ub);
        match op {
            CmpOp::Eq => o == std::cmp::Ordering::Equal,
            CmpOp::Ge => o != std::cmp::Ordering::Less,
            CmpOp::Gt => o == std::cmp::Ordering::Greater,
        }
    }

    /// `FPRoundInt`. `exact` is `FRINTX`, which reports inexact.
    pub fn round_int(&mut self, fmt: Fmt, a: u64, rounding: Rounding, exact: bool) -> u64 {
        let u = self.unpack(fmt, a);
        match u.kind {
            Kind::QNaN | Kind::SNaN => self.process_nan(fmt, &u, a),
            Kind::Inf => Self::inf(fmt, u.sign),
            Kind::Zero => Self::zero(fmt, u.sign),
            // No fraction bits left: already an integer (and possibly far
            // beyond 128 bits).
            Kind::Num if u.exp >= 0 => a,
            Kind::Num => {
                let (q, rest) = Self::split_int(&u);
                let q = q + Self::round_up(q, rest, u.sign, rounding) as u128;
                if exact && rest != Rest::Exact {
                    self.flags |= IXC;
                }
                if q == 0 {
                    return Self::zero(fmt, u.sign);
                }
                // An integer this came from is representable: exact.
                self.round(
                    fmt,
                    Real {
                        sign: u.sign,
                        mant: q,
                        exp: 0,
                    },
                    Rounding::Zero,
                )
            }
        }
    }

    /// The integer part of `|u|` and how the fraction compares with 1/2.
    /// Integer parts too large for 128 bits never occur where this is used
    /// with a result (the callers saturate first).
    fn split_int(u: &Unpacked) -> (u128, Rest) {
        if u.exp >= 0 {
            if u.exp >= 64 {
                return (u128::MAX, Rest::Exact);
            }
            ((u.mant as u128) << u.exp, Rest::Exact)
        } else {
            shr_rest(u.mant as u128, (-u.exp).min(200) as u32)
        }
    }

    /// Whether rounding the magnitude `q` + `rest` of a value of `sign` in
    /// `rounding` goes up (away from zero).
    fn round_up(q: u128, rest: Rest, sign: bool, rounding: Rounding) -> bool {
        match rounding {
            Rounding::TieEven => rest == Rest::Above || (rest == Rest::Half && q & 1 != 0),
            Rounding::TieAway => rest == Rest::Above || rest == Rest::Half,
            Rounding::PosInf => rest != Rest::Exact && !sign,
            Rounding::NegInf => rest != Rest::Exact && sign,
            Rounding::Zero | Rounding::Odd => false,
        }
    }

    /// `FPToFixed`: to a `bits`-wide signed or unsigned integer with `fbits`
    /// fraction bits, saturating. The result is sign-extended (or zero-
    /// extended) to 64 bits.
    pub fn to_fixed(
        &mut self,
        fmt: Fmt,
        a: u64,
        fbits: u32,
        unsigned: bool,
        bits: u32,
        rounding: Rounding,
    ) -> u64 {
        let u = self.unpack(fmt, a);
        let (max, min): (i128, i128) = if unsigned {
            ((1i128 << bits) - 1, 0)
        } else {
            ((1i128 << (bits - 1)) - 1, -(1i128 << (bits - 1)))
        };
        let clamp = |v: i128| (v.clamp(min, max) as u64) & (u64::MAX >> (64 - bits));
        let (value, rest) = match u.kind {
            Kind::QNaN | Kind::SNaN => {
                self.flags |= IOC;
                return 0;
            }
            Kind::Zero => (0i128, Rest::Exact),
            Kind::Inf => {
                self.flags |= IOC;
                return clamp(if u.sign { i128::MIN } else { i128::MAX });
            }
            Kind::Num => {
                let scaled = Unpacked {
                    exp: u.exp + fbits as i32,
                    ..u
                };
                let (q, rest) = Self::split_int(&scaled);
                if q > (1u128 << 100) {
                    self.flags |= IOC;
                    return clamp(if u.sign { i128::MIN } else { i128::MAX });
                }
                let q = q + Self::round_up(q, rest, u.sign, rounding) as u128;
                (if u.sign { -(q as i128) } else { q as i128 }, rest)
            }
        };
        if value > max || value < min {
            self.flags |= IOC;
        } else if rest != Rest::Exact {
            self.flags |= IXC;
        }
        clamp(value)
    }

    /// `FixedToFP`: a signed or unsigned integer with `fbits` fraction bits.
    pub fn from_fixed(&mut self, fmt: Fmt, v: u64, fbits: u32, unsigned: bool, bits: u32) -> u64 {
        let v = v & (u64::MAX >> (64 - bits));
        let (sign, mag) = if !unsigned && (v >> (bits - 1)) & 1 != 0 {
            (true, ((!v).wrapping_add(1)) & (u64::MAX >> (64 - bits)))
        } else {
            (false, v)
        };
        if mag == 0 {
            return Self::zero(fmt, false);
        }
        self.round_mode(
            fmt,
            Real {
                sign,
                mant: mag as u128,
                exp: -(fbits as i32),
            },
        )
    }

    /// `FPConvert` between formats. `rounding` is the instruction's (FPCR
    /// for `FCVT`, round-to-odd for `FCVTXN`).
    pub fn convert(&mut self, from: Fmt, to: Fmt, a: u64, rounding: Rounding) -> u64 {
        let ahp = self.ahp();
        let u = self.unpack_base(from, a, ahp);
        let alt_hp = to == Fmt::H && ahp;
        match u.kind {
            Kind::QNaN | Kind::SNaN => {
                if u.kind == Kind::SNaN || alt_hp {
                    self.flags |= IOC;
                }
                if alt_hp {
                    Self::zero(to, u.sign)
                } else if self.dn() {
                    Self::default_nan(to)
                } else {
                    // Keep the payload below the quiet bit, left-aligned.
                    let payload = a & ((1 << (from.f() - 1)) - 1);
                    let aligned = (payload as u128) << (64 - (from.f() - 1));
                    let frac = (aligned >> (64 - (to.f() - 1))) as u64;
                    Self::zero(to, u.sign) | (to.exp_mask() << to.f()) | (1 << (to.f() - 1)) | frac
                }
            }
            Kind::Inf => {
                if alt_hp {
                    self.flags |= IOC;
                    Self::zero(to, u.sign) | ((1 << (to.bits() - 1)) - 1)
                } else {
                    Self::inf(to, u.sign)
                }
            }
            Kind::Zero => Self::zero(to, u.sign),
            Kind::Num => self.round(to, Self::real(&u), rounding),
        }
    }

    /// `FPRecipEstimate` (`FRECPE`).
    pub fn recip_estimate(&mut self, fmt: Fmt, a: u64) -> u64 {
        let u = self.unpack(fmt, a);
        match u.kind {
            Kind::QNaN | Kind::SNaN => return self.process_nan(fmt, &u, a),
            Kind::Inf => return Self::zero(fmt, u.sign),
            Kind::Zero => {
                self.flags |= DZC;
                return Self::inf(fmt, u.sign);
            }
            Kind::Num => {}
        }
        let msb = u.exp + 63 - u.mant.leading_zeros() as i32;
        let tiny = match fmt {
            Fmt::H => -16,
            Fmt::S => -128,
            Fmt::D => -1024,
        };
        if msb < tiny {
            let to_inf = match self.rounding() {
                Rounding::PosInf => !u.sign,
                Rounding::NegInf => u.sign,
                Rounding::Zero => false,
                _ => true,
            };
            self.flags |= OFC | IXC;
            return if to_inf {
                Self::inf(fmt, u.sign)
            } else {
                Self::max_normal(fmt, u.sign)
            };
        }
        let huge = match fmt {
            Fmt::H => 14,
            Fmt::S => 126,
            Fmt::D => 1022,
        };
        if self.fz() && fmt != Fmt::H && msb >= huge {
            self.flags |= UFC;
            return Self::zero(fmt, u.sign);
        }
        let (mut fraction, mut exp) = Self::fraction52(fmt, a);
        if exp == 0 {
            if fraction >> 51 & 1 == 0 {
                exp = -1;
                fraction = (fraction << 2) & ((1 << 52) - 1);
            } else {
                fraction = (fraction << 1) & ((1 << 52) - 1);
            }
        }
        let scaled = (1 << 8) | (fraction >> 44) as u32;
        let mut result_exp = match fmt {
            Fmt::H => 29,
            Fmt::S => 253,
            Fmt::D => 2045,
        } - exp;
        let estimate = recip_estimate_int(scaled);
        let mut fraction = ((estimate & 0xFF) as u64) << 44;
        if result_exp == 0 {
            fraction = (1 << 51) | (fraction >> 1);
        } else if result_exp == -1 {
            fraction = (1 << 50) | (fraction >> 2);
            result_exp = 0;
        }
        Self::zero(fmt, u.sign)
            | ((result_exp as u64 & fmt.exp_mask()) << fmt.f())
            | (fraction >> (52 - fmt.f()))
    }

    /// The fraction left-aligned in 52 bits and the biased exponent field.
    fn fraction52(fmt: Fmt, a: u64) -> (u64, i32) {
        (
            (a & fmt.frac_mask()) << (52 - fmt.f()),
            ((a >> fmt.f()) & fmt.exp_mask()) as i32,
        )
    }

    /// `FPRSqrtEstimate` (`FRSQRTE`).
    pub fn rsqrt_estimate(&mut self, fmt: Fmt, a: u64) -> u64 {
        let u = self.unpack(fmt, a);
        match u.kind {
            Kind::QNaN | Kind::SNaN => return self.process_nan(fmt, &u, a),
            Kind::Zero => {
                self.flags |= DZC;
                return Self::inf(fmt, u.sign);
            }
            _ if u.sign => return self.invalid(fmt),
            Kind::Inf => return Self::zero(fmt, false),
            Kind::Num => {}
        }
        let (mut fraction, mut exp) = Self::fraction52(fmt, a);
        if exp == 0 {
            while fraction >> 51 & 1 == 0 {
                fraction <<= 1;
                exp -= 1;
            }
            fraction = (fraction << 1) & ((1 << 52) - 1);
        }
        let scaled = if exp & 1 == 0 {
            (1 << 8) | (fraction >> 44) as u32
        } else {
            (1 << 7) | (fraction >> 45) as u32
        };
        let result_exp = (match fmt {
            Fmt::H => 44,
            Fmt::S => 380,
            Fmt::D => 3068,
        } - exp)
            .div_euclid(2);
        let estimate = rsqrt_estimate_int(scaled);
        ((result_exp as u64 & fmt.exp_mask()) << fmt.f())
            | (((estimate & 0xFF) as u64) << (fmt.f() - 8))
    }

    /// `FPRecipStepFused` (`FRECPS`, `2 - a*b`) and `FPRSqrtStepFused`
    /// (`FRSQRTS`, `(3 - a*b) / 2`).
    pub fn step_fused(&mut self, fmt: Fmt, a: u64, b: u64, rsqrt: bool) -> u64 {
        let a = a ^ (1 << (fmt.bits() - 1));
        let ua = self.unpack(fmt, a);
        let ub = self.unpack(fmt, b);
        if let Some(n) = self.process_nans(fmt, &[(ua, a), (ub, b)]) {
            return n;
        }
        let inf_times_zero = matches!(
            (ua.kind, ub.kind),
            (Kind::Inf, Kind::Zero) | (Kind::Zero, Kind::Inf)
        );
        if inf_times_zero {
            return if rsqrt {
                Self::one_point_five(fmt, false)
            } else {
                Self::two(fmt, false)
            };
        }
        let sign_p = ua.sign ^ ub.sign;
        if ua.kind == Kind::Inf || ub.kind == Kind::Inf {
            return Self::inf(fmt, sign_p);
        }
        let constant = Real {
            sign: false,
            mant: if rsqrt { 3 } else { 1 },
            exp: if rsqrt { -1 } else { 1 },
        };
        let sum = if ua.kind == Kind::Zero || ub.kind == Kind::Zero {
            Some(constant)
        } else {
            let p = Real {
                sign: sign_p,
                mant: ua.mant as u128 * ub.mant as u128,
                exp: ua.exp + ub.exp - rsqrt as i32,
            };
            Self::add_real(constant, p)
        };
        match sum {
            Some(r) => self.round_mode(fmt, r),
            None => self.exact_zero(fmt),
        }
    }

    /// `FPRecpX` (`FRECPX`).
    pub fn recpx(&mut self, fmt: Fmt, a: u64) -> u64 {
        let u = self.unpack(fmt, a);
        if u.is_nan() {
            return self.process_nan(fmt, &u, a);
        }
        let exp = (a >> fmt.f()) & fmt.exp_mask();
        let e = if exp == 0 {
            fmt.exp_mask() - 1
        } else {
            !exp & fmt.exp_mask()
        };
        Self::zero(fmt, u.sign) | (e << fmt.f())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ge,
    Gt,
}

/// `RecipEstimate`: `a` in 256..512 (0.5 <= x < 1.0 in 1/512 steps) to a
/// result in 256..512 (1.0 <= r < 2.0).
pub fn recip_estimate_int(a: u32) -> u32 {
    let a = a * 2 + 1;
    let b = (1 << 19) / a;
    b.div_ceil(2)
}

/// `RecipSqrtEstimate`: `a` in 128..512 (0.25 <= x < 1.0).
pub fn rsqrt_estimate_int(a: u32) -> u32 {
    let a = if a < 256 {
        a * 2 + 1
    } else {
        ((a >> 1) << 1) * 2 + 2
    } as u64;
    let mut b = 512u64;
    while a * (b + 1) * (b + 1) < (1 << 28) {
        b += 1;
    }
    b.div_ceil(2) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn operand(seed: &mut u64) -> f64 {
        let v = rng(seed);
        match v % 8 {
            0 => f64::from_bits(v & 0x800F_FFFF_FFFF_FFFF), // denormal
            1 => (v % 1000) as f64 - 500.0,
            _ => f64::from_bits(v),
        }
    }

    /// Round-to-nearest results agree with the host for every operation
    /// the host can compute, NaNs aside (their payload rules differ).
    #[test]
    fn matches_host_in_round_to_nearest() {
        let mut s = 1;
        let mut fp = Fp::new(0);
        for _ in 0..200_000 {
            let (a, b, c) = (operand(&mut s), operand(&mut s), operand(&mut s));
            let (ab, bb, cb) = (a.to_bits(), b.to_bits(), c.to_bits());
            let checks = [
                (fp.add(Fmt::D, ab, bb, false), a + b),
                (fp.add(Fmt::D, ab, bb, true), a - b),
                (fp.mul(Fmt::D, ab, bb, false), a * b),
                (fp.div(Fmt::D, ab, bb), a / b),
                (fp.sqrt(Fmt::D, ab), a.sqrt()),
                (fp.mul_add(Fmt::D, cb, ab, bb), a.mul_add(b, c)),
                (
                    fp.convert(Fmt::D, Fmt::S, ab, Rounding::TieEven),
                    (a as f32) as f64,
                ),
            ];
            for (i, (got, want)) in checks.into_iter().enumerate() {
                let got = if i == 6 {
                    f32::from_bits(got as u32) as f64
                } else {
                    f64::from_bits(got)
                };
                if want.is_nan() {
                    assert!(got.is_nan(), "op {i} {a:e} {b:e} {c:e}: {got:e}");
                } else {
                    assert_eq!(got.to_bits(), want.to_bits(), "op {i} {a:e} {b:e} {c:e}");
                }
            }
        }
    }

    #[test]
    fn single_precision_matches_host() {
        let mut s = 7;
        let mut fp = Fp::new(0);
        for _ in 0..200_000 {
            let a = f32::from_bits(rng(&mut s) as u32);
            let b = f32::from_bits(rng(&mut s) as u32);
            let c = f32::from_bits(rng(&mut s) as u32);
            let (ab, bb, cb) = (a.to_bits() as u64, b.to_bits() as u64, c.to_bits() as u64);
            for (got, want) in [
                (fp.add(Fmt::S, ab, bb, false), a + b),
                (fp.mul(Fmt::S, ab, bb, false), a * b),
                (fp.div(Fmt::S, ab, bb), a / b),
                (fp.sqrt(Fmt::S, ab), a.sqrt()),
                (fp.mul_add(Fmt::S, cb, ab, bb), a.mul_add(b, c)),
            ] {
                let got = f32::from_bits(got as u32);
                if want.is_nan() {
                    assert!(got.is_nan());
                } else {
                    assert_eq!(got.to_bits(), want.to_bits(), "{a:e} {b:e} {c:e}");
                }
            }
        }
    }

    #[test]
    fn flags_and_nans_follow_the_pseudocode() {
        let mut fp = Fp::new(0);
        // 1/3 is inexact; 1/0 divides by zero.
        fp.div(Fmt::S, 1f32.to_bits() as u64, 3f32.to_bits() as u64);
        assert_eq!(fp.flags, IXC);
        fp.flags = 0;
        fp.div(Fmt::S, 1f32.to_bits() as u64, 0);
        assert_eq!(fp.flags, DZC);
        // An SNaN operand is quieted and raises invalid; it wins over a QNaN
        // in the first operand.
        fp.flags = 0;
        let r = fp.add(Fmt::S, 0x7FC0_0001, 0x7F80_0002, false);
        assert_eq!((r, fp.flags), (0x7FC0_0002, IOC));
        // With FPCR.DN the default NaN comes out instead.
        let mut dn = Fp::new(1 << 25);
        assert_eq!(dn.add(Fmt::S, 0x7FC0_0001, 0, false), 0x7FC0_0000);
    }

    #[test]
    fn estimates_match_known_values() {
        // FRECPE(1.0) = 0.998046875, FRSQRTE(1.0) = 0.998046875 (ARM ARM
        // tables); FRECPE(2.0) = 0.4990234375.
        let mut fp = Fp::new(0);
        assert_eq!(
            fp.recip_estimate(Fmt::S, 1f32.to_bits() as u64),
            0x3F7F_8000
        );
        assert_eq!(
            fp.recip_estimate(Fmt::S, 2f32.to_bits() as u64),
            0x3EFF_8000
        );
        assert_eq!(
            fp.rsqrt_estimate(Fmt::S, 1f32.to_bits() as u64),
            0x3F7F_8000
        );
    }
}
