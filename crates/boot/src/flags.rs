//! 启动器自己要看的命令行参数。它们在 kernel 启动之前就得知道。

use std::path::PathBuf;

use kernel_api::AppError;

/// 启动器的参数。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// `--trace`
    pub trace: bool,
    /// `--config <文件>` 或 `--config=<文件>`
    pub config: Option<PathBuf>,
    /// 其余的参数，原样保留顺序。
    pub rest: Vec<String>,
}

/// 从参数里挑出 `--trace` 和 `--config`。`args` 不含程序名。
pub fn launcher_flags(args: &[String]) -> Result<Flags, AppError> {
    let mut flags = Flags::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--trace" {
            flags.trace = true;
        } else if arg == "--config" {
            let path = iter
                .next()
                .filter(|p| !p.starts_with('-'))
                .ok_or_else(|| AppError::validation("--config 后面要跟配置文件的路径"))?;
            flags.config = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--config=") {
            flags.config = Some(PathBuf::from(path));
        } else {
            flags.rest.push(arg.clone());
        }
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Flags, AppError> {
        launcher_flags(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn picks_out_trace_and_config() {
        let flags = parse(&[
            "day",
            "--trace",
            "yesterday",
            "--config",
            "/tmp/a.toml",
            "--json",
        ])
        .unwrap();
        assert!(flags.trace);
        assert_eq!(flags.config, Some(PathBuf::from("/tmp/a.toml")));
        assert_eq!(flags.rest, ["day", "yesterday", "--json"]);

        assert_eq!(
            parse(&["--config=b.toml"]).unwrap().config,
            Some(PathBuf::from("b.toml"))
        );
        assert_eq!(parse(&[]).unwrap(), Flags::default());
    }

    #[test]
    fn config_needs_a_path() {
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--config", "--trace"]).is_err());
    }
}
