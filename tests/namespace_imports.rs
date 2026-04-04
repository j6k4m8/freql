use freql::{Compiler, CompilerOptions, Completeness};
use tempfile::tempdir;

fn check(files: &[(&str, &str)]) -> freql::Compilation {
    let directory = tempdir().unwrap();
    for (name, text) in files {
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    Compiler::new(CompilerOptions::default())
        .check_path(&directory.path().join("main.req"))
        .unwrap()
}

#[test]
fn namespace_types_units_functions_and_processes_resolve_to_the_same_definitions() {
    let result = check(&[
        (
            "module.req",
            r#"
unit Tick = Second * 2
unit Positive = Scalar(x) { requires x > 0 }
artifact Entry { duration: Tick }
fn plan(value: Tick) -> (duration: Tick) { duration = value * 2 }
process Execute {
    config duration: Tick
    export entry: Entry
    () => { entry = Entry(duration: duration) }
}
"#,
        ),
        (
            "main.req",
            r#"
import "module.req" as module
import "module.req" as other
import "module.req" (Entry)
process Main {
    config ratio: module.Positive = 1
    export entry: Entry
    export duration: module.Tick aggregate explicit
    () => {
        let planned = module.plan(value: 3 module.Tick)
        let execute = other.Execute(duration: planned.duration)()
        execute.entry -> entry
        duration = planned.duration
    }
}
"#,
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert!(
        result.report.diagnostics.is_empty(),
        "{}",
        result.render_human()
    );
    assert_eq!(result.model.artifacts.len(), 1);
    assert!(
        result
            .report
            .symbols
            .iter()
            .any(|s| s.path == "module.req::Entry")
    );
}

#[test]
fn separate_modules_can_export_equal_names_and_different_unit_scales() {
    let result = check(&[
        (
            "a.req",
            "unit Step = Meter * 2\nartifact Sample { size: Step }\nfn plan() -> (value: Step) { value = 1 Step }",
        ),
        (
            "b.req",
            "unit Step = Meter * 3\nartifact Sample { size: Step }\nfn plan() -> (value: Step) { value = 1 Step }",
        ),
        (
            "main.req",
            r#"
import "a.req" as a
import "b.req" as b
process Main {
    export total: Length aggregate explicit
    monitor total as Meter
    () => { total = a.plan().value + b.plan().value }
}
"#,
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert_eq!(result.model.artifacts.len(), 2);
    assert!(
        result
            .report
            .monitored_metrics
            .iter()
            .any(|m| m.value == "5 Meter"),
        "{}",
        result.render_human()
    );
}

#[test]
fn namespaces_handle_nested_relative_imports_and_compound_units() {
    let result = check(&[
        (
            "lib/units.req",
            "unit Pace = Meter / Second\nunit Interval = Second * 2",
        ),
        (
            "lib/plans.req",
            "import \"units.req\" as u\nunit Travel = u.Pace * u.Interval",
        ),
        (
            "main.req",
            r#"
import "lib/plans.req" as p
import "lib/units.req" as u
process Main {
    export area: p.Travel^2 aggregate explicit
    export rate: (p.Travel / u.Interval) aggregate explicit
    monitor area as Meter^2
    monitor rate as Meter / Second
    () => {
        area = 2 p.Travel^2
        rate = (3 p.Travel) / (1 u.Interval)
    }
}
"#,
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert!(
        result
            .report
            .monitored_metrics
            .iter()
            .any(|m| m.value == "8 Meter^2"),
        "{}",
        result.render_human()
    );
}

#[test]
fn bad_imports_and_unknown_namespace_members_are_actionable_errors() {
    for (source, expected) in [
        ("import \"lib.req\"", "explicit member list"),
        ("import \"lib.req\" ()", "explicit member list"),
        (
            "import \"lib.req\" as x\nimport \"lib.req\" as x",
            "conflicts",
        ),
        ("import \"lib.req\" as math", "reserved"),
        ("import \"lib.req\" as std", "reserved"),
        ("import \"lib.req\" as x\nprocess x {}", "conflicts"),
        (
            "import \"lib.req\" as x\nprocess Main { config v: x.Missing }",
            "does not export Missing",
        ),
        (
            "import \"lib.req\" as x\nprocess Main { () => { let v = x.missing() } }",
            "does not export missing",
        ),
        ("import \"lib.req\" (Value)\nprocess Value {}", "conflicts"),
    ] {
        let result = check(&[("lib.req", "unit Value = Scalar"), ("main.req", source)]);
        assert_eq!(
            result.report.status,
            Completeness::Invalid,
            "{source}\n{}",
            result.render_human()
        );
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|d| d.message.contains(expected)),
            "{source}\n{}",
            result.render_human()
        );
    }
}

#[test]
fn namespaces_do_not_implicitly_import_bare_declaration_names() {
    let result = check(&[
        ("lib.req", "unit Hidden = Meter"),
        (
            "main.req",
            "import \"lib.req\" as lib\nprocess Main { config x: Hidden }",
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Invalid,
        "{}",
        result.render_human()
    );
}

#[test]
fn namespace_import_cycles_still_fail_loading() {
    let directory = tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.req"),
        "import \"child.req\" as child",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("child.req"),
        "import \"main.req\" as parent",
    )
    .unwrap();
    let error = Compiler::new(CompilerOptions::default())
        .check_path(&directory.path().join("main.req"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("import cycle"));
}

#[test]
fn native_and_standard_imports_remain_available() {
    let result = check(&[(
        "main.req",
        r#"
import math
import std.collection as collection
process Main {
    export equal: Bool
    export root: Scalar
    () => {
        equal = collection.all_same(items: [1, 1]).result
        root = math.sqrt(4)
    }
}
"#,
    )]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
}

#[test]
fn tuple_fields_and_local_shadowing_use_lexical_scope() {
    let result = check(&[
        (
            "lib.req",
            "unit Distance = Meter * 2\nfn plan(value: (distance: Distance)) -> (total: Distance) { total = value.distance }",
        ),
        (
            "main.req",
            r#"
import "lib.req" as lib
process Main {
    config settings: (distance: lib.Distance) = (distance: 2 lib.Distance)
    export total: Length aggregate explicit
    monitor total as Meter
    () => {
        let result = lib.plan(value: settings)
        let lib = (distance: 1 Meter)
        total = result.total + lib.distance
    }
}
"#,
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert!(
        result
            .report
            .monitored_metrics
            .iter()
            .any(|m| m.value == "5 Meter"),
        "{}",
        result.render_human()
    );
}

#[test]
fn qualified_resources_inheritance_and_trait_bounds_keep_definition_identity() {
    let result = check(&[
        (
            "lib.req",
            r#"
resource Lane { capacity: 10 }
process Base { config amount: Scalar }
trait Accepted[T] { fn accept(value: T) -> (result: Bool) }
"#,
        ),
        (
            "main.req",
            r#"
import "lib.req" as lib
import "lib.req" as other
impl lib.Accepted[Scalar] {
    fn accept(value: Scalar) -> (result: Bool) { result = true }
}
fn identity[T](value: T) -> (result: T)
where T: other.Accepted, T: Copyable { result = value }
process Provision {
    export lanes: lib.Lane[2]
    () => { lanes = other.Lane[2] }
}
process Consumer: lib.Base {
    uses lane: other.Lane[1]
    () => { requires amount > 0 }
}
process Main {
    () => {
        let provision = Provision()()
        let consumer = Consumer(amount: identity(value: 1.0).result)()
        provision.lanes -> consumer.lane
    }
}
"#,
        ),
    ]);
    assert_eq!(
        result.report.status,
        Completeness::Complete,
        "{}",
        result.render_human()
    );
    assert_eq!(result.model.resources.len(), 1);
}

#[test]
fn namespace_syntax_formats_idempotently() {
    let source = "import \"units.req\" as units\nprocess Main {\n    config area: units.Length^2 = 2 units.Meter^2\n}\n";
    let first = freql::format_source(source).unwrap();
    assert_eq!(first, freql::format_source(&first).unwrap());
    let parsed = freql::parser::parse(&first);
    let freql::ast::Item::Import(import) = &parsed.module.items[0] else {
        panic!("import")
    };
    assert_eq!(import.alias.as_deref(), Some("units"));
}
