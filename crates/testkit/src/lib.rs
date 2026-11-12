//! 测试用的启动方式。
//!
//! 只装传进来的组件（顺便验证了「每个组件可以单独启用」），事件手动投递，
//! 监听记录收在内存里可以断言。换系统服务的办法和正式运行一样：
//! 往组件列表里放另一个提供同一契约的组件。testkit 不认识任何具体组件。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use kernel::{Delivery, Options, Running};
use kernel_api::{AppError, Component, ConfigSection, Contract, TraceRecord, TraceSink};
use serde_json::Value;

#[derive(Default)]
struct Recorder(Mutex<Vec<TraceRecord>>);

impl TraceSink for Recorder {
    fn record(&self, record: TraceRecord) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record);
    }
}

impl Recorder {
    fn all(&self) -> Vec<TraceRecord> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// 准备一组要装的组件。
#[derive(Default)]
pub struct Builder {
    components: Vec<Arc<dyn Component>>,
    configs: HashMap<String, ConfigSection>,
    background: bool,
    export: Option<kernel::Export>,
    connect: Option<std::path::PathBuf>,
}

/// 开始准备。
pub fn app() -> Builder {
    Builder::default()
}

impl Builder {
    /// 装一个组件。
    #[must_use]
    pub fn with(mut self, component: impl Component) -> Self {
        self.components.push(Arc::new(component));
        self
    }

    /// 给某个组件配置，写法和配置文件里那一段的内容相同。
    pub fn config(mut self, id: &str, value: Value) -> Result<Self, AppError> {
        self.configs
            .insert(id.to_owned(), ConfigSection::from_value(value)?);
        Ok(self)
    }

    /// 改用后台投递（默认是手动投递）。测后台任务和真实时序时用。
    #[must_use]
    pub fn background_delivery(mut self) -> Self {
        self.background = true;
        self
    }

    /// 把这些契约导出到 `socket`，像常驻进程那样（测跨进程调用时用）。
    #[must_use]
    pub fn export(mut self, socket: impl Into<std::path::PathBuf>, contracts: &[&str]) -> Self {
        self.export = Some(kernel::Export {
            socket: socket.into(),
            contracts: contracts.iter().map(|c| (*c).to_owned()).collect(),
        });
        self
    }

    /// `Import` 组件连这个 socket，像命令行进程那样。
    #[must_use]
    pub fn connect(mut self, socket: impl Into<std::path::PathBuf>) -> Self {
        self.connect = Some(socket.into());
        self
    }

    /// 启动。
    pub async fn boot(self) -> Result<TestApp, AppError> {
        let (trace, faults) = (Arc::new(Recorder::default()), Arc::new(Recorder::default()));
        let options = Options {
            trace: Some(trace.clone()),
            faults: Some(faults.clone()),
            delivery: if self.background {
                Delivery::Background
            } else {
                Delivery::Manual
            },
            queue_capacity: 1 << 16,
            stop_timeout: Duration::from_secs(5),
            export: self.export,
            connect: self.connect,
        };
        let running = kernel::start(self.components, &self.configs, options).await?;
        Ok(TestApp {
            running,
            trace,
            faults,
        })
    }
}

/// 一组已经启动的组件。
pub struct TestApp {
    running: Running,
    trace: Arc<Recorder>,
    faults: Arc<Recorder>,
}

impl TestApp {
    /// 已启动的组件 id，按启动顺序。
    pub fn started(&self) -> Vec<&'static str> {
        self.running.started()
    }

    /// 拿某个契约的句柄，像启动器那样。
    pub fn resolve<T>(&self, contract: &'static Contract<T>) -> Result<Arc<T>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.running.resolve(contract)
    }

    /// 把已经发布的事件投递完。
    pub async fn settle(&self) {
        self.running.settle().await;
    }

    /// 到目前为止的全部监听记录。
    pub fn trace(&self) -> Vec<TraceRecord> {
        self.trace.all()
    }

    /// 发布过的事件名，按先后。
    pub fn published(&self) -> Vec<String> {
        self.trace()
            .into_iter()
            .filter_map(|r| match r {
                TraceRecord::Publish { event, .. } => Some(event),
                _ => None,
            })
            .collect()
    }

    /// 请求记录，写成 `调用方 -> 提供方 契约.方法 = 结果`。
    pub fn requests(&self) -> Vec<String> {
        self.trace()
            .into_iter()
            .filter_map(|r| match r {
                TraceRecord::Request {
                    caller,
                    provider,
                    contract,
                    method,
                    outcome,
                    ..
                } => Some(format!(
                    "{caller} -> {provider} {contract}.{method} = {outcome}"
                )),
                _ => None,
            })
            .collect()
    }

    /// 故障记录：投递失败、后台任务崩溃或超时。
    pub fn faults(&self) -> Vec<TraceRecord> {
        self.faults.all()
    }

    /// 停止。
    pub async fn stop(self) {
        self.running.stop().await;
    }
}
