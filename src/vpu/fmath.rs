//! The `f32` transcendentals the VC4 FP ALU needs.
//!
//! `f32::sqrt`, `ceil`, `floor`, `log2` and `exp2` are inherent methods that
//! only exist in `std` — `core` has none of them, because they are libm calls.
//! So the `no_std` build routes them to the `libm` crate, which is the same
//! algorithms (a port of MUSL's libm) that the hosted ones end up in.
//!
//! Keeping this behind one module matters for the golden transcript: the two
//! builds must agree bit for bit on what `Flog2` produced, or the same
//! firmware prints different numbers depending on where it ran.

/// `2^n` for an integer `n` — the scale factor of the fixed-point convert ops
/// and the exponent of the 6-bit FP immediate. Exact in both builds: a power
/// of two is representable, so there is no rounding to disagree about.
pub fn exp2i(n: i32) -> f32 {
    #[cfg(feature = "std")]
    {
        2f32.powi(n)
    }
    #[cfg(not(feature = "std"))]
    {
        libm::exp2f(n as f32)
    }
}

macro_rules! unary {
    ($name:ident, $libm:ident) => {
        pub fn $name(x: f32) -> f32 {
            #[cfg(feature = "std")]
            {
                x.$name()
            }
            #[cfg(not(feature = "std"))]
            {
                libm::$libm(x)
            }
        }
    };
}

unary!(sqrt, sqrtf);
unary!(ceil, ceilf);
unary!(floor, floorf);
unary!(log2, log2f);
unary!(exp2, exp2f);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp2i_is_exact() {
        assert_eq!(exp2i(0), 1.0);
        assert_eq!(exp2i(3), 8.0);
        assert_eq!(exp2i(-2), 0.25);
    }

    #[test]
    fn the_transcendentals_agree_with_the_obvious_answers() {
        assert_eq!(sqrt(9.0), 3.0);
        assert_eq!(log2(8.0), 3.0);
        assert_eq!(exp2(3.0), 8.0);
        assert_eq!(ceil(1.25), 2.0);
        assert_eq!(floor(1.75), 1.0);
    }
}
