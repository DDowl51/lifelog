//! `project` 的纯逻辑：推断一条活动属于哪个项目。
//!
//! 按顺序试，第一个得出结果的算数：
//!
//! 1. 来源自己已经填了 `project`：不动。
//! 2. 配置的规则（应用、标题的正则）。
//! 3. `payload` 里的工作目录（[`payload_keys::CWD`]）落在某个「项目根」下面：根下面的第一级目录名。
//! 4. 标题里出现的路径，判断方法同上。终端常把路径缩写成 `~/w/p/lifelog`
//!    （fish 的 `prompt_pwd`），每一级是真实目录名的前缀，这里也认。
//!
//! 只看文字，不读文件系统。

use activity_contracts::{Activity, payload_keys};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一条归类规则：写了的条件都满足才算命中。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    /// 应用标识，正则。
    #[serde(default)]
    pub app: Option<String>,
    /// 标题，正则。可以用名为 `project` 的分组取出项目名。
    #[serde(default)]
    pub title: Option<String>,
    /// 窗口所在工作区的名字，正则。也可以用 `project` 分组，例如 `^work-(?<project>.+)$`。
    #[serde(default)]
    pub workspace: Option<String>,
    /// profile 名（浏览器的 profile、每个项目一个的浏览器实例），正则。也可以用 `project` 分组。
    #[serde(default)]
    pub profile: Option<String>,
    /// 命中时归到哪个项目。不写就用正则里的 `project` 分组。
    #[serde(default)]
    pub project: Option<String>,
}

struct Rule {
    app: Option<Regex>,
    title: Option<Regex>,
    workspace: Option<Regex>,
    profile: Option<Regex>,
    project: Option<String>,
}

/// 用 `re` 匹配 `text`：没写正则算通过（`Some(None)`）；不匹配是 `None`；
/// 匹配上了带出 `project` 分组（如果有）。
fn check(re: Option<&Regex>, text: Option<&str>) -> Option<Option<String>> {
    let Some(re) = re else {
        return Some(None);
    };
    let caps = re.captures(text?)?;
    Some(caps.name("project").map(|m| m.as_str().trim().to_owned()))
}

impl Rule {
    fn project(&self, activity: &Activity) -> Option<String> {
        let workspace = activity
            .payload
            .get(payload_keys::WORKSPACE)
            .and_then(Value::as_str);
        let from_app = check(self.app.as_ref(), activity.app.as_deref())?;
        let from_title = check(self.title.as_ref(), activity.title.as_deref())?;
        let from_workspace = check(self.workspace.as_ref(), workspace)?;
        let profile = activity
            .payload
            .get(payload_keys::PROFILE)
            .and_then(Value::as_str);
        let from_profile = check(self.profile.as_ref(), profile)?;
        self.project
            .clone()
            .or(from_profile)
            .or(from_workspace)
            .or(from_title)
            .or(from_app)
            .filter(|p| !p.is_empty())
    }
}

/// 编译好的归类规则。
pub struct Projects {
    rules: Vec<Rule>,
    /// 各个项目根，已经展开 `~` 并拆成一级一级的目录名。
    roots: Vec<Vec<String>>,
    home: Vec<String>,
}

fn components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .map(str::to_owned)
        .collect()
}

impl Projects {
    /// `home` 是用户主目录的绝对路径，用来展开 `~`。
    pub fn new(rules: &[RuleSpec], roots: &[String], home: &str) -> Result<Self, String> {
        let home = components(home);
        let rules = rules
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let n = i + 1;
                let compile = |what: &str, pattern: &str| {
                    Regex::new(pattern)
                        .map_err(|e| format!("第 {n} 条规则的 {what} 不是合法的正则：{e}"))
                };
                if spec.app.is_none()
                    && spec.title.is_none()
                    && spec.workspace.is_none()
                    && spec.profile.is_none()
                {
                    return Err(format!(
                        "第 {n} 条规则要写 app、title、workspace 或 profile"
                    ));
                }
                let compiled = |what: &str, pattern: &Option<String>| {
                    pattern.as_deref().map(|p| compile(what, p)).transpose()
                };
                let (app, title, workspace, profile) = (
                    compiled("app", &spec.app)?,
                    compiled("title", &spec.title)?,
                    compiled("workspace", &spec.workspace)?,
                    compiled("profile", &spec.profile)?,
                );
                let has_group = [&app, &title, &workspace, &profile]
                    .into_iter()
                    .flatten()
                    .any(|re| re.capture_names().any(|c| c == Some("project")));
                if spec.project.is_none() && !has_group {
                    return Err(format!(
                        "第 {n} 条规则既没写 project，正则里也没有 (?<project>…) 分组"
                    ));
                }
                Ok(Rule {
                    app,
                    title,
                    workspace,
                    profile,
                    project: spec.project.clone(),
                })
            })
            .collect::<Result<_, String>>()?;
        let roots = roots
            .iter()
            .map(|root| {
                let expanded = expand(root, &home)
                    .ok_or_else(|| format!("项目根 `{root}` 要写成绝对路径或以 ~ 开头"))?;
                if expanded.is_empty() {
                    return Err(format!("项目根 `{root}` 不能是 /"));
                }
                Ok(expanded)
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { rules, roots, home })
    }

    /// 填上 `project`（已经有的不动）。
    pub fn apply(&self, mut activity: Activity) -> Activity {
        if activity.project.is_none() {
            activity.project = self.infer(&activity);
        }
        activity
    }

    /// 推断项目，推断不出来是 `None`。
    pub fn infer(&self, activity: &Activity) -> Option<String> {
        if let Some(project) = self.rules.iter().find_map(|r| r.project(activity)) {
            return Some(project);
        }
        if let Some(cwd) = activity
            .payload
            .get(payload_keys::CWD)
            .and_then(Value::as_str)
            && let Some(project) = self.project_of_path(cwd, false)
        {
            return Some(project);
        }
        activity
            .title
            .as_deref()?
            .split_whitespace()
            .find_map(|word| {
                let word = word.trim_matches(|c: char| "\"'`()[]{}<>,;:".contains(c));
                self.project_of_path(word, true)
            })
    }

    /// 路径落在哪个项目根下面。`abbreviated` 为真时，每一级也可以是真实目录名的前缀。
    fn project_of_path(&self, path: &str, abbreviated: bool) -> Option<String> {
        let parts = expand(path, &self.home)?;
        self.roots.iter().find_map(|root| {
            let project = parts.get(root.len())?;
            let mut shortened = false;
            for (part, dir) in parts.iter().zip(root) {
                if part == dir {
                    continue;
                }
                if abbreviated && dir.starts_with(part.as_str()) {
                    shortened = true;
                    continue;
                }
                return None;
            }
            // 缩写的路径里，除了最后一级都是缩写：项目名后面还有下级时，项目名本身也是缩写，不可信
            if shortened && parts.len() > root.len() + 1 {
                return None;
            }
            (project != "..").then(|| project.clone())
        })
    }
}

/// 展开开头的 `~`，拆成目录名。既不是绝对路径也不以 `~` 开头时是 `None`。
fn expand(path: &str, home: &[String]) -> Option<Vec<String>> {
    if path == "~" {
        return Some(home.to_vec());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let mut parts = home.to_vec();
        parts.extend(components(rest));
        return Some(parts);
    }
    path.starts_with('/').then(|| components(path))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const HOME: &str = "/home/me";

    fn projects(rules: &[RuleSpec]) -> Projects {
        Projects::new(
            rules,
            &["~/work/projects".to_owned(), "/srv/work".to_owned()],
            HOME,
        )
        .unwrap()
    }

    fn titled(title: &str) -> Activity {
        Activity::new("focus", "window_focus", 0, 480)
            .with_app("com.mitchellh.ghostty")
            .with_title(title)
    }

    fn infer(title: &str) -> Option<String> {
        projects(&[]).infer(&titled(title))
    }

    #[test]
    fn paths_in_titles() {
        assert_eq!(infer("~/work/projects/lifelog").as_deref(), Some("lifelog"));
        assert_eq!(infer("~/w/p/lifelog").as_deref(), Some("lifelog"));
        assert_eq!(
            infer("nvim . ~/w/p/weather-station").as_deref(),
            Some("weather-station")
        );
        assert_eq!(
            infer("vim /home/me/work/projects/demo/src/main.rs").as_deref(),
            Some("demo"),
            "完整路径可以更深"
        );
        assert_eq!(
            infer("(/srv/work/api):").as_deref(),
            Some("api"),
            "去掉两边的标点"
        );
        assert_eq!(infer("~/w/p").as_deref(), None, "项目根本身不是项目");
        assert_eq!(infer("~/x/y/other-app").as_deref(), None, "不在项目根下");
        assert_eq!(
            infer("~/w/p/l/features").as_deref(),
            None,
            "项目名被缩写了，猜不出来"
        );
        assert_eq!(infer("✳ Kakoune 文档位置").as_deref(), None);
        assert_eq!(infer("work/projects/x").as_deref(), None, "相对路径不认");
    }

    #[test]
    fn working_directory_comes_before_the_title() {
        let a = titled("~/w/p/from-title")
            .with_payload(payload_keys::CWD, "/home/me/work/projects/from-cwd/sub");
        assert_eq!(projects(&[]).infer(&a).as_deref(), Some("from-cwd"));
        // 工作目录不在项目根下：再看标题
        let a = titled("~/w/p/from-title").with_payload(payload_keys::CWD, "/tmp");
        assert_eq!(projects(&[]).infer(&a).as_deref(), Some("from-title"));
        // 工作目录是完整路径，不按缩写认
        let a = titled("x").with_payload(payload_keys::CWD, "/home/me/w/p/short");
        assert_eq!(projects(&[]).infer(&a), None);
    }

    #[test]
    fn rules_come_first() {
        let p = projects(&[
            RuleSpec {
                app: Some("qutebrowser".to_owned()),
                title: Some(r"^\[(?<project>[^\]]+)\]".to_owned()),
                ..RuleSpec::default()
            },
            RuleSpec {
                title: Some("(?i)kakoune".to_owned()),
                project: Some("editor".to_owned()),
                ..RuleSpec::default()
            },
        ]);
        let browser = Activity::new("focus", "window_focus", 0, 480)
            .with_app("org.qutebrowser.qutebrowser")
            .with_title("[lifelog] docs.rs");
        assert_eq!(p.infer(&browser).as_deref(), Some("lifelog"));
        assert_eq!(
            p.infer(&titled("~/w/p/x - Kakoune")).as_deref(),
            Some("editor")
        );
        // 第一条要求 app 是 qutebrowser
        assert_eq!(p.infer(&titled("[y] ~/w/p/x")).as_deref(), Some("x"));
    }

    #[test]
    fn workspace_names_classify_windows_whose_titles_say_nothing() {
        let p = projects(&[RuleSpec {
            workspace: Some("^work-(?<project>.+)$".to_owned()),
            ..RuleSpec::default()
        }]);
        let claude = titled("✳ 气象站固件调试")
            .with_payload(payload_keys::WORKSPACE, "work-weather-station");
        assert_eq!(p.infer(&claude).as_deref(), Some("weather-station"));
        let other = titled("✳ 气象站固件调试").with_payload(payload_keys::WORKSPACE, "chat");
        assert_eq!(p.infer(&other), None);
        assert_eq!(p.infer(&titled("✳ x")), None, "没有工作区名");
        // 规则在路径之前：工作区说了算
        let both =
            titled("~/w/p/lifelog").with_payload(payload_keys::WORKSPACE, "work-weather-station");
        assert_eq!(p.infer(&both).as_deref(), Some("weather-station"));
    }

    #[test]
    fn browser_profiles_classify_visits() {
        let p = projects(&[RuleSpec {
            app: Some("^qutebrowser$".to_owned()),
            profile: Some("^(?<project>.+)$".to_owned()),
            ..RuleSpec::default()
        }]);
        let visit = |app: &str, profile: Option<&str>| {
            let mut a = Activity::new("browsing", "visit", 0, 0)
                .with_app(app)
                .with_title("docs.rs");
            if let Some(profile) = profile {
                a = a.with_payload(payload_keys::PROFILE, profile);
            }
            a
        };
        assert_eq!(
            p.infer(&visit("qutebrowser", Some("lifelog"))).as_deref(),
            Some("lifelog")
        );
        assert_eq!(
            p.infer(&visit("qutebrowser", None)),
            None,
            "默认实例没有 profile"
        );
        assert_eq!(
            p.infer(&visit("chrome", Some("工作"))),
            None,
            "规则限定了浏览器"
        );
    }

    #[test]
    fn existing_project_is_kept() {
        let a = titled("~/w/p/x").with_project("given");
        assert_eq!(projects(&[]).apply(a).project.as_deref(), Some("given"));
        let a = titled("~/w/p/x");
        assert_eq!(projects(&[]).apply(a).project.as_deref(), Some("x"));
    }

    #[test]
    fn bad_config_is_reported() {
        let err = |rules: &[RuleSpec], roots: &[&str]| {
            Projects::new(
                rules,
                &roots.iter().map(|r| (*r).to_owned()).collect::<Vec<_>>(),
                HOME,
            )
            .err()
            .unwrap()
        };
        assert!(err(&[RuleSpec::default()], &[]).contains("app、title、workspace 或 profile"));
        assert!(
            err(
                &[RuleSpec {
                    title: Some("x".to_owned()),
                    ..RuleSpec::default()
                }],
                &[]
            )
            .contains("(?<project>…)")
        );
        assert!(
            err(
                &[RuleSpec {
                    app: Some("(".to_owned()),
                    project: Some("p".to_owned()),
                    ..RuleSpec::default()
                }],
                &[]
            )
            .contains("app 不是合法的正则")
        );
        assert!(err(&[], &["codes"]).contains("绝对路径"));
        assert!(err(&[], &["/"]).contains("不能是 /"));
    }
}
