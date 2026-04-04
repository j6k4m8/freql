use std::path::PathBuf;

use freql::{AnalysisHost, CompilerOptions, Completeness};
use tempfile::tempdir;

#[test]
fn selected_entry_uses_all_open_overlays_and_shared_module_context() {
    let directory = tempdir().unwrap();
    let child = directory.path().join("child.req");
    let entry = directory.path().join("main.req");
    std::fs::write(
        &child,
        "process Child { config amount: Scalar\n() => { requires amount > 0 } }",
    )
    .unwrap();
    std::fs::write(
        &entry,
        "import \"child.req\" (Child)\nprocess Main { () => { let child = Child(amount: 1)() } }",
    )
    .unwrap();
    let mut host = AnalysisHost::new(CompilerOptions::default());
    host.set_entry_model(
        Some(PathBuf::from("main.req")),
        Some(directory.path().to_owned()),
    );
    host.set_file(
        &child,
        "process Child { config amount: Scalar\n() => { requires amount > 3 } }",
    );
    host.set_file(
        &entry,
        "import \"child.req\" (Child)\nprocess Main { () => { let child = Child(amount: 5)() } }",
    );
    let snapshot = host.snapshot(&child).unwrap();
    assert!(snapshot.entry_context.uses_configured_entry);
    assert_eq!(
        snapshot.entry_context.analysis_entry,
        entry.canonicalize().unwrap()
    );
    assert!(snapshot.compilation.model.processes.contains_key("Main"));
    assert_eq!(
        snapshot.compilation.report.status,
        Completeness::Complete,
        "{}",
        snapshot.compilation.render_human()
    );

    // Closing the entry restores its disk value, while retaining the child edit.
    host.remove_overlay(&entry);
    let snapshot = host.snapshot(&child).unwrap();
    assert_eq!(
        snapshot.compilation.report.status,
        Completeness::Invalid,
        "{}",
        snapshot.compilation.render_human()
    );
}

#[test]
fn files_outside_selected_closure_keep_standalone_analysis() {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("main.req");
    let unrelated = directory.path().join("other.req");
    std::fs::write(&entry, "process Main {}").unwrap();
    std::fs::write(&unrelated, "process Other {}").unwrap();
    let mut host = AnalysisHost::new(CompilerOptions::default());
    host.set_entry_model(Some(entry.clone()), None);
    let snapshot = host.snapshot(&unrelated).unwrap();
    assert!(!snapshot.entry_context.uses_configured_entry);
    assert_eq!(
        snapshot.entry_context.configured_entry,
        Some(entry.canonicalize().unwrap())
    );
    assert_eq!(
        snapshot.entry_context.analysis_entry,
        unrelated.canonicalize().unwrap()
    );
    assert!(snapshot.compilation.model.processes.contains_key("Other"));
    assert!(!snapshot.compilation.model.processes.contains_key("Main"));
}

#[test]
fn changing_or_clearing_entry_replaces_the_previous_context() {
    let directory = tempdir().unwrap();
    let child = directory.path().join("child.req");
    std::fs::write(&child, "process Child {}").unwrap();
    for name in ["First", "Second"] {
        std::fs::write(
            directory.path().join(format!("{name}.req")),
            format!("import \"child.req\" (Child)\nprocess {name} {{}}"),
        )
        .unwrap();
    }
    let mut host = AnalysisHost::new(CompilerOptions::default());
    for name in ["First", "Second"] {
        host.set_entry_model(
            Some(PathBuf::from(format!("{name}.req"))),
            Some(directory.path().to_owned()),
        );
        let snapshot = host.snapshot(&child).unwrap();
        assert!(snapshot.compilation.model.processes.contains_key(name));
        assert_eq!(snapshot.sources.files.len(), 2);
    }
    host.set_entry_model(None, Some(directory.path().to_owned()));
    let snapshot = host.snapshot(&child).unwrap();
    assert!(!snapshot.entry_context.uses_configured_entry);
    assert!(snapshot.entry_context.configured_entry.is_none());
    assert_eq!(snapshot.sources.files.len(), 1);
}

#[test]
fn invalid_selected_entry_is_actionable_and_never_silently_ignored() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("main.req");
    std::fs::write(&file, "process Main {}").unwrap();
    let mut host = AnalysisHost::new(CompilerOptions::default());
    host.set_entry_model(
        Some(PathBuf::from("missing.req")),
        Some(directory.path().to_owned()),
    );
    let error = host.snapshot(&file).unwrap_err().to_string();
    assert!(
        error.contains("freql.entry") && error.contains("missing.req") && error.contains("clear"),
        "{error}"
    );
    host.set_entry_model(Some(directory.path().to_owned()), None);
    assert!(
        host.snapshot(&file)
            .unwrap_err()
            .to_string()
            .contains("freql.entry")
    );
    host.set_entry_model(Some(PathBuf::from("main.req")), None);
    assert!(
        host.snapshot(&file)
            .unwrap_err()
            .to_string()
            .contains("no workspace folder")
    );
}

#[test]
fn selected_entry_can_import_a_new_unsaved_module() {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("main.req");
    let child = directory.path().join("new.req");
    std::fs::write(&entry, "process Main {}").unwrap();
    let mut host = AnalysisHost::new(CompilerOptions::default());
    host.set_entry_model(Some(entry.clone()), None);
    host.set_file(
        &entry,
        "import \"new.req\" as child\nprocess Main { () => { let run = child.Child()() } }",
    );
    host.set_file(&child, "process Child { () => {} }");
    let snapshot = host.snapshot(&child).unwrap();
    assert!(snapshot.entry_context.uses_configured_entry);
    assert_eq!(snapshot.sources.files.len(), 2);
    assert_eq!(
        snapshot.compilation.report.status,
        Completeness::Complete,
        "{}",
        snapshot.compilation.render_human()
    );
    assert!(!child.exists());
}
