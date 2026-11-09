//! A single-threaded asynchronous executor that waits on one port.
//!
//! Every wait a future registers gets its own key on the port, and the
//! packet with that key wakes the task that waits. A waker used on
//! another thread queues a packet with key 0 so that the executor's
//! thread stops waiting on the port.

mod reactor;
mod spin;

pub use reactor::{NextPacket, Packet, Packets, Reactor, Signals, Timeline};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};
use reactor::Taken;
use spin::Spin;

use crate::sys::{self, Status};

type Task = Pin<Box<dyn Future<Output = ()>>>;

/// The index of the future passed to `Executor::block_on`.
const MAIN: usize = usize::MAX;

/// The tasks that are ready to be polled, which wakers on any thread
/// add to.
struct Ready {
    ids: Spin<VecDeque<usize>>,
    /// Set while the executor's thread waits on the port.
    sleeping: AtomicBool,
    port: sys::Handle,
}

impl Ready {
    fn push(&self, id: usize) {
        self.ids.lock().push_back(id);
        if self.sleeping.swap(false, Ordering::AcqRel) {
            let packet = [0u8; reactor::PACKET_SIZE];
            // A full port already holds packets that end the wait.
            // SAFETY: the pointer refers to a whole packet.
            unsafe { sys::port_queue(self.port, packet.as_ptr()) };
        }
    }
}

struct TaskWaker {
    id: usize,
    ready: Arc<Ready>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.ready.push(self.id);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.push(self.id);
    }
}

/// Adds tasks to an executor, from inside its tasks too.
#[derive(Clone)]
pub struct Spawner {
    new: Rc<RefCell<Vec<Task>>>,
}

impl Spawner {
    /// Runs `future` on the executor until it completes.
    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        self.new.borrow_mut().push(Box::pin(future));
    }
}

/// Runs futures on the current thread.
pub struct Executor {
    reactor: Reactor,
    spawner: Spawner,
    tasks: Vec<Option<Task>>,
    ready: Arc<Ready>,
}

impl Executor {
    /// An executor with a new port.
    pub fn new() -> Result<Executor, Status> {
        let reactor = Reactor::new()?;
        let ready = Arc::new(Ready {
            ids: Spin::new(VecDeque::new()),
            sleeping: AtomicBool::new(false),
            port: reactor.port(),
        });
        Ok(Executor {
            reactor,
            spawner: Spawner {
                new: Rc::new(RefCell::new(Vec::new())),
            },
            tasks: Vec::new(),
            ready,
        })
    }

    /// The reactor, which futures register their waits with.
    pub fn reactor(&self) -> &Reactor {
        &self.reactor
    }

    pub fn spawner(&self) -> &Spawner {
        &self.spawner
    }

    fn waker(&self, id: usize) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            id,
            ready: self.ready.clone(),
        }))
    }

    /// Takes the spawned tasks, which are ready to be polled.
    fn adopt(&mut self) {
        let new = core::mem::take(&mut *self.spawner.new.borrow_mut());
        for task in new {
            let id = match self.tasks.iter().position(Option::is_none) {
                Some(free) => {
                    self.tasks[free] = Some(task);
                    free
                }
                None => {
                    self.tasks.push(Some(task));
                    self.tasks.len() - 1
                }
            };
            self.ready.ids.lock().push_back(id);
        }
    }

    /// Runs `main` and the spawned tasks until `main` completes, and
    /// returns its output. Tasks that have not completed stay for the
    /// next call.
    pub fn block_on<T>(&mut self, main: impl Future<Output = T>) -> T {
        let mut main = core::pin::pin!(main);
        let main_waker = self.waker(MAIN);
        self.ready.ids.lock().push_back(MAIN);
        loop {
            self.adopt();
            // Each round polls the tasks that were ready when it began,
            // then delivers the packets already queued, so that tasks
            // that wake themselves do not keep packets from the others.
            let mut round = core::mem::take(&mut *self.ready.ids.lock());
            while let Some(id) = round.pop_front() {
                if id == MAIN {
                    let mut cx = Context::from_waker(&main_waker);
                    if let Poll::Ready(out) = main.as_mut().poll(&mut cx) {
                        // The rest of the round stays ready for the next
                        // call.
                        let mut ids = self.ready.ids.lock();
                        round.extend(ids.drain(..));
                        *ids = round;
                        return out;
                    }
                } else {
                    self.poll_task(id);
                }
            }
            self.deliver_queued();
            if self.ready.ids.lock().is_empty() && self.spawner.new.borrow().is_empty() {
                self.wait();
            }
        }
    }

    /// Delivers the packets queued on the port, without waiting.
    fn deliver_queued(&self) {
        loop {
            match self.reactor.take(0) {
                Taken::Packet(packet) => self.reactor.deliver(packet),
                Taken::Wakeup => {}
                Taken::Empty => return,
            }
        }
    }

    fn poll_task(&mut self, id: usize) {
        let waker = self.waker(id);
        let Some(Some(task)) = self.tasks.get_mut(id) else {
            // A task that has completed may still be woken.
            return;
        };
        if task
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_ready()
        {
            self.tasks[id] = None;
        }
    }

    /// Waits on the port for a packet and delivers it, unless a task
    /// became ready meanwhile.
    fn wait(&self) {
        self.ready.sleeping.store(true, Ordering::Release);
        if !self.ready.ids.lock().is_empty() {
            self.ready.sleeping.store(false, Ordering::Release);
            return;
        }
        let taken = self.reactor.take(i64::MAX);
        self.ready.sleeping.store(false, Ordering::Release);
        if let Taken::Packet(packet) = taken {
            self.reactor.deliver(packet);
        }
    }
}
