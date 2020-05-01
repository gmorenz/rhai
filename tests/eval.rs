use rhai::{Engine, EvalAltResult, Scope, INT};

#[test]
fn test_eval() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    assert_eq!(
        engine.eval::<INT>(
            r#"
                eval("40 + 2")
    "#
        )?,
        42
    );

    Ok(())
}

#[test]
#[cfg(not(feature = "no_function"))]
fn test_eval_function() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();
    let mut scope = Scope::new();

    assert_eq!(
        engine.eval_with_scope::<INT>(
            &mut scope,
            r#"
                let x = 10;

                fn foo(x) { x += 12; x }

                let script = "let y = x;";      // build a script
                script +=    "y += foo(y);";
                script +=    "x + y";

                eval(script) + x + y
    "#
        )?,
        84
    );

    assert_eq!(
        scope
            .get_value::<INT, _>("x")
            .expect("variable x should exist"),
        10
    );

    assert_eq!(
        scope
            .get_value::<INT, _>("y")
            .expect("variable y should exist"),
        32
    );

    assert!(scope.contains("script"));
    assert_eq!(scope.len(), 3);

    Ok(())
}

#[test]
#[cfg(not(feature = "no_function"))]
fn test_eval_override() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    assert_eq!(
        engine.eval::<String>(
            r#"
                fn eval(x) { x }    // reflect the script back

                eval("40 + 2")
    "#
        )?,
        "40 + 2"
    );

    Ok(())
}
