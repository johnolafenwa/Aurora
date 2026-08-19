use aura_compiler::SEMANTIC_INTERFACE_SCHEMA_VERSION;
use std::fs;
use std::path::PathBuf;

#[test]
fn the_current_language_surface_has_a_compiler_owned_semantic_interface_schema() {
    assert_eq!(
        SEMANTIC_INTERFACE_SCHEMA_VERSION, 6,
        "the checked 0.3 surface requires schema 6 across compiler services and native cache keys"
    );
}

#[test]
fn maintained_protocol_docs_track_the_compiler_owned_schema() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let version = SEMANTIC_INTERFACE_SCHEMA_VERSION;
    let expectations = [
        (
            "crates/aura/README.md",
            vec![
                format!("semantic-interface version `{version}`"),
                format!("semantic-interface schema `v{version}`"),
            ],
        ),
        (
            "tools/aura-language-server/README.md",
            vec![format!("semantic_interface_version: {version}")],
        ),
        (
            "docs/manual/cli-and-tooling.md",
            vec![
                format!("semantic_interface_version: {version}"),
                format!("semantic-interface schema version `{version}`"),
            ],
        ),
        (
            "docs/manual/diagnostics.md",
            vec![format!("semantic-interface version is `{version}`")],
        ),
        (
            "docs/manual/status-and-compatibility.md",
            vec![format!("semantic schema version `{version}`")],
        ),
    ];

    for (relative_path, required_texts) in expectations {
        let source = fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("failed to read {relative_path}: {error}"));
        for required in required_texts {
            assert!(
                source.contains(&required),
                "{relative_path} must document compiler semantic schema {version} with {required:?}"
            );
        }
    }
}
