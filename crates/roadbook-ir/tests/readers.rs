//! Every reader of the IR in this repository accepts the version the compiler writes. The
//! TypeScript core and the viewer keep their own copy of the version; when the IR version
//! changes, this test points at the copies that were not updated.

use std::path::Path;

#[test]
fn typescript_readers_accept_this_ir_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in ["packages/core/src/ir.ts", "packages/viewer/src/ir.ts"] {
        let src = std::fs::read_to_string(root.join(file)).unwrap();
        let line = src
            .lines()
            .find(|l| l.starts_with("export const IR_VERSION"))
            .unwrap_or_else(|| panic!("{file} has no `export const IR_VERSION`"));
        let quoted = [
            format!("\"{}\"", roadbook_ir::IR_VERSION),
            format!("'{}'", roadbook_ir::IR_VERSION),
        ];
        assert!(
            quoted.iter().any(|q| line.contains(q.as_str())),
            "{file} reads IR version {line:?}, but the compiler writes {}",
            roadbook_ir::IR_VERSION
        );
    }
}
