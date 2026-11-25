//! x86-64 GDT, per-CPU TSS (`RSP0` & `IST1`), and 256-entry IDT trap vectors.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use knightos_hal::{CpuArch, MAX_CPUS, SerialArch, TimerTickFn};

use crate::apic;
use crate::serial::X86Serial;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x18 | 3; // 0x1B
pub const USER_CS: u16 = 0x20 | 3; // 0x23

pub static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static USER_FAULT_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

pub type UserFaultFn = extern "C" fn(vector: u64, rip: u64, cr2: u64) -> !;

pub fn register_timer_hook(hook: TimerTickFn) {
    TIMER_HOOK.store(hook as *mut (), Ordering::Release);
}

pub fn register_user_fault_hook(hook: UserFaultFn) {
    USER_FAULT_HOOK.store(hook as *mut (), Ordering::Release);
}

#[repr(C, packed)]
struct DescriptorPtr {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Tss64 {
    reserved0: u32,
    pub rsp: [u64; 3],
    reserved1: u64,
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    pub iomap_base: u16,
}

impl Tss64 {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: core::mem::size_of::<Self>() as u16,
        }
    }
}

const GDT_LEN: usize = 5 + MAX_CPUS * 2;

#[repr(C, align(16))]
struct GdtTable {
    entries: [u64; GDT_LEN],
}

impl GdtTable {
    const fn new() -> Self {
        let mut entries = [0u64; GDT_LEN];
        entries[0] = 0x0000_0000_0000_0000; // 0x00: Null
        entries[1] = 0x00AF_9A00_0000_FFFF; // 0x08: Kernel Code 64-bit
        entries[2] = 0x00CF_9200_0000_FFFF; // 0x10: Kernel Data
        entries[3] = 0x00CF_F200_0000_FFFF; // 0x18: User Data (Ring 3)
        entries[4] = 0x00AF_FA00_0000_FFFF; // 0x20: User Code 64-bit (Ring 3)
        Self { entries }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn interrupt_gate(handler_addr: u64, ist_idx: u8) -> Self {
        Self {
            offset_low: handler_addr as u16,
            selector: KERNEL_CS,
            ist: ist_idx & 0x7,
            type_attr: 0x8E,
            offset_mid: (handler_addr >> 16) as u16,
            offset_high: (handler_addr >> 32) as u32,
            zero: 0,
        }
    }
}

#[repr(C, align(16))]
struct IdtTable([IdtEntry; 256]);

static mut CPUS_TSS: [Tss64; MAX_CPUS] = [Tss64::new(); MAX_CPUS];
static mut BSP_IST1_STACK: [u8; 16 * 1024] = [0; 16 * 1024];
static mut KERNEL_GDT: GdtTable = GdtTable::new();
static mut KERNEL_IDT: IdtTable = IdtTable([IdtEntry::missing(); 256]);

#[repr(C)]
#[derive(Debug)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

unsafe extern "C" {
    static knight_isr_table: [u64; 256];
}

global_asm!(
    r#"
.macro ISR_NOERR vec
.align 8
isr_\vec:
    push 0
    push \vec
    jmp isr_common
.endm

.macro ISR_ERR vec
.align 8
isr_\vec:
    push \vec
    jmp isr_common
.endm

isr_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    // If CS on interrupt frame ([rsp + 144]) has CPL == 3, swapgs to restore kernel GS_BASE.
    test qword ptr [rsp + 144], 3
    jz 1f
    swapgs
1:
    mov rdi, rsp
    call {dispatch}

    test qword ptr [rsp + 144], 3
    jz 2f
    swapgs
2:
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq

ISR_NOERR 0
ISR_NOERR 1
ISR_NOERR 2
ISR_NOERR 3
ISR_NOERR 4
ISR_NOERR 5
ISR_NOERR 6
ISR_NOERR 7
ISR_ERR   8
ISR_NOERR 9
ISR_ERR   10
ISR_ERR   11
ISR_ERR   12
ISR_ERR   13
ISR_ERR   14
ISR_NOERR 15
ISR_NOERR 16
ISR_ERR   17
ISR_NOERR 18
ISR_NOERR 19
ISR_NOERR 20
ISR_ERR   21
ISR_NOERR 32
ISR_NOERR 255

.section .rodata
.align 8
.global knight_isr_table
knight_isr_table:
    .quad isr_0, isr_1, isr_2, isr_3, isr_4, isr_5, isr_6, isr_7
    .quad isr_8, isr_9, isr_10, isr_11, isr_12, isr_13, isr_14, isr_15
    .quad isr_16, isr_17, isr_18, isr_19, isr_20, isr_21
    .rept 10
    .quad isr_0
    .endr
    .quad isr_32
    .rept 222
    .quad isr_0
    .endr
    .quad isr_255
"#,
    dispatch = sym interrupt_dispatch
);

extern "C" fn interrupt_dispatch(frame: &InterruptFrame) {
    match frame.vector {
        32 => {
            TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
            apic::eoi();
            let hook = TIMER_HOOK.load(Ordering::Acquire);
            if !hook.is_null() {
                let f: TimerTickFn = unsafe { core::mem::transmute(hook) };
                f();
            }
        }
        255 => {}
        vec => {
            let mut cr2: u64 = 0;
            if vec == 14 {
                unsafe {
                    asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack));
                }
            }
            X86Serial::write_str("\n[x86_64 EXCEPTION] vec=");
            write_hex(vec);
            X86Serial::write_str(" err=");
            write_hex(frame.error_code);
            X86Serial::write_str(" rip=");
            write_hex(frame.rip);
            X86Serial::write_str(" cs=");
            write_hex(frame.cs);
            X86Serial::write_str(" cr2=");
            write_hex(cr2);
            X86Serial::write_str("\n");
            if (frame.cs & 3) == 3 {
                let hook = USER_FAULT_HOOK.load(Ordering::Acquire);
                if !hook.is_null() {
                    let f: UserFaultFn = unsafe { core::mem::transmute(hook) };
                    f(vec, frame.rip, cr2);
                }
            }
            crate::X86Cpu::halt_forever();
        }
    }
}

fn write_hex(val: u64) {
    X86Serial::write_str("0x");
    for shift in (0..16).rev() {
        let nib = ((val >> (shift * 4)) & 0xF) as u8;
        let ch = if nib < 10 { b'0' + nib } else { b'a' + (nib - 10) };
        X86Serial::write_byte(ch);
    }
}

pub fn set_tss_rsp0(cpu_idx: usize, rsp0: u64) {
    let idx = cpu_idx.min(MAX_CPUS - 1);
    unsafe {
        CPUS_TSS[idx].rsp[0] = rsp0;
    }
}

fn install_cpu_tss_descriptor(cpu_idx: usize) -> u16 {
    let idx = cpu_idx.min(MAX_CPUS - 1);
    let gdt_slot = 5 + idx * 2;
    unsafe {
        let tss_addr = &raw const CPUS_TSS[idx] as u64;
        let tss_limit = (core::mem::size_of::<Tss64>() - 1) as u64;
        let low = (tss_limit & 0xFFFF)
            | ((tss_addr & 0xFF_FFFF) << 16)
            | (0x89u64 << 40)
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_addr >> 24) & 0xFF) << 56);
        let high = tss_addr >> 32;
        KERNEL_GDT.entries[gdt_slot] = low;
        KERNEL_GDT.entries[gdt_slot + 1] = high;
    }
    (gdt_slot * 8) as u16
}

/// Initializes GDT, per-CPU TSS (`IST1`), and IDT on the BSP.
pub fn init_bsp() {
    unsafe {
        let ist1_top = (&raw const BSP_IST1_STACK as *const u8 as u64) + (16 * 1024) as u64;
        for cpu in 0..MAX_CPUS {
            CPUS_TSS[cpu].ist[0] = ist1_top;
        }
        for vec in 0..256usize {
            let ist = if vec == 8 { 1 } else { 0 };
            KERNEL_IDT.0[vec] = IdtEntry::interrupt_gate(knight_isr_table[vec], ist);
        }
    }
    let tss_sel = install_cpu_tss_descriptor(0);
    unsafe {
        load_gdt_and_idt(tss_sel);
    }
}

/// Installs the shared kernel GDT/IDT and per-CPU TSS on an Application Processor.
pub fn init_ap(cpu_idx: usize) {
    let tss_sel = install_cpu_tss_descriptor(cpu_idx);
    unsafe {
        load_gdt_and_idt(tss_sel);
    }
}

unsafe fn load_gdt_and_idt(tss_sel: u16) {
    let gdtr = DescriptorPtr {
        limit: (core::mem::size_of::<GdtTable>() - 1) as u16,
        base: &raw const KERNEL_GDT as u64,
    };
    let idtr = DescriptorPtr {
        limit: (core::mem::size_of::<IdtTable>() - 1) as u16,
        base: &raw const KERNEL_IDT as u64,
    };
    unsafe {
        asm!(
            "lgdt [{gdtr}]",
            "lidt [{idtr}]",
            "push {kcs}",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, {kds}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "ltr {tss:x}",
            gdtr = in(reg) &gdtr,
            idtr = in(reg) &idtr,
            kcs = const KERNEL_CS as u64,
            kds = const KERNEL_DS,
            tss = in(reg) tss_sel,
            out("rax") _,
        );
    }
}
