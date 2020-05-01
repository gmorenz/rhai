use rhai::{Engine, EvalAltResult, RegisterFn, INT};

#[test]
fn test_mismatched_op() {
    let engine = Engine::new();

    assert!(matches!(
        *engine.eval::<INT>(r#""hello, " + "world!""#).expect_err("expects error"),
        EvalAltResult::ErrorMismatchOutputType(err, _) if err == "string"
    ));
}

#[test]
#[cfg(not(feature = "no_object"))]
fn test_mismatched_op_custom_type() {
    #[derive(Clone)]
    struct TestStruct {
        x: INT,
    }

    impl TestStruct {
        fn new() -> Self {
            TestStruct { x: 1 }
        }
    }

    let mut engine = Engine::new();
    engine.register_type_with_name::<TestStruct>("TestStruct");
    engine.register_fn("new_ts", TestStruct::new);

    let r = engine
        .eval::<INT>("60 + new_ts()")
        .expect_err("expects error");

    #[cfg(feature = "only_i32")]
    assert!(matches!(
        *r,
        EvalAltResult::ErrorFunctionNotFound(err, _) if err.get_str() == "+ (i32, TestStruct)"
    ));

    #[cfg(not(feature = "only_i32"))]
    assert!(matches!(
        *r,
        EvalAltResult::ErrorFunctionNotFound(err, _) if err.get_str() == "+ (i64, TestStruct)"
    ));
}
