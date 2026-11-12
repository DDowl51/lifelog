//! `docs/configuration.md` 和代码里的默认值对得上。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use config_doc::{KNOWN_COMPONENTS, LINKED_COMPONENTS, compare, parse};
use kernel_api::registered_components;
use serde_json::Value;

const DOC: &str = include_str!("../../../docs/configuration.md");

#[test]
fn every_component_is_linked_into_this_check() {
    let mut linked = LINKED_COMPONENTS.to_vec();
    linked.sort_unstable();
    assert_eq!(
        linked, KNOWN_COMPONENTS,
        "新组件要加进 tests/config-doc/Cargo.toml 的依赖"
    );
}

#[test]
fn the_document_matches_the_defaults() {
    let doc = parse(DOC).unwrap();
    let mut errors = Vec::new();

    let kernel = serde_json::to_value(boot::Settings::default().kernel).unwrap();
    match (doc.get("kernel"), kernel) {
        (Some(section), Value::Object(defaults)) => {
            errors.extend(compare("kernel", &defaults, section));
        }
        _ => errors.push("缺 [kernel] 一节".to_owned()),
    }

    for component in registered_components() {
        let id = component.manifest().id;
        let Some(defaults) = component.config_defaults() else {
            errors.push(format!("`{id}` 没有实现 config_defaults"));
            continue;
        };
        match doc.get(id) {
            Some(section) => errors.extend(compare(id, defaults.as_map(), section)),
            None => errors.push(format!("缺 [components.{id}] 一节")),
        }
    }

    for id in doc.keys() {
        if id != "kernel" && !KNOWN_COMPONENTS.contains(&id.as_str()) {
            errors.push(format!("[components.{id}]：没有这个组件"));
        }
    }

    assert!(
        errors.is_empty(),
        "docs/configuration.md 和代码对不上：\n{}",
        errors.join("\n")
    );
}
