//! AArch64 `VBAR_EL1` exception vector table supporting EL1 faults, EL0 `SVC` syscalls, and IRQs.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use knightos_hal::{CpuArch, SerialArch, TimerTickFn};

use crate::ArmCpu;
use crate::context;
use crate::gic;
use crate::serial::ArmSerial;

pub static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static USER_FAULT_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub type UserFaultFn = extern "C" fn(esr: u64, elr: u64, far: u64) -> !;

pub fn register_timer_hook(hook: TimerTickFn) {
    TIMER_HOOK.store(hook as *mut (), Ordering::Release);
}

pub fn register_user_fault_hook(hook: UserFaultFn) {
    USER_FAULT_HOOK.store(hook as *mut (), Ordering::Release);
}

#[repr(C)]
pub struct El0TrapFrame {
    pub x: [u64; 31],
    pub sp_el0: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

unsafe extern "C" {
    static knight_arm64_vectors: u8;
}

global_asm!(
    r#"
.section .text.vectors, "ax"
.balign 2048
.global knight_arm64_vectors
knight_arm64_vectors:

.macro VECTOR_SLOT handler
    .balign 0x80
    b \handler
.endm

    // Current EL with SP_EL0
    VECTOR_SLOT arm64_sync_trap
    VECTOR_SLOT arm64_irq_trap
    VECTOR_SLOT arm64_fiq_trap
    VECTOR_SLOT arm64_serror_trap

    // Current EL with SP_ELx (kernel mode)
    VECTOR_SLOT arm64_sync_trap
    VECTOR_SLOT arm64_irq_trap
    VECTOR_SLOT arm64_fiq_trap
    VECTOR_SLOT arm64_serror_trap

    // Lower EL using AArch64 (EL0 user space)
    VECTOR_SLOT arm64_el0_sync_trap
    VECTOR_SLOT arm64_el0_irq_trap
    VECTOR_SLOT arm64_fiq_trap
    VECTOR_SLOT arm64_serror_trap

    // Lower EL using AArch32
    VECTOR_SLOT arm64_sync_trap
    VECTOR_SLOT arm64_irq_trap
    VECTOR_SLOT arm64_fiq_trap
    VECTOR_SLOT arm64_serror_trap

.macro SAVE_EL1_REGS
    sub sp, sp, #(16 * 10)
    stp x0, x1, [sp, #(16 * 0)]
    stp x2, x3, [sp, #(16 * 1)]
    stp x4, x5, [sp, #(16 * 2)]
    stp x6, x7, [sp, #(16 * 3)]
    stp x8, x9, [sp, #(16 * 4)]
    stp x10, x11, [sp, #(16 * 5)]
    stp x12, x13, [sp, #(16 * 6)]
    stp x14, x15, [sp, #(16 * 7)]
    stp x16, x17, [sp, #(16 * 8)]
    stp x29, x30, [sp, #(16 * 9)]
.endm

.macro RESTORE_EL1_REGS
    ldp x29, x30, [sp, #(16 * 9)]
    ldp x16, x17, [sp, #(16 * 8)]
    ldp x14, x15, [sp, #(16 * 7)]
    ldp x12, x13, [sp, #(16 * 6)]
    ldp x10, x11, [sp, #(16 * 5)]
    ldp x8, x9, [sp, #(16 * 4)]
    ldp x6, x7, [sp, #(16 * 3)]
    ldp x4, x5, [sp, #(16 * 2)]
    ldp x2, x3, [sp, #(16 * 1)]
    ldp x0, x1, [sp, #(16 * 0)]
    add sp, sp, #(16 * 10)
.endm

// El0TrapFrame: 31 x u64 (x0..x30) + sp_el0 + elr_el1 + spsr_el1 = 34 * 8 = 272 bytes (16B aligned).
.macro SAVE_EL0_FRAME
    sub sp, sp, #272
    stp x0, x1, [sp, #(16 * 0)]
    stp x2, x3, [sp, #(16 * 1)]
    stp x4, x5, [sp, #(16 * 2)]
    stp x6, x7, [sp, #(16 * 3)]
    stp x8, x9, [sp, #(16 * 4)]
    stp x10, x11, [sp, #(16 * 5)]
    stp x12, x13, [sp, #(16 * 6)]
    stp x14, x15, [sp, #(16 * 7)]
    stp x16, x17, [sp, #(16 * 8)]
    stp x18, x19, [sp, #(16 * 9)]
    stp x20, x21, [sp, #(16 * 10)]
    stp x22, x23, [sp, #(16 * 11)]
    stp x24, x25, [sp, #(16 * 12)]
    stp x26, x27, [sp, #(16 * 13)]
    stp x28, x29, [sp, #(16 * 14)]
    mrs x9, sp_el0
    mrs x10, elr_el1
    mrs x11, spsr_el1
    stp x30, x9, [sp, #(16 * 15)]
    stp x10, x11, [sp, #(16 * 16)]
.endm

.macro RESTORE_EL0_FRAME
    ldp x10, x11, [sp, #(16 * 16)]
    ldp x30, x9, [sp, #(16 * 15)]
    msr sp_el0, x9
    msr elr_el1, x10
    msr spsr_el1, x11
    ldp x28, x29, [sp, #(16 * 14)]
    ldp x26, x27, [sp, #(16 * 13)]
    ldp x24, x25, [sp, #(16 * 12)]
    ldp x22, x23, [sp, #(16 * 11)]
    ldp x20, x21, [sp, #(16 * 10)]
    ldp x18, x19, [sp, #(16 * 9)]
    ldp x16, x17, [sp, #(16 * 8)]
    ldp x14, x15, [sp, #(16 * 7)]
    ldp x12, x13, [sp, #(16 * 6)]
    ldp x10, x11, [sp, #(16 * 5)]
    ldp x8, x9, [sp, #(16 * 4)]
    ldp x6, x7, [sp, #(16 * 3)]
    ldp x4, x5, [sp, #(16 * 2)]
    ldp x2, x3, [sp, #(16 * 1)]
    ldp x0, x1, [sp, #(16 * 0)]
    add sp, sp, #272
.endm

arm64_sync_trap:
    SAVE_EL1_REGS
    mrs x0, esr_el1
    mrs x1, elr_el1
    mrs x2, far_el1
    bl {sync_handler}
    RESTORE_EL1_REGS
    eret

arm64_irq_trap:
    SAVE_EL1_REGS
    bl {irq_handler}
    RESTORE_EL1_REGS
    eret

arm64_el0_sync_trap:
    SAVE_EL0_FRAME
    mov x0, sp
    bl {el0_sync_handler}
    RESTORE_EL0_FRAME
    eret

arm64_el0_irq_trap:
    SAVE_EL0_FRAME
    bl {irq_handler}
    RESTORE_EL0_FRAME
    eret

arm64_fiq_trap:
    SAVE_EL1_REGS
    mrs x0, esr_el1
    mrs x1, elr_el1
    mrs x2, far_el1
    bl {sync_handler}
    RESTORE_EL1_REGS
    eret

arm64_serror_trap:
    SAVE_EL1_REGS
    mrs x0, esr_el1
    mrs x1, elr_el1
    mrs x2, far_el1
    bl {sync_handler}
    RESTORE_EL1_REGS
    eret
"#,
    sync_handler = sym sync_exception_dispatch,
    el0_sync_handler = sym el0_sync_dispatch,
    irq_handler = sym irq_dispatch
);

extern "C" fn sync_exception_dispatch(esr: u64, elr: u64, far: u64) -> ! {
    ArmSerial::write_str("\n[AArch64 EL1 EXCEPTION] esr=");
    write_hex(esr);
    ArmSerial::write_str(" elr=");
    write_hex(elr);
    ArmSerial::write_str(" far=");
    write_hex(far);
    ArmSerial::write_str("\n");
    ArmCpu::halt_forever();
}

extern "C" fn el0_sync_dispatch(frame: &mut El0TrapFrame) {
    let esr: u64;
    let far: u64;
    unsafe {
        asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack));
        asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack));
    }
    let ec = (esr >> 26) & 0x3F;
    if ec == 0x15 {
        // SVC instruction from AArch64 EL0.
        let ret = context::dispatch_syscall(
            frame.x[8],
            frame.x[0],
            frame.x[1],
            frame.x[2],
            frame.x[3],
            frame.x[4],
            frame.x[5],
        );
        frame.x[0] = ret as u64;
    } else {
        ArmSerial::write_str("\n[AArch64 EL0 EXCEPTION] esr=");
        write_hex(esr);
        ArmSerial::write_str(" elr=");
        write_hex(frame.elr_el1);
        ArmSerial::write_str(" far=");
        write_hex(far);
        ArmSerial::write_str("\n");
        let hook = USER_FAULT_HOOK.load(Ordering::Acquire);
        if !hook.is_null() {
            let f: UserFaultFn = unsafe { core::mem::transmute(hook) };
            f(esr, frame.elr_el1, far);
        }
        ArmCpu::halt_forever();
    }
}

extern "C" fn irq_dispatch() {
    let iar = gic::ack_irq();
    let intid = iar & 0x00FF_FFFF;
    let mut timer_fired = false;
    if intid == gic::TIMER_PPI_INTID {
        TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
        gic::rearm_timer();
        timer_fired = true;
    }
    if intid < 1020 {
        gic::eoi_irq(iar);
    }
    if timer_fired {
        let hook = TIMER_HOOK.load(Ordering::Acquire);
        if !hook.is_null() {
            let f: TimerTickFn = unsafe { core::mem::transmute(hook) };
            f();
        }
    }
}

fn write_hex(val: u64) {
    ArmSerial::write_str("0x");
    for shift in (0..16).rev() {
        let nib = ((val >> (shift * 4)) & 0xF) as u8;
        let ch = if nib < 10 { b'0' + nib } else { b'a' + (nib - 10) };
        ArmSerial::write_byte(ch);
    }
}

pub fn init_vbar() {
    let vbar = &raw const knight_arm64_vectors as *const u8 as u64;
    unsafe {
        // Ensure FP/SIMD/NEON (CPACR_EL1.FPEN = 0b11, bits 21:20) is enabled for EL0/EL1.
        asm!(
            "mrs x9, cpacr_el1",
            "orr x9, x9, #(3 << 20)",
            "msr cpacr_el1, x9",
            "msr vbar_el1, {vbar}",
            "isb",
            vbar = in(reg) vbar,
            out("x9") _,
            options(nostack),
        );
    }
}
