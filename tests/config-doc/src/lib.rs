//! 链接全部组件，给 `tests/doc.rs` 用。核对的逻辑在共享仓库的 `config-doc` 包里。

include!(concat!(env!("OUT_DIR"), "/components.rs"));
