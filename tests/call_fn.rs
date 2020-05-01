#![cfg(not(feature = "no_function"))]
use rhai::{Engine, EvalAltResult, Func, ParseErrorType, Scope, INT};

#[test]
fn test_fn() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    // Expect duplicated parameters error
    match engine
        .compile("fn hello(x, x) { x }")
        .expect_err("should be error")
        .error_type()
    {
        ParseErrorType::FnDuplicatedParam(f, p) if f == "hello" && p == "x" => (),
        _ => assert!(false, "wrong error"),
    }

    Ok(())
}

#[test]
fn test_call_fn() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();
    let mut scope = Scope::new();

    scope.push("foo", 42 as INT);

    let ast = engine.compile(
        r"
            fn hello(x, y) {
                x + y
            }
            fn hello(x) {
                x = x * foo;
                foo = 1;
                x
            }
            fn hello() {
                41 + foo
            }
        ",
    )?;

    let r: i64 = engine.call_fn(&mut scope, &ast, &"hello".into(), (42 as INT, 123 as INT))?;
    assert_eq!(r, 165);

    let r: i64 = engine.call_fn(&mut scope, &ast, &"hello".into(), (123 as INT,))?;
    assert_eq!(r, 5166);

    let r: i64 = engine.call_fn(&mut scope, &ast, &"hello".into(), ())?;
    assert_eq!(r, 42);

    assert_eq!(
        scope
            .get_value::<INT>(&"foo".into())
            .expect("variable foo should exist"),
        1
    );

    Ok(())
}

#[test]
fn test_anonymous_fn() -> Result<(), Box<EvalAltResult>> {
    let calc_func = Func::<(INT, INT, INT), INT>::create_from_script(
        Engine::new(),
        "fn calc(x, y, z) { (x + y) * z }",
        "calc",
    )?;

    assert_eq!(calc_func(42, 123, 9)?, 1485);

    Ok(())
}
