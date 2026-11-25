//! Kernel Task, Thread, Capability Object, Futex, and SMP Preemptive Scheduler integration.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use knight_abi::{FbInfo, Handle, KnightStatus, Rights};
use knightos_cap::CNode;
use knightos_hal::{CpuArch, MAX_CPUS, MmuArch, PAGE_SIZE_4K, PhysAddr, PhysFrameAllocator};
use knightos_ipc::{ChannelEndpoint, FutexTable, Port};
use knightos_mm::pmm::{BitmapPmm, PmmRef};
use knightos_mm::vmar::Vmar;
use knightos_mm::vmo::Vmo;
use knightos_sched::{RunQueue, SchedClass, SchedEntity, ThreadId};

use crate::arch_select::{ActiveCpu, ActiveMmu, arch_ctx};

const KERNEL_STACK_PAGES: usize = 4; // 16 KiB per thread
const KERNEL_STACK_BYTES: usize = KERNEL_STACK_PAGES * PAGE_SIZE_4K;

pub static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
pub static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);
pub static FUTEX_TABLE: FutexTable = FutexTable::new();

static mut PMM_PTR: *const BitmapPmm = core::ptr::null();
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);
static ARCH_ID: AtomicU64 = AtomicU64::new(0);
static PCIE_ECAM_PHYS: AtomicU64 = AtomicU64::new(0);
static IRQ_CTRL_PHYS: AtomicU64 = AtomicU64::new(0);
static FB_INFO: IrqSpinLock<FbInfo> = IrqSpinLock::new(FbInfo {
    phys_base: 0,
    size: 0,
    width: 0,
    height: 0,
    pitch_bytes: 0,
    pixel_format: 0,
});

pub fn set_fb_info(info: FbInfo) {
    *FB_INFO.lock() = info;
}

pub fn get_fb_info() -> FbInfo {
    *FB_INFO.lock()
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum KernelObject {
    Vmo(Arc<Vmo>),
    Channel(ChannelEndpoint<KernelObject>),
    Port(Arc<Port>),
    Task(Arc<Task>),
    Thread(Arc<Thread>),
    HardwareRoot,
}

/// Simple IRQ-safe spinlock for core kernel structures.
pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    inner: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Send for IrqSpinLock<T> {}
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(val: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: core::cell::UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> IrqSpinGuard<'_, T> {
        let saved_irq = save_and_disable_irq();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        IrqSpinGuard {
            lock: self,
            saved_irq,
        }
    }
}

pub struct IrqSpinGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    saved_irq: u64,
}

impl<T> core::ops::Deref for IrqSpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.inner.get() }
    }
}

impl<T> core::ops::DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        restore_irq(self.saved_irq);
    }
}

#[inline]
pub fn save_and_disable_irq() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let flags: u64;
        unsafe {
            core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags, options(nomem));
        }
        flags
    }
    #[cfg(target_arch = "aarch64")]
    {
        let daif: u64;
        unsafe {
            core::arch::asm!("mrs {}, daif", "msr daifset, #2", out(reg) daif, options(nomem, nostack));
        }
        daif
    }
}

#[inline]
pub fn restore_irq(saved: u64) {
    #[cfg(target_arch = "x86_64")]
    if (saved & 0x200) != 0 {
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }
    #[cfg(target_arch = "aarch64")]
    if (saved & (1 << 7)) == 0 {
        unsafe {
            core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        }
    }
}

/// Isolated user-space task owning a [`Vmar`] address space and [`CNode`] capability table.
pub struct Task {
    pub id: u64,
    pub vmar: IrqSpinLock<Vmar>,
    pub cnode: IrqSpinLock<CNode<KernelObject>>,
    #[allow(dead_code)]
    pub alive: AtomicBool,
}

impl Task {
    pub fn create_user() -> Result<Arc<Self>, &'static str> {
        let pmm = pmm();
        let hhdm = hhdm_offset();
        let mut alloc = PmmRef(pmm);
        let root = ActiveMmu::new_user_root(hhdm, &mut alloc)?;
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Self {
            id,
            vmar: IrqSpinLock::new(Vmar::new(root, hhdm)),
            cnode: IrqSpinLock::new(CNode::new()),
            alive: AtomicBool::new(true),
        }))
    }

    pub fn create_kernel_idle() -> Arc<Self> {
        let root = ActiveMmu::active_kernel_root();
        let hhdm = hhdm_offset();
        Arc::new(Self {
            id: 0,
            vmar: IrqSpinLock::new(Vmar::new(root, hhdm)),
            cnode: IrqSpinLock::new(CNode::new()),
            alive: AtomicBool::new(true),
        })
    }

    pub fn root_phys(&self) -> PhysAddr {
        self.vmar.lock().root()
    }

    pub fn insert_handle(&self, obj: KernelObject, rights: Rights) -> Result<Handle, KnightStatus> {
        self.cnode.lock().insert(obj, rights)
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadState {
    Ready = 0,
    Running = 1,
    Blocked = 2,
    Exited = 3,
}

pub struct Thread {
    pub id: ThreadId,
    pub task: Arc<Task>,
    pub state: AtomicU8,
    pub is_idle: bool,
    pub kernel_stack_phys: PhysAddr,
    pub kernel_stack_top: u64,
    pub saved_kernel_sp: core::cell::UnsafeCell<u64>,
    pub fpu_state: core::cell::UnsafeCell<arch_ctx::FpuState>,
    pub user_entry: u64,
    pub user_sp: u64,
    pub user_arg: u64,
    pub sched_entity: IrqSpinLock<SchedEntity>,
}

unsafe impl Sync for Thread {}
unsafe impl Send for Thread {}

impl Thread {
    pub fn set_state(&self, st: ThreadState) {
        self.state.store(st as u8, Ordering::Release);
    }

    pub fn get_state(&self) -> ThreadState {
        match self.state.load(Ordering::Acquire) {
            0 => ThreadState::Ready,
            1 => ThreadState::Running,
            2 => ThreadState::Blocked,
            _ => ThreadState::Exited,
        }
    }
}

struct SchedulerState {
    runq: RunQueue,
    threads: Vec<Arc<Thread>>,
    current: [Option<Arc<Thread>>; MAX_CPUS],
    idle: [Option<Arc<Thread>>; MAX_CPUS],
    pending_requeue: [Option<Arc<Thread>>; MAX_CPUS],
}

unsafe impl Send for SchedulerState {}

impl SchedulerState {
    const fn new() -> Self {
        const NONE_THREAD: Option<Arc<Thread>> = None;
        Self {
            runq: RunQueue::new(),
            threads: Vec::new(),
            current: [NONE_THREAD; MAX_CPUS],
            idle: [NONE_THREAD; MAX_CPUS],
            pending_requeue: [NONE_THREAD; MAX_CPUS],
        }
    }

    fn find_thread(&self, tid: ThreadId) -> Option<Arc<Thread>> {
        self.threads.iter().find(|t| t.id == tid).cloned()
    }
}

static SCHED: IrqSpinLock<SchedulerState> = IrqSpinLock::new(SchedulerState::new());

pub fn init_kernel_runtime(
    pmm: &'static BitmapPmm,
    hhdm: u64,
    arch_id: u64,
    cpu_count: usize,
    pcie_ecam_phys: u64,
    irq_ctrl_phys: u64,
) {
    unsafe {
        PMM_PTR = pmm as *const BitmapPmm;
    }
    HHDM_OFFSET.store(hhdm, Ordering::Release);
    ARCH_ID.store(arch_id, Ordering::Release);
    PCIE_ECAM_PHYS.store(pcie_ecam_phys, Ordering::Release);
    IRQ_CTRL_PHYS.store(irq_ctrl_phys, Ordering::Release);

    let idle_task = Task::create_kernel_idle();
    let mut sched = SCHED.lock();
    for cpu in 0..cpu_count.clamp(1, MAX_CPUS) {
        let t = Arc::new(Thread {
            id: 0,
            task: idle_task.clone(),
            state: AtomicU8::new(ThreadState::Running as u8),
            is_idle: true,
            kernel_stack_phys: PhysAddr(0),
            kernel_stack_top: 0,
            saved_kernel_sp: core::cell::UnsafeCell::new(0),
            fpu_state: core::cell::UnsafeCell::new(arch_ctx::FpuState::new_clean()),
            user_entry: 0,
            user_sp: 0,
            user_arg: 0,
            sched_entity: IrqSpinLock::new(SchedEntity::new(0, SchedClass::Idle, 0)),
        });
        sched.idle[cpu] = Some(t.clone());
        sched.current[cpu] = Some(t);
    }
}

pub fn pmm() -> &'static BitmapPmm {
    unsafe { &*PMM_PTR }
}

pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Acquire)
}

pub fn arch_id() -> u32 {
    ARCH_ID.load(Ordering::Acquire) as u32
}

pub fn pcie_ecam_phys() -> u64 {
    PCIE_ECAM_PHYS.load(Ordering::Acquire)
}

pub fn irq_ctrl_phys() -> u64 {
    IRQ_CTRL_PHYS.load(Ordering::Acquire)
}

/// Allocates a user-backed thread ready to enter Ring 3 / EL0 at `(entry, user_sp, arg)`.
pub fn spawn_user_thread(
    task: Arc<Task>,
    entry: u64,
    user_sp: u64,
    arg: u64,
    class: SchedClass,
) -> Result<Arc<Thread>, &'static str> {
    let pmm = pmm();
    let hhdm = hhdm_offset();
    let mut alloc = PmmRef(pmm);
    let stack_phys = alloc
        .alloc_contiguous_zeroed(KERNEL_STACK_PAGES, hhdm)
        .ok_or("OOM allocating thread kernel stack")?;
    let stack_top = hhdm + stack_phys.as_u64() + KERNEL_STACK_BYTES as u64;

    let initial_sp = init_thread_kernel_stack(stack_top);
    let tid = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);

    let mut sched = SCHED.lock();
    let min_vr = sched.runq.min_vruntime();
    let thread = Arc::new(Thread {
        id: tid,
        task,
        state: AtomicU8::new(ThreadState::Ready as u8),
        is_idle: false,
        kernel_stack_phys: stack_phys,
        kernel_stack_top: stack_top,
        saved_kernel_sp: core::cell::UnsafeCell::new(initial_sp),
        fpu_state: core::cell::UnsafeCell::new(arch_ctx::FpuState::new_clean()),
        user_entry: entry,
        user_sp,
        user_arg: arg,
        sched_entity: IrqSpinLock::new(SchedEntity::new(tid, class, min_vr)),
    });

    let ent = *thread.sched_entity.lock();
    sched.threads.push(thread.clone());
    sched.runq.enqueue(ent);
    Ok(thread)
}

fn init_thread_kernel_stack(stack_top: u64) -> u64 {
    let tramp_addr = user_thread_trampoline as *const () as usize as u64;

    #[cfg(target_arch = "x86_64")]
    unsafe {
        // Matches `arch_ctx::switch_kernel_context` pop order:
        // [sp+0]=rflags, [sp+8..48]=r15..rbp, [sp+56]=ret_addr (trampoline).
        let sp = stack_top - 64;
        let frame = sp as *mut u64;
        frame.add(0).write(0x002); // RFLAGS (IF=0 while finishing switch)
        for i in 1..7 {
            frame.add(i).write(0);
        }
        frame.add(7).write(tramp_addr);
        sp
    }

    #[cfg(target_arch = "aarch64")]
    unsafe {
        // Matches `arch_ctx::switch_kernel_context` load order (14 qwords = 112 bytes):
        // x19..x28 [0..9], x29 [10], x30=tramp_addr [11], daif=0x3C0 [12].
        let sp = stack_top - 112;
        let frame = sp as *mut u64;
        for i in 0..14 {
            frame.add(i).write(0);
        }
        frame.add(11).write(tramp_addr);
        frame.add(12).write(0x3C0);
        sp
    }
}

extern "C" fn user_thread_trampoline() -> ! {
    finish_switch();
    let cur = current_thread().expect("active thread in trampoline");
    let cpu_idx = arch_ctx::current_cpu_index();
    arch_ctx::set_kernel_stack(cpu_idx, cur.kernel_stack_top);
    ActiveMmu::switch_user_root(cur.task.root_phys());
    unsafe {
        arch_ctx::enter_usermode(cur.user_entry, cur.user_sp, cur.user_arg);
    }
}

pub fn current_thread() -> Option<Arc<Thread>> {
    let cpu_idx = arch_ctx::current_cpu_index();
    SCHED.lock().current[cpu_idx].clone()
}

pub fn current_task() -> Option<Arc<Task>> {
    current_thread().map(|t| t.task.clone())
}

/// Re-enqueues any thread that was preempted on this CPU after its registers were saved.
fn finish_switch() {
    let cpu_idx = arch_ctx::current_cpu_index();
    let mut sched = SCHED.lock();
    if let Some(prev) = sched.pending_requeue[cpu_idx].take()
        && prev.get_state() == ThreadState::Ready
    {
        let ent = *prev.sched_entity.lock();
        sched.runq.enqueue(ent);
    }
}

/// Core scheduler decision: transitions `current` to `new_state_for_current` and switches
/// to the next runnable thread (or this CPU's idle thread).
pub fn schedule_transition(new_state_for_current: ThreadState) {
    let saved_irq = save_and_disable_irq();
    let cpu_idx = arch_ctx::current_cpu_index();

    let switch_pair: Option<(
        *mut u64,
        u64,
        *mut arch_ctx::FpuState,
        *const arch_ctx::FpuState,
        PhysAddr,
        u64,
    )> = {
        let mut sched = SCHED.lock();
        let Some(prev) = sched.current[cpu_idx].clone() else {
            drop(sched);
            restore_irq(saved_irq);
            return;
        };

        // Update prev state if not idle.
        if !prev.is_idle {
            prev.set_state(new_state_for_current);
        }

        // Charge 10 ms tick when preempting.
        if new_state_for_current == ThreadState::Ready && !prev.is_idle {
            prev.sched_entity.lock().charge_tick(10_000_000);
        }

        // Pick next runnable thread (or fall back to idle if prev can no longer run).
        let next_opt = loop {
            match sched.runq.pick_next() {
                Some(ent) => {
                    if let Some(t) = sched.find_thread(ent.tid)
                        && t.get_state() == ThreadState::Ready
                        && t.task.alive.load(Ordering::Acquire)
                    {
                        break Some(t);
                    }
                }
                None => break None,
            }
        };

        let next = match next_opt {
            Some(t) => t,
            None => {
                if !prev.is_idle
                    && new_state_for_current == ThreadState::Ready
                    && prev.task.alive.load(Ordering::Acquire)
                {
                    // Nothing else ready; keep running `prev`.
                    prev.set_state(ThreadState::Running);
                    drop(sched);
                    restore_irq(saved_irq);
                    return;
                }
                if prev.is_idle {
                    drop(sched);
                    restore_irq(saved_irq);
                    return;
                }
                sched.idle[cpu_idx].clone().expect("idle thread initialized")
            }
        };

        if Arc::ptr_eq(&prev, &next) {
            prev.set_state(ThreadState::Running);
            drop(sched);
            restore_irq(saved_irq);
            return;
        }

        next.set_state(ThreadState::Running);
        if !prev.is_idle && new_state_for_current == ThreadState::Ready {
            sched.pending_requeue[cpu_idx] = Some(prev.clone());
        }
        sched.current[cpu_idx] = Some(next.clone());

        let old_sp_ptr = prev.saved_kernel_sp.get();
        let new_sp = unsafe { *next.saved_kernel_sp.get() };
        let old_fpu = prev.fpu_state.get();
        let new_fpu = next.fpu_state.get() as *const arch_ctx::FpuState;
        let next_root = next.task.root_phys();
        let next_kstack = next.kernel_stack_top;
        Some((old_sp_ptr, new_sp, old_fpu, new_fpu, next_root, next_kstack))
    };

    if let Some((old_sp_ptr, new_sp, old_fpu, new_fpu, next_root, next_kstack)) = switch_pair {
        if next_kstack != 0 {
            arch_ctx::set_kernel_stack(cpu_idx, next_kstack);
        }
        ActiveMmu::switch_user_root(next_root);
        unsafe {
            arch_ctx::save_fpu(old_fpu);
            arch_ctx::switch_kernel_context(old_sp_ptr, new_sp);
            arch_ctx::restore_fpu(new_fpu);
        }
        finish_switch();
    }

    restore_irq(saved_irq);
}

pub extern "C" fn on_timer_tick() {
    schedule_transition(ThreadState::Ready);
}

pub fn yield_current() {
    schedule_transition(ThreadState::Ready);
}

pub fn block_current() {
    schedule_transition(ThreadState::Blocked);
}

pub fn exit_current_thread() -> ! {
    schedule_transition(ThreadState::Exited);
    ActiveCpu::halt_forever();
}

pub fn exit_current_task() -> ! {
    if let Some(task) = current_task() {
        task.alive.store(false, Ordering::Release);
        let sched = SCHED.lock();
        for th in sched.threads.iter() {
            if Arc::ptr_eq(&th.task, &task) && th.get_state() != ThreadState::Running {
                th.set_state(ThreadState::Exited);
            }
        }
    }
    exit_current_thread();
}

pub extern "C" fn on_user_fault(_info0: u64, _info1: u64, _info2: u64) -> ! {
    exit_current_task();
}

/// Reclaims exited thread objects and frees their kernel stack pages when no longer active.
pub fn reap_exited_threads() {
    let mut to_free = Vec::new();
    {
        let mut sched = SCHED.lock();
        let mut i = 0usize;
        while i < sched.threads.len() {
            let th = &sched.threads[i];
            let is_active_anywhere = sched
                .current
                .iter()
                .flatten()
                .any(|c| Arc::ptr_eq(c, th))
                || sched
                    .pending_requeue
                    .iter()
                    .flatten()
                    .any(|p| Arc::ptr_eq(p, th));
            if th.get_state() == ThreadState::Exited && !is_active_anywhere && Arc::strong_count(th) == 1 {
                let removed = sched.threads.swap_remove(i);
                to_free.push(removed.kernel_stack_phys);
            } else {
                i += 1;
            }
        }
    }
    if !to_free.is_empty() {
        let mut alloc = PmmRef(pmm());
        for base in to_free {
            if base.as_u64() != 0 {
                for p in 0..KERNEL_STACK_PAGES {
                    alloc.free_frame(PhysAddr(base.as_u64() + (p * PAGE_SIZE_4K) as u64));
                }
            }
        }
    }
}

pub fn wake_thread(tid: ThreadId) {
    let mut sched = SCHED.lock();
    if let Some(t) = sched.find_thread(tid)
        && t.get_state() == ThreadState::Blocked
    {
        t.set_state(ThreadState::Ready);
        let ent = *t.sched_entity.lock();
        sched.runq.enqueue(ent);
    }
}

pub extern "C" fn ap_sched_loop(cpu_idx: usize) -> ! {
    // Capture AP's own kernel stack top into its idle thread record.
    let rsp: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack));
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) rsp, options(nomem, nostack));
    }
    {
        let sched = SCHED.lock();
        if let Some(idle) = &sched.idle[cpu_idx] {
            unsafe {
                *idle.saved_kernel_sp.get() = rsp;
            }
        }
    }
    loop {
        reap_exited_threads();
        if !SCHED.lock().runq.is_empty() {
            schedule_transition(ThreadState::Ready);
        }
        ActiveCpu::enable_interrupts();
        ActiveCpu::halt_once();
    }
}

pub fn bsp_sched_loop() -> ! {
    loop {
        reap_exited_threads();
        if !SCHED.lock().runq.is_empty() {
            schedule_transition(ThreadState::Ready);
        }
        ActiveCpu::enable_interrupts();
        ActiveCpu::halt_once();
    }
}
