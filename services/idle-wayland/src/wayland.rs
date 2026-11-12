//! 和 Wayland 合成器打交道的部分。全是同步代码，跑在自己的线程上。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tokio::sync::mpsc::UnboundedSender;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

/// 合成器说的话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// 已经这么久没有操作了。
    Idled,
    /// 又有操作了。
    Resumed,
}

#[derive(Default)]
pub(crate) struct State {
    signals: Vec<Signal>,
    /// 留着，不让它被回收。
    _notification: Option<ExtIdleNotificationV1>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.signals.push(Signal::Idled),
            ext_idle_notification_v1::Event::Resumed => state.signals.push(Signal::Resumed),
            _ => {}
        }
    }
}

/// 连上合成器、登记好「`timeout_ms` 没有操作就通知我」。出错时返回给人看的原因。
pub(crate) fn connect(timeout_ms: u32) -> Result<(EventQueue<State>, State), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("连不上 Wayland 合成器：{e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("读不到 Wayland 全局对象：{e}"))?;
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "合成器没有提供 wl_seat".to_owned())?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "合成器不支持 ext-idle-notify-v1 协议".to_owned())?;
    let mut state = State {
        _notification: Some(notifier.get_idle_notification(timeout_ms, &seat, &qh, ())),
        ..State::default()
    };
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("和合成器的握手失败：{e}"))?;
    Ok((queue, state))
}

/// 每隔多久看一眼要不要停。
const POLL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 250_000_000,
};

/// 读合成器的消息，翻译成 [`Signal`] 发出去，直到 `stop` 被设上或者接收方不要了。
pub(crate) fn pump(
    mut queue: EventQueue<State>,
    mut state: State,
    tx: UnboundedSender<Signal>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let lost = |e: &dyn std::fmt::Display| format!("和合成器的连接断了：{e}");
    loop {
        queue.dispatch_pending(&mut state).map_err(|e| lost(&e))?;
        for signal in state.signals.drain(..) {
            if tx.send(signal).is_err() {
                return Ok(());
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        queue.flush().map_err(|e| lost(&e))?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let ready = {
            let fd = guard.connection_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            match poll(&mut fds, Some(&POLL)) {
                Ok(n) => n > 0,
                Err(rustix::io::Errno::INTR) => false,
                Err(e) => return Err(lost(&e)),
            }
        };
        if ready {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(lost(&e)),
            }
        }
    }
}
