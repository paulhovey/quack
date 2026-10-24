use calc::{eval, parse, Error, Expr};

fn num(n: f64) -> Box<Expr> {
    Box::new(Expr::Num(n))
}

#[test]
fn power_binds_tighter_than_minus_and_groups_right() {
    assert_eq!(eval("2 * 3 ^ 2"), Ok(18.0));
    assert_eq!(eval("(-2) ^ 2"), Ok(4.0));
    assert_eq!(eval("-(2) ^ 2"), Ok(-4.0));
    assert_eq!(eval("2 ^ -1"), Ok(0.5));
    assert_eq!(eval("-2 ^ -2"), Ok(-0.25));
    assert_eq!(eval("2 ^ 2 ^ 3"), Ok(256.0));
    assert_eq!(
        parse("-2^2"),
        Ok(Expr::Neg(Box::new(Expr::Bin('^', num(2.0), num(2.0)))))
    );
}

#[test]
fn the_rest_still_groups_left() {
    assert_eq!(eval("1 - 2 - 3"), Ok(-4.0));
    assert_eq!(eval("8 / 2 / 2"), Ok(2.0));
    assert_eq!(eval("-2 * 3"), Ok(-6.0));
    assert_eq!(eval("2 * -3 + 1"), Ok(-5.0));
    assert_eq!(eval("--3"), Ok(3.0));
}

#[test]
fn remainders() {
    assert_eq!(eval("-7 % 3"), Ok(-1.0));
    assert_eq!(eval("7 % -3"), Ok(1.0));
    assert_eq!(eval("2 * 7 % 4"), Ok(2.0));
    assert_eq!(eval("20 % 7 % 4"), Ok(2.0));
    assert_eq!(eval("1 + 7 % 4"), Ok(4.0));
    assert_eq!(eval("5.5 % 2"), Ok(1.5));
    assert_eq!(eval("7 % 0"), Err(Error::DivisionByZero));
    assert_eq!(eval("7 % (2 - 2)"), Err(Error::DivisionByZero));
}

#[test]
fn factorials_bind_tightest() {
    assert_eq!(eval("0!"), Ok(1.0));
    assert_eq!(eval("-3!"), Ok(-6.0));
    assert_eq!(eval("2 ^ 3!"), Ok(64.0));
    assert_eq!(eval("3!!"), Ok(720.0));
    assert_eq!(eval("(1 + 2)! * 2"), Ok(12.0));
    assert_eq!(eval("2 * 3! - 1"), Ok(11.0));
    assert_eq!(eval("3! ^ 2"), Ok(36.0));
    assert_eq!(eval("170!").map(f64::is_finite), Ok(true));
}

#[test]
fn factorials_outside_their_domain() {
    assert_eq!(eval("2.5!"), Err(Error::Domain));
    assert_eq!(eval("(-1)!"), Err(Error::Domain));
    assert_eq!(eval("171!"), Err(Error::Domain));
    assert_eq!(eval("(1 / 0)!"), Err(Error::DivisionByZero));
    assert_eq!(eval("!3"), Err(Error::UnexpectedToken("Op('!')".into())));
}
