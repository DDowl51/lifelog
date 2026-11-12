use std::sync::Mutex;

use cli_contract::{ArgSpec, Output};
use kernel_api::ErrorKind;
use testkit::TestApp;

use super::*;

/// 把本来要写到标准流的内容记在内存里，环境由测试给定。
#[derive(Clone)]
struct Capture {
    env: Env,
    written: Arc<Mutex<Vec<Outcome>>>,
}

impl Io for Capture {
    fn env(&self) -> Env {
        self.env
    }

    fn write(&self, outcome: &Outcome) {
        self.written.lock().unwrap().push(outcome.clone());
    }
}

/// 和 `CliClap` 是同一个组件，只是标准流换成了 `Capture`。
impl Component for Capture {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        start(config, host, Arc::new(self.clone()))
    }
}

fn arg(name: &str, kind: ArgKind) -> ArgSpec {
    ArgSpec {
        name: name.to_owned(),
        help: format!("{name} 的说明"),
        kind,
    }
}

/// 登记两个子命令的假功能：`echo` 把收到的调用原样打出来，`fail` 按参数报错。
struct Demo;

#[async_trait]
impl CommandProvider for Demo {
    async fn commands(&self) -> Result<Vec<CommandSpec>, AppError> {
        let by = ArgKind::Option {
            values: vec!["app".to_owned(), "title".to_owned()],
            default: Some("app".to_owned()),
        };
        Ok(vec![
            CommandSpec {
                name: "echo".to_owned(),
                about: "把参数打出来".to_owned(),
                args: vec![
                    arg("what", ArgKind::Positional),
                    arg("by", by),
                    arg("loud", ArgKind::Flag),
                ],
            },
            CommandSpec {
                name: "words".to_owned(),
                about: "把剩下的词连起来".to_owned(),
                args: vec![
                    arg("words", ArgKind::Rest),
                    arg(
                        "by",
                        ArgKind::Option {
                            values: vec![],
                            default: None,
                        },
                    ),
                ],
            },
            CommandSpec {
                name: "fail".to_owned(),
                about: "总是失败".to_owned(),
                args: vec![arg("kind", ArgKind::Positional)],
            },
        ])
    }

    async fn run(&self, invocation: Invocation) -> Result<Output, AppError> {
        if invocation.command == "fail" {
            let kind = match invocation.args.get("kind").map(String::as_str) {
                Some("not_found") => ErrorKind::NotFound,
                Some("unavailable") => ErrorKind::Unavailable,
                _ => ErrorKind::Validation,
            };
            return Err(AppError::new(kind, "故意的"));
        }
        let args: Vec<String> = invocation
            .args
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        Ok(Output {
            text: format!("{} | {:?}", args.join(" "), invocation.format),
        })
    }
}

/// 把一个 `CommandProvider` 包成组件。
struct Contributor(&'static str);

impl Component for Contributor {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: self.0,
            kind: Kind::Feature,
            summary: "测试用",
            provides: vec![],
            requires: vec![CLI_COMMANDS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(&CLI_COMMANDS, Arc::new(Demo))
    }
}

const TTY: Env = Env {
    tty: true,
    no_color: false,
    width: 100,
};
const PIPE: Env = Env {
    tty: false,
    no_color: false,
    width: 80,
};

async fn boot(env: Env, contributors: &[&'static str]) -> (TestApp, Capture) {
    let capture = Capture {
        env,
        written: Arc::default(),
    };
    let mut builder = testkit::app().with(capture.clone());
    for id in contributors {
        builder = builder.with(Contributor(id));
    }
    (builder.boot().await.unwrap(), capture)
}

async fn run(args: &[&str], env: Env) -> Outcome {
    let (app, capture) = boot(env, &["demo"]).await;
    let argv = std::iter::once("/usr/bin/lifelog")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect();
    let code = app.resolve(&CLI).unwrap().run(argv).await.unwrap();
    let written = capture.written.lock().unwrap().clone();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].code, code);
    written[0].clone()
}

#[tokio::test]
async fn dispatches_with_parsed_arguments() {
    let out = run(&["echo", "hello", "--by", "title", "--loud"], TTY).await;
    assert_eq!(out.code, 0);
    assert_eq!(
        out.stdout,
        "by=title loud=true what=hello | Text { color: true, width: 100 }\n"
    );
    assert_eq!(out.stderr, "");
}

#[tokio::test]
async fn the_rest_of_the_words_are_joined() {
    let out = run(&["words", "jiff", "文档", "--by", "x", "上周"], TTY).await;
    assert_eq!(
        out.stdout, "by=x words=jiff 文档 上周 | Text { color: true, width: 100 }\n",
        "选项可以夹在中间"
    );
    let out = run(&["words"], TTY).await;
    assert_eq!(
        out.stdout, " | Text { color: true, width: 100 }\n",
        "可以不给"
    );
}

#[tokio::test]
async fn omitted_arguments_are_absent_and_defaults_are_filled_in() {
    let out = run(&["echo"], TTY).await;
    assert_eq!(out.stdout, "by=app | Text { color: true, width: 100 }\n");
}

#[tokio::test]
async fn output_format_follows_the_terminal_and_the_flags() {
    let format = |out: Outcome| out.stdout.split(" | ").nth(1).unwrap().trim().to_owned();
    // 不是终端：默认 JSON，对管道友好
    assert_eq!(format(run(&["echo"], PIPE).await), "Json");
    assert_eq!(format(run(&["echo", "--json"], TTY).await), "Json");
    // 管道里也可以明确要文本；不是终端就不带颜色
    assert_eq!(
        format(run(&["echo", "--text"], PIPE).await),
        "Text { color: false, width: 80 }"
    );
    assert_eq!(
        format(run(&["echo", "--text", "--color", "always"], PIPE).await),
        "Text { color: true, width: 80 }"
    );
    assert_eq!(
        format(run(&["echo", "--color", "never"], TTY).await),
        "Text { color: false, width: 100 }"
    );
    let no_color = Env {
        no_color: true,
        ..TTY
    };
    assert_eq!(
        format(run(&["echo"], no_color).await),
        "Text { color: false, width: 100 }"
    );
    // 全局参数写在子命令前面也行
    assert_eq!(format(run(&["--json", "echo"], TTY).await), "Json");
}

#[tokio::test]
async fn errors_go_to_stderr_with_the_matching_exit_code() {
    let out = run(&["fail"], TTY).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (65, "", "lifelog: 故意的\n")
    );
    assert_eq!(run(&["fail", "not_found"], TTY).await.code, 66);
    assert_eq!(run(&["fail", "unavailable"], TTY).await.code, 69);

    // JSON 模式下错误也是 JSON
    let out = run(&["fail", "not_found", "--json"], TTY).await;
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "{\"error\":{\"kind\":\"not_found\",\"message\":\"故意的\"}}\n"
    );
}

#[tokio::test]
async fn usage_errors_exit_64() {
    for args in [
        &["nope"][..],
        &["echo", "--by", "color"],
        &["echo", "a", "b"],
        &["echo", "--json", "--text"],
        &[],
    ] {
        let out = run(args, TTY).await;
        assert_eq!(out.code, EXIT_USAGE, "{args:?}");
        assert_eq!(out.stdout, "", "{args:?}");
        assert!(!out.stderr.is_empty(), "{args:?}");
    }
    let out = run(&["echo", "--by", "color"], TTY).await;
    assert!(
        out.stderr.contains("app") && out.stderr.contains("title"),
        "报错里列出可选的值：{}",
        out.stderr
    );
}

#[tokio::test]
async fn help_lists_contributed_commands() {
    let out = run(&["--help"], TTY).await;
    assert_eq!(out.code, 0);
    assert_eq!(out.stderr, "");
    for expected in [
        "echo",
        "把参数打出来",
        "fail",
        "总是失败",
        "--json",
        "--trace",
        "--config",
    ] {
        assert!(
            out.stdout.contains(expected),
            "帮助里没有 {expected}：\n{}",
            out.stdout
        );
    }
    let out = run(&["echo", "--help"], TTY).await;
    assert!(
        out.stdout.contains("--by") && out.stdout.contains("what 的说明"),
        "{}",
        out.stdout
    );
}

#[tokio::test]
async fn launcher_flags_are_accepted_and_ignored() {
    let out = run(&["echo", "--trace", "--config", "/tmp/x.toml"], TTY).await;
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "by=app | Text { color: true, width: 100 }\n");
}

#[tokio::test]
async fn two_components_registering_the_same_command_is_an_error() {
    let (app, _) = boot(TTY, &["demo", "demo-again"]).await;
    let err = app
        .resolve(&CLI)
        .unwrap()
        .run(vec!["lifelog".to_owned(), "echo".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("`echo`"), "{err}");
}

#[tokio::test]
async fn works_with_no_commands_at_all() {
    let (app, capture) = boot(TTY, &[]).await;
    let code = app
        .resolve(&CLI)
        .unwrap()
        .run(vec!["lifelog".to_owned()])
        .await
        .unwrap();
    assert_eq!(code, EXIT_USAGE);
    assert!(
        capture.written.lock().unwrap()[0]
            .stderr
            .contains("lifelog")
    );
}

#[test]
fn exit_codes_follow_sysexits() {
    use ErrorKind::*;
    let codes: Vec<u8> = [
        Validation,
        NotFound,
        VersionConflict,
        Unauthorized,
        TempFail,
        Unavailable,
        Config,
    ]
    .into_iter()
    .map(exit_code)
    .collect();
    assert_eq!(codes, [65, 66, 3, 77, 75, 69, 78]);
}
