use freql::{CalculatorSession, Compiler, CompilerOptions, Completeness};
use tempfile::tempdir;

fn check(body: &str) -> freql::Compilation {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("main.req");
    std::fs::write(&entry, body).unwrap();
    Compiler::new(CompilerOptions::default())
        .check_path(&entry)
        .unwrap()
}

#[test]
fn calculator_let_is_mutable_and_redeclaration_warns_once() {
    let directory = tempdir().unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator.execute("let i = 1").unwrap();
    calculator.execute("i = 2 # assignment").unwrap();
    assert_eq!(calculator.execute("i").unwrap().as_deref(), Some("2"));
    calculator.execute("let i = i + 1").unwrap();
    assert_eq!(calculator.warnings().len(), 1);
    assert!(calculator.warnings()[0].contains("W0601"));
    assert_eq!(calculator.execute("i").unwrap().as_deref(), Some("3"));
    assert!(calculator.warnings().is_empty());
    calculator.execute("i += 2").unwrap();
    assert!(calculator.warnings().is_empty());
    calculator.execute("let i = 1 Meter").unwrap();
    assert_eq!(calculator.warnings().len(), 1);
    assert_eq!(calculator.execute("i").unwrap().as_deref(), Some("1 Meter"));
}

#[test]
fn calculator_constants_reject_writes_and_redeclarations_without_corrupting_scope() {
    let directory = tempdir().unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    calculator.execute("const i = 1").unwrap();
    for input in [
        "i = 2",
        "i += 1",
        "i -= 1",
        "i *= 2",
        "i /= 2",
        "i ^= 2",
        "const i = 2",
        "let i = 2",
    ] {
        assert!(calculator.execute(input).is_err(), "{input}");
        assert_eq!(calculator.execute("i").unwrap().as_deref(), Some("1"));
    }
    calculator.execute("let other = 2").unwrap();
    assert!(calculator.execute("const other = 3").is_err());
    assert!(calculator.execute("unknown += 1").is_err());
    assert!(calculator.execute("i +=").is_err());
}

#[test]
fn compound_arithmetic_is_exact_and_checks_dimensions() {
    let directory = tempdir().unwrap();
    let mut calculator = CalculatorSession::new(directory.path());
    for input in [
        "let n = 8",
        "n += 2",
        "n -= 1",
        "n *= 2",
        "n /= 3",
        "n ^= 2",
    ] {
        calculator.execute(input).unwrap();
    }
    assert_eq!(calculator.execute("n").unwrap().as_deref(), Some("36"));
    calculator.execute("let distance = 1 Meter").unwrap();
    calculator.execute("distance += 50 Centimeter").unwrap();
    assert_eq!(
        calculator.execute("distance").unwrap().as_deref(),
        Some("1.5 Meter")
    );
    for input in ["distance += 1 Second", "distance *= 1 Meter", "n = 1 Meter"] {
        assert!(calculator.execute(input).is_err(), "{input}");
    }
}

#[test]
fn declarations_and_compound_assignments_work_in_source_and_functions() {
    let source = r#"
fn calculate() -> (result: Scalar) {
    let count = 1
    count = 2
    let count = count + 1
    const factor = 2
    count *= factor
    count -= 2
    count /= 2
    count ^= 3
    result = count
}
process Result {
    monitor value: Scalar
    () => {
        let count = 1
        count += 2
        let count = count + 1
        const factor = 2
        count *= factor
        let planned = calculate()
        value = count + planned.result
    }
}
"#;
    let formatted = freql::format_source(source).unwrap();
    assert!(formatted.contains("count *= factor"));
    assert_eq!(freql::format_source(&formatted).unwrap(), formatted);
    let result = check(&formatted);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert_eq!(result.report.monitored_metrics[0].value, "16");
    assert_eq!(
        result
            .report
            .diagnostics
            .iter()
            .filter(|d| d.code == "W0601")
            .count(),
        2
    );
}

#[test]
fn constants_and_protected_names_are_checked_in_both_body_kinds() {
    for body in [
        "const i = 1\ni = 2",
        "const i = 1\ni += 2",
        "const i = 1\nconst i = 2",
        "const i = 1\nlet i = 2",
    ] {
        for source in [
            format!("process Result {{ () => {{ {body} }} }}"),
            format!(
                "fn calculate() -> (out: Scalar) {{ {body}\nout = i }}\nprocess Result {{ () => {{ let result = calculate() }} }}"
            ),
        ] {
            let result = check(&source);
            assert_eq!(result.report.status, Completeness::Invalid, "{source}");
        }
    }
    for source in [
        "process P { config input: Integer = 1 () => { let input = 2 } }",
        "fn f(input: Integer) -> (out: Integer) { let input = 2 out = input }",
        "process Child {} process P { () => { let child = Child() let child = Child() } }",
    ] {
        assert_eq!(
            check(source).report.status,
            Completeness::Invalid,
            "{source}"
        );
    }
}

#[test]
fn redeclaration_does_not_retain_the_previous_refinement() {
    let result = check(
        "unit Positive = Scalar(x) { requires x > 0 }\nprocess P { monitor value: Scalar () => { let n: Positive = 1\nlet n = -2\nn = -3\nvalue = n } }",
    );
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert_eq!(result.report.monitored_metrics[0].value, "-3");
}
