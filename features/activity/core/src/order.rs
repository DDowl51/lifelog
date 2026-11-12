//! 处理器按什么顺序执行。顺序是策略，来自配置。

/// 按配置排出执行顺序，返回 `registered` 的下标。
///
/// - 配置里列了、但没登记的名字：跳过（那个组件被停用了）。
/// - 登记了、但配置里没列的处理器：报错。顺序会影响结果（脱敏应该排在最后），
///   不能替用户猜。
pub fn order_processors(
    registered: &[String],
    configured: &[String],
) -> Result<Vec<usize>, String> {
    for (i, name) in registered.iter().enumerate() {
        if registered[..i].contains(name) {
            return Err(format!("有两个处理器都叫 `{name}`"));
        }
    }
    for (i, name) in configured.iter().enumerate() {
        if configured[..i].contains(name) {
            return Err(format!("processors 里 `{name}` 写了两次"));
        }
    }
    if let Some(missing) = registered.iter().find(|name| !configured.contains(name)) {
        return Err(format!(
            "处理器 `{missing}` 已经启用，但没有写进 [components.activity] 的 processors。\
             处理器的先后会影响结果，请写明顺序，例如 processors = [{}]",
            registered
                .iter()
                .map(|n| format!("{n:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(configured
        .iter()
        .filter_map(|name| registered.iter().position(|r| r == name))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn follows_the_configured_order() {
        let registered = names(&["privacy", "project"]);
        assert_eq!(
            order_processors(&registered, &names(&["project", "privacy"])),
            Ok(vec![1, 0])
        );
    }

    #[test]
    fn skips_configured_but_absent() {
        let registered = names(&["privacy"]);
        assert_eq!(
            order_processors(&registered, &names(&["project", "privacy"])),
            Ok(vec![0])
        );
        assert_eq!(order_processors(&[], &names(&["project"])), Ok(vec![]));
    }

    #[test]
    fn unlisted_processor_is_an_error() {
        let err =
            order_processors(&names(&["privacy", "project"]), &names(&["project"])).unwrap_err();
        assert!(
            err.contains("`privacy`") && err.contains(r#"["privacy", "project"]"#),
            "{err}"
        );
    }

    #[test]
    fn duplicates_are_errors() {
        assert!(order_processors(&names(&["a", "a"]), &names(&["a"])).is_err());
        assert!(order_processors(&names(&["a"]), &names(&["a", "a"])).is_err());
    }
}
