//! A calculator for arithmetic expressions: `+ - * / % ^`, unary minus, postfix `!`
//! (factorial) and parentheses.
//!
//! Precedence, loosest first: `+ -`, then `* / %`, then unary minus, then `^`, then `!`. So
//! `-2^2` is `-(2^2)`, `-3!` is `-(3!)` and `2^3!` is `2^(3!)`. `^` groups to the right
//! (`2^3^2` is `2^(3^2)`), `!` can repeat (`3!!` is `(3!)!`), and the others group to the
//! left.
//!
//! `a % b` is the remainder with the sign of `a`, like Rust's `%`; `a % 0` is
//! `Error::DivisionByZero`, as `a / 0` is. `n!` is defined for whole numbers from 0 to 170;
//! any other `n` is `Error::Domain`.

mod lexer;
mod parser;

pub use parser::{parse, Expr};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    UnexpectedChar(char),
    UnexpectedEnd,
    UnexpectedToken(String),
    DivisionByZero,
    Domain,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnexpectedChar(c) => write!(f, "unexpected character {c:?}"),
            Error::UnexpectedEnd => write!(f, "the expression ends too soon"),
            Error::UnexpectedToken(t) => write!(f, "unexpected {t}"),
            Error::DivisionByZero => write!(f, "division by zero"),
            Error::Domain => write!(f, "factorial outside the whole numbers 0 to 170"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse and evaluate `src`.
pub fn eval(src: &str) -> Result<f64, Error> {
    parse(src)?.value()
}
