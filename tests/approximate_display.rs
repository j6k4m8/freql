use freql::{Compiler, CompilerOptions, Completeness, RenderOptions, graph_document, render_html};
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
fn monitor_reports_keep_exact_values_separate_from_approximate_display() {
    let compilation = check(
        r#"
process Result {
    monitor duration: Time as ~Hour
    () => { duration = 2 Hour / 3 }
}
"#,
    );
    assert_eq!(
        compilation.report.status,
        Completeness::Complete,
        "{}",
        compilation.render_human()
    );
    let metric = &compilation.report.monitored_metrics[0];
    assert_eq!(metric.value, "2/3 Hour");
    assert_eq!(metric.approximate_value.as_deref(), Some("≈0.666667 Hour"));
    assert!(compilation.render_human().contains("≈0.666667 Hour"));
    let report = serde_json::to_value(&compilation.report).unwrap();
    assert_eq!(report["monitored_metrics"][0]["value"], "2/3 Hour");
    assert_eq!(
        report["monitored_metrics"][0]["approximate_value"],
        "≈0.666667 Hour"
    );
    let graph = graph_document(&compilation, &RenderOptions::default()).unwrap();
    let html = render_html(&graph);
    assert!(html.contains("≈0.666667 Hour"));
}

#[test]
fn approximate_monitors_are_inherited_and_propagated_to_child_totals() {
    let compilation = check(
        r#"
process Contract {
    export duration: Time aggregate sum
    monitor duration as ~Hour
}
process Leaf: Contract {
    () => { duration = 2 Hour / 3 }
}
process Root: Contract {
    () => { let child = Leaf()() }
}
"#,
    );
    assert_eq!(
        compilation.report.status,
        Completeness::Complete,
        "{}",
        compilation.render_human()
    );
    for path in ["Root.duration", "Root.child.duration"] {
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|m| m.path == path)
            .unwrap();
        assert_eq!(metric.value, "2/3 Hour");
        assert_eq!(metric.display_value(), "≈0.666667 Hour");
    }
}

#[test]
fn inferred_approximation_preserves_symbolic_values_and_dimension_checks() {
    let compilation = check(
        r#"
process Result {
    config input: Time
    monitor duration: Time as ~
    () => { duration = input }
}
"#,
    );
    assert_ne!(
        compilation.report.status,
        Completeness::Invalid,
        "{}",
        compilation.render_human()
    );
    assert!(
        compilation.report.monitored_metrics[0]
            .approximate_value
            .is_none()
    );
    for source in [
        "process P { monitor value: Scalar as ~Hour () => { value = 2 / 3 } }",
        "process P { monitor value: Time as ~Meter () => { value = 1 Second } }",
        "process P { monitor value: Bool as ~ () => { value = true } }",
        "process P { monitor value: Time as ~MissingUnit () => { value = 1 Second } }",
    ] {
        assert_eq!(
            check(source).report.status,
            Completeness::Invalid,
            "{source}"
        );
    }
}

#[test]
fn approximate_monitor_formatting_is_idempotent() {
    let source =
        "process P { monitor a:Time as ~Hour\nmonitor b:Time as ~\n()=>{a=2 Hour/3\nb=1 Second/3}}";
    let formatted = freql::format_source(source).unwrap();
    assert!(formatted.contains("as ~Hour"), "{formatted}");
    assert_eq!(freql::format_source(&formatted).unwrap(), formatted);
    assert_eq!(check(&formatted).report.status, Completeness::Complete);
}
