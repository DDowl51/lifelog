//! 系统服务 `cli-clap`：用 clap 解析命令行，分派给各组件登记的子命令。
//!
//! 它不认识任何子命令。命令行解析库只出现在这个包里：换一个库 = 换这个组件。

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::sync::Arc;

use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, Command};
use cli_contract::{
    ArgKind, CLI, CLI_COMMANDS, Cli, CommandProvider, CommandSpec, EXIT_USAGE, Format, Invocation,
    exit_code,
};
use kernel_api::{
    AppError, Collector, Component, ConfigSection, Host, Kind, Manifest, async_trait,
    register_component,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {}

/// 进程所处的环境里和输出有关的部分。单独拿出来，测试时可以给定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Env {
    /// stdout 是不是终端。
    tty: bool,
    /// 设置了 `NO_COLOR`。
    no_color: bool,
    /// 终端有多少列。
    width: u16,
}

/// 标准流。单独拿出来，测试时可以换成记在内存里的。
trait Io: Send + Sync + 'static {
    fn env(&self) -> Env;
    fn write(&self, outcome: &Outcome);
}

/// 进程真正的标准流。
struct ProcessIo;

impl Io for ProcessIo {
    fn env(&self) -> Env {
        Env {
            tty: std::io::stdout().is_terminal(),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            width: std::env::var("COLUMNS")
                .ok()
                .and_then(|c| c.parse().ok())
                .unwrap_or(80),
        }
    }

    fn write(&self, outcome: &Outcome) {
        // 管道另一头关了（`lifelog day | head -1`）不算错
        let _ = std::io::stdout()
            .lock()
            .write_all(outcome.stdout.as_bytes());
        let _ = std::io::stderr()
            .lock()
            .write_all(outcome.stderr.as_bytes());
    }
}

/// 一次调用的结果：进程该怎么退出、该往哪写什么。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Outcome {
    code: u8,
    stdout: String,
    stderr: String,
}

fn program_name(argv: &[String]) -> String {
    argv.first()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map_or_else(|| "cli".to_owned(), |n| n.to_string_lossy().into_owned())
}

/// 所有子命令共用的参数。
fn global_args() -> [Arg; 5] {
    [
        Arg::new("json")
            .long("json")
            .global(true)
            .action(ArgAction::SetTrue)
            .conflicts_with("text")
            .help("输出 JSON（stdout 不是终端时默认如此）"),
        Arg::new("text")
            .long("text")
            .global(true)
            .action(ArgAction::SetTrue)
            .help("输出给人看的文本（stdout 是终端时默认如此）"),
        Arg::new("color")
            .long("color")
            .global(true)
            .value_parser(["auto", "always", "never"])
            .default_value("auto")
            .help("文本输出要不要用颜色"),
        // 下面两个由启动器在启动前处理；这里只是让它们出现在帮助里、不被当成错误
        Arg::new("trace")
            .long("trace")
            .global(true)
            .action(ArgAction::SetTrue)
            .help("把组件之间的每次请求和事件按行写到 stderr"),
        Arg::new("config")
            .long("config")
            .global(true)
            .value_name("文件")
            .help("用另一个配置文件"),
    ]
}

fn build(program: &str, specs: &[CommandSpec]) -> Command {
    let mut root = Command::new(program.to_owned())
        .subcommand_required(true)
        .arg_required_else_help(true)
        .disable_help_subcommand(true)
        .args(global_args());
    for spec in specs {
        let mut sub = Command::new(spec.name.clone()).about(spec.about.clone());
        for arg in &spec.args {
            let base = Arg::new(arg.name.clone()).help(arg.help.clone());
            sub = sub.arg(match &arg.kind {
                ArgKind::Positional => base,
                ArgKind::Rest => base.num_args(1..).action(ArgAction::Append),
                ArgKind::Flag => base.long(arg.name.clone()).action(ArgAction::SetTrue),
                ArgKind::Option { values, default } => {
                    let mut a = base.long(arg.name.clone()).value_name("值");
                    if !values.is_empty() {
                        a = a.value_parser(PossibleValuesParser::new(values.clone()));
                    }
                    if let Some(default) = default {
                        a = a.default_value(default.clone());
                    }
                    a
                }
            });
        }
        root = root.subcommand(sub);
    }
    root
}

fn format_of(json: bool, text: bool, color: &str, env: Env) -> Format {
    if json || (!text && !env.tty) {
        return Format::Json;
    }
    let color = match color {
        "always" => true,
        "never" => false,
        _ => env.tty && !env.no_color,
    };
    Format::Text {
        color,
        width: env.width,
    }
}

struct Service {
    providers: Collector<dyn CommandProvider>,
    io: Arc<dyn Io>,
}

impl Service {
    /// 解析、分派、执行。不碰标准流，结果全在返回值里。
    async fn execute(&self, argv: Vec<String>, env: Env) -> Result<Outcome, AppError> {
        let program = program_name(&argv);

        let providers = self.providers.all();
        let mut owners: BTreeMap<String, Arc<dyn CommandProvider>> = BTreeMap::new();
        let mut specs = Vec::new();
        for provider in providers {
            for spec in provider.commands().await? {
                if owners.insert(spec.name.clone(), provider.clone()).is_some() {
                    return Err(AppError::config(format!(
                        "有两个组件都登记了子命令 `{}`",
                        spec.name
                    )));
                }
                specs.push(spec);
            }
        }
        specs.sort_by(|a, b| a.name.cmp(&b.name));

        let matches = match build(&program, &specs).try_get_matches_from(&argv) {
            Ok(matches) => matches,
            Err(e) => {
                let text = e.render().to_string();
                return Ok(if e.use_stderr() {
                    Outcome {
                        code: EXIT_USAGE,
                        stderr: text,
                        ..Outcome::default()
                    }
                } else {
                    // --help
                    Outcome {
                        stdout: text,
                        ..Outcome::default()
                    }
                });
            }
        };
        let Some((name, sub)) = matches.subcommand() else {
            return Err(AppError::config("clap 没有给出子命令"));
        };
        let (Some(spec), Some(owner)) = (specs.iter().find(|s| s.name == name), owners.get(name))
        else {
            return Err(AppError::config(format!("子命令 `{name}` 没有登记者")));
        };

        let mut args = BTreeMap::new();
        for arg in &spec.args {
            let value = match arg.kind {
                ArgKind::Flag => sub.get_flag(&arg.name).then(|| "true".to_owned()),
                ArgKind::Rest => sub
                    .get_many::<String>(&arg.name)
                    .map(|words| words.cloned().collect::<Vec<_>>().join(" ")),
                _ => sub.get_one::<String>(&arg.name).cloned(),
            };
            if let Some(value) = value {
                args.insert(arg.name.clone(), value);
            }
        }
        let color = sub
            .get_one::<String>("color")
            .map_or("auto", String::as_str);
        let format = format_of(sub.get_flag("json"), sub.get_flag("text"), color, env);

        let invocation = Invocation {
            command: name.to_owned(),
            args,
            format,
        };
        Ok(match owner.run(invocation).await {
            Ok(output) => {
                let mut stdout = output.text;
                if !stdout.is_empty() && !stdout.ends_with('\n') {
                    stdout.push('\n');
                }
                Outcome {
                    stdout,
                    ..Outcome::default()
                }
            }
            Err(e) => {
                let stderr = match format {
                    Format::Json => {
                        format!(
                            "{}\n",
                            json!({ "error": { "kind": e.kind, "message": e.message } })
                        )
                    }
                    Format::Text { .. } => format!("{program}: {}\n", e.message),
                };
                Outcome {
                    code: exit_code(e.kind),
                    stderr,
                    ..Outcome::default()
                }
            }
        })
    }
}

#[async_trait]
impl Cli for Service {
    async fn run(&self, argv: Vec<String>) -> Result<u8, AppError> {
        let outcome = self.execute(argv, self.io.env()).await?;
        self.io.write(&outcome);
        Ok(outcome.code)
    }
}

fn start(config: &ConfigSection, host: &mut Host<'_>, io: Arc<dyn Io>) -> Result<(), AppError> {
    let _: Config = config.parse()?;
    let providers = host.collector(&CLI_COMMANDS)?;
    host.provide(&CLI, Arc::new(Service { providers, io }))
}

fn manifest() -> Manifest {
    Manifest {
        id: "cli-clap",
        kind: Kind::Service,
        summary: "解析命令行，分派给各组件登记的子命令",
        provides: vec![CLI.id(), CLI_COMMANDS.id()],
        requires: vec![],
        optional_requires: vec![],
    }
}

pub struct CliClap;

impl Component for CliClap {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        manifest()
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        start(config, host, Arc::new(ProcessIo))
    }
}

register_component!(CliClap);

#[cfg(test)]
mod tests;
