//! `Host`：组件在 `start` 里拿到的 kernel 接口。
//!
//! `Host` 是具体的 struct，带泛型方法；它把类型擦除后交给 [`HostBackend`]，
//! kernel 实现后者。组件只用 `Host`。

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::remote::{Dispatcher, Transport};
use crate::{AppError, Contract, ContractId, EventDef, Message, TraceSink, Tracer};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 擦除了类型的值。里面装的是 `Arc<dyn 契约>` 或事件内容。
pub type ErasedAny = Arc<dyn Any + Send + Sync>;

/// 擦除了类型的事件处理函数。
pub type ErasedHandler =
    Arc<dyn Fn(ErasedAny) -> BoxFuture<'static, Result<(), AppError>> + Send + Sync>;

/// 取消信号。后台任务用它得知该收尾了。
#[derive(Clone)]
pub struct Cancel(CancellationToken);

impl Cancel {
    #[doc(hidden)]
    pub fn new(token: CancellationToken) -> Self {
        Self(token)
    }

    /// 等到被要求停止。
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// 扩展点的登记从哪取。kernel 实现。
pub trait CollectSource: Send + Sync {
    /// 当前全部登记，以及各自是哪个组件登记的。
    fn all(&self) -> Vec<(ErasedAny, String)>;
}

/// 事件往哪发。kernel 实现。
pub trait PublishSink: Send + Sync {
    fn publish(&self, payload: ErasedAny) -> BoxFuture<'_, Result<(), AppError>>;
}

/// kernel 这一侧的接口，类型已擦除。组件不直接用它。
pub trait HostBackend {
    fn component_id(&self) -> &str;
    fn provide(&mut self, id: ContractId, imp: ErasedAny) -> Result<(), AppError>;
    /// 返回提供者的句柄和它的组件 id。可选依赖缺席时返回 `None`。
    fn resolve(
        &mut self,
        id: ContractId,
        optional: bool,
    ) -> Result<Option<(ErasedAny, String)>, AppError>;
    fn contribute(&mut self, id: ContractId, imp: ErasedAny) -> Result<(), AppError>;
    fn collector(&mut self, id: ContractId) -> Result<Arc<dyn CollectSource>, AppError>;
    fn publisher(
        &mut self,
        owner: &'static str,
        event: String,
    ) -> Result<Arc<dyn PublishSink>, AppError>;
    fn subscribe(
        &mut self,
        owner: &'static str,
        event: String,
        subscriber: &'static str,
        handler: ErasedHandler,
    ) -> Result<(), AppError>;
    fn on_ready(&mut self, f: Box<dyn FnOnce() -> BoxFuture<'static, Result<(), AppError>> + Send>);
    fn on_stop(&mut self, f: Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>);
    fn spawn(
        &mut self,
        name: &'static str,
        f: Box<dyn FnOnce(Cancel) -> BoxFuture<'static, ()> + Send>,
    );
    /// 开启了追踪时，记录交给谁。
    fn trace_sink(&self) -> Option<Arc<dyn TraceSink>>;
    /// 连到另一个进程的传输。这个进程没有配置远程时报错。
    fn remote(&mut self) -> Result<Arc<dyn Transport>, AppError>;
    /// 记下一个服务契约的分派，进程要导出它时用。
    fn dispatcher(&mut self, id: ContractId, dispatcher: Dispatcher);
}

fn downcast<T: ?Sized + 'static>(id: ContractId, any: &ErasedAny) -> Result<Arc<T>, AppError> {
    any.downcast_ref::<Arc<T>>().cloned().ok_or_else(|| {
        AppError::config(format!(
            "契约 `{id}` 的提供者类型对不上：是不是有两个同名的契约定义"
        ))
    })
}

/// 组件在 `start` 里能做的全部事情。
pub struct Host<'a> {
    backend: &'a mut dyn HostBackend,
}

impl<'a> Host<'a> {
    #[doc(hidden)]
    pub fn new(backend: &'a mut dyn HostBackend) -> Self {
        Self { backend }
    }

    /// 当前组件的 id。
    pub fn id(&self) -> &str {
        self.backend.component_id()
    }

    /// 我提供这个契约。必须已经写在 `Manifest::provides` 里。
    pub fn provide<T>(
        &mut self,
        contract: &'static Contract<T>,
        imp: Arc<T>,
    ) -> Result<(), AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.backend
            .dispatcher(contract.id(), contract.dispatcher(imp.clone()));
        self.backend.provide(contract.id(), Arc::new(imp))
    }

    /// 连到另一个进程的传输。只给 [`Import`](crate::Import) 用。
    #[doc(hidden)]
    pub fn remote(&mut self) -> Result<Arc<dyn Transport>, AppError> {
        self.backend.remote()
    }

    /// 拿到提供者的句柄。必须已经写在 `Manifest::requires` 里。
    pub fn resolve<T>(&mut self, contract: &'static Contract<T>) -> Result<Arc<T>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let id = contract.id();
        match self.backend.resolve(id, false)? {
            Some((any, provider)) => self.wrap(contract, &any, &provider),
            None => Err(AppError::config(format!("契约 `{id}` 没有提供者"))),
        }
    }

    /// 可选依赖：没有提供者时返回 `None`，由调用方降级。
    /// 必须已经写在 `Manifest::optional_requires` 里。
    pub fn try_resolve<T>(
        &mut self,
        contract: &'static Contract<T>,
    ) -> Result<Option<Arc<T>>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        match self.backend.resolve(contract.id(), true)? {
            Some((any, provider)) => self.wrap(contract, &any, &provider).map(Some),
            None => Ok(None),
        }
    }

    fn wrap<T>(
        &self,
        contract: &'static Contract<T>,
        any: &ErasedAny,
        provider: &str,
    ) -> Result<Arc<T>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let inner = downcast::<T>(contract.id(), any)?;
        Ok(match self.backend.trace_sink() {
            Some(sink) => contract.__wrap(inner, Tracer::new(sink, self.id(), provider)),
            None => inner,
        })
    }

    /// 向扩展点登记。扩展点必须已经写在 `requires` 或 `optional_requires` 里。
    pub fn contribute<T>(
        &mut self,
        point: &'static Contract<T>,
        imp: Arc<T>,
    ) -> Result<(), AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.backend.contribute(point.id(), Arc::new(imp))
    }

    /// 扩展点的所有者拿到「取回全部登记」的句柄。扩展点必须写在 `provides` 里。
    ///
    /// 登记发生在各组件的 `start` 里，所以要等全部启动完（`on_ready` 之后）再调 `all()`。
    pub fn collector<T>(&mut self, point: &'static Contract<T>) -> Result<Collector<T>, AppError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let source = self.backend.collector(point.id())?;
        Ok(Collector {
            point,
            source,
            trace: self
                .backend
                .trace_sink()
                .map(|sink| (sink, self.id().to_owned())),
        })
    }

    /// 拿到发布某种事件的句柄。事件所属的契约必须写在 `provides` 里。
    pub fn publisher<E: Message>(
        &mut self,
        event: &'static EventDef<E>,
    ) -> Result<Publisher<E>, AppError> {
        let sink = self.backend.publisher(event.owner(), event.full_name())?;
        Ok(Publisher {
            sink,
            _payload: PhantomData,
        })
    }

    /// 订阅事件。事件所属的契约必须写在 `requires`、`optional_requires` 或 `provides` 里。
    /// `subscriber` 是这个订阅在组件内的名字，出现在监听记录里。
    pub fn subscribe<E, F, Fut>(
        &mut self,
        event: &'static EventDef<E>,
        subscriber: &'static str,
        handler: F,
    ) -> Result<(), AppError>
    where
        E: Message,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        let name = event.full_name();
        let label = name.clone();
        let erased: ErasedHandler = Arc::new(move |any: ErasedAny| match any.downcast_ref::<E>() {
            Some(payload) => Box::pin(handler(payload.clone())),
            None => {
                let label = label.clone();
                Box::pin(async move {
                    Err(AppError::config(format!("事件 `{label}` 的内容类型对不上")))
                })
            }
        });
        self.backend
            .subscribe(event.owner(), name, subscriber, erased)
    }

    /// 全部组件 `start` 完之后、后台任务开始之前执行，按启动顺序。
    /// 这时可以调别的组件了。失败则整体启动失败。
    pub fn on_ready<F, Fut>(&mut self, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        self.backend.on_ready(Box::new(move || Box::pin(f())));
    }

    /// 停止时执行，按启动的逆序。
    pub fn on_stop<F, Fut>(&mut self, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.backend.on_stop(Box::new(move || Box::pin(f())));
    }

    /// 起一个后台任务。全部 `on_ready` 完成后才开始跑；停止时先收到取消信号。
    pub fn spawn<F, Fut>(&mut self, name: &'static str, f: F)
    where
        F: FnOnce(Cancel) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.backend
            .spawn(name, Box::new(move |cancel| Box::pin(f(cancel))));
    }
}

/// 扩展点所有者手里的句柄。
pub struct Collector<T: ?Sized + 'static> {
    point: &'static Contract<T>,
    source: Arc<dyn CollectSource>,
    trace: Option<(Arc<dyn TraceSink>, String)>,
}

impl<T: ?Sized + Send + Sync + 'static> Collector<T> {
    /// 当前全部登记，按登记的先后。
    pub fn all(&self) -> Vec<Arc<T>> {
        self.source
            .all()
            .iter()
            .filter_map(|(any, provider)| {
                let inner = any.downcast_ref::<Arc<T>>().cloned()?;
                Some(match &self.trace {
                    Some((sink, caller)) => self
                        .point
                        .__wrap(inner, Tracer::new(sink.clone(), caller, provider)),
                    None => inner,
                })
            })
            .collect()
    }
}

impl<T: ?Sized + 'static> Clone for Collector<T> {
    fn clone(&self) -> Self {
        Self {
            point: self.point,
            source: self.source.clone(),
            trace: self.trace.clone(),
        }
    }
}

/// 发布某种事件的句柄。
pub struct Publisher<E> {
    sink: Arc<dyn PublishSink>,
    _payload: PhantomData<fn(E)>,
}

impl<E: Message> Publisher<E> {
    /// 发布。只保证进了队列，不等订阅者处理完。
    pub async fn publish(&self, payload: E) -> Result<(), AppError> {
        self.sink.publish(Arc::new(payload)).await
    }
}

impl<E> Clone for Publisher<E> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            _payload: PhantomData,
        }
    }
}
