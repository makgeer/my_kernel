//! Тестовый модуль ядра (правило .kodaruls п.12).
//!
//! Проверка функций самого ядра: многозадачность, paging, PMM и т.д.
//! Вызывается из shell командой test_<name>.
//!
//! В отличие от отладочного модуля (src/debug.rs), здесь находятся
//! тесты, которые запускают пользовательские приложения и проверяют
//! работу подсистем ядра.

// Тест вытесняющего планировщика: запускает 2 процесса (loop_a + loop_b),
// которые печатают "A" и "B" по 20 раз каждый, затем завершаются через exit.
// Ожидаемый результат: чередование "A" и "B" (AAABBBAAABBB...),
// затем возврат в shell после завершения обоих процессов.
pub fn test_scheduler() {
    use crate::println;
    static LOOP_A: &[u8] = include_bytes!("../user_programs/loop_a.elf");
    static LOOP_B: &[u8] = include_bytes!("../user_programs/loop_b.elf");

    println!("[test_scheduler] Creating process A (loop_a)...");
    let proc_a = match crate::process::create_elf_process(LOOP_A) {
        Ok(p) => {
            println!("  Process A: P4=0x{:x}, entry=0x{:016x}", p.p4_phys, p.entry_point);
            p
        }
        Err(e) => {
            println!("  FAILED A: {}", e);
            return;
        }
    };

    println!("[test_scheduler] Creating process B (loop_b)...");
    let _proc_b = match crate::process::create_elf_process(LOOP_B) {
        Ok(p) => {
            println!("  Process B: P4=0x{:x}, entry=0x{:016x}", p.p4_phys, p.entry_point);
            p
        }
        Err(e) => {
            println!("  FAILED B: {}", e);
            return;
        }
    };

    println!("[test_scheduler] Launching A (timer will switch to B)...");
    println!("[test_scheduler] Expected: alternating A and B output");
    // Запускаем процесс A. run_process — noreturn.
    // Timer IRQ будет переключать на B через 5 тиков.
    crate::process::run_process(&proc_a);
    // Сюда не вернёмся — возврат через kernel_return_to_shell
    // (когда процесс вызовет exit).
}

// Тест IPC с блокировкой: запускает 2 процесса.
// Процесс A (ipc_receiver, PID=1) запускается ПЕРВЫМ и вызывает
// ipc_recv → блокируется (нет сообщений) → планировщик переключается
// на процесс B (ipc_sender, PID=2). B отправляет сообщение "HELLO"
// процессу A (разблокирует его), затем exit. При exit B планировщик
// переключается на A (теперь Ready). A повторяет int 0x80 → ipc_recv
// получает "HELLO" и выводит его.
// Ожидаемый результат: вывод "HELLO", затем возврат в shell.
pub fn test_ipc() {
    use crate::println;
    static RECEIVER: &[u8] = include_bytes!("../user_programs/ipc_receiver.elf");
    static SENDER: &[u8] = include_bytes!("../user_programs/ipc_sender.elf");

    println!("[test_ipc] Creating process A (ipc_receiver, PID=1)...");
    let proc_a = match crate::process::create_elf_process(RECEIVER) {
        Ok(p) => {
            println!("  Process A: P4=0x{:x}, entry=0x{:016x}", p.p4_phys, p.entry_point);
            p
        }
        Err(e) => {
            println!("  FAILED A: {}", e);
            return;
        }
    };

    println!("[test_ipc] Creating process B (ipc_sender, PID=2)...");
    let _proc_b = match crate::process::create_elf_process(SENDER) {
        Ok(p) => {
            println!("  Process B: P4=0x{:x}, entry=0x{:016x}", p.p4_phys, p.entry_point);
            p
        }
        Err(e) => {
            println!("  FAILED B: {}", e);
            return;
        }
    };

    println!("[test_ipc] Launching A (receiver, will block)...");
    println!("[test_ipc] Expected: HELLO from sender, then return to shell");
    // Запускаем процесс A. run_process — noreturn.
    // A вызовет ipc_recv → блокируется → переключение на B (sender).
    crate::process::run_process(&proc_a);
    // Сюда не вернёмся — возврат через kernel_return_to_shell.
}
