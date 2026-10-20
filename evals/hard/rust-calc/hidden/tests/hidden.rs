use calc::{eval, parse, Expr};

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
