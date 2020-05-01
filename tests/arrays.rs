#![cfg(not(feature = "no_index"))]
use rhai::{Array, Engine, EvalAltResult, RegisterFn, INT};

#[test]
fn test_arrays() -> Result<(), Box<EvalAltResult>> {
    let engine = Engine::new();

    println!("Test {}", line!());
    assert_eq!(engine.eval::<INT>("let x = [1, 2, 3]; x[1]")?, 2);

    println!("Test {}", line!());
    assert_eq!(engine.eval::<INT>("let y = [1, 2, 3]; y[1] = 5; y[1]")?, 5);

    println!("Test {}", line!());
    assert_eq!(
        engine.eval::<char>(r#"let y = [1, [ 42, 88, "93" ], 3]; y[1][2][1]"#)?,
        '3'
    );

    println!("Test {}", line!());
    assert!(engine.eval::<bool>("let y = [1, 2, 3]; 2 in y")?);


    println!("Test {}", line!());
    #[cfg(not(feature = "no_object"))]
    assert_eq!(
        engine.eval::<INT>(
            r"
                let x = [2, 9];
                x.insert(-1, 1);
                x.insert(999, 3);

                let r = x.remove(2);

                let y = [4, 5];
                x.append(y);

                x.len() + r
           "
        )?,
        14
    );


    println!("Test {}", line!());
    assert_eq!(
        engine.eval::<INT>(
            r"
                let x = [1, 2, 3];
                x += [4, 5];
                len(x)
           "
        )?,
        5
    );


    println!("Test {}", line!());
    assert_eq!(
        engine
            .eval::<Array>(
                r"
                    let x = [1, 2, 3];
                    let y = [4, 5];
                    x + y
           "
            )?
            .len(),
        5
    );

    Ok(())
}

#[test]
#[cfg(not(feature = "no_object"))]
fn test_array_with_structs() -> Result<(), Box<EvalAltResult>> {
    #[derive(Clone)]
    struct TestStruct {
        x: INT,
    }

    impl TestStruct {
        fn update(&mut self) {
            self.x += 1000;
        }

        fn get_x(&mut self) -> INT {
            self.x
        }

        fn set_x(&mut self, new_x: INT) {
            self.x = new_x;
        }

        fn new() -> Self {
            TestStruct { x: 1 }
        }
    }

    let mut engine = Engine::new();

    engine.register_type::<TestStruct>();

    engine.register_get_set("x", TestStruct::get_x, TestStruct::set_x);
    engine.register_fn("update", TestStruct::update);
    engine.register_fn("new_ts", TestStruct::new);

    assert_eq!(engine.eval::<INT>("let a = [new_ts()]; a[0].x")?, 1);

    assert_eq!(
        engine.eval::<INT>(
            r"
                let a = [new_ts()];
                a[0].x = 100;
                a[0].update();
                a[0].x
            "
        )?,
        1100
    );

    Ok(())
}
