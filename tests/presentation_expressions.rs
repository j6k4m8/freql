use freql::{CalculatorSession, Compiler, CompilerOptions, Completeness};
use tempfile::tempdir;

fn check(source: &str) -> freql::Compilation {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("main.req");
    std::fs::write(&entry, source).unwrap();
    Compiler::new(CompilerOptions::default())
        .check_path(&entry)
        .unwrap()
}

#[test]
fn nested_presentation_composes_with_arithmetic_and_retains_exact_values() {
    let directory = tempdir().unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator.execute("unit Item = Count").unwrap();
    calculator.execute("import math").unwrap();
    for (source, expected) in [
        ("((2 / 3) as ~Item) * (2 Byte / Item)", "≈1.333333 Byte"),
        ("((2 / 3) as ~Item) * (3 Byte / Item)", "2 Byte"),
        ("(1 Hour as Minute) + 30 Second", "60.5 Minute"),
        ("(1 Hour as Minute) as Second", "3600 Second"),
        ("math.ceil(1.2 Hour as Minute)", "72 Minute"),
        ("math.ceil(20.1 Celsius as Fahrenheit)", "69 Fahrenheit"),
        ("2^(3 as Scalar)", "8"),
        ("(20 Celsius as Fahrenheit) - 10 Celsius", "18 Rankine"),
    ] {
        assert_eq!(
            calculator.execute(source).unwrap().as_deref(),
            Some(expected),
            "{source}"
        );
    }
    calculator.execute("let count = 2 / 3 as ~Item").unwrap();
    assert_eq!(
        calculator
            .execute("count * (2 Byte / Item)")
            .unwrap()
            .as_deref(),
        Some("≈1.333333 Byte")
    );
    assert!(
        calculator
            .execute("(20 Celsius as Fahrenheit) * 2")
            .is_err()
    );
}

#[test]
fn real_code_supports_presentations_in_bindings_calls_arrays_and_assignments() {
    let source = r#"
unit Item = Count
fn twice(value: Time) -> (result: Time) { result = value * 2 }
process Result {
    monitor bytes: Information
    monitor duration: Time
    monitor exact: Scalar
    () => {
        let count = 2 / 3 as ~Item
        let whole: Integer = 3 as Item
        requires whole == 3
        let estimates = [count * (2 Byte / Item)]
        bytes = count * (2 Byte / Item)
        let planned = twice(value: 1 Hour as Minute)
        duration = planned.result as Second
        exact = (2 / 3 as ~) * 3
        requires exact == 2
    }
}
"#;
    let formatted = freql::format_source(source).unwrap();
    assert_eq!(freql::format_source(&formatted).unwrap(), formatted);
    let compilation = check(&formatted);
    assert_eq!(
        compilation.report.status,
        Completeness::Complete,
        "{}",
        compilation.render_human()
    );
    for (name, exact, display) in [
        ("Result.bytes", "4/3 Byte", "≈1.333333 Byte"),
        ("Result.duration", "7200 Second", "7200 Second"),
        ("Result.exact", "2", "2"),
    ] {
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == name)
            .unwrap();
        assert_eq!(metric.value, exact, "{name}");
        assert_eq!(metric.display_value(), display, "{name}");
    }
}

#[test]
fn nested_presentations_cannot_hide_invalid_values_or_change_dimensions() {
    for expression in [
        "(1 Meter as Hour) * 2",
        "(2 / 3 as ~Hour) * 2",
        "(1 Meter as MissingUnit) * 2",
        "(true as ~) * 2",
        "(missing as Minute) * 2",
    ] {
        let compilation = check(&format!(
            "process Result {{ monitor value: Time () => {{ value = {expression} }} }}"
        ));
        assert_eq!(
            compilation.report.status,
            Completeness::Invalid,
            "{expression}"
        );
    }
}

#[test]
fn conversions_resolve_namespaced_units_without_treating_them_as_value_bindings() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("units.req"), "unit Tick = Second * 2").unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator.execute("import \"units.req\" as units").unwrap();
    calculator
        .execute("let duration = 1 Minute as units.Tick")
        .unwrap();
    assert_eq!(
        calculator
            .execute("(duration + 1 units.Tick) as Second")
            .unwrap()
            .as_deref(),
        Some("62 Second")
    );
}
