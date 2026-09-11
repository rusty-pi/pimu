//! The core's error type.
//!
//! `anyhow` is hosted-only (it needs `std::error::Error`), and the parsers it
//! was used in — ELF32, the EEPROM section table, payload loading — are
//! exactly the code a bare-metal frontend needs (#32 stage 1). So they carry
//! this instead: one owned message, built the same way `anyhow` built it.
//!
//! Deliberately not a chain. `anyhow`'s context stack only ever reached the
//! user as `outer: inner` on one line, so flattening at the point of
//! `.context()` loses nothing and keeps the type a `String`. The hosted
//! frontend still speaks `anyhow`: [`Error`] implements `std::error::Error`
//! under the `std` feature, so `?` in `main.rs` converts as before.

use alloc::string::{String, ToString};
use core::fmt;

/// An error with a human-readable message.
#[derive(Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn msg(m: impl Into<String>) -> Error {
        Error(m.into())
    }

    /// Prefix the message with `ctx`, as `anyhow`'s `.context()` renders.
    pub fn context(self, ctx: impl fmt::Display) -> Error {
        Error(alloc::format!("{ctx}: {}", self.0))
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Shown as the bare message: an `Err` out of `main` is printed with `Debug`,
/// and `anyhow`'s own rendering of a leaf error is the message alone.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

impl From<&str> for Error {
    fn from(s: &str) -> Error {
        Error(s.to_string())
    }
}

impl From<String> for Error {
    fn from(s: String) -> Error {
        Error(s)
    }
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Attach context to a failure, as `anyhow::Context` does.
pub trait Context<T> {
    fn context(self, ctx: impl fmt::Display) -> Result<T>;
    fn with_context<D: fmt::Display>(self, f: impl FnOnce() -> D) -> Result<T>;
}

impl<T> Context<T> for Result<T> {
    fn context(self, ctx: impl fmt::Display) -> Result<T> {
        self.map_err(|e| e.context(ctx))
    }

    fn with_context<D: fmt::Display>(self, f: impl FnOnce() -> D) -> Result<T> {
        self.map_err(|e| e.context(f()))
    }
}

/// `Option` gets the same two methods, so `opt.context("...")?` keeps working.
impl<T> Context<T> for Option<T> {
    fn context(self, ctx: impl fmt::Display) -> Result<T> {
        self.ok_or_else(|| Error(ctx.to_string()))
    }

    fn with_context<D: fmt::Display>(self, f: impl FnOnce() -> D) -> Result<T> {
        self.ok_or_else(|| Error(f().to_string()))
    }
}

/// `anyhow::bail!`, without `anyhow`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::error::Error::msg(::alloc::format!($($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.context()` has to render the way `anyhow`'s did — the boot-check and
    /// `recon` error messages in the docs are written in that shape.
    #[test]
    fn context_reads_outer_then_inner() {
        let e: Result<()> = Err(Error::msg("not an ELF file"));
        let e = e.context("parsing ELF payload").unwrap_err();
        assert_eq!(e.message(), "parsing ELF payload: not an ELF file");
    }

    #[test]
    fn option_context_makes_the_message() {
        let e: Result<()> = None.context("no bootcode section");
        assert_eq!(e.unwrap_err().message(), "no bootcode section");
    }
}
