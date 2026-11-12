//! 监听记录和故障怎么输出：按行写到 stderr。

use std::io::Write;

use kernel_api::{AppError, TraceRecord, TraceSink};

fn write_line(line: &str) {
    let mut err = std::io::stderr().lock();
    // stderr 写不进去时没有别处可报。
    let _ = writeln!(err, "{line}");
}

/// `--trace`：每条记录一行 JSON。
pub struct StderrTrace;

impl TraceSink for StderrTrace {
    fn record(&self, record: TraceRecord) {
        if let Ok(line) = serde_json::to_string(&record) {
            write_line(&line);
        }
    }
}

/// 故障（投递失败、后台任务崩溃或超时）：一行人能读的文字。
pub struct StderrFaults {
    pub app: &'static str,
}

impl TraceSink for StderrFaults {
    fn record(&self, record: TraceRecord) {
        let text = match record {
            TraceRecord::Deliver {
                event,
                component,
                subscriber,
                outcome,
                error,
                ..
            } => format!(
                "事件 {event} 投给 {component}:{subscriber} 失败（{outcome}）：{}",
                error.unwrap_or_default()
            ),
            TraceRecord::Lifecycle { component, phase } => format!("{component}：{phase}"),
            other => format!("{other:?}"),
        };
        write_line(&format!("{}: {text}", self.app));
    }
}

/// 把一个错误报到 stderr：`<程序>: <消息>`。启动器在启动失败时用。
pub fn report(app: &str, error: &AppError) {
    write_line(&format!("{app}: {}", error.message));
}

/// 往 stdout 写一段文本（启动器自己的帮助信息）。
pub fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
}
