use rhai::{Engine, EvalAltResult, INT};

#[test]
fn test_decrement() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    assert_eq!(engine.eval::<INT>("let x = 10; x -= 7; x")?, 3);

    assert!(matches!(
        *engine.eval::<String>(r#"let s = "test"; s -= "ing"; s"#).expect_err("expects error"),
        EvalAltResult::ErrorFunctionNotFound(err, _) if err.get_str() == "- (string, string)"
    ));

    Ok(())
}
