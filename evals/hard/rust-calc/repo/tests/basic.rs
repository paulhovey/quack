use calc::{eval, Error};

#[test]
fn arithmetic() {
    assert_eq!(eval("1 + 2 * 3"), Ok(7.0));
    assert_eq!(eval("(1 + 2) * 3"), Ok(9.0));
    assert_eq!(eval("10 - 4 - 3"), Ok(3.0));
    assert_eq!(eval("2.5 * 4"), Ok(10.0));
}

#[test]
fn powers() {
    assert_eq!(eval("2 ^ 10"), Ok(1024.0));
    assert_eq!(eval("2 ^ 3 ^ 2"), Ok(512.0));
    assert_eq!(eval("-2 ^ 2"), Ok(-4.0));
}

#[test]
fn remainders_and_factorials() {
    assert_eq!(eval("7 % 4"), Ok(3.0));
    assert_eq!(eval("4!"), Ok(24.0));
}

#[test]
fn errors() {
    assert_eq!(eval("1 / 0"), Err(Error::DivisionByZero));
    assert_eq!(eval("1 +"), Err(Error::UnexpectedEnd));
    assert_eq!(eval("1 $ 2"), Err(Error::UnexpectedChar('$')));
}
