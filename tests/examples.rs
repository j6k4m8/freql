//! Public examples must load without private fixtures or workspace dependencies.
use std::path::Path;
use freql::{Compiler, CompilerOptions, Completeness};

#[test]
fn public_examples_are_valid_standalone_models() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();
    for entry in [
        "docs/examples/HelloWorld/hello.req",
        "docs/examples/Basic/main.req",
        "docs/examples/Basic/named-tuples.req",
        "docs/examples/Basic/refinements.req",
        "docs/examples/Basic/namespaces/main.req",
        "docs/examples/Advanced/main.req",
        "docs/examples/Dimensional-Calculator.req",
        "docs/examples/Provisioned-Facility.req",
    ] {
        let compilation = Compiler::new(CompilerOptions::default())
            .check_path(&root.join(entry))
            .unwrap_or_else(|error| panic!("{entry}: {error}"));
        assert_eq!(compilation.report.status, Completeness::Complete,
            "{entry}: {}", compilation.render_human());
        let sources = freql::source::load(&root.join(entry), false).unwrap();
        for source in sources.files.values() {
            if let Some(path) = &source.path {
                assert!(path.starts_with(&root), "external example dependency: {}", path.display());
            }
        }
    }
}
