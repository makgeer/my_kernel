use x86_64::instructions::segmentation::{CS, DS, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/* IST-индексы для обработчиков прерываний. Каждый обработчик получает
   СОБСТВЕННЫЙ стек через TSS — прерывания больше не зависят от глубины
   стека прерванного контекста. Это устраняет DOUBLE FAULT, возникавший
   при переполнении общего стека ядра (16 КБ из boot.asm) при прерывании
   во время глубокого вывода/работы задачи. */
pub const DOUBLE_FAULT_IST_INDEX: u16 = 1;
pub const TIMER_IST_INDEX: u16 = 2;
pub const KEYBOARD_IST_INDEX: u16 = 3;

/// Селекторы сегментов (индексы в GDT).
/// Каждый селектор = индекс × 8 + DPL (0=kernel, 3=user).
/// GDT layout:
///   0x00: null
///   0x08: kernel code  (index 1)
///   0x10: kernel data  (index 2)
///   0x18: TSS          (index 3)
///   0x20: user data    (index 4)
///   0x28: user code    (index 5)
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_DATA_SELECTOR: u16 = 0x20 | 0x03; // index 4, RPL=3
pub const USER_CODE_SELECTOR: u16 = 0x28 | 0x03; // index 5, RPL=3

/* Отдельные стеки для обработчиков прерываний.
   16 КБ для #DF (нужен запас при диагностике), по 8 КБ для IRQ0/IRQ1.
   Размещаются в .bss ядра (higher-half, отображён в page tables).
   CPU переключается на них при входе в обработчик через IST и
   возвращается на прежний RSP при iretq. */
static mut DOUBLE_FAULT_STACK: [u8; 16384] = [0; 16384];
static mut TIMER_STACK: [u8; 8192] = [0; 8192];
static mut KEYBOARD_STACK: [u8; 8192] = [0; 8192];

/// Kernel stack для возврата из user mode (ring 3 → ring 0).
/// При int 0x80 из user mode CPU переключается на TSS.rsp0 — этот стек.
/// После модификации IRET-фрейма в syscall_handler, iretq возвращает
/// нас в kernel mode на этом стеке.
pub static mut KERNEL_RETURN_STACK: [u8; 16384] = [0; 16384];

/* GDT и TSS живут в static mut — append() требует &mut self, load() требует
   &'static self. init_gdt() вызывается один раз при старте ядра — data race
   невозможна. */
#[allow(static_mut_refs)]
static mut TSS: TaskStateSegment = TaskStateSegment::new();

#[allow(static_mut_refs)]
static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

#[allow(static_mut_refs)]
pub fn init_gdt() {
    unsafe {
        // Заполняем TSS: каждый IST-слот указывает на ВЕРХ своего стека
        // (стек растёт вниз, при входе в обработчик RSP = вершина).
        TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            VirtAddr::new(DOUBLE_FAULT_STACK.as_ptr() as u64 + DOUBLE_FAULT_STACK.len() as u64);
        TSS.interrupt_stack_table[TIMER_IST_INDEX as usize] =
            VirtAddr::new(TIMER_STACK.as_ptr() as u64 + TIMER_STACK.len() as u64);
        TSS.interrupt_stack_table[KEYBOARD_IST_INDEX as usize] =
            VirtAddr::new(KEYBOARD_STACK.as_ptr() as u64 + KEYBOARD_STACK.len() as u64);

        // RSP0 — стек для перехода из ring 3 в ring 0 (int 0x80, прерывания).
        // Без RSP0 при int из user mode CPU использует RSP=0 → page fault.
        // В x86_64 crate TSS.rsp0 называется privilege_stack_table[0].
        // Указываем на вершину KERNEL_RETURN_STACK (стек растёт вниз).
        TSS.privilege_stack_table[0] = VirtAddr::new(
            KERNEL_RETURN_STACK.as_ptr() as u64 + KERNEL_RETURN_STACK.len() as u64
        );

        // Добавляем 64-битный сегмент кода (kernel_code_segment)
        let code_sel = GDT.append(Descriptor::kernel_code_segment());

        // Добавляем сегмент данных (kernel_data_segment)
        let data_sel = GDT.append(Descriptor::kernel_data_segment());

        // Добавляем сегмент состояния задачи (TSS) — для IST-стеков
        let tss_sel = GDT.append(Descriptor::tss_segment(&TSS));

        // Пользовательские сегменты для ring 3:
        // user_data_segment — DPL=3, позволяет ring 3 доступ к данным
        // user_code_segment — DPL=3, позволяет ring 3 выполнение кода
        // Нужны для перехода в user mode через IRETQ (CS:RPL=3, SS:RPL=3)
        let _user_data_sel = GDT.append(Descriptor::user_data_segment());
        let _user_code_sel = GDT.append(Descriptor::user_code_segment());

        // Загружаем GDT
        GDT.load();

        // Устанавливаем селектор кода и данных (после замены GDT)
        CS::set_reg(code_sel);
        DS::set_reg(data_sel);
        // КРИТИЧЕСКИ ВАЖНО: SS тоже должен указывать на НАШ сегмент данных.
        // Limine оставляет SS своим (0x30 в его GDT). При прерывании CPU
        // кладёт текущий SS в IRET-фрейм, а при iretq ищет этот селектор
        // в НАШЕЙ GDT (где его нет) → #GP → DOUBLE FAULT!
        SS::set_reg(data_sel);

        // Активируем TSS командой ltr — с этого момента IST работают
        load_tss(tss_sel);
    }
}

/// Аварийный возврат в kernel shell при критической ошибке.
/// Вызывается из обработчика page fault при возврате из user mode.
pub fn kernel_panic(msg: &str) -> ! {
    use x86_64::structures::paging::PhysFrame;
    use x86_64::registers::control::{Cr3, Cr3Flags};
    use crate::paging;

    // Восстанавливаем kernel CR3
    let kernel_p4 = paging::kernel_p4_phys();
    if kernel_p4 != 0 {
        unsafe {
            let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(kernel_p4));
            Cr3::write(frame, Cr3Flags::empty());
        }
    }

    crate::println!("KERNEL PANIC: {}", msg);
    crate::println!("SS=0x{:x}, CS=0x{:x}", 
        x86_64::instructions::segmentation::SS::get_reg().0,
        x86_64::instructions::segmentation::CS::get_reg().0
    );
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
