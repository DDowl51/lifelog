use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use std::path::PathBuf;

use kernel_api::{
    AppError, BoxFuture, Cancel, CollectSource, Component, ConfigSection, Contract, ContractId,
    Dispatcher, ErasedAny, ErasedHandler, Host, HostBackend, Manifest, PublishSink, TraceRecord,
    TraceSink, Tracer, Transport,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::plan;
use crate::remote::{self, Dispatchers, Export, SocketTransport};

/// 事件怎么投递。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// 后台任务持续投递。正常运行用。
    Background,
    /// 只有调用 [`Running::settle`] 时才投递。测试用，结果可重复。
    Manual,
}

/// 启动参数。全部由启动器决定，kernel 自己没有默认值。
pub struct Options {
    /// 开启追踪时，每次请求、发布、投递的记录交给它。
    pub trace: Option<Arc<dyn TraceSink>>,
    /// 出错的记录（投递失败、后台任务崩溃或超时）交给它，不管有没有开追踪。
    pub faults: Option<Arc<dyn TraceSink>>,
    pub delivery: Delivery,
    /// 事件队列的长度。满了 `publish` 会等待。
    pub queue_capacity: usize,
    /// 停止时等后台任务收尾的最长时间，超时的直接中止。
    pub stop_timeout: Duration,
    /// 把哪些契约导出到本地 socket，给别的进程用。
    pub export: Option<Export>,
    /// 导入契约时连哪个 socket（`Import` 组件用）。
    pub connect: Option<PathBuf>,
}

type ReadyFn = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), AppError>> + Send>;
type StopFn = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;
type TaskFn = Box<dyn FnOnce(Cancel) -> BoxFuture<'static, ()> + Send>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Envelope {
    event: Arc<str>,
    payload: ErasedAny,
}

enum Item {
    Event(Envelope),
    /// 队列里排在它前面的都投递完了就回信。
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
struct Sub {
    component: Arc<str>,
    subscriber: &'static str,
    handler: ErasedHandler,
}

#[derive(Default)]
struct PointList(Mutex<Vec<(ErasedAny, String)>>);

impl CollectSource for PointList {
    fn all(&self) -> Vec<(ErasedAny, String)> {
        lock(&self.0).clone()
    }
}

struct Shared {
    services: Mutex<HashMap<&'static str, (ErasedAny, String)>>,
    points: Mutex<HashMap<&'static str, Arc<PointList>>>,
    subs: Mutex<HashMap<String, Vec<Sub>>>,
    tx: mpsc::Sender<Item>,
    trace: Option<Arc<dyn TraceSink>>,
    faults: Option<Arc<dyn TraceSink>>,
    dispatchers: Arc<Dispatchers>,
    connect: Option<PathBuf>,
}

impl Shared {
    fn lifecycle(&self, component: &str, phase: &str) {
        if let Some(sink) = &self.trace {
            sink.record(TraceRecord::Lifecycle {
                component: component.to_owned(),
                phase: phase.to_owned(),
            });
        }
    }

    fn fault(&self, component: &str, what: String) {
        let record = TraceRecord::Lifecycle {
            component: component.to_owned(),
            phase: what,
        };
        if let Some(sink) = &self.trace {
            sink.record(record.clone());
        }
        if let Some(sink) = &self.faults {
            sink.record(record);
        }
    }

    fn point(&self, name: &'static str) -> Arc<PointList> {
        lock(&self.points).entry(name).or_default().clone()
    }
}

tokio::task_local! {
    /// 投递过程中、在处理函数里发布的事件先排在这里，紧接着投递。
    /// 这样处理函数发布事件时不会因为队列满而等自己。
    static LOCAL: RefCell<VecDeque<Envelope>>;
}

struct Pub {
    shared: Arc<Shared>,
    event: Arc<str>,
    publisher: Arc<str>,
}

impl PublishSink for Pub {
    fn publish(&self, payload: ErasedAny) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async move {
            if let Some(sink) = &self.shared.trace {
                sink.record(TraceRecord::Publish {
                    publisher: self.publisher.to_string(),
                    event: self.event.to_string(),
                });
            }
            let mut envelope = Some(Envelope {
                event: self.event.clone(),
                payload,
            });
            let _ = LOCAL.try_with(|q| {
                if let Some(e) = envelope.take() {
                    q.borrow_mut().push_back(e);
                }
            });
            match envelope {
                None => Ok(()),
                Some(e) => self
                    .shared
                    .tx
                    .send(Item::Event(e))
                    .await
                    .map_err(|_| AppError::unavailable("kernel 已经停止，事件没有发出去")),
            }
        })
    }
}

/// 投递一个事件，以及处理过程中连带发布的事件。
async fn deliver_one(shared: &Shared, first: Envelope) {
    LOCAL
        .scope(RefCell::new(VecDeque::from([first])), async {
            loop {
                let next = LOCAL.with(|q| q.borrow_mut().pop_front());
                let Some(envelope) = next else { break };
                let subs = lock(&shared.subs)
                    .get(&*envelope.event)
                    .cloned()
                    .unwrap_or_default();
                for sub in subs {
                    let started = Instant::now();
                    let result = (sub.handler)(envelope.payload.clone()).await;
                    let failed = result.is_err();
                    if shared.trace.is_none() && !(failed && shared.faults.is_some()) {
                        continue;
                    }
                    let (outcome, error) = match &result {
                        Ok(()) => ("ok", None),
                        Err(e) => (e.kind.as_str(), Some(e.message.clone())),
                    };
                    let record = TraceRecord::Deliver {
                        event: envelope.event.to_string(),
                        component: sub.component.to_string(),
                        subscriber: sub.subscriber.to_owned(),
                        outcome: outcome.to_owned(),
                        error,
                        micros: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                    };
                    if let Some(sink) = &shared.trace {
                        sink.record(record.clone());
                    }
                    if failed && let Some(sink) = &shared.faults {
                        sink.record(record);
                    }
                }
            }
        })
        .await;
}

async fn deliver_loop(shared: Arc<Shared>, mut rx: mpsc::Receiver<Item>, stop: CancellationToken) {
    loop {
        tokio::select! {
            biased;
            item = rx.recv() => match item {
                Some(Item::Event(envelope)) => deliver_one(&shared, envelope).await,
                Some(Item::Flush(done)) => { let _ = done.send(()); }
                None => break,
            },
            () = stop.cancelled() => break,
        }
    }
}

#[derive(Default)]
struct Hooks {
    ready: Vec<ReadyFn>,
    stop: Vec<StopFn>,
    tasks: Vec<(&'static str, TaskFn)>,
}

/// 一个组件在 `start` 期间看到的 kernel。
struct Ctx<'a> {
    shared: &'a Arc<Shared>,
    manifest: &'a Manifest,
    hooks: Hooks,
}

impl Ctx<'_> {
    fn err(&self, what: String) -> AppError {
        AppError::config(format!("组件 `{}` {what}", self.manifest.id))
    }

    fn uses(&self, id: ContractId) -> bool {
        self.manifest.requires.contains(&id) || self.manifest.optional_requires.contains(&id)
    }

    fn knows(&self, name: &str) -> bool {
        let m = self.manifest;
        m.provides
            .iter()
            .chain(&m.requires)
            .chain(&m.optional_requires)
            .any(|c| c.name == name)
    }
}

impl HostBackend for Ctx<'_> {
    fn component_id(&self) -> &str {
        self.manifest.id
    }

    fn provide(&mut self, id: ContractId, imp: ErasedAny) -> Result<(), AppError> {
        if id.many {
            return Err(self.err(format!(
                "对扩展点 `{id}` 调了 provide；扩展点的所有者用 collector"
            )));
        }
        if !self.manifest.provides.contains(&id) {
            return Err(self.err(format!(
                "提供了契约 `{id}`，但没有写进 Manifest 的 provides"
            )));
        }
        let mut services = lock(&self.shared.services);
        if services.contains_key(id.name) {
            return Err(self.err(format!("对契约 `{id}` 调了两次 provide")));
        }
        services.insert(id.name, (imp, self.manifest.id.to_owned()));
        Ok(())
    }

    fn resolve(
        &mut self,
        id: ContractId,
        optional: bool,
    ) -> Result<Option<(ErasedAny, String)>, AppError> {
        if id.many {
            return Err(self.err(format!(
                "对扩展点 `{id}` 调了 resolve；登记用 contribute，所有者取回用 collector"
            )));
        }
        let declared = if optional {
            self.uses(id)
        } else {
            self.manifest.requires.contains(&id)
        };
        if !declared {
            let list = if optional {
                "optional_requires"
            } else {
                "requires"
            };
            return Err(self.err(format!("用了契约 `{id}`，但没有写进 Manifest 的 {list}")));
        }
        match lock(&self.shared.services).get(id.name) {
            Some((any, provider)) => Ok(Some((any.clone(), provider.clone()))),
            None if optional => Ok(None),
            None => Err(self.err(format!(
                "需要契约 `{id}`，它的提供者声明了 provides 却没有在 start 里调用 provide"
            ))),
        }
    }

    fn contribute(&mut self, id: ContractId, imp: ErasedAny) -> Result<(), AppError> {
        if !id.many {
            return Err(self.err(format!("对服务契约 `{id}` 调了 contribute；它不是扩展点")));
        }
        if !self.uses(id) && !self.manifest.provides.contains(&id) {
            return Err(self.err(format!(
                "向扩展点 `{id}` 登记，但没有写进 Manifest 的 requires 或 optional_requires"
            )));
        }
        lock(&self.shared.point(id.name).0).push((imp, self.manifest.id.to_owned()));
        Ok(())
    }

    fn collector(&mut self, id: ContractId) -> Result<Arc<dyn CollectSource>, AppError> {
        if !id.many || !self.manifest.provides.contains(&id) {
            return Err(self.err(format!(
                "要取回扩展点 `{id}` 的登记，但它不是自己 provides 里的扩展点"
            )));
        }
        Ok(self.shared.point(id.name))
    }

    fn publisher(
        &mut self,
        owner: &'static str,
        event: String,
    ) -> Result<Arc<dyn PublishSink>, AppError> {
        if !self.manifest.provides.iter().any(|c| c.name == owner) {
            return Err(self.err(format!(
                "要发布事件 `{event}`，但它所属的契约 `{owner}` 不在自己的 provides 里"
            )));
        }
        Ok(Arc::new(Pub {
            shared: self.shared.clone(),
            event: event.into(),
            publisher: self.manifest.id.into(),
        }))
    }

    fn subscribe(
        &mut self,
        owner: &'static str,
        event: String,
        subscriber: &'static str,
        handler: ErasedHandler,
    ) -> Result<(), AppError> {
        if !self.knows(owner) {
            return Err(self.err(format!(
                "订阅了事件 `{event}`，但它所属的契约 `{owner}` 没有写进 Manifest 的 requires 或 optional_requires"
            )));
        }
        lock(&self.shared.subs).entry(event).or_default().push(Sub {
            component: self.manifest.id.into(),
            subscriber,
            handler,
        });
        Ok(())
    }

    fn on_ready(&mut self, f: ReadyFn) {
        self.hooks.ready.push(f);
    }

    fn on_stop(&mut self, f: StopFn) {
        self.hooks.stop.push(f);
    }

    fn spawn(&mut self, name: &'static str, f: TaskFn) {
        self.hooks.tasks.push((name, f));
    }

    fn trace_sink(&self) -> Option<Arc<dyn TraceSink>> {
        self.shared.trace.clone()
    }

    fn remote(&mut self) -> Result<Arc<dyn Transport>, AppError> {
        match &self.shared.connect {
            Some(socket) => Ok(Arc::new(SocketTransport {
                socket: socket.clone(),
            })),
            None => Err(self.err("要连到另一个进程，但这个进程没有配置要连哪里".to_owned())),
        }
    }

    fn dispatcher(&mut self, id: ContractId, dispatcher: Dispatcher) {
        lock(&self.shared.dispatchers).insert(id.name, (id.version, dispatcher));
    }
}

struct Started {
    id: &'static str,
    stop: Vec<StopFn>,
}

struct Task {
    label: String,
    component: &'static str,
    outer: JoinHandle<()>,
    abort: AbortHandle,
}

enum Deliverer {
    Background {
        task: Task,
        stop: CancellationToken,
    },
    Manual {
        rx: tokio::sync::Mutex<mpsc::Receiver<Item>>,
    },
}

/// 起一个任务并盯着它：崩溃了就记下来并通知启动器。
fn spawn_watched(
    shared: &Arc<Shared>,
    faults: &mpsc::UnboundedSender<String>,
    component: &'static str,
    label: String,
    fut: BoxFuture<'static, ()>,
) -> Task {
    let inner = tokio::spawn(fut);
    let abort = inner.abort_handle();
    let shared = shared.clone();
    let faults = faults.clone();
    let name = label.clone();
    let outer = tokio::spawn(async move {
        if let Err(e) = inner.await
            && e.is_panic()
        {
            shared.fault(component, format!("后台任务 `{name}` 崩溃"));
            let _ = faults.send(name);
        }
    });
    Task {
        label,
        component,
        outer,
        abort,
    }
}

/// 启动一组组件。
///
/// `configs` 的键是组件 id。没有对应配置段的组件用空配置（即默认值）启动。
pub async fn start(
    components: Vec<Arc<dyn Component>>,
    configs: &HashMap<String, ConfigSection>,
    options: Options,
) -> Result<Running, AppError> {
    // 去掉停用的
    let mut active = Vec::new();
    for component in components {
        let manifest = component.manifest();
        let config = configs.get(manifest.id).cloned().unwrap_or_default();
        let enabled = config
            .enabled()
            .map_err(|e| e.context(format!("[components.{}]", manifest.id)))?;
        if enabled {
            active.push((component, manifest, config));
        }
    }
    let manifests: Vec<Manifest> = active.iter().map(|a| a.1.clone()).collect();
    let plan = plan::plan(&manifests)?;

    let (tx, rx) = mpsc::channel(options.queue_capacity.max(1));
    let shared = Arc::new(Shared {
        services: Mutex::default(),
        points: Mutex::default(),
        subs: Mutex::default(),
        tx,
        trace: options.trace,
        faults: options.faults,
        dispatchers: Arc::default(),
        connect: options.connect,
    });
    let (fault_tx, fault_rx) = mpsc::unbounded_channel();

    let deliverer = match options.delivery {
        Delivery::Manual => Deliverer::Manual {
            rx: tokio::sync::Mutex::new(rx),
        },
        Delivery::Background => {
            let stop = CancellationToken::new();
            let fut = Box::pin(deliver_loop(shared.clone(), rx, stop.clone()));
            let task = spawn_watched(
                &shared,
                &fault_tx,
                "kernel",
                "kernel:deliver".to_owned(),
                fut,
            );
            Deliverer::Background { task, stop }
        }
    };

    let mut running = Running {
        shared,
        started: Vec::new(),
        tasks: Vec::new(),
        cancel: CancellationToken::new(),
        deliverer,
        fault_tx,
        fault_rx: tokio::sync::Mutex::new(fault_rx),
        stop_timeout: options.stop_timeout,
        socket: None,
    };

    // start：只做登记
    let mut ready = Vec::new();
    let mut tasks = Vec::new();
    for i in plan.order {
        let (component, manifest, config) = &active[i];
        running.shared.lifecycle(manifest.id, "start");
        let mut ctx = Ctx {
            shared: &running.shared,
            manifest,
            hooks: Hooks::default(),
        };
        let result = component.start(config, &mut Host::new(&mut ctx));
        let hooks = ctx.hooks;
        running.started.push(Started {
            id: manifest.id,
            stop: hooks.stop,
        });
        if let Err(e) = result {
            running.stop().await;
            return Err(e.context(format!("组件 `{}` 启动失败", manifest.id)));
        }
        ready.push((manifest.id, hooks.ready));
        tasks.push((manifest.id, hooks.tasks));
    }

    // on_ready：全部登记完了，可以互相调用
    for (id, fns) in ready {
        for f in fns {
            if let Err(e) = f().await {
                running.stop().await;
                return Err(e.context(format!("组件 `{id}` 就绪失败")));
            }
        }
        running.shared.lifecycle(id, "ready");
    }

    // 导出：全部就绪之后才接受别的进程的调用
    if let Some(mut export) = options.export {
        // 提供它的组件被停用了：不导出，调用方会得到 not_found。按配置停用组件是正常操作，不该让进程起不来
        export
            .contracts
            .retain(|name| lock(&running.shared.dispatchers).contains_key(name.as_str()));
        let listener = match remote::bind(&export.socket).await {
            Ok(listener) => listener,
            Err(e) => {
                running.stop().await;
                return Err(e);
            }
        };
        let fut = Box::pin(remote::serve(
            listener,
            export.contracts,
            running.shared.dispatchers.clone(),
            running.cancel.clone(),
        ));
        let task = spawn_watched(
            &running.shared,
            &running.fault_tx,
            "kernel",
            "kernel:export".to_owned(),
            fut,
        );
        running.tasks.push(task);
        running.socket = Some(export.socket);
    }

    // 后台任务
    for (id, fns) in tasks {
        for (name, f) in fns {
            let fut = f(Cancel::new(running.cancel.clone()));
            let label = format!("{id}:{name}");
            let task = spawn_watched(&running.shared, &running.fault_tx, id, label, fut);
            running.tasks.push(task);
        }
    }

    Ok(running)
}

/// 一组已经启动的组件。
pub struct Running {
    shared: Arc<Shared>,
    started: Vec<Started>,
    tasks: Vec<Task>,
    cancel: CancellationToken,
    deliverer: Deliverer,
    fault_tx: mpsc::UnboundedSender<String>,
    fault_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
    stop_timeout: Duration,
    /// 导出用的 socket，停止时删掉。
    socket: Option<PathBuf>,
}

impl Running {
    /// 已启动的组件 id，按启动顺序。
    pub fn started(&self) -> Vec<&'static str> {
        self.started.iter().map(|s| s.id).collect()
    }

    /// 启动器拿某个契约的句柄（比如命令行启动器要调命令行服务）。
    /// 监听记录里调用方记为 `launcher`。
    pub fn resolve<T>(&self, contract: &'static Contract<T>) -> Result<Arc<T>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let id = contract.id();
        let found = lock(&self.shared.services).get(id.name).cloned();
        let Some((any, provider)) = found else {
            return Err(AppError::config(format!(
                "契约 `{id}` 没有提供者：对应的组件没有启用"
            )));
        };
        let inner = any.downcast_ref::<Arc<T>>().cloned().ok_or_else(|| {
            AppError::config(format!(
                "契约 `{id}` 的提供者类型对不上：是不是有两个同名的契约定义"
            ))
        })?;
        Ok(match &self.shared.trace {
            Some(sink) => contract.__wrap(inner, Tracer::new(sink.clone(), "launcher", &provider)),
            None => inner,
        })
    }

    /// 等到此刻已经发布的事件（以及它们连带引发的事件）都投递完。
    ///
    /// 手动投递模式下，事件只在这里投递。
    pub async fn settle(&self) {
        match &self.deliverer {
            Deliverer::Manual { rx } => {
                let mut rx = rx.lock().await;
                loop {
                    match rx.try_recv() {
                        Ok(Item::Event(envelope)) => deliver_one(&self.shared, envelope).await,
                        Ok(Item::Flush(done)) => {
                            let _ = done.send(());
                        }
                        Err(_) => break,
                    }
                }
            }
            Deliverer::Background { .. } => {
                let (done_tx, done_rx) = oneshot::channel();
                if self.shared.tx.send(Item::Flush(done_tx)).await.is_ok() {
                    let _ = done_rx.await;
                }
            }
        }
    }

    /// 等到有后台任务崩溃，返回它的名字。正常情况下一直等下去。
    ///
    /// 崩溃说明有 bug，组件的状态已经不可信。常驻进程的启动器应该停止并以失败退出，
    /// 交给进程管理器重启。
    pub async fn fault(&self) -> String {
        match self.fault_rx.lock().await.recv().await {
            Some(label) => label,
            None => std::future::pending().await,
        }
    }

    /// 停止：通知后台任务收尾 → 按启动的逆序执行各组件的 `on_stop`。
    /// 每个组件停止后，它在收尾时发布的事件会先投递完，再停下一个。
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(socket) = self.socket.take() {
            let _ = std::fs::remove_file(socket);
        }
        let deadline = tokio::time::Instant::now() + self.stop_timeout;
        for mut task in std::mem::take(&mut self.tasks) {
            if tokio::time::timeout_at(deadline, &mut task.outer)
                .await
                .is_err()
            {
                task.abort.abort();
                let _ = (&mut task.outer).await;
                self.shared.fault(
                    task.component,
                    format!("后台任务 `{}` 收尾超时，已中止", task.label),
                );
            }
        }
        self.settle().await;

        while let Some(component) = self.started.pop() {
            for f in component.stop.into_iter().rev() {
                f().await;
            }
            self.settle().await;
            for subs in lock(&self.shared.subs).values_mut() {
                subs.retain(|s| *s.component != *component.id);
            }
            self.shared.lifecycle(component.id, "stop");
        }

        if let Deliverer::Background { task, stop } = self.deliverer {
            stop.cancel();
            let _ = task.outer.await;
        }
        drop(self.fault_tx);
    }
}
