use freql::CalculatorSession;
use tempfile::tempdir;

#[test]
fn quantity_comparisons_return_booleans_and_preserve_the_session() {
    let directory = tempdir().unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator.execute("let j = 1 Mile").unwrap();
    calculator.execute("let m = Mile").unwrap();
    for (input, expected) in [
        ("m", "1 Mile"),
        ("m == j", "true"),
        ("m != j", "false"),
        ("m > 1 Meter", "true"),
        ("m < j", "false"),
        ("m >= j", "true"),
        ("m <= j", "true"),
        ("1 Hour == 60 Minute", "true"),
        ("not (m == j)", "false"),
        ("m == j and 1 Meter < 1 Mile", "true"),
        ("m != j or false", "false"),
        ("true", "true"),
        ("false", "false"),
        ("true == false", "false"),
        ("true != false", "true"),
        ("(m == j) == true", "true"),
        ("j", "1 Mile"),
    ] {
        assert_eq!(
            calculator.execute(input).unwrap().as_deref(),
            Some(expected),
            "{input}"
        );
    }
    for input in [
        "m == 1 Second",
        "true as ~",
        "(m == j) as Hour",
        "unknown == m",
    ] {
        assert!(calculator.execute(input).is_err(), "{input}");
        assert_eq!(
            calculator.execute("m == j").unwrap().as_deref(),
            Some("true")
        );
    }
}

#[test]
fn boolean_bindings_strings_and_helper_outputs_share_the_analyzers_evaluator() {
    let directory = tempdir().unwrap();
    std::fs::write(
        directory.path().join("helpers.req"),
        "fn large(value: Length) -> (result: Bool) { result = value > 1 Meter }",
    )
    .unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator
        .execute("import \"helpers.req\" (large)")
        .unwrap();
    calculator
        .execute("let valid = 1 Hour == 60 Minute")
        .unwrap();
    calculator
        .execute("let result = large(value: 2 Meter)")
        .unwrap();
    calculator.execute("let label = \"parcel\"").unwrap();
    for (input, expected) in [
        ("valid", "true"),
        ("not valid", "false"),
        ("label == \"parcel\"", "true"),
        ("label != \"parcel\"", "false"),
        ("result.result", "true"),
    ] {
        assert_eq!(
            calculator.execute(input).unwrap().as_deref(),
            Some(expected),
            "{input}"
        );
    }
}
