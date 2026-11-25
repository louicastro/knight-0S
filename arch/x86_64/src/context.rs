//! x86-64 SYSCALL/SYSRET setup, per-CPU GS state, kernel context switch, and Ring-3 entry.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicPtr, Ordering};

use knightos_hal::{MAX_CPUS, SyscallDispatchFn};

use crate::gdt_idt::{self, KERNEL_CS, USER_CS, USER_DS};

const MSR_EFER: u32 = 0xC000_0080;
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;
const MSR_GS_BASE: u32 = 0xC000_0101;
const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// Per-CPU state pointed to by `GS_BASE` in kernel mode (`KERNEL_GS_BASE` in Ring 3).
///
/// Field offsets are used directly by `knight_x86_syscall_entry`:
/// - `0x00`: `kernel_rsp`
/// - `0x08`: `user_rsp_scratch`
/// - `0x10`: `logical_cpu_id`
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct PerCpuState {
    pub kernel_rsp: u64,
    pub user_rsp_scratch: u64,
    pub logical_cpu_id: u64,
}

impl PerCpuState {
    const fn new() -> Self {
        Self {
            kernel_rsp: 0,
            user_rsp_scratch: 0,
            logical_cpu_id: 0,
        }
    }
}

static mut PER_CPU: [PerCpuState; MAX_CPUS] = [PerCpuState::new(); MAX_CPUS];
static SYSCALL_CALLBACK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub fn register_syscall_handler(handler: SyscallDispatchFn) {
    SYSCALL_CALLBACK.store(handler as *mut (), Ordering::Release);
}

pub fn init_per_cpu(cpu_idx: usize, kernel_rsp: u64) {
    let idx = cpu_idx.min(MAX_CPUS - 1);
    unsafe {
        let state_ptr = (&raw mut PER_CPU[idx]);
        (*state_ptr).kernel_rsp = kernel_rsp;
        (*state_ptr).logical_cpu_id = idx as u64;
        wrmsr(MSR_GS_BASE, state_ptr as u64);
        wrmsr(MSR_KERNEL_GS_BASE, 0);
    }
    gdt_idt::set_tss_rsp0(idx, kernel_rsp);
}

pub fn set_kernel_stack(cpu_idx: usize, kernel_rsp: u64) {
    let idx = cpu_idx.min(MAX_CPUS - 1);
    unsafe {
        PER_CPU[idx].kernel_rsp = kernel_rsp;
    }
    gdt_idt::set_tss_rsp0(idx, kernel_rsp);
}

pub fn current_cpu_index() -> usize {
    let idx: u64;
    unsafe {
        asm!("mov {}, gs:[0x10]", out(reg) idx, options(nomem, nostack));
    }
    (idx as usize).min(MAX_CPUS - 1)
}

/// Programs `EFER.SCE`, `STAR`, `LSTAR`, and `SFMASK` on the current CPU core.
pub fn init_syscall_msrs() {
    unsafe {
        let mut efer = rdmsr(MSR_EFER);
        efer |= 1; // EFER.SCE
        wrmsr(MSR_EFER, efer);

        // STAR[47:32] = KERNEL_CS (0x08), STAR[63:48] = 0x10 (SYSRET CS=0x23, SS=0x1B).
        let star = ((0x0010u64) << 48) | ((KERNEL_CS as u64) << 32);
        wrmsr(MSR_STAR, star);
        wrmsr(MSR_LSTAR, knight_x86_syscall_entry as *const () as usize as u64);
        // Clear IF (bit 9) and DF (bit 10) on SYSCALL entry.
        wrmsr(MSR_SFMASK, 0x0600);
    }
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack)
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack)
        );
    }
}

unsafe extern "C" {
    fn knight_x86_syscall_entry();
}

global_asm!(
    r#"
.global knight_x86_syscall_entry
knight_x86_syscall_entry:
    swapgs
    mov gs:[0x08], rsp
    mov rsp, gs:[0x00]

    // Build iret-style / sysret frame on kernel stack and preserve user argument registers.
    push qword ptr gs:[0x08]   // User RSP
    push r11                   // User RFLAGS
    push rcx                   // User RIP
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9

    // Arrange C ABI arguments for `syscall_trampoline(num, a0, a1, a2, a3, a4, a5)`:
    // Kernel incoming registers: rax=num, rdi=a0, rsi=a1, rdx=a2, r10=a3, r8=a4, r9=a5.
    // SysV C ABI expects: rdi=num, rsi=a0, rdx=a1, rcx=a2, r8=a3, r9=a4, [rsp]=a5.
    // Currently 15 pushes (120 bytes) -> push r9 makes 16 pushes (128 bytes, 16B aligned!).
    push r9                    // 7th arg (a5)
    mov r9, r8                 // 6th arg (a4)
    mov r8, r10                // 5th arg (a3)
    mov rcx, rdx               // 4th arg (a2)
    mov rdx, rsi               // 3rd arg (a1)
    mov rsi, rdi               // 2nd arg (a0)
    mov rdi, rax               // 1st arg (num)
    call {trampoline}
    add rsp, 8                 // Discard 7th arg slot

    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop rcx                    // Restore user RIP
    pop r11                    // Restore user RFLAGS
    and r11d, 0x003F0202       // Strip TF, IOPL, NT; preserve IF=1
    or r11d, 0x202
    pop rsp                    // Restore user RSP
    swapgs
    sysretq
"#,
    trampoline = sym syscall_trampoline
);

extern "C" fn syscall_trampoline(
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

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct FpuState {
    pub data: [u8; 512],
}

impl FpuState {
    pub const fn new_clean() -> Self {
        let mut data = [0u8; 512];
        // x87 FCW = 0x037F (bytes 0..2), MXCSR = 0x1F80 (bytes 24..28)
        data[0] = 0x7F;
        data[1] = 0x03;
        data[24] = 0x80;
        data[25] = 0x1F;
        Self { data }
    }
}

pub unsafe fn save_fpu(dst: *mut FpuState) {
    unsafe {
        asm!("fxsave64 [{}]", in(reg) dst, options(nostack));
    }
}

pub unsafe fn restore_fpu(src: *const FpuState) {
    unsafe {
        asm!("fxrstor64 [{}]", in(reg) src, options(nostack));
    }
}

/// Switches callee-saved registers and kernel stack between two kernel/user threads.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_kernel_context(_old_rsp: *mut u64, _new_rsp: u64) {
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "pushfq",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "popfq",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

/// Enters Ring 3 user mode cleanly via `iretq` (with `swapgs` preserving `KERNEL_GS_BASE`).
///
/// # Safety
/// `entry_rip` and `user_rsp` must point to valid mapped lower-half user code and stack.
#[unsafe(naked)]
pub unsafe extern "C" fn enter_usermode(_entry_rip: u64, _user_rsp: u64, _arg0: u64) -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov ax, {user_ds}",
        "mov ds, ax",
        "mov es, ax",
        "push {user_ds}",    // SS
        "push rsi",          // User RSP
        "push 0x202",        // RFLAGS (IF=1, reserved bit 1=1)
        "push {user_cs}",    // CS
        "push rdi",          // User RIP
        "mov rdi, rdx",      // arg0 in RDI
        "xor rax, rax",
        "xor rbx, rbx",
        "xor rcx, rcx",
        "xor rdx, rdx",
        "xor rsi, rsi",
        "xor rbp, rbp",
        "xor r8, r8",
        "xor r9, r9",
        "xor r10, r10",
        "xor r11, r11",
        "xor r12, r12",
        "xor r13, r13",
        "xor r14, r14",
        "xor r15, r15",
        "swapgs",
        "iretq",
        user_ds = const USER_DS as u64,
        user_cs = const USER_CS as u64,
    );
}
