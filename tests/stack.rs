#![cfg(not(feature = "no_function"))]
use rhai::{Engine, EvalAltResult};

#[test]
fn test_stack_overflow() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    assert_eq!(
        engine.eval::<i64>(
            r"
                fn foo(n) { if n == 0 { 0 } else { n + foo(n-1) } }
                foo(25)
    ",
        )?,
        325
    );

    match engine.eval::<()>(
        r"
            fn foo(n) { if n == 0 { 0 } else { n + foo(n-1) } }
            foo(1000)
    ",
    ) {
        Ok(_) => panic!("should be stack overflow"),
        Err(err) => match *err {
            EvalAltResult::ErrorStackOverflow(_) => (),
            _ => panic!("should be stack overflow"),
        },
    }

    Ok(())
}
