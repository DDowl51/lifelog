//! 契约：组件对外的请求 / 响应定义。

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::remote::{Dispatcher, Transport};
use crate::{AppError, BoxFuture, Tracer};

/// 可以放进消息里的类型：可序列化、拥有所有权、可以跨线程。
///
/// 所有满足条件的类型自动实现。回调、引用、共享的可变对象不满足，
/// 所以把它们写进契约会编译失败（原则 5：契约只传纯数据）。
pub trait Message: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}

impl<T> Message for T where T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}

/// 契约的标识。依赖声明在它上面，不在组件 id 上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContractId {
    pub name: &'static str,
    pub version: u32,
    /// `true`：扩展点，可以有多个登记者。`false`：服务，只有一个提供者。
    pub many: bool,
}

impl std::fmt::Display for ContractId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// 一个契约：标识 + 给句柄加记录点的办法。用 [`contract!`](crate::contract) 声明。
pub struct Contract<T: ?Sized + 'static> {
    id: ContractId,
    wrap: fn(Arc<T>, Tracer) -> Arc<T>,
    dispatch: DispatchFn<T>,
    proxy: fn(Arc<dyn Transport>) -> Arc<T>,
}

type DispatchFn<T> = fn(Arc<T>, String, String) -> BoxFuture<'static, Result<String, AppError>>;

impl<T: ?Sized + 'static> Contract<T> {
    #[doc(hidden)]
    pub const fn __new(
        name: &'static str,
        version: u32,
        many: bool,
        wrap: fn(Arc<T>, Tracer) -> Arc<T>,
        dispatch: DispatchFn<T>,
        proxy: fn(Arc<dyn Transport>) -> Arc<T>,
    ) -> Self {
        Self {
            id: ContractId {
                name,
                version,
                many,
            },
            wrap,
            dispatch,
            proxy,
        }
    }

    pub const fn id(&self) -> ContractId {
        self.id
    }

    /// 给句柄套上记录点。
    #[doc(hidden)]
    pub fn __wrap(&self, inner: Arc<T>, tracer: Tracer) -> Arc<T> {
        (self.wrap)(inner, tracer)
    }

    /// 按方法名调用 `imp`。导出这个契约的进程用。
    pub fn dispatcher(&self, imp: Arc<T>) -> Dispatcher
    where
        T: Send + Sync,
    {
        let dispatch = self.dispatch;
        Arc::new(move |method, params| dispatch(imp.clone(), method, params))
    }

    /// 一个把调用都转给 `transport` 的代理。
    #[doc(hidden)]
    pub fn __proxy(&self, transport: Arc<dyn Transport>) -> Arc<T> {
        (self.proxy)(transport)
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __contract_many {
    (service) => {
        false
    };
    (point) => {
        true
    };
}

/// 声明一个契约。
///
/// ```
/// use kernel_api::{AppError, contract};
///
/// contract! {
///     /// 打招呼。
///     pub trait Greeter as GREETER = service("greeter", 1) {
///         async fn greet(&self, name: String) -> Result<String, AppError>;
///     }
/// }
/// assert_eq!(GREETER.id().to_string(), "greeter@1");
/// ```
///
/// 回调不是数据，写进契约会编译失败（和上面只差参数类型）：
///
/// ```compile_fail
/// use kernel_api::{AppError, contract};
///
/// contract! {
///     pub trait Greeter as GREETER = service("greeter", 1) {
///         async fn greet(&self, name: Box<dyn Fn() -> String + Send + Sync>) -> Result<String, AppError>;
///     }
/// }
/// ```
///
/// 共享的可变对象也一样：
///
/// ```compile_fail
/// use kernel_api::{AppError, contract};
///
/// contract! {
///     pub trait Greeter as GREETER = service("greeter", 1) {
///         async fn greet(&self, names: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Result<String, AppError>;
///     }
/// }
/// ```
///
/// - `service`：只有一个提供者，用 `host.provide` / `host.resolve`。
/// - `point`：扩展点，可以有多个登记者，用 `host.contribute` / `host.collector`。
///
/// 生成三样东西：trait（方法都是 `async`）、静态标识（上例的 `CLOCK`）、
/// 一个带记录点的包装（开启追踪时 `resolve` 返回的就是它）。
/// 每个参数和返回值都必须是 [`Message`]，否则编译失败。
#[macro_export]
macro_rules! contract {
    (
        $(#[$meta:meta])*
        $vis:vis trait $name:ident as $stat:ident = $kind:ident($cname:literal, $ver:literal) {
            $(
                $(#[$mmeta:meta])*
                async fn $method:ident(&self $(, $arg:ident : $aty:ty)* $(,)?) -> Result<$ret:ty, AppError>;
            )*
        }
    ) => {
        $(#[$meta])*
        #[$crate::async_trait]
        $vis trait $name: ::core::marker::Send + ::core::marker::Sync + 'static {
            $(
                $(#[$mmeta])*
                // 这里的 `AppError` 在调用处解析，所以调用处要 `use kernel_api::AppError`，
                // 和它写出来的签名一致。
                async fn $method(&self $(, $arg: $aty)*) -> ::core::result::Result<$ret, AppError>;
            )*
        }

        #[doc = concat!("契约 `", $cname, "` 的标识。")]
        $vis static $stat: $crate::Contract<dyn $name> = {
            struct Traced {
                inner: ::std::sync::Arc<dyn $name>,
                tracer: $crate::Tracer,
            }

            #[$crate::async_trait]
            impl $name for Traced {
                $(
                    async fn $method(&self $(, $arg: $aty)*) -> ::core::result::Result<$ret, $crate::AppError> {
                        let started = ::std::time::Instant::now();
                        let out = self.inner.$method($($arg),*).await;
                        self.tracer.request($cname, stringify!($method), started, &out);
                        out
                    }
                )*
            }

            fn wrap(
                inner: ::std::sync::Arc<dyn $name>,
                tracer: $crate::Tracer,
            ) -> ::std::sync::Arc<dyn $name> {
                ::std::sync::Arc::new(Traced { inner, tracer })
            }

            // 跨进程：按方法名分派（导出的一方）
            fn dispatch(
                inner: ::std::sync::Arc<dyn $name>,
                method: ::std::string::String,
                params: ::std::string::String,
            ) -> $crate::BoxFuture<'static, ::core::result::Result<::std::string::String, $crate::AppError>> {
                ::std::boxed::Box::pin(async move {
                    #[allow(unused_mut, unused_variables)]
                    let mut params = $crate::__params(&params)?;
                    match method.as_str() {
                        $(
                            stringify!($method) => {
                                $( let $arg: $aty = $crate::__take(&mut params, stringify!($arg))?; )*
                                let out = inner.$method($($arg),*).await?;
                                $crate::__encode(&out)
                            }
                        )*
                        other => ::core::result::Result::Err($crate::__no_method($cname, other)),
                    }
                })
            }

            // 跨进程：代理（导入的一方）
            struct Proxy(::std::sync::Arc<dyn $crate::Transport>);

            #[$crate::async_trait]
            impl $name for Proxy {
                $(
                    async fn $method(&self $(, $arg: $aty)*) -> ::core::result::Result<$ret, $crate::AppError> {
                        #[allow(unused_mut)]
                        let mut params = ::std::default::Default::default();
                        $( $crate::__put(&mut params, stringify!($arg), &$arg)?; )*
                        let text = self
                            .0
                            .call($cname, $ver, stringify!($method), $crate::__params_text(params))
                            .await?;
                        $crate::__decode::<$ret>(&text)
                    }
                )*
            }

            fn proxy(transport: ::std::sync::Arc<dyn $crate::Transport>) -> ::std::sync::Arc<dyn $name> {
                ::std::sync::Arc::new(Proxy(transport))
            }

            // 原则 5：消息必须是纯数据。不满足 `Message` 的类型在这里编译失败。
            #[allow(dead_code)]
            fn messages_are_plain_data() {
                fn is_message<M: $crate::Message>() {}
                $(
                    $( is_message::<$aty>(); )*
                    is_message::<$ret>();
                )*
            }

            $crate::Contract::__new($cname, $ver, $crate::__contract_many!($kind), wrap, dispatch, proxy)
        };
    };
}
