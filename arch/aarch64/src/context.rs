//! AArch64 per-CPU state (`TPIDR_EL1`), context switch, and EL0 user-mode entry.

use core::arch::asm;
use core::sync::atomic::{AtomicPtr, Ordering};

use knightos_hal::{MAX_CPUS, SyscallDispatchFn};

static SYSCALL_CALLBACK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub fn register_syscall_handler(handler: SyscallDispatchFn) {
    SYSCALL_CALLBACK.store(handler as *mut (), Ordering::Release);
}

pub fn dispatch_syscall(
    num: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> i64 {
    let ptr = SYSCALL_CALLBACK.load(Ordering::Acquire);
    if ptr.is_null() {
        return -12;
    }
    let f: SyscallDispatchFn = unsafe { core::mem::transmute(ptr) };
    f(num, a0, a1, a2, a3, a4, a5)
}

pub fn init_per_cpu(cpu_idx: usize, _kernel_rsp: u64) {
    let idx = cpu_idx.min(MAX_CPUS - 1) as u64;
    unsafe {
        asm!("msr tpidr_el1, {}", in(reg) idx, options(nomem, nostack));
    }
}

pub fn set_kernel_stack(_cpu_idx: usize, _kernel_rsp: u64) {
    // On AArch64, `SP_EL1` is automatically preserved/restored per kernel thread by
    // `switch_kernel_context` while EL0 executes with `SP_EL0`.
}

pub fn current_cpu_index() -> usize {
    let idx: u64;
    unsafe {
        asm!("mrs {}, tpidr_el1", out(reg) idx, options(nomem, nostack));
    }
    (idx as usize).min(MAX_CPUS - 1)
}

pub fn init_syscall_msrs() {
    // AArch64 routes `SVC #0` through `VBAR_EL1` (`vectors::init_vbar`).
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct FpuState {
    pub q: [[u64; 2]; 32],
    pub fpsr: u64,
    pub fpcr: u64,
}

impl FpuState {
    pub const fn new_clean() -> Self {
        Self {
            q: [[0; 2]; 32],
            fpsr: 0,
            fpcr: 0,
        }
    }
}

#[inline(never)]
pub unsafe fn save_fpu(dst: *mut FpuState) {
    unsafe {
        asm!(
            "stp q0, q1, [{p}, #(32 * 0)]",
            "stp q2, q3, [{p}, #(32 * 1)]",
            "stp q4, q5, [{p}, #(32 * 2)]",
            "stp q6, q7, [{p}, #(32 * 3)]",
            "stp q8, q9, [{p}, #(32 * 4)]",
            "stp q10, q11, [{p}, #(32 * 5)]",
            "stp q12, q13, [{p}, #(32 * 6)]",
            "stp q14, q15, [{p}, #(32 * 7)]",
            "stp q16, q17, [{p}, #(32 * 8)]",
            "stp q18, q19, [{p}, #(32 * 9)]",
            "stp q20, q21, [{p}, #(32 * 10)]",
            "stp q22, q23, [{p}, #(32 * 11)]",
            "stp q24, q25, [{p}, #(32 * 12)]",
            "stp q26, q27, [{p}, #(32 * 13)]",
            "stp q28, q29, [{p}, #(32 * 14)]",
            "stp q30, q31, [{p}, #(32 * 15)]",
            "mrs x9, fpsr",
            "mrs x10, fpcr",
            "str x9, [{p}, #512]",
            "str x10, [{p}, #520]",
            p = in(reg) dst,
            out("x9") _,
            out("x10") _,
            options(nostack)
        );
    }
}

#[inline(never)]
pub unsafe fn restore_fpu(src: *const FpuState) {
    unsafe {
        asm!(
            "ldp q0, q1, [{p}, #(32 * 0)]",
            "ldp q2, q3, [{p}, #(32 * 1)]",
            "ldp q4, q5, [{p}, #(32 * 2)]",
            "ldp q6, q7, [{p}, #(32 * 3)]",
            "ldp q8, q9, [{p}, #(32 * 4)]",
            "ldp q10, q11, [{p}, #(32 * 5)]",
            "ldp q12, q13, [{p}, #(32 * 6)]",
            "ldp q14, q15, [{p}, #(32 * 7)]",
            "ldp q16, q17, [{p}, #(32 * 8)]",
            "ldp q18, q19, [{p}, #(32 * 9)]",
            "ldp q20, q21, [{p}, #(32 * 10)]",
            "ldp q22, q23, [{p}, #(32 * 11)]",
            "ldp q24, q25, [{p}, #(32 * 12)]",
            "ldp q26, q27, [{p}, #(32 * 13)]",
            "ldp q28, q29, [{p}, #(32 * 14)]",
            "ldp q30, q31, [{p}, #(32 * 15)]",
            "ldr x9, [{p}, #512]",
            "ldr x10, [{p}, #520]",
            "msr fpsr, x9",
            "msr fpcr, x10",
            p = in(reg) src,
            out("x9") _,
            out("x10") _,
            options(nostack)
        );
    }
}

/// Switches callee-saved registers (`x19..x30`) and `SP_EL1` between threads.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_kernel_context(_old_sp: *mut u64, _new_sp: u64) {
    core::arch::naked_asm!(
        "mrs x9, daif",
        "sub sp, sp, #(16 * 7)",
        "stp x19, x20, [sp, #(16 * 0)]",
        "stp x21, x22, [sp, #(16 * 1)]",
        "stp x23, x24, [sp, #(16 * 2)]",
        "stp x25, x26, [sp, #(16 * 3)]",
        "stp x27, x28, [sp, #(16 * 4)]",
        "stp x29, x30, [sp, #(16 * 5)]",
        "str x9, [sp, #(16 * 6)]",
        "mov x9, sp",
        "str x9, [x0]",
        "mov sp, x1",
        "ldr x9, [sp, #(16 * 6)]",
        "ldp x29, x30, [sp, #(16 * 5)]",
        "ldp x27, x28, [sp, #(16 * 4)]",
        "ldp x25, x26, [sp, #(16 * 3)]",
        "ldp x23, x24, [sp, #(16 * 2)]",
        "ldp x21, x22, [sp, #(16 * 1)]",
        "ldp x19, x20, [sp, #(16 * 0)]",
        "add sp, sp, #(16 * 7)",
        "msr daif, x9",
        "ret",
    );
}

/// Enters EL0 user mode via `ERET` (`SPSR_EL1 = 0` = EL0t with IRQs unmasked).
///
/// # Safety
/// `entry_pc` and `user_sp` must point to valid mapped lower-half `TTBR0_EL1` user pages.
#[unsafe(naked)]
pub unsafe extern "C" fn enter_usermode(_entry_pc: u64, _user_sp: u64, _arg0: u64) -> ! {
    core::arch::naked_asm!(
        "msr daifset, #0xf",
        "msr elr_el1, x0",
        "msr sp_el0, x1",
        "msr spsr_el1, xzr", // EL0t, NZCV=0, DAIF unmasked
        "mov x0, x2",        // arg0 -> X0
        "mov x1, xzr",
        "mov x2, xzr",
        "mov x3, xzr",
        "mov x4, xzr",
        "mov x5, xzr",
        "mov x6, xzr",
        "mov x7, xzr",
        "mov x8, xzr",
        "mov x9, xzr",
        "mov x10, xzr",
        "mov x11, xzr",
        "mov x12, xzr",
        "mov x13, xzr",
        "mov x14, xzr",
        "mov x15, xzr",
        "mov x16, xzr",
        "mov x17, xzr",
        "mov x18, xzr",
        "mov x19, xzr",
        "mov x20, xzr",
        "mov x21, xzr",
        "mov x22, xzr",
        "mov x23, xzr",
        "mov x24, xzr",
        "mov x25, xzr",
        "mov x26, xzr",
        "mov x27, xzr",
        "mov x28, xzr",
        "mov x29, xzr",
        "mov x30, xzr",
        "eret",
    );
}
