//! A Pratt parser: each operator has a left and a right binding power.

use crate::lexer::{tokens, Token};
use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn value(&self) -> Result<f64, Error> {
        Ok(match self {
            Expr::Num(n) => *n,
            Expr::Neg(e) => -e.value()?,
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.value()?, b.value()?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' if b == 0.0 => return Err(Error::DivisionByZero),
                    '/' => a / b,
                    '^' => a.powf(b),
                    _ => unreachable!("the lexer only makes these operators"),
                }
            }
        })
    }
}

/// How tightly an infix operator holds its left and right operands.
fn infix(op: char) -> (u8, u8) {
    match op {
        '+' | '-' => (1, 2),
        '*' | '/' => (3, 4),
        '^' => (7, 8),
        _ => unreachable!("the lexer only makes these operators"),
    }
}

/// How tightly unary minus holds its operand.
const PREFIX: u8 = 9;

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).copied();
        self.pos += 1;
        t
    }

    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).copied()
    }

    fn expr(&mut self, min: u8) -> Result<Expr, Error> {
        let mut lhs = match self.next() {
            Some(Token::Num(n)) => Expr::Num(n),
            Some(Token::Op('-')) => Expr::Neg(Box::new(self.expr(PREFIX)?)),
            Some(Token::LParen) => {
                let inner = self.expr(0)?;
                match self.next() {
                    Some(Token::RParen) => inner,
                    Some(t) => return Err(Error::UnexpectedToken(format!("{t:?}"))),
                    None => return Err(Error::UnexpectedEnd),
                }
            }
            Some(t) => return Err(Error::UnexpectedToken(format!("{t:?}"))),
            None => return Err(Error::UnexpectedEnd),
        };
        loop {
            let op = match self.peek() {
                Some(Token::Op(op)) => op,
                Some(Token::RParen) | None => break,
                Some(t) => return Err(Error::UnexpectedToken(format!("{t:?}"))),
            };
            let (left, right) = infix(op);
            if left < min {
                break;
            }
            self.next();
            let rhs = self.expr(right)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
}

pub fn parse(src: &str) -> Result<Expr, Error> {
    let mut p = Parser {
        tokens: tokens(src)?,
        pos: 0,
    };
    let e = p.expr(0)?;
    match p.next() {
        None => Ok(e),
        Some(t) => Err(Error::UnexpectedToken(format!("{t:?}"))),
    }
}
