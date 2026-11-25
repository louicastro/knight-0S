; KnightOS x86-64 Application Processor (AP) SMP Startup Trampoline
; Loaded at physical 0x8000 (SIPI vector 0x08).
; Mailbox + GDT reside in separate RW page at physical 0x9000 (keeping 0x8000 strictly RX):
;   [0x9000] u64 cr3_phys
;   [0x9008] u64 stack_top_virt
;   [0x9010] u64 entry_virt
;   [0x9018] u64 logical_cpu_id
;   [0x9020] u64 gdt[5] (40 bytes, Accessed bits pre-set)
;   [0x9048] gdtr (2-byte limit + 4-byte base = 0x9020)

[BITS 16]
[ORG 0x8000]

ap_start_16:
    cli
    cld
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax

    ; Load 32/64-bit GDT located in RW page at [0x9048]
    o32 lgdt [0x9048]

    ; Enable CR0.PE (bit 0)
    mov eax, cr0
    or eax, 1
    mov cr0, eax

    ; Far jump into 32-bit protected mode code segment (0x08)
    jmp dword 0x08:ap_start_32

[BITS 32]
ap_start_32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor ax, ax
    mov fs, ax
    mov gs, ax

    ; Enable PAE (5), PGE (7), OSFXSR (9), OSXMMEXCPT (10) in CR4
    mov eax, cr4
    or eax, (1 << 5) | (1 << 7) | (1 << 9) | (1 << 10)
    mov cr4, eax

    ; Load kernel PML4 physical address from mailbox [0x9000]
    mov eax, [0x9000]
    mov cr3, eax

    ; Enable EFER.LME (8), EFER.NXE (11), EFER.SCE (0)
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 0) | (1 << 8) | (1 << 11)
    wrmsr

    ; Enable CR0.PG (31), CR0.WP (16), CR0.MP (1)
    mov eax, cr0
    or eax, 0x80010002
    mov cr0, eax

    ; Far jump into 64-bit long mode code segment (0x18)
    jmp dword 0x18:ap_start_64

[BITS 64]
ap_start_64:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor ax, ax
    mov fs, ax
    mov gs, ax

    ; Load higher-half AP stack, logical CPU ID (RDI), and call Rust entry
    mov rsp, [0x9008]
    and rsp, -16
    xor rbp, rbp
    mov rdi, [0x9018]
    mov rax, [0x9010]
    call rax
.hang:
    cli
    hlt
    jmp .hang
