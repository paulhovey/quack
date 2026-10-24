use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token {
    Num(f64),
    Op(char),
    LParen,
    RParen,
}

pub fn tokens(src: &str) -> Result<Vec<Token>, Error> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let n = text.parse().map_err(|_| Error::UnexpectedChar(c))?;
                out.push(Token::Num(n));
            }
            '+' | '-' | '*' | '/' | '%' | '^' | '!' => {
                out.push(Token::Op(c));
                i += 1;
            }
            '(' => {
                out.push(Token::LParen);
                i += 1;
            }
            ')' => {
                out.push(Token::RParen);
                i += 1;
            }
            other => return Err(Error::UnexpectedChar(other)),
        }
    }
    Ok(out)
}
