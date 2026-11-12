//! 契约：增量读浏览器的访问历史。
//!
//! 一台机器上常常同时有好几种浏览器、每种好几个 profile。两层：
//!
//! - 扩展点 [`BROWSER_ADAPTERS`]：每种浏览器的适配（系统服务）登记一份，报告自己能读的来源（一个 profile 一个）。
//! - 请求 [`BROWSER_HISTORY`]：一个系统服务拥有上面的扩展点，把所有来源合在一起对外提供。
//!
//! 读的一方（应用功能）只用请求契约，逐个来源保存游标。扩展点不能由应用功能拥有：
//! 那样登记它的系统服务就依赖了应用功能。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一个可读的历史来源，通常是某个浏览器的某个 profile。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySource {
    /// 在所有登记者之间唯一、跨重启不变，例如 `zen/Default`。
    pub id: String,
    /// 浏览器的名字，例如 `zen`。
    pub browser: String,
    /// profile 的名字；浏览器只有一个默认 profile 时可以是 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// 一次访问。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    /// 在这个来源里唯一。
    pub id: String,
    /// 访问的时刻，UTC 毫秒。
    pub at: i64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// 读到的一页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitPage {
    /// 按访问时刻升序。
    pub visits: Vec<Visit>,
    /// 下次从这里接着读。内容由提供者决定，读的一方只保存、原样传回。
    pub next: String,
    /// 现在马上再读一次，可能还有。
    pub more: bool,
}

contract! {
    /// 浏览器的访问历史：所有浏览器、所有 profile 合在一起。
    pub trait BrowserHistory as BROWSER_HISTORY = service("browser.history", 1) {
        /// 现在能读的来源。浏览器没装、profile 被删了，就不在里面。
        async fn sources(&self) -> Result<Vec<HistorySource>, AppError>;

        /// 从游标 `after` 之后读最多 `limit` 条（`None` 表示从头读）。来源不存在是 `not_found`。
        /// 可能把给过的访问再给一次，读的一方按 `id` 去重。
        async fn read(&self, source: String, after: Option<String>, limit: u32) -> Result<VisitPage, AppError>;
    }
}

contract! {
    /// 扩展点：一种浏览器的适配。方法的含义同 [`BrowserHistory`]，只管自己的来源。
    pub trait BrowserAdapter as BROWSER_ADAPTERS = point("browser.adapters", 1) {
        async fn sources(&self) -> Result<Vec<HistorySource>, AppError>;

        async fn read(&self, source: String, after: Option<String>, limit: u32) -> Result<VisitPage, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 契约测试，和一个放在内存里的假实现。

    use std::collections::BTreeMap;
    use std::future::Future;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use kernel_api::{
        AppError, Collector, Component, ConfigSection, Host, Kind, Manifest, async_trait,
    };

    use super::{BROWSER_ADAPTERS, BrowserAdapter, HistorySource, Visit, VisitPage};

    pub fn visit(id: &str, at: i64, url: &str, title: &str) -> Visit {
        Visit {
            id: id.to_owned(),
            at,
            url: url.to_owned(),
            title: Some(title.to_owned()),
        }
    }

    /// 适配的契约测试。`history` 里要有一个来源 `source`，开始时是空的；
    /// `add` 往这个来源追加访问（`id` 由提供者决定，所以只比较时刻、网址和标题）。
    pub async fn browser_adapter_contract<F, Fut>(
        history: Arc<dyn BrowserAdapter>,
        source: &str,
        add: F,
    ) -> Result<(), AppError>
    where
        F: Fn(Vec<Visit>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let sources = history.sources().await?;
        assert!(
            sources.iter().any(|s| s.id == source),
            "来源 {source} 不在 {sources:?} 里"
        );
        let brief = |page: &VisitPage| -> Vec<(i64, String, Option<String>)> {
            page.visits
                .iter()
                .map(|v| (v.at, v.url.clone(), v.title.clone()))
                .collect()
        };

        let empty = history.read(source.to_owned(), None, 10).await?;
        assert!(empty.visits.is_empty() && !empty.more, "{empty:?}");

        let first = vec![
            visit("", 1_000, "https://a.example/", "甲"),
            visit("", 2_000, "https://b.example/x?q=1", "乙"),
            visit("", 3_000, "https://c.example/", "丙"),
        ];
        add(first.clone()).await;
        let page = history.read(source.to_owned(), None, 10).await?;
        let expected: Vec<_> = first
            .iter()
            .map(|v| (v.at, v.url.clone(), v.title.clone()))
            .collect();
        assert_eq!(brief(&page), expected, "按时刻升序，网址和标题原样");
        let mut ids: Vec<&str> = page.visits.iter().map(|v| v.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), 3, "每次访问有自己的 id");
        assert!(!page.more);
        let rest = history
            .read(source.to_owned(), Some(page.next.clone()), 10)
            .await?;
        assert!(rest.visits.is_empty(), "{rest:?}");

        let one = history.read(source.to_owned(), None, 2).await?;
        assert_eq!(brief(&one), expected[..2]);
        assert!(one.more);
        let two = history.read(source.to_owned(), Some(one.next), 2).await?;
        assert_eq!(brief(&two), expected[2..]);
        assert!(!two.more);

        add(vec![visit("", 4_000, "https://d.example/", "丁")]).await;
        let new = history.read(source.to_owned(), Some(page.next), 10).await?;
        assert_eq!(
            brief(&new),
            [(
                4_000,
                "https://d.example/".to_owned(),
                Some("丁".to_owned())
            )]
        );

        match history.read("no-such-source".to_owned(), None, 10).await {
            Err(e) => assert_eq!(e.kind, kernel_api::ErrorKind::NotFound, "{e}"),
            Ok(page) => panic!("不存在的来源应该是 not_found：{page:?}"),
        }
        Ok(())
    }

    type Sources = BTreeMap<String, (HistorySource, Vec<Visit>)>;

    /// 放在内存里的历史，每个来源一份。游标是读过的条数。克隆出来的是同一份。
    #[derive(Clone, Default)]
    pub struct MemoryBrowser(Arc<Mutex<Sources>>);

    impl MemoryBrowser {
        /// 往来源 `source` 里追加访问（来源不存在就建一个，浏览器名取 `/` 前面的部分）。
        /// `id` 为空的按序号补上。
        pub fn push(&self, source: &str, visits: impl IntoIterator<Item = Visit>) {
            let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let (_, list) = all.entry(source.to_owned()).or_insert_with(|| {
                let (browser, profile) = source.split_once('/').unwrap_or((source, ""));
                (
                    HistorySource {
                        id: source.to_owned(),
                        browser: browser.to_owned(),
                        profile: (!profile.is_empty()).then(|| profile.to_owned()),
                    },
                    Vec::new(),
                )
            });
            for mut v in visits {
                if v.id.is_empty() {
                    v.id = (list.len() + 1).to_string();
                }
                list.push(v);
            }
        }
    }

    #[async_trait]
    impl BrowserAdapter for MemoryBrowser {
        async fn sources(&self) -> Result<Vec<HistorySource>, AppError> {
            let all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            Ok(all.values().map(|(s, _)| s.clone()).collect())
        }

        async fn read(
            &self,
            source: String,
            after: Option<String>,
            limit: u32,
        ) -> Result<VisitPage, AppError> {
            let all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let (_, list) = all
                .get(&source)
                .ok_or_else(|| AppError::not_found(format!("没有来源 `{source}`")))?;
            let start = match after {
                Some(c) => c
                    .parse::<usize>()
                    .map_err(|_| AppError::validation(format!("看不懂的游标 `{c}`")))?,
                None => 0,
            };
            let end = list.len().min(start + limit as usize);
            Ok(VisitPage {
                visits: list.get(start..end).unwrap_or_default().to_vec(),
                next: end.max(start).to_string(),
                more: end < list.len(),
            })
        }
    }

    /// 组件 `browser-memory`：登记到扩展点的一种「浏览器」。
    impl Component for MemoryBrowser {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "browser-memory",
                kind: Kind::Service,
                summary: "测试用：放在内存里的浏览历史",
                provides: vec![],
                requires: vec![BROWSER_ADAPTERS.id()],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.contribute(&BROWSER_ADAPTERS, Arc::new(self.clone()))
        }
    }

    /// 组件 `browser-probe`：扩展点的假所有者，适配的测试里用它拿到登记。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct AdapterProbe(Arc<OnceLock<Collector<dyn BrowserAdapter>>>);

    impl AdapterProbe {
        /// 全部登记，按登记的先后。启动之前是空的。
        pub fn all(&self) -> Vec<Arc<dyn BrowserAdapter>> {
            self.0.get().map(Collector::all).unwrap_or_default()
        }
    }

    impl Component for AdapterProbe {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "browser-probe",
                kind: Kind::Service,
                summary: "测试用：取出登记到浏览器适配扩展点的全部实现",
                provides: vec![BROWSER_ADAPTERS.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.0.set(host.collector(&BROWSER_ADAPTERS)?);
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn memory_browser_passes_the_contract() {
            let browser = MemoryBrowser::default();
            browser.push("zen/Default", []);
            let source = browser.clone();
            browser_adapter_contract(Arc::new(browser), "zen/Default", |visits| {
                source.push("zen/Default", visits);
                async {}
            })
            .await
            .unwrap();
        }
    }
}
