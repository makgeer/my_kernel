use x86_64::instructions::segmentation::{CS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};

/* IST index для Double Fault — будет использоваться при настройке TSS */
#[allow(dead_code)]
pub const DOUBLE_FAULT_IST_INDEX: u16 = 1;

/* GDT живёт в static mut — append() требует &mut self, load() требует &'static self.
   init_gdt() вызывается один раз при старте ядра — data race невозможна. */
#[allow(static_mut_refs)]
static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

#[allow(static_mut_refs)]
pub fn init_gdt() {
    unsafe {
        // Добавляем 64-битный сегмент кода (kernel_code_segment)
        let code_sel = GDT.append(Descriptor::kernel_code_segment());

        // Добавляем сегмент данных (kernel_data_segment)
        let _data_sel = GDT.append(Descriptor::kernel_data_segment());

        // Загружаем GDT
        GDT.load();

        // Устанавливаем селектор кода
        CS::set_reg(code_sel);
    }
}
