//! 启动前的校验和排序。纯函数，只看各组件的 `Manifest`。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use kernel_api::{AppError, ContractId, Kind, Manifest};

/// 校验通过后的启动计划。
#[derive(Debug)]
pub(crate) struct Plan {
    /// 启动顺序，值是 `manifests` 的下标。
    pub order: Vec<usize>,
}

/// 校验并排序。出错时消息写明是哪个组件、哪个契约、怎么改。
pub(crate) fn plan(manifests: &[Manifest]) -> Result<Plan, AppError> {
    // id 不能重复
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (i, m) in manifests.iter().enumerate() {
        if seen.insert(m.id, i).is_some() {
            return Err(AppError::config(format!("有两个组件的 id 都是 `{}`", m.id)));
        }
    }

    // 契约名 → 提供者
    let mut providers: HashMap<&str, (usize, ContractId)> = HashMap::new();
    for (i, m) in manifests.iter().enumerate() {
        for c in &m.provides {
            if let Some((other, _)) = providers.insert(c.name, (i, *c)) {
                return Err(AppError::config(format!(
                    "契约 `{}` 同时由 `{}` 和 `{}` 提供。替换实现时在配置里停用其中一个：\
                     [components.<id>] enabled = false",
                    c.name, manifests[other].id, m.id
                )));
            }
        }
    }

    // 依赖边：提供者 → 使用者
    let mut needs: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); manifests.len()];
    for (i, m) in manifests.iter().enumerate() {
        let required = m.requires.iter().map(|c| (c, true));
        let optional = m.optional_requires.iter().map(|c| (c, false));
        for (c, must) in required.chain(optional) {
            let Some((p, provided)) = providers.get(c.name).copied() else {
                if must {
                    return Err(AppError::config(format!(
                        "组件 `{}` 需要契约 `{c}`，但没有启用的组件提供它。\
                         启用一个提供者，或者停用 `{}`",
                        m.id, m.id
                    )));
                }
                continue;
            };
            if provided.version != c.version || provided.many != c.many {
                return Err(AppError::config(format!(
                    "组件 `{}` 需要契约 `{c}`，但 `{}` 提供的是 `{provided}`（版本或种类不一致）",
                    m.id, manifests[p].id
                )));
            }
            if m.kind == Kind::Service && manifests[p].kind == Kind::Feature {
                return Err(AppError::config(format!(
                    "系统服务 `{}` 依赖了应用功能 `{}` 提供的契约 `{c}`。\
                     系统服务不能依赖应用功能：在系统服务的契约里声明扩展点，让应用功能来登记",
                    m.id, manifests[p].id
                )));
            }
            if p != i {
                needs[i].insert(p);
            }
        }
    }

    // 拓扑排序。同一层按 id 排，保证每次启动顺序相同。
    let by_id: BTreeMap<&str, usize> = manifests
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id, i))
        .collect();
    let mut done: BTreeSet<usize> = BTreeSet::new();
    let mut order = Vec::with_capacity(manifests.len());
    while order.len() < manifests.len() {
        let next = by_id
            .values()
            .copied()
            .find(|i| !done.contains(i) && needs[*i].iter().all(|p| done.contains(p)));
        match next {
            Some(i) => {
                done.insert(i);
                order.push(i);
            }
            None => {
                let stuck: Vec<&str> = by_id
                    .iter()
                    .filter(|(_, i)| !done.contains(*i))
                    .map(|(id, _)| *id)
                    .collect();
                return Err(AppError::config(format!(
                    "组件之间的依赖成环：{}。让被依赖的一方声明扩展点，另一方来登记",
                    stuck.join("、")
                )));
            }
        }
    }

    Ok(Plan { order })
}

#[cfg(test)]
mod tests {
    use kernel_api::ErrorKind;

    use super::*;

    const fn c(name: &'static str) -> ContractId {
        ContractId {
            name,
            version: 1,
            many: false,
        }
    }

    fn m(
        id: &'static str,
        kind: Kind,
        provides: &[ContractId],
        requires: &[ContractId],
    ) -> Manifest {
        Manifest {
            id,
            kind,
            summary: "",
            provides: provides.to_vec(),
            requires: requires.to_vec(),
            optional_requires: vec![],
        }
    }

    fn ids(manifests: &[Manifest]) -> Vec<&'static str> {
        plan(manifests)
            .unwrap()
            .order
            .into_iter()
            .map(|i| manifests[i].id)
            .collect()
    }

    fn err(manifests: &[Manifest]) -> String {
        let e = plan(manifests).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Config);
        e.message
    }

    #[test]
    fn providers_start_before_users() {
        let all = [
            m("app", Kind::Feature, &[], &[c("store"), c("clock")]),
            m("store", Kind::Service, &[c("store")], &[c("clock")]),
            m("clock", Kind::Service, &[c("clock")], &[]),
        ];
        assert_eq!(ids(&all), ["clock", "store", "app"]);
    }

    #[test]
    fn independent_components_start_in_id_order() {
        let all = [
            m("zeta", Kind::Service, &[], &[]),
            m("alpha", Kind::Service, &[], &[]),
        ];
        assert_eq!(ids(&all), ["alpha", "zeta"]);
    }

    #[test]
    fn missing_provider_names_component_and_contract() {
        let msg = err(&[m("app", Kind::Feature, &[], &[c("store")])]);
        assert!(msg.contains("`app`") && msg.contains("store@1"), "{msg}");
    }

    #[test]
    fn optional_requirement_may_be_absent() {
        let mut app = m("app", Kind::Feature, &[], &[]);
        app.optional_requires = vec![c("search")];
        assert_eq!(ids(&[app]), ["app"]);
    }

    #[test]
    fn optional_provider_still_starts_first() {
        let mut app = m("app", Kind::Feature, &[], &[]);
        app.optional_requires = vec![c("search")];
        let all = [app, m("search", Kind::Feature, &[c("search")], &[])];
        assert_eq!(ids(&all), ["search", "app"]);
    }

    #[test]
    fn two_providers_of_one_contract() {
        let msg = err(&[
            m("clock-a", Kind::Service, &[c("clock")], &[]),
            m("clock-b", Kind::Service, &[c("clock")], &[]),
        ]);
        assert!(
            msg.contains("clock-a") && msg.contains("clock-b") && msg.contains("enabled"),
            "{msg}"
        );
    }

    #[test]
    fn cycle_lists_members() {
        let msg = err(&[
            m("a", Kind::Feature, &[c("a")], &[c("b")]),
            m("b", Kind::Feature, &[c("b")], &[c("a")]),
            m("free", Kind::Service, &[], &[]),
        ]);
        assert!(msg.contains("a、b") && !msg.contains("free"), "{msg}");
    }

    #[test]
    fn service_must_not_depend_on_feature() {
        let msg = err(&[
            m("svc", Kind::Service, &[], &[c("notes")]),
            m("notes", Kind::Feature, &[c("notes")], &[]),
        ]);
        assert!(
            msg.contains("系统服务 `svc`") && msg.contains("扩展点"),
            "{msg}"
        );
    }

    #[test]
    fn version_mismatch() {
        let v2 = ContractId {
            name: "clock",
            version: 2,
            many: false,
        };
        let msg = err(&[
            m("app", Kind::Feature, &[], &[v2]),
            m("clock", Kind::Service, &[c("clock")], &[]),
        ]);
        assert!(msg.contains("clock@2") && msg.contains("clock@1"), "{msg}");
    }

    #[test]
    fn duplicate_id() {
        let msg = err(&[
            m("x", Kind::Service, &[], &[]),
            m("x", Kind::Service, &[], &[]),
        ]);
        assert!(msg.contains("`x`"), "{msg}");
    }
}
