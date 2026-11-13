//! Менеджер процессов для вытесняющего планировщика (round-robin).
//!
//! Это НОВЫЙ КОД (бирюзовый, правило .kodaruls п.6).
//!
//! Текущий кооперативный планировщик (scheduler.rs) просто вызывает
//! функции по очереди. Этот модуль — основа для вытесняющего:
//! - ProcessContext: сохранённые регистры CPU для переключения контекста
//! - ProcessState: Ready / Running / Blocked / Terminated
//! - ManagedProcess: процесс с контекстом, состоянием, P4, стеком
//! - ProcessManager: таблица процессов, round-robin выбор следующего
//!
//! Переключение контекста (context_switch через asm) реализовано ниже.
//! Текущий этап: структуры данных + логика выбора процесса + context_switch.

use spin::Mutex;
use x86_64::instructions::interrupts;

/// Контекст следующего процесса для переключения из syscall exit.
/// Возвращается функцией switch_to_next_after_exit.
pub struct NextProcessInfo {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rsp: u64,
    pub cs: u64,
    pub ss: u64,
    pub rflags: u64,
    pub cr3: u64,
}

/// Переключается на следующий готовый процесс после terminate_current.
/// Помечает его Running, сбрасывает квант, инициализирует контекст
/// для первого запуска если нужно.
pub fn switch_to_next_after_exit(next_idx: usize) -> NextProcessInfo {
    let mut pm = PROCESS_MANAGER.lock();
    let info = {
        let proc = pm.processes[next_idx].as_mut().unwrap();
        proc.state = ProcessState::Running;
        proc.ticks_left = proc.quantum;
        if !proc.started {
            proc.context.rip = proc.entry_point;
            proc.context.rsp = proc.stack_top;
            proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
            proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
            proc.context.rflags = 0x202;
            proc.context.cr3 = proc.p4_phys;
            proc.started = true;
        }
        NextProcessInfo {
            rax: proc.context.rax,
            rbx: proc.context.rbx,
            rcx: proc.context.rcx,
            rdx: proc.context.rdx,
            rsi: proc.context.rsi,
            rdi: proc.context.rdi,
            rbp: proc.context.rbp,
            r8: proc.context.r8,
            r9: proc.context.r9,
            r10: proc.context.r10,
            r11: proc.context.r11,
            r12: proc.context.r12,
            r13: proc.context.r13,
            r14: proc.context.r14,
            r15: proc.context.r15,
            rip: proc.context.rip,
            rsp: proc.context.rsp,
            cs: proc.context.cs,
            ss: proc.context.ss,
            rflags: proc.context.rflags,
            cr3: proc.p4_phys,
        }
    };
    pm.current = Some(next_idx);
    info
}

/// Максимальное количество процессов в системе.
pub const MAX_PROCESSES: usize = 16;

/// Максимальная длина данных в одном сообщении IPC.
pub const IPC_MSG_SIZE: usize = 32;

/// Код возврата ipc_send: получатель не найден (EPIPE).
pub const IPC_NO_RECEIVER: i64 = -1;

/// Код возврата ipc_send: почтовый ящик получателя переполнен.
/// Блокирующий вариант send должен приостановить отправителя и повторить.
pub const IPC_FULL: i64 = -2;

/// Количество сообщений в почтовом ящике процесса.
pub const IPC_MAILBOX_SIZE: usize = 4;

/// Сообщение IPC — единица обмена данными между процессами.
///
/// Содержит PID отправителя, длину данных и сами данные.
/// Фиксированный размер (без аллокации) — для no_std.
#[derive(Clone, Copy)]
pub struct IpcMessage {
    /// PID процесса-отправителя (0 = ядро).
    pub from_pid: usize,
    /// Длина полезных данных (байт, 0..=IPC_MSG_SIZE).
    pub len: u8,
    /// Данные сообщения (заполняются до len, остальное — нули).
    pub data: [u8; IPC_MSG_SIZE],
}

impl IpcMessage {
    /// Создаёт пустое сообщение.
    pub const fn empty() -> Self {
        Self {
            from_pid: 0,
            len: 0,
            data: [0; IPC_MSG_SIZE],
        }
    }
}

/// Почтовый ящик процесса — кольцевый буфер сообщений IPC.
///
/// Процесс может получить сообщение через syscall ipc_recv.
/// Если почтовый ящик пуст — процесс блокируется (Blocked) до получения.
#[derive(Clone, Copy)]
pub struct Mailbox {
    /// Кольцевый буфер сообщений.
    messages: [IpcMessage; IPC_MAILBOX_SIZE],
    /// Индекс головы (куда писать новое сообщение).
    head: usize,
    /// Индекс хвоста (откуда читать следующее сообщение).
    tail: usize,
    /// Количество сообщений в буфере.
    count: usize,
}

impl Mailbox {
    /// Создаёт пустой почтовый ящик.
    pub const fn new() -> Self {
        Self {
            messages: [IpcMessage::empty(); IPC_MAILBOX_SIZE],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Добавляет сообщение в почтовый ящик.
    /// Возвращает false если буфер переполнен.
    pub fn push(&mut self, msg: IpcMessage) -> bool {
        if self.count >= IPC_MAILBOX_SIZE {
            return false;
        }
        self.messages[self.head] = msg;
        self.head = (self.head + 1) % IPC_MAILBOX_SIZE;
        self.count += 1;
        true
    }

    /// Извлекает следующее сообщение из почтового ящика.
    /// Возвращает None если буфер пуст.
    pub fn pop(&mut self) -> Option<IpcMessage> {
        if self.count == 0 {
            return None;
        }
        let msg = self.messages[self.tail];
        self.tail = (self.tail + 1) % IPC_MAILBOX_SIZE;
        self.count -= 1;
        Some(msg)
    }
}

/// Состояние процесса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Готов к выполнению (ждёт кванта времени).
    Ready,
    /// Сейчас выполняется на CPU.
    Running,
    /// Заблокирован (ждёт ввода, IPC и т.д.).
    Blocked,
    /// Завершён (слот можно переиспользовать).
    Terminated,
}

/// Причина блокировки процесса.
///
/// Нужна, чтобы будить процесс только по тому событию, которого он ждёт:
/// ipc_send не должен будить процесс в irq_wait и наоборот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    /// Процесс не заблокирован.
    None,
    /// Ожидание аппаратного прерывания (syscall irq_wait #42).
    Irq(u8),
    /// Ожидание сообщения IPC (syscall ipc_recv #41).
    IpcRecv,
    /// Ожидание освобождения ящика получателя (ipc_send #40).
    IpcSend(usize),
}

/// Сохранённый контекст CPU для переключения между процессами.
///
/// При переключении контекста ядро сохраняет регистры текущего
/// процесса в эту структуру, затем восстанавливает регистры
/// следующего процесса из его структуры.
///
/// Порядок полей важен для asm-кода context_switch: он читает/пишет
/// поля по смещениям через указатель на структуру.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessContext {
    // Регистры общего назначения (сохраняются при переключении)
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    // Указатель инструкций (точка возобновления выполнения)
    pub rip: u64,
    // Селектор сегмента кода (CS из IRET-фрейма)
    pub cs: u64,
    // Указатель стека (RSP из IRET-фрейма)
    pub rsp: u64,
    // Селектор сегмента стека (SS из IRET-фрейма)
    pub ss: u64,
    // Флаги (RFLAGS)
    pub rflags: u64,
    // CR3 — физический адрес P4 таблицы процесса
    pub cr3: u64,
}

/// IRET-фрейм на стеке после входа в interrupt handler.
///
/// При переходе из ring 3 CPU пушит 5 значений:
///   SS, RSP, RFLAGS, CS, RIP (в порядке пуша — SS первым).
/// При использовании IST (таймер, #DF, #PF) CPU в long mode пушит те же
/// 5 значений и из ring 0. Только обычное прерывание без IST из ring 0
/// даёт 3-словный фрейм (RIP, CS, RFLAGS).
///
/// Порядок полей = порядок на стеке (RIP по наименьшему адресу).
/// Используется для модификации IRET-фрейма при переключении контекста
/// из timer IRQ: меняем RIP/RSP/CS/SS/RFLAGS → iretq переходит к
/// следующему процессу.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IretFrame {
    /// RIP — точка возобновления выполнения.
    pub rip: u64,
    /// CS — селектор сегмента кода (с RPL).
    pub cs: u64,
    /// RFLAGS — флаги процессора.
    pub rflags: u64,
    /// RSP — указатель стека пользователя.
    pub rsp: u64,
    /// SS — селектор сегмента стека (с RPL).
    pub ss: u64,
}

/// Регистры общего назначения, сохраняемые на стеке naked timer handler.
///
/// Порядок полей = ОБРАТНЫЙ порядок push'ей в asm.
/// Стек растёт вниз: первый push (r15) — по наибольшему адресу,
/// последний push (rax) — по наименьшему адресу (RSP).
/// `mov rdi, rsp` → rdi указывает на rax (наименьший адрес) → первое поле.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GprRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

// Layout GprRegs обязан совпадать со стеком timer_handler_naked:
// push r15 первым … push rax последним → по адресу rsp лежит rax.
// 15 * 8 = 120 байт. Структура НЕ взаимозаменяема с SyscallRegisters
// (там обратный порядок push и первое поле r15).
const _: () = assert!(core::mem::size_of::<GprRegs>() == 120);

impl ProcessContext {
    /// Создаёт пустой контекст (все регистры = 0).
    pub const fn new() -> Self {
        Self {
            rax: 0, rbx: 0, rcx: 0, rdx: 0,
            rsi: 0, rdi: 0, rbp: 0,
            r8: 0, r9: 0, r10: 0, r11: 0,
            r12: 0, r13: 0, r14: 0, r15: 0,
            rip: 0, cs: 0, rsp: 0, ss: 0, rflags: 0, cr3: 0,
        }
    }
}

/// Процесс для вытесняющего планировщика.
///
/// Расширяет базовую структуру process::Process сохранённым контекстом
/// и состоянием для планирования.
#[derive(Clone, Copy)]
pub struct ManagedProcess {
    /// PID — уникальный идентификатор процесса (из next_pid, не индекс).
    pub pid: usize,
    /// Имя процесса (для диагностики).
    pub name: &'static str,
    /// Состояние процесса.
    pub state: ProcessState,
    /// Физический адрес P4 таблицы процесса (CR3).
    pub p4_phys: u64,
    /// Точка входа (RIP при первом запуске).
    pub entry_point: u64,
    /// Вершина стека пользователя.
    pub stack_top: u64,
    /// Сохранённый контекст CPU.
    pub context: ProcessContext,
    /// Квант времени в тиках таймера (по умолчанию 5).
    pub quantum: u8,
    /// Оставшиеся тики до вытеснения.
    pub ticks_left: u8,
    /// Был ли процесс уже запущен (для первого запуска нужны USER selectors).
    pub started: bool,
    /// Почтовый ящик для сообщений IPC.
    pub mailbox: Mailbox,
    /// Причина текущей блокировки (None — процесс не заблокирован).
    /// Используется механизмом доставки прерываний и блокирующим IPC.
    pub wait: WaitKind,
}

impl ManagedProcess {
    /// Создаёт новый процесс в состоянии Ready.
    pub fn new(
        pid: usize,
        name: &'static str,
        p4_phys: u64,
        entry_point: u64,
        stack_top: u64,
    ) -> Self {
        Self {
            pid,
            name,
            state: ProcessState::Ready,
            p4_phys,
            entry_point,
            stack_top,
            context: ProcessContext::new(),
            quantum: 5,
            ticks_left: 5,
            started: false,
            mailbox: Mailbox::new(),
            wait: WaitKind::None,
        }
    }
}

/// Глобальный менеджер процессов.
///
/// Хранит таблицу процессов и индекс текущего.
/// Доступ через Mutex — защищает от data race при доступе из timer IRQ.
pub struct ProcessManager {
    /// Таблица процессов (слоты).
    pub processes: [Option<ManagedProcess>; MAX_PROCESSES],
    /// Индекс текущего процесса в таблице (None = ядро, нет активного процесса).
    pub current: Option<usize>,
    /// Счётчик созданных процессов (для PID).
    pub next_pid: usize,
}

impl ProcessManager {
    /// Создаёт пустой менеджер.
    pub const fn new() -> Self {
        Self {
            // init массива Option<ManagedProcess> = None
            // const fn не позволяет массивные инициализаторы, используем workaround:
            // [None; MAX_PROCESSES] работает для типов, реализующих Copy
            processes: [None; MAX_PROCESSES],
            current: None,
            next_pid: 1,
        }
    }

    /// Добавляет процесс в таблицу.
    ///
    /// Возвращает PID при успехе, или ошибку если нет свободных слотов.
    pub fn add(&mut self, name: &'static str, p4_phys: u64, entry_point: u64, stack_top: u64) -> Result<usize, &'static str> {
        for i in 0..MAX_PROCESSES {
            if self.processes[i].is_none() || self.processes[i].as_ref().unwrap().state == ProcessState::Terminated {
                let pid = self.next_pid;
                self.next_pid += 1;
                self.processes[i] = Some(ManagedProcess::new(pid, name, p4_phys, entry_point, stack_top));
                return Ok(pid);
            }
        }
        Err("process_manager: no free process slots")
    }

    /// Выбирает следующий готовый процесс (round-robin).
    ///
    /// Ищет процесс в состоянии Ready, начиная со следующего индекса
    /// после текущего. Если ни один процесс не готов — возвращает None
    /// (ядро продолжает работу).
    pub fn select_next(&mut self) -> Option<usize> {
        let start = match self.current {
            Some(idx) => (idx + 1) % MAX_PROCESSES,
            None => 0,
        };

        for offset in 0..MAX_PROCESSES {
            let i = (start + offset) % MAX_PROCESSES;
            if let Some(ref proc) = self.processes[i] {
                if proc.state == ProcessState::Ready {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Отмечает процесс как завершённый.
    ///
    /// Освобождает IRQ, которым владел процесс, и будит отправителей,
    /// ждавших освобождения его почтового ящика (иначе они остались бы
    /// Blocked навсегда).
    pub fn terminate(&mut self, index: usize) {
        let pid = match self.processes[index].as_ref() {
            Some(p) => p.pid,
            None => return,
        };
        if let Some(proc) = self.processes[index].as_mut() {
            proc.state = ProcessState::Terminated;
            proc.wait = WaitKind::None;
        }
        if self.current == Some(index) {
            self.current = None;
        }
        if pid != 0 {
            release_irq_owner(pid);
            wake_senders_waiting_for(self, pid);
        }
    }

    /// Завершает процесс по PID (а не по индексу слота).
    /// Возвращает true, если процесс найден.
    pub fn terminate_by_pid(&mut self, pid: usize) -> bool {
        for i in 0..MAX_PROCESSES {
            let found = self.processes[i]
                .as_ref()
                .map(|p| p.pid == pid && p.state != ProcessState::Terminated)
                .unwrap_or(false);
            if found {
                let name = self.processes[i].as_ref().unwrap().name;
                self.terminate(i);
                crate::println!("  process PID={} ({}) -> Terminated", pid, name);
                return true;
            }
        }
        false
    }

    /// Паника текущего процесса: terminate + switch к следующему.
    /// Используется для обработки фатальных ошибок (stack overflow,
    /// illegal instruction, и т.д.) — процесс уничтожается, система
    /// продолжает работу с другим процессом.
    pub fn panic_current(&mut self, reason: &str) {
        if let Some(idx) = self.current {
            let (pid, name) = self.processes[idx]
                .as_ref()
                .map(|p| (p.pid, p.name))
                .unwrap_or((0, "unknown"));
            crate::println!(
                "  Terminating process PID={} ({}) reason='{}'",
                pid,
                name,
                reason
            );
            self.terminate(idx);
            // Переключаемся к следующему процессу. ВАЖНО: не вызываем
            // next_process() — он попытался бы повторно взять
            // PROCESS_MANAGER, который уже удерживается вызывающим кодом.
            if let Some(next) = self.select_next() {
                {
                    let proc = self.processes[next].as_mut().unwrap();
                    proc.state = ProcessState::Running;
                    proc.ticks_left = proc.quantum;
                    if !proc.started {
                        proc.context.rip = proc.entry_point;
                        proc.context.rsp = proc.stack_top;
                        proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
                        proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
                        proc.context.rflags = 0x202;
                        proc.context.cr3 = proc.p4_phys;
                        proc.started = true;
                    }
                }
                crate::println!("  Switched to PID={}", self.processes[next].as_ref().unwrap().pid);
                let to_ctx = core::ptr::addr_of!(self.processes[next]
                    .as_ref()
                    .unwrap()
                    .context);
                self.current = Some(next);
                // context_switch не возвращается, поэтому guard мьютекса
                // не будет освобождён обычным Drop. Освобождаем вручную,
                // иначе следующий процесс зависнет на первом же lock.
                unsafe {
                    crate::process_manager::PROCESS_MANAGER.force_unlock();
                }
                unsafe {
                    crate::process_manager::context_switch(to_ctx);
                }
            } else {
                // Нет других процессов — возвращаемся в shell
                crate::println!("  No other processes — returning to shell");
                crate::process::restore_kernel_cr3();
            }
        }
    }

    /// Печатает список процессов (для команды shell).
    ///
    /// ОПТИМИЗАЦИЯ: отключаем auto_flush на время вывода, чтобы избежать
    /// N полных перерисовок framebuffer (2000 ячеек × 128 пикселей каждая)
    /// — в Bochs это тормозит на десятки секунд. Вместо этого делаем
    /// одну перерисовку в конце.
    pub fn list(&self) {
        // Отключаем авто-flush — текст пишется в буфер терминала и serial,
        // но экран не перерисовывается до явного flush().
        crate::terminal::TERMINAL.lock().set_auto_flush(false);
        crate::println!("PID  State     Name        Entry              P4");
        crate::println!("---  --------  ----------  -----------------  ------------------");
        for i in 0..MAX_PROCESSES {
            if let Some(ref proc) = self.processes[i] {
                if proc.state != ProcessState::Terminated {
                    let state_str = match proc.state {
                        ProcessState::Ready => "Ready",
                        ProcessState::Running => "Running",
                        ProcessState::Blocked => "Blocked",
                        ProcessState::Terminated => "Terminated",
                    };
                    crate::println!(
                        "{:3}  {:8}  {:10}  0x{:016x}  0x{:016x}",
                        proc.pid, state_str, proc.name, proc.entry_point, proc.p4_phys
                    );
                }
            }
        }
        // Включаем авто-flush обратно и делаем одну перерисовку.
        let mut term = crate::terminal::TERMINAL.lock();
        term.set_auto_flush(true);
        term.flush();
    }
}

/// Будит отправителей, которые ждали освобождения ящика процесса `pid`.
/// При завершении получателя они повторят ipc_send и получат
/// IPC_NO_RECEIVER вместо вечной блокировки.
fn wake_senders_waiting_for(pm: &mut ProcessManager, pid: usize) {
    for i in 0..MAX_PROCESSES {
        if let Some(p) = pm.processes[i].as_mut() {
            if p.wait == WaitKind::IpcSend(pid) && p.state == ProcessState::Blocked {
                p.wait = WaitKind::None;
                p.state = ProcessState::Ready;
            }
        }
    }
}

/// Глобальный экземпляр менеджера процессов.
pub static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());

/// Добавляет процесс в менеджер (обёртка над PROCESS_MANAGER.lock()).
/// Возвращает PID при успехе.
///
/// without_interrupts: отключаем прерывания на время захвата lock,
/// чтобы timer IRQ не попытался залочить PROCESS_MANAGER повторно → deadlock.
pub fn spawn(name: &'static str, p4_phys: u64, entry_point: u64, stack_top: u64) -> Result<usize, &'static str> {
    interrupts::without_interrupts(|| {
        PROCESS_MANAGER.lock().add(name, p4_phys, entry_point, stack_top)
    })
}

/// Печатает список процессов (для команды shell `ps`).
///
/// without_interrupts: та же причина — пока мы держим lock и выводим
/// таблицу, timer IRQ не должен прерывать и пытаться залочить PROCESS_MANAGER.
pub fn list_processes() {
    interrupts::without_interrupts(|| {
        PROCESS_MANAGER.lock().list();
    })
}

/// Завершает процесс по PID (команда shell `kill`).
/// Возвращает true, если процесс найден.
///
/// without_interrupts: таймер не должен пытаться взять PROCESS_MANAGER
/// во время работы с таблицей → deadlock.
pub fn terminate_by_pid(pid: usize) -> bool {
    interrupts::without_interrupts(|| {
        PROCESS_MANAGER.lock().terminate_by_pid(pid)
    })
}

/// Отмечает текущий процесс как завершённый (Terminated).
/// Вызывается из обработчика syscall exit (int 0x80, #60).
/// После этого слот процесса можно переиспользовать.
///
/// Возвращает индекс следующего готового (Ready) процесса, если такой есть,
/// иначе None. Это позволяет обработчику exit переключиться на другой
/// процесс вместо возврата в shell.
///
/// without_interrupts: syscall выполняется в ring 0, прерывания могут
/// сработать во время держания lock → deadlock.
pub fn terminate_current() -> Option<usize> {
    interrupts::without_interrupts(|| {
        let mut pm = PROCESS_MANAGER.lock();
        let terminated_idx = pm.current;
        if let Some(idx) = terminated_idx {
            let (pid, name) = pm.processes[idx]
                .as_ref()
                .map(|p| (p.pid, p.name))
                .unwrap_or((0, "unknown"));
            crate::println!("  process PID={} ({}) -> Terminated", pid, name);
            // terminate освобождает IRQ и будит отправителей,
            // ожидавших освобождения ящика этого процесса.
            pm.terminate(idx);
        }
        // Ищем следующий готовый процесс (после завершённого, round-robin)
        let start = terminated_idx.map(|i| i + 1).unwrap_or(0);
        for i in 0..MAX_PROCESSES {
            let idx = (start + i) % MAX_PROCESSES;
            if let Some(proc) = pm.processes[idx].as_ref() {
                if proc.state == ProcessState::Ready {
                    return Some(idx);
                }
            }
        }
        None
    })
}

/// Отправляет сообщение IPC от текущего процесса процессу с указанным PID.
///
/// Копирует данные из user-буфера в сообщение и помещает его в почтовый
/// ящик получателя. Если получатель заблокирован (Blocked, ждёт recv) —
/// разблокирует его (переводит в Ready).
///
/// Возвращает 0 при успехе, -1 при ошибке (нет получателя, переполнен ящик).
pub fn ipc_send(to_pid: usize, data: &[u8]) -> i64 {
    interrupts::without_interrupts(|| {
        let mut pm = PROCESS_MANAGER.lock();
        let from_pid = pm.current
            .and_then(|idx| pm.processes[idx].as_ref())
            .map(|p| p.pid)
            .unwrap_or(0);

        // Ограничиваем длину данных
        let len = data.len().min(IPC_MSG_SIZE) as u8;

        // Ищем получателя по PID
        let recv_idx = (0..MAX_PROCESSES).find(|&i| {
            pm.processes[i].as_ref()
                .map(|p| p.pid == to_pid && p.state != ProcessState::Terminated)
                .unwrap_or(false)
        });

        let recv_idx = match recv_idx {
            Some(idx) => idx,
            None => return IPC_NO_RECEIVER, // Получатель не найден
        };

        // Формируем сообщение
        let mut msg = IpcMessage::empty();
        msg.from_pid = from_pid;
        msg.len = len;
        let copy_len = len as usize;
        msg.data[..copy_len].copy_from_slice(&data[..copy_len]);

        // Помещаем в почтовый ящик получателя
        let proc = pm.processes[recv_idx].as_mut().unwrap();
        if !proc.mailbox.push(msg) {
            // Почтовый ящик переполнен. Асинхронный send возвращает
            // ошибку; блокирующий вариант (ipc_send_blocking) спишет
            // отправителя в ожидание и повторит попытку позже.
            IPC_FAILED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return IPC_FULL;
        }

        IPC_SENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        // Будим получателя, только если он ждёт именно ipc_recv.
        // Нельзя будить процессы в irq_wait или в блокирующем send.
        if proc.state == ProcessState::Blocked && proc.wait == WaitKind::IpcRecv {
            proc.wait = WaitKind::None;
            proc.state = ProcessState::Ready;
        }

        0
    })
}

/// Получает сообщение IPC для текущего процесса.
///
/// Если в почтовом ящике есть сообщение — извлекает его и копирует данные
/// в user-буфер. Если пусто — возвращает None (вызывающий код должен
/// заблокировать процесс и переключиться).
///
/// Возвращает (from_pid, len) при успехе.
pub fn ipc_recv(buf: &mut [u8]) -> Option<(usize, usize)> {
    interrupts::without_interrupts(|| {
        let mut pm = PROCESS_MANAGER.lock();
        let cur_idx = pm.current?;
        let recv_pid = pm.processes[cur_idx].as_ref()?.pid;
        let proc = pm.processes[cur_idx].as_mut()?;
        let msg = proc.mailbox.pop()?;
        let copy_len = (msg.len as usize).min(buf.len());
        buf[..copy_len].copy_from_slice(&msg.data[..copy_len]);

        IPC_RECV.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        // Освободилось место в ящике — будим отправителей, которые
        // ждали именно этого получателя (блокирующий ipc_send).
        for i in 0..MAX_PROCESSES {
            if let Some(p) = pm.processes[i].as_mut() {
                if p.wait == WaitKind::IpcSend(recv_pid)
                    && p.state == ProcessState::Blocked
                {
                    p.wait = WaitKind::None;
                    p.state = ProcessState::Ready;
                }
            }
        }

        Some((msg.from_pid, copy_len))
    })
}

/// Печатает статистику IPC (команда shell `ipcstat`).
pub fn print_ipc_stats() {
    use core::sync::atomic::Ordering;
    crate::println!(
        "IPC: sent={}, received={}, failed={}",
        IPC_SENT.load(Ordering::Relaxed),
        IPC_RECV.load(Ordering::Relaxed),
        IPC_FAILED.load(Ordering::Relaxed)
    );
}

/// Блокирует текущий процесс и ищет следующий готовый.
///
/// Сохраняет контекст текущего процесса (GPR + IRET-фрейм) с RIP,
/// уменьшенным на 2 (размер int 0x80), чтобы при возобновлении
/// процесс повторил syscall (например ipc_recv) — к этому моменту
/// сообщение уже будет в почтовом ящике.
///
/// `wait` устанавливается в том же критическом разделе, что и
/// `state = Blocked`: таймер не может вытеснить процесс между
/// «пометить ожидание» и «заблокировать» и сохранить RIP после int 0x80.
///
/// Возвращает (next_idx, NextProcessInfo) для переключения.
/// Если готовых процессов нет:
/// - для WaitKind::Irq процесс остаётся Blocked (вызывающий вернётся
///   в kernel shell, IRQ разбудит процесс позже);
/// - для IPC блокировка откатывается (процесс остаётся Running), чтобы
///   состояние не рассогласовывалось с current.
pub fn block_current_and_switch(
    gpr: &[u64; 15],
    rip: u64,
    rsp: u64,
    cs: u64,
    ss: u64,
    rflags: u64,
    wait: WaitKind,
) -> Option<(usize, NextProcessInfo)> {
    let mut pm = PROCESS_MANAGER.lock();

    let cur_idx = pm.current?;
    {
        let proc = pm.processes[cur_idx].as_mut()?;
        proc.state = ProcessState::Blocked;
        proc.started = true;
        proc.wait = wait;

        // Сохраняем GPR (порядок: rax, rbx, rcx, rdx, rsi, rdi, rbp,
        // r8, r9, r10, r11, r12, r13, r14, r15 — как в GprRegs)
        proc.context.rax  = gpr[0];
        proc.context.rbx  = gpr[1];
        proc.context.rcx  = gpr[2];
        proc.context.rdx  = gpr[3];
        proc.context.rsi  = gpr[4];
        proc.context.rdi  = gpr[5];
        proc.context.rbp  = gpr[6];
        proc.context.r8   = gpr[7];
        proc.context.r9   = gpr[8];
        proc.context.r10  = gpr[9];
        proc.context.r11  = gpr[10];
        proc.context.r12  = gpr[11];
        proc.context.r13  = gpr[12];
        proc.context.r14  = gpr[13];
        proc.context.r15  = gpr[14];

        // RIP -= 2: при возобновлении процесс повторит int 0x80
        // (перезапуск syscall). int 0x80 = CD 80 = 2 байта.
        proc.context.rip = rip - 2;
        proc.context.rsp = rsp;
        proc.context.cs = cs;
        proc.context.ss = ss;
        proc.context.rflags = rflags;
        proc.context.cr3 = proc.p4_phys;
    }

    pm.current = None;

    // Ищем следующий Ready процесс (round-robin начиная с cur_idx+1)
    let start = cur_idx + 1;
    let next_idx = (0..MAX_PROCESSES)
        .map(|i| (start + i) % MAX_PROCESSES)
        .find(|&idx| {
            pm.processes[idx].as_ref()
                .map(|p| p.state == ProcessState::Ready)
                .unwrap_or(false)
        });

    let next_idx = match next_idx {
        Some(idx) => idx,
        None => {
            if matches!(wait, WaitKind::Irq(_)) {
                // Драйвер ждёт IRQ: оставляем Blocked, управление
                // вернётся в shell (idle), IRQ разбудит процесс.
                return None;
            }
            // IPC: готовых процессов нет — откатываем блокировку,
            // чтобы процесс не остался Blocked с current=None.
            if let Some(proc) = pm.processes[cur_idx].as_mut() {
                proc.state = ProcessState::Running;
                proc.wait = WaitKind::None;
            }
            pm.current = Some(cur_idx);
            return None;
        }
    };

    let info = {
        let proc = pm.processes[next_idx].as_mut()?;
        proc.state = ProcessState::Running;
        proc.ticks_left = proc.quantum;
        if !proc.started {
            proc.context.rip = proc.entry_point;
            proc.context.rsp = proc.stack_top;
            proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
            proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
            proc.context.rflags = 0x202;
            proc.context.cr3 = proc.p4_phys;
            proc.started = true;
        }
        NextProcessInfo {
            rax:  proc.context.rax,
            rbx:  proc.context.rbx,
            rcx:  proc.context.rcx,
            rdx:  proc.context.rdx,
            rsi:  proc.context.rsi,
            rdi:  proc.context.rdi,
            rbp:  proc.context.rbp,
            r8:   proc.context.r8,
            r9:   proc.context.r9,
            r10:  proc.context.r10,
            r11:  proc.context.r11,
            r12:  proc.context.r12,
            r13:  proc.context.r13,
            r14:  proc.context.r14,
            r15:  proc.context.r15,
            rip:  proc.context.rip,
            rsp:  proc.context.rsp,
            cs:   proc.context.cs,
            ss:   proc.context.ss,
            rflags: proc.context.rflags,
            cr3:  proc.p4_phys,
        }
    };

    pm.current = Some(next_idx);
    Some((next_idx, info))
}

/// Возвращает PID текущего процесса (None — если активно ядро/shell).
///
/// PID — это номер процесса для user space (syscall getpid #39),
/// а не индекс слота в таблице.
pub fn current_pid() -> Option<usize> {
    interrupts::without_interrupts(|| {
        let pm = PROCESS_MANAGER.lock();
        pm.current
            .and_then(|idx| pm.processes[idx].as_ref().map(|p| p.pid))
    })
}

// ---------------------------------------------------------------------------
// Доставка прерываний в user space (основа для выноса драйверов).
//
// Ядро больше не обязано само обслуживать устройство: драйвер-процесс
// заявляет владение IRQ (claim_irq), ждёт прерывание (irq_wait) и читает
// порты через sys_io_in. Ядро в этом случае только считает прерывания
// и будит ожидающего — минимальная работа, как в микроядре.
// ---------------------------------------------------------------------------

/// Счётчики непрочитанных прерываний по номерам IRQ (0..15).
/// Инкрементируется в IRQ-handler, декрементируется в irq_wait.
static IRQ_PENDING: [core::sync::atomic::AtomicU32; 16] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 16];

/// PID процесса-владельца IRQ (0 — IRQ обслуживается ядром).
static IRQ_OWNER: [core::sync::atomic::AtomicUsize; 16] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; 16];

/// Статистика IPC — для диагностики и команды `ipcstat`.
pub static IPC_SENT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static IPC_RECV: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static IPC_FAILED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Сохранённый контекст ядра (shell), вытесненного таймером.
///
/// Это «idle-поток» микроядерной модели: когда планировщику нечего
/// запускать, управление возвращается ровно в ту точку shell, где его
/// прервал таймер. Без этого возврат шёл бы через kernel_return_to_shell,
/// который заново вызывает shell() — рекурсия на каждый блок процесса.
#[derive(Clone, Copy)]
struct IdleContext {
    gpr: GprRegs,
    iret: IretFrame,
    cr3: u64,
}

static mut IDLE_CTX: IdleContext = IdleContext {
    gpr: GprRegs {
        rax: 0, rbx: 0, rcx: 0, rdx: 0, rsi: 0, rdi: 0, rbp: 0,
        r8: 0, r9: 0, r10: 0, r11: 0, r12: 0, r13: 0, r14: 0, r15: 0,
    },
    iret: IretFrame { rip: 0, cs: 0, rflags: 0, rsp: 0, ss: 0 },
    cr3: 0,
};

/// Валиден ли сохранённый контекст ядра.
static IDLE_CTX_VALID: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Shell находится в idle (спит в sti; hlt и ждёт ввода).
///
/// Таймер запускает Ready-процесс только при этом флаге: иначе процесс
/// мог бы начать работу, пока ядро держит TERMINAL.lock внутри println!,
/// и его write завис бы на том же спин-мьютексе.
static SHELL_IDLE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Сообщает планировщику, что shell простаивает и готов уступить CPU.
pub fn set_shell_idle(idle: bool) {
    SHELL_IDLE.store(idle, core::sync::atomic::Ordering::SeqCst);
}

/// Сохраняет контекст ядра перед переключением на user-процесс.
/// Вызывается из таймера, когда shell вытесняется впервые.
///
/// `frame_rsp`/`frame_ss` — реальные RSP и SS прерванного контекста.
/// Таймер доставляется через IST, поэтому CPU в long mode пушит SS:RSP
/// безусловно (даже из ring 0), и эти поля IRET-фрейма валидны.
///
/// # Safety
/// Вызывается только из обработчика прерывания (монопольный доступ
/// к статике через addr_of_mut, без создания ссылок на static mut).
unsafe fn save_idle_context(
    regs: *const GprRegs,
    frame: *const IretFrame,
    frame_rsp: u64,
    frame_ss: u64,
) {
    let ctx = core::ptr::addr_of_mut!(IDLE_CTX);
    (*ctx).gpr = *regs;
    (*ctx).iret = *frame;
    (*ctx).iret.rsp = frame_rsp;
    (*ctx).iret.ss = frame_ss;
    (*ctx).cr3 = crate::paging::kernel_p4_phys();
    IDLE_CTX_VALID.store(true, core::sync::atomic::Ordering::SeqCst);
}

/// Возвращает сохранённый контекст ядра, если он есть.
/// Используется обработчиком syscall, когда процесс заблокировался,
/// а других готовых процессов нет: возврат в shell идёт ровно в ту
/// точку, где его прервал таймер.
pub fn take_idle_context() -> Option<(GprRegs, IretFrame, u64)> {
    // Забираем контекст ровно один раз: после возврата в shell он устарел,
    // при следующем вытеснении таймер сохранит новый.
    if IDLE_CTX_VALID.swap(false, core::sync::atomic::Ordering::SeqCst) == false {
        return None;
    }
    let ctx = core::ptr::addr_of!(IDLE_CTX);
    // SAFETY: читаем неизменяемую копию структуры; поля — Copy.
    let gpr = unsafe { (*ctx).gpr };
    let iret = unsafe { (*ctx).iret };
    let cr3 = unsafe { (*ctx).cr3 };
    Some((gpr, iret, cr3))
}

/// Возобновляет выполнение ядра (shell) из сохранённого idle-контекста.
///
/// Возвращает false, если сохранённого контекста нет.
///
/// Почему не iretq: вызов идёт из обычной Rust-функции на стеке
/// обработчика, а не из asm-обёртки с готовым фреймом. Поэтому RSP
/// восстанавливается явно, а переход выполняется через jmp.
/// (В long mode iretq, в отличие от 32-битного режима, загружает
/// RSP и SS и при возврате в ring 0.)
///
/// Регистры-источники (rax, rcx, rdx, rsi, rdi, r8, r9, r10) не
/// пересекаются с восстанавливаемыми (rsp, rbp, rbx, r12-r15),
/// поэтому порядок инструкций безопасен.
pub fn resume_idle_context() -> bool {
    let (gpr, iret, cr3) = match take_idle_context() {
        Some(v) => v,
        None => return false,
    };

    if cr3 != 0 {
        unsafe {
            let frame = x86_64::structures::paging::PhysFrame::containing_address(
                x86_64::PhysAddr::new(cr3),
            );
            x86_64::registers::control::Cr3::write(
                frame,
                x86_64::registers::control::Cr3Flags::empty(),
            );
        }
    }

    // SS/DS/ES после int 0x80 из ring 3 равны NULL: CPU обнуляет их при
    // переходе в ring 0. jmp сегменты не восстанавливает, поэтому
    // следующий iretq из таймера вытолкнул бы SS=0 — по SDM это #GP.
    //
    // В long mode SS не задаёт базу стека (адресация плоская), поэтому
    // RSP из IDLE_CTX и SS=0x10 согласованы: стек выбирается по RSP.
    unsafe {
        core::arch::asm!(
            "mov ax, 0x10",
            "mov ss, ax",
            "mov ds, ax",
            "mov es, ax",
            out("ax") _,
            options(nomem, nostack, preserves_flags)
        );
    }

    unsafe {
        core::arch::asm!(
            "mov rsp, rax",
            "mov rbp, rcx",
            "mov rbx, rdx",
            "mov r12, rsi",
            "mov r13, rdi",
            "mov r14, r8",
            "mov r15, r9",
            "jmp r10",
            in("rax") iret.rsp,
            in("rcx") gpr.rbp,
            in("rdx") gpr.rbx,
            in("rsi") gpr.r12,
            in("rdi") gpr.r13,
            in("r8")  gpr.r14,
            in("r9")  gpr.r15,
            in("r10") iret.rip,
            options(noreturn)
        );
    }
}

/// Регистрирует процесс как владельца IRQ. После этого ядро перестаёт
/// само обслуживать устройство и только уведомляет драйвер.
///
/// Возвращает true при успехе.
pub fn claim_irq(irq: u8, pid: usize) -> bool {
    if irq >= 16 || pid == 0 {
        return false;
    }
    // Захватываем IRQ только если он свободен. Повторный claim тем же
    // процессом идемпотентен; захват IRQ у работающего драйвера запрещён.
    match IRQ_OWNER[irq as usize].compare_exchange(
        0,
        pid,
        core::sync::atomic::Ordering::SeqCst,
        core::sync::atomic::Ordering::SeqCst,
    ) {
        Ok(_) => true,
        Err(current) => current == pid,
    }
}

/// Возвращает PID владельца IRQ (None — IRQ обслуживает ядро).
pub fn irq_owner(irq: u8) -> Option<usize> {
    if irq >= 16 {
        return None;
    }
    match IRQ_OWNER[irq as usize].load(core::sync::atomic::Ordering::SeqCst) {
        0 => None,
        pid => Some(pid),
    }
}

/// Снимает владение IRQ со стороны процесса (например, при завершении
/// драйвера). После этого IRQ снова обслуживается ядром.
pub fn release_irq_owner(pid: usize) {
    if pid == 0 {
        return;
    }
    for irq in 0..16 {
        let _ = IRQ_OWNER[irq].compare_exchange(
            pid,
            0,
            core::sync::atomic::Ordering::SeqCst,
            core::sync::atomic::Ordering::SeqCst,
        );
    }
}

/// Вызывается из IRQ-handler: учитывает прерывание и будит процесс,
/// который его ждёт (если такой есть).
///
/// Возвращает true, если прерывание кому-то доставлено (есть владелец
/// или ожидающий процесс) — ядру в этом случае не нужно обслуживать
/// устройство самому.
pub fn notify_irq(irq: u8) -> bool {
    if irq >= 16 {
        return false;
    }
    IRQ_PENDING[irq as usize].fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    // ВАЖНО: обработчик прерывания не должен блокироваться на мьютексе —
    // если ядро уже держит PROCESS_MANAGER, try_lock вернёт None, и
    // ожидающего разбудит таймер (та же проверка выполняется в
    // timer_schedule_rust). Счётчик прерывания уже увеличен, поэтому
    // прерывание не потеряется.
    let mut delivered = false;
    if let Some(mut pm) = PROCESS_MANAGER.try_lock() {
        for i in 0..MAX_PROCESSES {
            if let Some(proc) = pm.processes[i].as_mut() {
                if proc.wait == WaitKind::Irq(irq) && proc.state == ProcessState::Blocked {
                    proc.wait = WaitKind::None;
                    proc.state = ProcessState::Ready;
                    delivered = true;
                }
            }
        }
    }
    delivered
}

/// Пытается забрать одно прерывание из счётчика (неблокирующе).
/// Возвращает true, если прерывание было (счётчик уменьшен).
pub fn irq_try_consume(irq: u8) -> bool {
    if irq >= 16 {
        return false;
    }
    let counter = &IRQ_PENDING[irq as usize];
    let mut cur = counter.load(core::sync::atomic::Ordering::SeqCst);
    while cur > 0 {
        match counter.compare_exchange(
            cur,
            cur - 1,
            core::sync::atomic::Ordering::SeqCst,
            core::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => return true,
            Err(actual) => cur = actual,
        }
    }
    false
}

/// Добровольная передача кванта (syscall yield #24).
///
/// В отличие от block_current_and_switch, текущий процесс НЕ блокируется:
/// он остаётся Ready и будет выбран снова в следующем раунде. RIP
/// сохраняется без вычитания 2 — syscall уже выполнен, при возобновлении
/// процесс продолжит со следующей инструкции после int 0x80.
///
/// Возвращает (next_idx, NextProcessInfo) для переключения.
/// None — других готовых процессов нет, текущий продолжает работу.
pub fn yield_current_and_switch(
    gpr: &[u64; 15],
    rip: u64,
    rsp: u64,
    cs: u64,
    ss: u64,
    rflags: u64,
) -> Option<(usize, NextProcessInfo)> {
    let mut pm = PROCESS_MANAGER.lock();

    let cur_idx = pm.current?;

    // Ищем следующий Ready процесс (round-robin начиная с cur_idx+1).
    // Текущий процесс исключаем явно: переключение на себя бессмысленно.
    let start = cur_idx + 1;
    let next_idx = (0..MAX_PROCESSES)
        .map(|i| (start + i) % MAX_PROCESSES)
        .find(|&idx| {
            idx != cur_idx
                && pm.processes[idx]
                    .as_ref()
                    .map(|p| p.state == ProcessState::Ready)
                    .unwrap_or(false)
        })?;

    // Сохраняем контекст текущего процесса. Состояние — Ready, а не
    // Blocked: процесс готов продолжить, как только до него дойдёт очередь.
    {
        let proc = pm.processes[cur_idx].as_mut()?;
        proc.state = ProcessState::Ready;
        proc.started = true;

        proc.context.rax = gpr[0];
        proc.context.rbx = gpr[1];
        proc.context.rcx = gpr[2];
        proc.context.rdx = gpr[3];
        proc.context.rsi = gpr[4];
        proc.context.rdi = gpr[5];
        proc.context.rbp = gpr[6];
        proc.context.r8 = gpr[7];
        proc.context.r9 = gpr[8];
        proc.context.r10 = gpr[9];
        proc.context.r11 = gpr[10];
        proc.context.r12 = gpr[11];
        proc.context.r13 = gpr[12];
        proc.context.r14 = gpr[13];
        proc.context.r15 = gpr[14];

        proc.context.rip = rip;
        proc.context.rsp = rsp;
        proc.context.cs = cs;
        proc.context.ss = ss;
        proc.context.rflags = rflags;
        proc.context.cr3 = proc.p4_phys;
    }

    let info = {
        let proc = pm.processes[next_idx].as_mut()?;
        proc.state = ProcessState::Running;
        proc.ticks_left = proc.quantum;
        if !proc.started {
            proc.context.rip = proc.entry_point;
            proc.context.rsp = proc.stack_top;
            proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
            proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
            proc.context.rflags = 0x202;
            proc.context.cr3 = proc.p4_phys;
            proc.started = true;
        }
        NextProcessInfo {
            rax: proc.context.rax,
            rbx: proc.context.rbx,
            rcx: proc.context.rcx,
            rdx: proc.context.rdx,
            rsi: proc.context.rsi,
            rdi: proc.context.rdi,
            rbp: proc.context.rbp,
            r8: proc.context.r8,
            r9: proc.context.r9,
            r10: proc.context.r10,
            r11: proc.context.r11,
            r12: proc.context.r12,
            r13: proc.context.r13,
            r14: proc.context.r14,
            r15: proc.context.r15,
            rip: proc.context.rip,
            rsp: proc.context.rsp,
            cs: proc.context.cs,
            ss: proc.context.ss,
            rflags: proc.context.rflags,
            cr3: proc.p4_phys,
        }
    };

    pm.current = Some(next_idx);
    Some((next_idx, info))
}

/// Планировщик, вызываемый из naked timer handler.
///
/// # Аргументы (от asm):
/// - `regs` — указатель на сохранённые GPR на стеке (массив 15 u64)
/// - `frame` — указатель на IRET-фрейм на стеке
/// - `rsp_at_entry` — RSP сразу после push 15 GPR (не используется:
///   при доставке таймера через IST IRET-фрейм всегда 5-словный)
///
/// # Возврат
/// - `false` — переключения нет, asm просто восстанавливает GPR и iretq
/// - `true` — контекст и IRET-фрейм модифицированы, asm восстановит
///   новые GPR и iretq к следующему процессу
///
/// # Логика
/// 1. Увеличить счётчик тиков.
/// 2. Разбудить процессы, чьи IRQ уже накопились (страховка notify_irq).
/// 3. Если нет текущего процесса (ядро работает) — запустить Ready или
///    вернуть false.
/// 4. Не вытеснять процесс, прерванный в ring 0 (внутри syscall).
/// 5. Уменьшить квант. Если не истёк — вернуть false.
/// 6. Квант истёк: сохранить контекст текущего процесса
///    (GPR + IRET-фрейм + CR3).
/// 7. Выбрать следующий процесс. Если нет — сбросить квант, вернуть false.
/// 8. Записать контекст следующего процесса в GPR и IRET-фрейм на стеке.
/// 9. Переключить CR3 на P4 следующего процесса.
/// 10. Вернуть true.
#[no_mangle]
pub extern "C" fn timer_schedule_rust(
    regs: *mut GprRegs,
    frame: *mut IretFrame,
    rsp_at_entry: u64,
) -> bool {
    // Счётчик тиков для диагностики
    crate::interrupts::TIMER_TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // Реальные RSP/SS прерванного контекста.
    //
    // Таймер доставляется через IST (TIMER_IST_INDEX), поэтому CPU в long
    // mode ВСЕГДА пушит 5 слов — SS, RSP, RFLAGS, CS, RIP — даже если
    // прерывание произошло в ring 0 (Intel SDM: при использовании IST
    // SS:RSP пушатся безусловно). Значит поля frame.rsp/frame.ss валидны
    // в обоих случаях, и их нужно ЧИТАТЬ, а не вычислять их адрес.
    //
    // ИСТОРИЯ БАГА: раньше для ring 0 сохранялся адрес поля RSP
    // (rsp_at_entry + 0x78 + 24). В IDLE_CTX попадал указатель внутрь
    // TIMER_STACK; возврат из idle шёл на IST-стек таймера, следующий
    // тик затирал рабочие данные read_line → DOUBLE FAULT.
    let (frame_rsp, frame_ss) = unsafe { ((*frame).rsp, (*frame).ss) };
    let _ = rsp_at_entry;

    let mut pm = PROCESS_MANAGER.lock();


    // Страховка для доставки прерываний в user space: если IRQ-handler
    // не смог взять лок (был занят), ожидающий процесс будится здесь.
    for i in 0..MAX_PROCESSES {
        let pending_irq = match pm.processes[i].as_ref() {
            Some(p) if p.state == ProcessState::Blocked => match p.wait {
                WaitKind::Irq(irq) => Some(irq),
                _ => None,
            },
            _ => None,
        };
        if let Some(irq) = pending_irq {
            if irq < 16
                && IRQ_PENDING[irq as usize].load(core::sync::atomic::Ordering::SeqCst) > 0
            {
                if let Some(p) = pm.processes[i].as_mut() {
                    p.wait = WaitKind::None;
                    p.state = ProcessState::Ready;
                }
            }
        }
    }

    let current_idx = match pm.current {
        Some(idx) => idx,
        None => {
            // Ядро (shell) активно работает: нельзя запускать user-процесс
            // из прерывания. Иначе процесс начнёт выполняться, пока ядро
            // держит TERMINAL.lock (например, внутри println!) — его write
            // попытается взять тот же lock и повиснет навсегда.
            //
            // Процесс запускается из таймера только в idle: shell спит
            // в sti; hlt и явно выставил SHELL_IDLE (см. shell::wait_byte).
            if !SHELL_IDLE.load(core::sync::atomic::Ordering::SeqCst) {
                return false;
            }
            // Idle-цикл микроядерной модели: shell — «фоновый» поток
            // ядра, а сервисы получают CPU по событиям.
            let next_idx = match pm.select_next() {
                Some(idx) => idx,
                None => return false,
            };
            let proc = match pm.processes[next_idx].as_mut() {
                Some(p) => p,
                None => return false,
            };
            proc.state = ProcessState::Running;
            proc.ticks_left = proc.quantum;
            if !proc.started {
                proc.context.rip = proc.entry_point;
                proc.context.rsp = proc.stack_top;
                proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
                proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
                proc.context.rflags = 0x202;
                proc.context.cr3 = proc.p4_phys;
                proc.started = true;
            }
            let ctx = proc.context;

            // КРИТИЧНО: сохраняем контекст ядра ДО переключения, чтобы
            // вернуться в ту же точку shell, а не запускать shell() заново.
            unsafe {
                save_idle_context(regs, frame, frame_rsp, frame_ss);
            }

            unsafe {
                let r = &mut *regs;
                r.rax = ctx.rax; r.rbx = ctx.rbx; r.rcx = ctx.rcx; r.rdx = ctx.rdx;
                r.rsi = ctx.rsi; r.rdi = ctx.rdi; r.rbp = ctx.rbp;
                r.r8 = ctx.r8; r.r9 = ctx.r9; r.r10 = ctx.r10; r.r11 = ctx.r11;
                r.r12 = ctx.r12; r.r13 = ctx.r13; r.r14 = ctx.r14; r.r15 = ctx.r15;

                let f = &mut *frame;
                f.rip = ctx.rip;
                f.cs = ctx.cs;
                f.rflags = ctx.rflags;
                f.rsp = ctx.rsp;
                f.ss = ctx.ss;
            }
            unsafe {
                let phys = x86_64::PhysAddr::new(ctx.cr3);
                let phys_frame = x86_64::structures::paging::PhysFrame::containing_address(phys);
                x86_64::registers::control::Cr3::write(
                    phys_frame,
                    x86_64::registers::control::Cr3Flags::empty(),
                );
            }
            pm.current = Some(next_idx);
            return true;
        }
    };

    // КРИТИЧНО: не вытесняем процесс, прерванный в ring 0 (внутри
    // syscall). TSS.rsp0 общий для всех процессов, поэтому второй
    // процесс, войдя в syscall, затёр бы kernel-стек первого, а
    // сохранённый IRET-фрейм указывал бы в середину syscall_handler.
    // Явные переключения (block/yield/exit) сохраняют user-контекст
    // и этому ограничению не подвержены.
    if (unsafe { (*frame).cs } & 3) == 0 {
        return false;
    }

    // Уменьшаем квант текущего процесса. Если не истёк — продолжаем его.
    let expired = {
        let proc = match pm.processes[current_idx].as_mut() {
            Some(p) => p,
            None => { pm.current = None; return false; }
        };
        if proc.ticks_left > 0 {
            proc.ticks_left -= 1;
        }
        proc.ticks_left == 0
    };
    if !expired {
        return false;
    }

    // Квант истёк. Сохраняем контекст текущего процесса.
    {
        let proc = match pm.processes[current_idx].as_mut() {
            Some(p) => p,
            None => { pm.current = None; return false; }
        };
        proc.state = ProcessState::Ready;
        proc.started = true;
        // Сохраняем GPR из стека в контекст процесса
        let r = unsafe { &*regs };
        proc.context.rax = r.rax;
        proc.context.rbx = r.rbx;
        proc.context.rcx = r.rcx;
        proc.context.rdx = r.rdx;
        proc.context.rsi = r.rsi;
        proc.context.rdi = r.rdi;
        proc.context.rbp = r.rbp;
        proc.context.r8 = r.r8;
        proc.context.r9 = r.r9;
        proc.context.r10 = r.r10;
        proc.context.r11 = r.r11;
        proc.context.r12 = r.r12;
        proc.context.r13 = r.r13;
        proc.context.r14 = r.r14;
        proc.context.r15 = r.r15;
        // Сохраняем IRET-фрейм
        let f = unsafe { &*frame };
        proc.context.rip = f.rip;
        proc.context.cs = f.cs;
        proc.context.rflags = f.rflags;
        proc.context.rsp = frame_rsp;
        proc.context.ss = frame_ss;
        proc.context.cr3 = proc.p4_phys;
    }

    // Выбираем следующий процесс (round-robin)
    let next_idx = match pm.select_next() {
        Some(idx) => idx,
        None => {
            // Нет других готовых процессов — текущий продолжает работать.
            // Сбрасываем квант, чтобы не переключаться каждый тик.
            pm.processes[current_idx].as_mut().unwrap().ticks_left =
                pm.processes[current_idx].as_ref().unwrap().quantum;
            return false;
        }
    };

    // Помечаем следующий как Running, сбрасываем квант
    {
        let proc = pm.processes[next_idx].as_mut().unwrap();
        proc.state = ProcessState::Running;
        proc.ticks_left = proc.quantum;
        // Если процесс запускается впервые — инициализируем контекст
        // для входа в ring 3 (USER selectors, entry_point, stack_top).
        if !proc.started {
            proc.context.rip = proc.entry_point;
            proc.context.rsp = proc.stack_top;
            proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
            proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
            proc.context.rflags = 0x202; // IF=1, резервный бит 1
            proc.context.cr3 = proc.p4_phys;
            proc.started = true;
        }
    }

    // Записываем контекст следующего процесса в GPR на стеке
    {
        let proc = pm.processes[next_idx].as_ref().unwrap();
        let r = unsafe { &mut *regs };
        r.rax = proc.context.rax;
        r.rbx = proc.context.rbx;
        r.rcx = proc.context.rcx;
        r.rdx = proc.context.rdx;
        r.rsi = proc.context.rsi;
        r.rdi = proc.context.rdi;
        r.rbp = proc.context.rbp;
        r.r8 = proc.context.r8;
        r.r9 = proc.context.r9;
        r.r10 = proc.context.r10;
        r.r11 = proc.context.r11;
        r.r12 = proc.context.r12;
        r.r13 = proc.context.r13;
        r.r14 = proc.context.r14;
        r.r15 = proc.context.r15;

        // Записываем IRET-фрейм следующего процесса
        let f = unsafe { &mut *frame };
        f.rip = proc.context.rip;
        f.cs = proc.context.cs;
        f.rflags = proc.context.rflags;
        f.rsp = proc.context.rsp;
        f.ss = proc.context.ss;
    }

    // Переключаем CR3 на P4 следующего процесса
    {
        let next_p4 = pm.processes[next_idx].as_ref().unwrap().p4_phys;
        unsafe {
            use x86_64::registers::control::{Cr3, Cr3Flags};
            use x86_64::structures::paging::PhysFrame;
            use x86_64::PhysAddr;
            let frame = PhysFrame::containing_address(PhysAddr::new(next_p4));
            Cr3::write(frame, Cr3Flags::empty());
        }
    }

    pm.current = Some(next_idx);
    true
}

/// Переключает контекст CPU на следующий процесс через iretq.
///
/// # Аргументы
/// - `next` — контекст следующего процесса (текущий уже завершён,
///   его регистры не сохраняются)
///
/// # Что делает
/// 1. Загружает GPR из `next`
/// 2. Переключает CR3 на P4 следующего процесса
/// 3. Строит IRET-фрейм (RIP/CS/RFLAGS/RSP/SS) из `next`
/// 4. iretq — переход в ring 3 (или ring 0 для idle-контекста)
///
/// # Смещения ProcessContext (repr(C), 22 поля u64)
///   rax=0x00, rbx=0x08, rcx=0x10, rdx=0x18, rsi=0x20, rdi=0x28,
///   rbp=0x30, r8=0x38, r9=0x40, r10=0x48, r11=0x50, r12=0x58,
///   r13=0x60, r14=0x68, r15=0x70, rip=0x78, cs=0x80, rsp=0x88,
///   ss=0x90, rflags=0x98, cr3=0xA0.
///
/// # Безопасность
/// Функция naked: не имеет пролога/эпилога компилятора. Вызывается
/// только из panic_current, когда текущий процесс уже завершён.
#[unsafe(naked)]
pub unsafe extern "C" fn context_switch(next: *const ProcessContext) -> ! {
    // rdi = next.
    // IRET-фрейм строим на текущем стеке; iretq сам загрузит RSP из фрейма.
    core::arch::naked_asm!(
        // Переключаем CR3 на P4 следующего процесса (cr3 = 0xA0)
        "mov rax, [rdi + 0xA0]",
        "mov cr3, rax",

        // Загружаем GPR следующего процесса (rdi — позже, он нужен
        // как указатель на структуру)
        "mov rax, [rdi + 0x00]",
        "mov rbx, [rdi + 0x08]",
        "mov rcx, [rdi + 0x10]",
        "mov rdx, [rdi + 0x18]",
        "mov rbp, [rdi + 0x30]",
        "mov r8,  [rdi + 0x38]",
        "mov r9,  [rdi + 0x40]",
        "mov r10, [rdi + 0x48]",
        "mov r11, [rdi + 0x50]",
        "mov r12, [rdi + 0x58]",
        "mov r13, [rdi + 0x60]",
        "mov r14, [rdi + 0x68]",
        "mov r15, [rdi + 0x70]",

        // IRET-фрейм: SS, RSP, RFLAGS, CS, RIP (снизу вверх)
        "push qword ptr [rdi + 0x90]", // SS
        "push qword ptr [rdi + 0x88]", // RSP
        "push qword ptr [rdi + 0x98]", // RFLAGS
        "push qword ptr [rdi + 0x80]", // CS
        "push qword ptr [rdi + 0x78]", // RIP

        // Восстанавливаем RSI и RDI последними
        "mov rsi, [rdi + 0x20]",
        "mov rdi, [rdi + 0x28]",

        // Переход к следующему процессу (ring 3 или ring 0)
        "iretq",
    );
}
