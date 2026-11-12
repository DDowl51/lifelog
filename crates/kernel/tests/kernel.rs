//! 用假组件验证 kernel 的三件事：生命周期、命名、消息传递，以及监听点。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kernel::{Delivery, Options, Running};
use kernel_api::{
    AppError, Collector, Component, ConfigSection, ContractId, ErrorKind, EventDef, Host, Kind,
    Manifest, Publisher, TraceRecord, TraceSink, async_trait, contract,
};
use serde_json::json;

contract! {
    /// 打招呼。
    pub trait Greeter as GREETER = service("greeter", 1) {
        async fn greet(&self, name: String) -> Result<String, AppError>;
    }
}

contract! {
    /// 谁想在招呼里加个称呼，就来登记。
    pub trait Namer as NAMERS = point("greeter.namers", 1) {
        async fn title(&self) -> Result<String, AppError>;
    }
}

static GREETED: EventDef<String> = EventDef::new("greeter", "greeted");

const ECHOER: ContractId = ContractId {
    name: "echoer",
    version: 1,
    many: false,
};
static ECHOED: EventDef<String> = EventDef::new("echoer", "echoed");

// ---------- 测试用的零件 ----------

type Log = Arc<Mutex<Vec<String>>>;

fn log(log: &Log, line: impl Into<String>) {
    log.lock().unwrap().push(line.into());
}

fn lines(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[derive(Default)]
struct Rec(Mutex<Vec<TraceRecord>>);

impl TraceSink for Rec {
    fn record(&self, record: TraceRecord) {
        self.0.lock().unwrap().push(record);
    }
}

impl Rec {
    fn all(&self) -> Vec<TraceRecord> {
        self.0.lock().unwrap().clone()
    }
}

type StartFn = Box<dyn Fn(&ConfigSection, &mut Host<'_>) -> Result<(), AppError> + Send + Sync>;

struct Fake {
    manifest: Manifest,
    start: StartFn,
}

impl Component for Fake {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        (self.start)(config, host)
    }
}

struct Spec {
    id: &'static str,
    kind: Kind,
    provides: Vec<ContractId>,
    requires: Vec<ContractId>,
    optional: Vec<ContractId>,
}

fn spec(id: &'static str, kind: Kind) -> Spec {
    Spec {
        id,
        kind,
        provides: vec![],
        requires: vec![],
        optional: vec![],
    }
}

impl Spec {
    fn provides(mut self, c: &[ContractId]) -> Self {
        self.provides = c.to_vec();
        self
    }
    fn requires(mut self, c: &[ContractId]) -> Self {
        self.requires = c.to_vec();
        self
    }
    fn optional(mut self, c: &[ContractId]) -> Self {
        self.optional = c.to_vec();
        self
    }
    fn start(
        self,
        f: impl Fn(&ConfigSection, &mut Host<'_>) -> Result<(), AppError> + Send + Sync + 'static,
    ) -> Arc<dyn Component> {
        Arc::new(Fake {
            manifest: Manifest {
                id: self.id,
                kind: self.kind,
                summary: "测试用",
                provides: self.provides,
                requires: self.requires,
                optional_requires: self.optional,
            },
            start: Box::new(f),
        })
    }
}

struct Hello {
    greeted: Publisher<String>,
    namers: Collector<dyn Namer>,
}

#[async_trait]
impl Greeter for Hello {
    async fn greet(&self, name: String) -> Result<String, AppError> {
        if name.is_empty() {
            return Err(AppError::validation("名字不能为空"));
        }
        let mut titles = String::new();
        for namer in self.namers.all() {
            titles.push_str(&namer.title().await?);
            titles.push(' ');
        }
        self.greeted.publish(name.clone()).await?;
        Ok(format!("hello {titles}{name}"))
    }
}

/// 系统服务：提供 `Greeter`，拥有扩展点 `NAMERS`，发布 `GREETED`。
fn greeter(l: &Log) -> Arc<dyn Component> {
    let l = l.clone();
    spec("greeter", Kind::Service)
        .provides(&[GREETER.id(), NAMERS.id()])
        .start(move |_, host| {
            log(&l, "start greeter");
            let hello = Hello {
                greeted: host.publisher(&GREETED)?,
                namers: host.collector(&NAMERS)?,
            };
            let l2 = l.clone();
            host.on_stop(move || async move { log(&l2, "stop greeter") });
            host.provide(&GREETER, Arc::new(hello))
        })
}

struct Title(&'static str);

#[async_trait]
impl Namer for Title {
    async fn title(&self) -> Result<String, AppError> {
        Ok(self.0.to_owned())
    }
}

/// 应用功能：向 `NAMERS` 登记一个称呼。
fn namer(id: &'static str, title: &'static str) -> Arc<dyn Component> {
    spec(id, Kind::Feature)
        .requires(&[NAMERS.id()])
        .start(move |_, host| host.contribute(&NAMERS, Arc::new(Title(title))))
}

/// 应用功能：订阅 `GREETED`，把收到的名字记下来。
fn listener(id: &'static str, l: &Log) -> Arc<dyn Component> {
    let l = l.clone();
    spec(id, Kind::Feature)
        .requires(&[GREETER.id()])
        .start(move |_, host| {
            log(&l, format!("start {id}"));
            let l2 = l.clone();
            host.subscribe(&GREETED, "note", move |name: String| {
                let l2 = l2.clone();
                async move {
                    log(&l2, format!("{id} heard {name}"));
                    Ok(())
                }
            })?;
            let l3 = l.clone();
            host.on_stop(move || async move { log(&l3, format!("stop {id}")) });
            Ok(())
        })
}

fn options(delivery: Delivery) -> Options {
    Options {
        trace: None,
        faults: None,
        delivery,
        queue_capacity: 16,
        stop_timeout: Duration::from_secs(5),
        export: None,
        connect: None,
    }
}

async fn boot(components: Vec<Arc<dyn Component>>, options: Options) -> Result<Running, AppError> {
    kernel::start(components, &HashMap::new(), options).await
}

fn config_err(result: Result<Running, AppError>) -> String {
    match result {
        Ok(_) => panic!("应该启动失败"),
        Err(e) => {
            assert_eq!(e.kind, ErrorKind::Config, "{e}");
            e.message
        }
    }
}

// ---------- 生命周期 ----------

#[tokio::test]
async fn starts_providers_first_and_stops_in_reverse() {
    let l = Log::default();
    // 故意把使用者放在前面
    let running = boot(
        vec![listener("ears", &l), greeter(&l)],
        options(Delivery::Manual),
    )
    .await
    .unwrap();
    assert_eq!(running.started(), ["greeter", "ears"]);
    running.stop().await;
    assert_eq!(
        lines(&l),
        ["start greeter", "start ears", "stop ears", "stop greeter"]
    );
}

#[tokio::test]
async fn disabled_component_is_not_loaded() {
    let l = Log::default();
    let configs = HashMap::from([(
        "ears".to_owned(),
        ConfigSection::from_value(json!({ "enabled": false })).unwrap(),
    )]);
    let running = kernel::start(
        vec![listener("ears", &l), greeter(&l)],
        &configs,
        options(Delivery::Manual),
    )
    .await
    .unwrap();
    assert_eq!(running.started(), ["greeter"]);

    // 它的订阅也跟着消失：发布了也没人收
    running
        .resolve(&GREETER)
        .unwrap()
        .greet("ann".into())
        .await
        .unwrap();
    running.settle().await;
    assert_eq!(lines(&l), ["start greeter"]);
    running.stop().await;
}

#[tokio::test]
async fn missing_provider_fails_at_startup() {
    let l = Log::default();
    let msg = config_err(boot(vec![listener("ears", &l)], options(Delivery::Manual)).await);
    assert!(msg.contains("`ears`") && msg.contains("greeter@1"), "{msg}");
    assert!(lines(&l).is_empty(), "校验不过就不该启动任何组件");
}

#[tokio::test]
async fn optional_requirement_degrades() {
    let l = Log::default();
    let l2 = l.clone();
    let app = spec("app", Kind::Feature)
        .optional(&[GREETER.id()])
        .start(move |_, host| {
            let found = host.try_resolve(&GREETER)?.is_some();
            log(&l2, format!("greeter present: {found}"));
            Ok(())
        });
    boot(vec![app.clone()], options(Delivery::Manual))
        .await
        .unwrap()
        .stop()
        .await;
    boot(vec![app, greeter(&l)], options(Delivery::Manual))
        .await
        .unwrap()
        .stop()
        .await;
    assert_eq!(
        lines(&l),
        [
            "greeter present: false",
            "start greeter",
            "greeter present: true",
            "stop greeter"
        ]
    );
}

#[tokio::test]
async fn using_an_undeclared_contract_fails_and_unwinds() {
    let l = Log::default();
    let sneaky = spec("sneaky", Kind::Feature).start(|_, host| host.resolve(&GREETER).map(|_| ()));
    let msg = config_err(boot(vec![greeter(&l), sneaky], options(Delivery::Manual)).await);
    assert!(
        msg.contains("`sneaky`") && msg.contains("requires"),
        "{msg}"
    );
    // 已经启动的要收尾
    assert_eq!(lines(&l), ["start greeter", "stop greeter"]);
}

#[tokio::test]
async fn providing_an_undeclared_contract_fails() {
    let liar = spec("liar", Kind::Service).start(|_, host| {
        let hello = Hello {
            greeted: host.publisher(&GREETED)?,
            namers: host.collector(&NAMERS)?,
        };
        host.provide(&GREETER, Arc::new(hello))
    });
    let msg = config_err(boot(vec![liar], options(Delivery::Manual)).await);
    assert!(msg.contains("`liar`") && msg.contains("provides"), "{msg}");
}

#[tokio::test]
async fn bad_config_names_the_section() {
    let l = Log::default();
    let configs = HashMap::from([(
        "greeter".to_owned(),
        ConfigSection::from_value(json!({ "enabled": "yes" })).unwrap(),
    )]);
    let msg =
        config_err(kernel::start(vec![greeter(&l)], &configs, options(Delivery::Manual)).await);
    assert!(msg.contains("[components.greeter]"), "{msg}");
}

#[tokio::test]
async fn on_ready_runs_after_everyone_started() {
    let l = Log::default();
    let l2 = l.clone();
    let app = spec("app", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(move |_, host| {
            log(&l2, "start app");
            let greeter = host.resolve(&GREETER)?;
            let l3 = l2.clone();
            host.on_ready(move || async move {
                log(&l3, greeter.greet("ann".into()).await?);
                Ok(())
            });
            Ok(())
        });
    let late = spec("zz-late", Kind::Service).start({
        let l = l.clone();
        move |_, _| {
            log(&l, "start zz-late");
            Ok(())
        }
    });
    boot(vec![app, greeter(&l), late], options(Delivery::Manual))
        .await
        .unwrap();
    assert_eq!(
        lines(&l),
        ["start greeter", "start app", "start zz-late", "hello ann"]
    );
}

#[tokio::test]
async fn failing_on_ready_fails_startup_and_unwinds() {
    let l = Log::default();
    let app = spec("app", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(|_, host| {
            let greeter = host.resolve(&GREETER)?;
            host.on_ready(move || async move { greeter.greet(String::new()).await.map(|_| ()) });
            Ok(())
        });
    let err = boot(vec![app, greeter(&l)], options(Delivery::Manual))
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Validation);
    assert!(err.message.contains("`app`"), "{err}");
    assert_eq!(lines(&l), ["start greeter", "stop greeter"]);
}

// ---------- 命名：扩展点 ----------

#[tokio::test]
async fn extension_point_collects_every_contribution() {
    let l = Log::default();
    let running = boot(
        vec![
            namer("b-title", "dr."),
            namer("a-title", "the great"),
            greeter(&l),
        ],
        options(Delivery::Manual),
    )
    .await
    .unwrap();
    let hello = running
        .resolve(&GREETER)
        .unwrap()
        .greet("ann".into())
        .await
        .unwrap();
    // 登记顺序 = 启动顺序 = 同层按 id
    assert_eq!(hello, "hello the great dr. ann");
}

#[tokio::test]
async fn contributing_to_an_absent_optional_point_is_harmless() {
    let lonely = spec("lonely", Kind::Feature)
        .optional(&[NAMERS.id()])
        .start(|_, host| host.contribute(&NAMERS, Arc::new(Title("sir"))));
    boot(vec![lonely], options(Delivery::Manual))
        .await
        .unwrap()
        .stop()
        .await;
}

// ---------- 消息：事件 ----------

#[tokio::test]
async fn manual_delivery_waits_for_settle_and_keeps_subscription_order() {
    let l = Log::default();
    let running = boot(
        vec![listener("b-ears", &l), listener("a-ears", &l), greeter(&l)],
        options(Delivery::Manual),
    )
    .await
    .unwrap();
    l.lock().unwrap().clear();

    let greeter = running.resolve(&GREETER).unwrap();
    greeter.greet("ann".into()).await.unwrap();
    greeter.greet("bob".into()).await.unwrap();
    assert!(lines(&l).is_empty(), "手动模式下 settle 之前不投递");

    running.settle().await;
    assert_eq!(
        lines(&l),
        [
            "a-ears heard ann",
            "b-ears heard ann",
            "a-ears heard bob",
            "b-ears heard bob"
        ]
    );
}

/// 订阅者在处理函数里再发布事件。队列长度只有 1 也不能卡住。
fn echoer() -> Arc<dyn Component> {
    spec("echoer", Kind::Feature)
        .provides(&[ECHOER])
        .requires(&[GREETER.id()])
        .start(|_, host| {
            let echoed = host.publisher(&ECHOED)?;
            host.subscribe(&GREETED, "echo", move |name: String| {
                let echoed = echoed.clone();
                async move {
                    for i in 1..=3 {
                        echoed.publish(format!("{name}{i}")).await?;
                    }
                    Ok(())
                }
            })
        })
}

fn echo_listener(l: &Log) -> Arc<dyn Component> {
    let l = l.clone();
    spec("echo-ears", Kind::Feature)
        .requires(&[ECHOER])
        .start(move |_, host| {
            let l = l.clone();
            host.subscribe(&ECHOED, "note", move |text: String| {
                let l = l.clone();
                async move {
                    log(&l, text);
                    Ok(())
                }
            })
        })
}

#[tokio::test]
async fn publishing_from_a_handler_does_not_deadlock() {
    let l = Log::default();
    let mut opts = options(Delivery::Background);
    opts.queue_capacity = 1;
    let running = boot(vec![greeter(&l), echoer(), echo_listener(&l)], opts)
        .await
        .unwrap();
    l.lock().unwrap().clear();

    let greeter = running.resolve(&GREETER).unwrap();
    let work = async {
        greeter.greet("a".into()).await.unwrap();
        greeter.greet("b".into()).await.unwrap();
        running.settle().await;
    };
    tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .expect("卡住了");
    assert_eq!(lines(&l), ["a1", "a2", "a3", "b1", "b2", "b3"]);
    running.stop().await;
}

#[tokio::test]
async fn publishing_an_event_you_do_not_own_fails() {
    let thief = spec("thief", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(|_, host| host.publisher(&GREETED).map(|_| ()));
    let l = Log::default();
    let msg = config_err(boot(vec![greeter(&l), thief], options(Delivery::Manual)).await);
    assert!(
        msg.contains("`thief`") && msg.contains("greeter.greeted"),
        "{msg}"
    );
}

#[tokio::test]
async fn events_published_while_stopping_are_still_delivered() {
    let l = Log::default();
    // 停止时才打招呼的组件：它收尾时 greeter 还在，订阅者也要能收到
    let farewell = spec("zz-farewell", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(|_, host| {
            let greeter = host.resolve(&GREETER)?;
            host.on_stop(move || async move {
                let _ = greeter.greet("bye".into()).await;
            });
            Ok(())
        });
    let running = boot(
        vec![greeter(&l), listener("ears", &l), farewell],
        options(Delivery::Background),
    )
    .await
    .unwrap();
    l.lock().unwrap().clear();
    running.stop().await;
    assert_eq!(lines(&l), ["ears heard bye", "stop ears", "stop greeter"]);
}

// ---------- 后台任务 ----------

#[tokio::test]
async fn tasks_start_after_ready_and_get_cancelled_on_stop() {
    let l = Log::default();
    let l2 = l.clone();
    let worker = spec("worker", Kind::Service).start(move |_, host| {
        let (a, b, c) = (l2.clone(), l2.clone(), l2.clone());
        host.on_ready(move || async move {
            log(&a, "ready");
            Ok(())
        });
        host.spawn("loop", move |cancel| async move {
            log(&b, "task running");
            cancel.cancelled().await;
            log(&b, "task done");
        });
        host.on_stop(move || async move { log(&c, "on_stop") });
        Ok(())
    });
    let running = boot(vec![worker], options(Delivery::Background))
        .await
        .unwrap();
    running.stop().await;
    assert_eq!(lines(&l), ["ready", "task running", "task done", "on_stop"]);
}

#[tokio::test]
async fn a_crashed_task_is_reported() {
    let bad = spec("bad", Kind::Service).start(|_, host| {
        host.spawn("boom", |_| async { panic!("测试用的崩溃") });
        Ok(())
    });
    let faults = Arc::new(Rec::default());
    let mut opts = options(Delivery::Background);
    opts.faults = Some(faults.clone());
    let running = boot(vec![bad], opts).await.unwrap();
    let label = tokio::time::timeout(Duration::from_secs(5), running.fault())
        .await
        .unwrap();
    assert_eq!(label, "bad:boom");
    assert!(matches!(
        &faults.all()[..],
        [TraceRecord::Lifecycle { component, phase }] if component == "bad" && phase.contains("崩溃")
    ));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_task_that_ignores_cancel_is_aborted_after_the_timeout() {
    let stuck = spec("stuck", Kind::Service).start(|_, host| {
        host.spawn("forever", |_| std::future::pending());
        Ok(())
    });
    let faults = Arc::new(Rec::default());
    let mut opts = options(Delivery::Background);
    opts.faults = Some(faults.clone());
    let running = boot(vec![stuck], opts).await.unwrap();
    let before = tokio::time::Instant::now();
    running.stop().await;
    assert_eq!(before.elapsed(), Duration::from_secs(5));
    assert!(matches!(
        &faults.all()[..],
        [TraceRecord::Lifecycle { phase, .. }] if phase.contains("stuck:forever") && phase.contains("超时")
    ));
}

// ---------- 监听点 ----------

#[tokio::test]
async fn trace_shows_who_called_whom() {
    let l = Log::default();
    let trace = Arc::new(Rec::default());
    let mut opts = options(Delivery::Manual);
    opts.trace = Some(trace.clone());
    let app = spec("app", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(|_, host| {
            let greeter = host.resolve(&GREETER)?;
            host.on_ready(move || async move { greeter.greet("ann".into()).await.map(|_| ()) });
            Ok(())
        });
    let running = boot(
        vec![
            greeter(&l),
            namer("title", "dr."),
            listener("ears", &l),
            app,
        ],
        opts,
    )
    .await
    .unwrap();
    running.settle().await;

    let brief: Vec<String> = trace
        .all()
        .into_iter()
        .map(|r| match r {
            TraceRecord::Lifecycle { component, phase } => format!("{phase} {component}"),
            TraceRecord::Request {
                caller,
                provider,
                contract,
                method,
                outcome,
                ..
            } => {
                format!("{caller} -> {provider} {contract}.{method} = {outcome}")
            }
            TraceRecord::Publish { publisher, event } => format!("{publisher} publishes {event}"),
            TraceRecord::Deliver {
                event,
                component,
                subscriber,
                outcome,
                ..
            } => {
                format!("{event} => {component}:{subscriber} = {outcome}")
            }
        })
        .collect();
    assert_eq!(
        brief,
        [
            "start greeter",
            "start app",
            "start ears",
            "start title",
            "ready greeter",
            "greeter -> title greeter.namers.title = ok",
            "greeter publishes greeter.greeted",
            "app -> greeter greeter.greet = ok",
            "ready app",
            "ready ears",
            "ready title",
            "greeter.greeted => ears:note = ok",
        ]
    );
}

#[tokio::test]
async fn failed_requests_and_deliveries_are_visible() {
    let l = Log::default();
    let (trace, faults) = (Arc::new(Rec::default()), Arc::new(Rec::default()));
    let mut opts = options(Delivery::Manual);
    opts.trace = Some(trace.clone());
    opts.faults = Some(faults.clone());
    let grumpy = spec("a-grumpy", Kind::Feature)
        .requires(&[GREETER.id()])
        .start(|_, host| {
            host.subscribe(&GREETED, "refuse", |_: String| async {
                Err(AppError::temp_fail("今天不想听"))
            })
        });
    let running = boot(vec![greeter(&l), grumpy, listener("ears", &l)], opts)
        .await
        .unwrap();
    l.lock().unwrap().clear();

    let greeter = running.resolve(&GREETER).unwrap();
    assert_eq!(
        greeter.greet(String::new()).await.unwrap_err().kind,
        ErrorKind::Validation
    );
    greeter.greet("ann".into()).await.unwrap();
    running.settle().await;

    // 一个订阅者失败不影响后面的
    assert_eq!(lines(&l), ["ears heard ann"]);
    assert!(trace.all().iter().any(|r| matches!(
        r,
        TraceRecord::Request { caller, outcome, .. } if caller == "launcher" && outcome == "validation"
    )));
    // 失败的投递进 faults，成功的不进
    assert!(matches!(
        &faults.all()[..],
        [TraceRecord::Deliver { component, outcome, error: Some(error), .. }]
            if component == "a-grumpy" && outcome == "temp_fail" && error == "今天不想听"
    ));
}

#[tokio::test]
async fn without_trace_handles_are_not_wrapped() {
    let l = Log::default();
    let running = boot(vec![greeter(&l)], options(Delivery::Manual))
        .await
        .unwrap();
    assert_eq!(
        running
            .resolve(&GREETER)
            .unwrap()
            .greet("ann".into())
            .await
            .unwrap(),
        "hello ann"
    );
}
