//! `docs/configuration.md` 和代码里的默认值对得上。

use config_doc_test::{KNOWN_COMPONENTS, LINKED_COMPONENTS};

#[test]
fn the_document_matches_the_defaults() {
    if let Err(e) = config_doc::check(
        include_str!("../../../docs/configuration.md"),
        KNOWN_COMPONENTS,
        LINKED_COMPONENTS,
    ) {
        panic!("{e}");
    }
}
