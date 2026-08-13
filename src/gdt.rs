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

/* Отдельные стеки для обработчиков прерываний.
   16 КБ для #DF (нужен запас при диагностике), по 8 КБ для IRQ0/IRQ1.
   Размещаются в .bss ядра (higher-half, отображён в page tables).
   CPU переключается на них при входе в обработчик через IST и
   возвращается на прежний RSP при iretq. */
static mut DOUBLE_FAULT_STACK: [u8; 16384] = [0; 16384];
static mut TIMER_STACK: [u8; 8192] = [0; 8192];
static mut KEYBOARD_STACK: [u8; 8192] = [0; 8192];

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

        // Добавляем 64-битный сегмент кода (kernel_code_segment)
        let code_sel = GDT.append(Descriptor::kernel_code_segment());

        // Добавляем сегмент данных (kernel_data_segment)
        let data_sel = GDT.append(Descriptor::kernel_data_segment());

        // Добавляем сегмент состояния задачи (TSS) — для IST-стеков
        let tss_sel = GDT.append(Descriptor::tss_segment(&TSS));

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
