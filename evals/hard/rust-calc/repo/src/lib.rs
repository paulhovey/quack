//! A calculator for arithmetic expressions: `+ - * / ^`, unary minus and parentheses.
//!
//! Precedence, loosest first: `+ -`, then `* /`, then unary minus, then `^`. So `-2^2` is
//! `-(2^2)`. `^` groups to the right (`2^3^2` is `2^(3^2)`); the others group to the left.

mod lexer;
mod parser;

pub use parser::{parse, Expr};

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    UnexpectedChar(char),
    UnexpectedEnd,
    UnexpectedToken(String),
    DivisionByZero,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnexpectedChar(c) => write!(f, "unexpected character {c:?}"),
            Error::UnexpectedEnd => write!(f, "the expression ends too soon"),
            Error::UnexpectedToken(t) => write!(f, "unexpected {t}"),
            Error::DivisionByZero => write!(f, "division by zero"),
        }
    }
}

impl std::error::Error for Error {}

/// Parse and evaluate `src`.
pub fn eval(src: &str) -> Result<f64, Error> {
    parse(src)?.value()
}
