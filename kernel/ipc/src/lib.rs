#![cfg_attr(not(test), no_std)]
//! KnightOS IPC primitives: Bidirectional Channels, Ports, and Futex Wait Queues.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use knight_abi::{KnightStatus, PortPacket, Rights, status};

pub const MAX_CHANNEL_BYTES: usize = 4096;
pub const MAX_CHANNEL_HANDLES: usize = 16;

#[derive(Clone)]
pub struct TransferredCap<T: Clone> {
    pub object: T,
    pub rights: Rights,
}

#[derive(Clone)]
pub struct ChannelMessage<T: Clone> {
    pub bytes: Vec<u8>,
    pub caps: Vec<TransferredCap<T>>,
}

struct SpinMutex<T> {
    locked: AtomicBool,
    inner: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinMutex<T> {}
unsafe impl<T: Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    const fn new(val: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: core::cell::UnsafeCell::new(val),
        }
    }

    fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { m: self }
    }
}

struct SpinGuard<'a, T> {
    m: &'a SpinMutex<T>,
}

impl<T> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.m.inner.get() }
    }
}

impl<T> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.m.inner.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.m.locked.store(false, Ordering::Release);
    }
}

struct EndpointSide<T: Clone> {
    inbox: VecDeque<ChannelMessage<T>>,
    waiters: VecDeque<u64>,
    closed: bool,
}

struct ChannelCore<T: Clone> {
    sides: SpinMutex<[EndpointSide<T>; 2]>,
}

/// One endpoint of a bidirectional capability-transferring IPC channel.
pub struct ChannelEndpoint<T: Clone> {
    core: Arc<ChannelCore<T>>,
    side: usize, // 0 or 1
}

impl<T: Clone> Clone for ChannelEndpoint<T> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            side: self.side,
        }
    }
}

impl<T: Clone> ChannelEndpoint<T> {
    pub fn create_pair() -> (Self, Self) {
        let core = Arc::new(ChannelCore {
            sides: SpinMutex::new([
                EndpointSide {
                    inbox: VecDeque::new(),
                    waiters: VecDeque::new(),
                    closed: false,
                },
                EndpointSide {
                    inbox: VecDeque::new(),
                    waiters: VecDeque::new(),
                    closed: false,
                },
            ]),
        });
        (
            Self {
                core: core.clone(),
                side: 0,
            },
            Self { core, side: 1 },
        )
    }

    pub fn write(&self, msg: ChannelMessage<T>) -> Result<Option<u64>, KnightStatus> {
        if msg.bytes.len() > MAX_CHANNEL_BYTES || msg.caps.len() > MAX_CHANNEL_HANDLES {
            return Err(status::ERR_INVALID_ARG);
        }
        let peer = self.side ^ 1;
        let mut sides = self.core.sides.lock();
        if sides[peer].closed {
            return Err(status::ERR_PEER_CLOSED);
        }
        sides[peer].inbox.push_back(msg);
        Ok(sides[peer].waiters.pop_front())
    }

    pub fn try_read(&self) -> Result<ChannelMessage<T>, KnightStatus> {
        let mut sides = self.core.sides.lock();
        if let Some(msg) = sides[self.side].inbox.pop_front() {
            return Ok(msg);
        }
        if sides[self.side ^ 1].closed {
            return Err(status::ERR_PEER_CLOSED);
        }
        Err(status::ERR_SHOULD_WAIT)
    }

    /// Reads the next message only if it fits within `(max_bytes, max_handles)`.
    /// Otherwise leaves the message intact and returns `Err((ERR_BUFFER_TOO_SMALL, actual_bytes, actual_handles))`.
    pub fn try_read_checked(
        &self,
        max_bytes: usize,
        max_handles: usize,
    ) -> Result<ChannelMessage<T>, (KnightStatus, usize, usize)> {
        let mut sides = self.core.sides.lock();
        if let Some(front) = sides[self.side].inbox.front() {
            let nb = front.bytes.len();
            let nh = front.caps.len();
            if nb > max_bytes || nh > max_handles {
                return Err((status::ERR_BUFFER_TOO_SMALL, nb, nh));
            }
            return Ok(sides[self.side].inbox.pop_front().unwrap());
        }
        if sides[self.side ^ 1].closed {
            return Err((status::ERR_PEER_CLOSED, 0, 0));
        }
        Err((status::ERR_SHOULD_WAIT, 0, 0))
    }

    pub fn register_waiter(&self, tid: u64) {
        let mut sides = self.core.sides.lock();
        if !sides[self.side].waiters.contains(&tid) {
            sides[self.side].waiters.push_back(tid);
        }
    }

    pub fn close_endpoint(&self) -> Vec<u64> {
        let mut sides = self.core.sides.lock();
        sides[self.side].closed = true;
        let peer = self.side ^ 1;
        sides[peer].waiters.drain(..).collect()
    }
}

/// Asynchronous event port queueing [`PortPacket`]s.
pub struct Port {
    inner: SpinMutex<PortInner>,
}

struct PortInner {
    packets: VecDeque<PortPacket>,
    waiters: VecDeque<u64>,
}

impl Port {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: SpinMutex::new(PortInner {
                packets: VecDeque::new(),
                waiters: VecDeque::new(),
            }),
        })
    }

    pub fn queue(&self, pkt: PortPacket) -> Option<u64> {
        let mut guard = self.inner.lock();
        guard.packets.push_back(pkt);
        guard.waiters.pop_front()
    }

    pub fn try_pop(&self) -> Option<PortPacket> {
        self.inner.lock().packets.pop_front()
    }

    pub fn register_waiter(&self, tid: u64) {
        let mut guard = self.inner.lock();
        if !guard.waiters.contains(&tid) {
            guard.waiters.push_back(tid);
        }
    }
}

/// Futex wait queue keyed by `(task_id, user_vaddr)`.
pub struct FutexTable {
    entries: SpinMutex<Vec<FutexBucket>>,
}

struct FutexBucket {
    task_id: u64,
    vaddr: u64,
    waiters: VecDeque<u64>,
}

impl FutexTable {
    pub const fn new() -> Self {
        Self {
            entries: SpinMutex::new(Vec::new()),
        }
    }

    pub fn enqueue(&self, task_id: u64, vaddr: u64, tid: u64) {
        let mut guard = self.entries.lock();
        if let Some(b) = guard.iter_mut().find(|b| b.task_id == task_id && b.vaddr == vaddr) {
            b.waiters.push_back(tid);
        } else {
            let mut waiters = VecDeque::new();
            waiters.push_back(tid);
            guard.push(FutexBucket {
                task_id,
                vaddr,
                waiters,
            });
        }
    }

    pub fn wake(&self, task_id: u64, vaddr: u64, max_count: usize) -> Vec<u64> {
        let mut guard = self.entries.lock();
        let mut woken = Vec::new();
        if let Some(pos) = guard.iter().position(|b| b.task_id == task_id && b.vaddr == vaddr) {
            for _ in 0..max_count {
                if let Some(tid) = guard[pos].waiters.pop_front() {
                    woken.push(tid);
                } else {
                    break;
                }
            }
            if guard[pos].waiters.is_empty() {
                guard.swap_remove(pos);
            }
        }
        woken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_and_port_and_futex_roundtrip() {
        let (a, b) = ChannelEndpoint::<u32>::create_pair();
        assert_eq!(b.try_read().err(), Some(status::ERR_SHOULD_WAIT));

        let wake_tid = a
            .write(ChannelMessage {
                bytes: alloc::vec![1, 2, 3, 4, 5],
                caps: alloc::vec![TransferredCap {
                    object: 77,
                    rights: Rights::READ,
                }],
            })
            .unwrap();
        assert_eq!(wake_tid, None);

        // Too-small buffer must NOT drop the message.
        assert_eq!(b.try_read_checked(2, 1).err(), Some((status::ERR_BUFFER_TOO_SMALL, 5, 1)));
        let msg = b.try_read_checked(8, 2).unwrap();
        assert_eq!(msg.bytes, &[1, 2, 3, 4, 5]);
        assert_eq!(msg.caps[0].object, 77);

        let port = Port::new();
        port.register_waiter(9);
        assert_eq!(port.queue(PortPacket { key: 55, kind: 1, status: 0, data: [1, 2] }), Some(9));
        assert_eq!(port.try_pop().unwrap().key, 55);

        let futex = FutexTable::new();
        futex.enqueue(1, 0x1000, 10);
        futex.enqueue(1, 0x1000, 11);
        assert_eq!(futex.wake(1, 0x1000, 1), &[10]);
        assert_eq!(futex.wake(1, 0x1000, 4), &[11]);
    }
}
