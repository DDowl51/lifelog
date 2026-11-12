//! 组件能看到的全部 kernel 接口。
//!
//! 这里只有定义：错误、契约、事件、组件、`Host`、监听记录。
//! 实现在 `kernel` 包里，组件不依赖它。这个包不认识任何 lifelog 名词。

mod component;
mod config;
mod contract;
mod error;
mod event;
mod host;
mod remote;
mod trace;

pub use async_trait::async_trait;
#[doc(hidden)]
pub use inventory;

pub use component::{Component, ComponentFactory, Kind, Manifest, registered_components};
pub use config::{ConfigSection, NoConfig};
pub use contract::{Contract, ContractId, Message};
pub use error::{AppError, ErrorKind};
pub use event::EventDef;
pub use host::{
    BoxFuture, Cancel, CollectSource, Collector, ErasedAny, ErasedHandler, Host, HostBackend,
    PublishSink, Publisher,
};
#[doc(hidden)]
pub use remote::{__decode, __encode, __no_method, __params, __params_text, __put, __take};
pub use remote::{
    Dispatcher, Import, Transport, WireRequest, WireResponse, decode_request, decode_response,
    encode_request, encode_response,
};
pub use trace::{TraceRecord, TraceSink, Tracer};
