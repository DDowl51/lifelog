//! 发布 / 订阅的事件定义。

use std::marker::PhantomData;

/// 一种事件。属于某个契约：提供这个契约的组件可以发布它，需要这个契约的组件可以订阅它。
pub struct EventDef<E> {
    owner: &'static str,
    name: &'static str,
    _payload: PhantomData<fn(E)>,
}

impl<E> EventDef<E> {
    /// `owner` 是所属契约的名字，`name` 是事件在契约内的名字。
    pub const fn new(owner: &'static str, name: &'static str) -> Self {
        Self {
            owner,
            name,
            _payload: PhantomData,
        }
    }

    /// 所属契约的名字。
    pub const fn owner(&self) -> &'static str {
        self.owner
    }

    /// 完整名字：`<契约>.<事件>`。
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.owner, self.name)
    }
}
