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

    /// Возвращает true если в почтовом ящике есть сообщения.
    pub fn has_messages(&self) -> bool {
        self.count > 0
    }

    /// Количество свободных мест в почтовом ящике.
    pub fn free_slots(&self) -> usize {
        IPC_MAILBOX_SIZE - self.count
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
/// На x86_64 CPU всегда пушит 5 значений (даже для ring 0 → ring 0):
///   SS, RSP, RFLAGS, CS, RIP (в порядке пуша — SS первым).
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
    /// PID — идентификатор процесса (индекс в таблице + 1).
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
    /// IRQ, которого ждёт процесс (None — не ждёт прерывания).
    /// Используется механизмом доставки прерываний в user space:
    /// irq_wait блокирует процесс, IRQ-handler будит ожидающего.
    pub waiting_irq: Option<u8>,
    /// PID получателя, освобождения ящика которого ждёт процесс
    /// (блокирующий ipc_send при переполненном ящике).
    pub waiting_send_to: Option<usize>,
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
            waiting_irq: None,
            waiting_send_to: None,
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
    pub fn terminate(&mut self, index: usize) {
        if let Some(ref mut proc) = self.processes[index] {
            proc.state = ProcessState::Terminated;
        }
        if self.current == Some(index) {
            self.current = None;
        }
    }

    /// Паника текущего процесса: terminate + switch к следующему.
    /// Используется для обработки фатальных ошибок (stack overflow,
    /// illegal instruction, и т.д.) — процесс уничтожается, система
    /// продолжает работу с другим процессом.
    pub fn panic_current(&mut self, reason: &str) {
        if let Some(idx) = self.current {
            crate::println!("  Terminating process PID={} ({}) reason='{}'",
                idx + 1,
                self.processes[idx].as_ref().map_or("unknown", |p| &p.name),
                reason
            );
            self.terminate(idx);
            // Переключаемся к следующему процессу (standalone функция)
            if let Some(next) = next_process() {
                crate::println!("  Switched to PID={}", next + 1);
                // Сохраняем контекст текущего (уже terminated) и переключаемся
                unsafe {
                    let from_ctx = core::ptr::addr_of_mut!(self.processes[idx]).cast::<crate::process_manager::ProcessContext>();
                    let to_ctx = core::ptr::addr_of_mut!(self.processes[next]).cast::<crate::process_manager::ProcessContext>();
                    crate::process_manager::context_switch(from_ctx, to_ctx);
                }
            } else {
                // Нет других процессов — возвращаемся в shell
                crate::println!("  No other processes — returning to shell");
                crate::process::restore_kernel_cr3();
            }
        }
    }

    /// Сбрасывает квант времени для текущего процесса.
    pub fn reset_quantum(&mut self) {
        if let Some(idx) = self.current {
            if let Some(ref mut proc) = self.processes[idx] {
                proc.ticks_left = proc.quantum;
            }
        }
    }

    /// Уменьшает оставшийся квант. Возвращает true если квант истёк.
    pub fn tick(&mut self) -> bool {
        if let Some(idx) = self.current {
            if let Some(ref mut proc) = self.processes[idx] {
                if proc.ticks_left > 0 {
                    proc.ticks_left -= 1;
                }
                return proc.ticks_left == 0;
            }
        }
        false
    }

    /// Возвращает количество активных процессов (не Terminated, не None).
    pub fn active_count(&self) -> usize {
        self.processes
            .iter()
            .filter(|p| p.as_ref().map_or(false, |p| p.state != ProcessState::Terminated))
            .count()
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

/// Тикает планировщик: уменьшает квант текущего процесса.
/// Возвращает true если квонт истёк и нужно переключиться.
pub fn scheduler_tick() -> bool {
    PROCESS_MANAGER.lock().tick()
}

/// Сбрасывает квант текущего процесса (если он Running).
/// Вызывается из timer_handler когда квант истёк, но реальное
/// переключение контекста ещё не реализовано.
pub fn reset_quantum_if_running() {
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(idx) = pm.current {
        if let Some(ref mut proc) = pm.processes[idx] {
            if proc.state == ProcessState::Running {
                proc.ticks_left = proc.quantum;
            }
        }
    }
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
            let mut pid = 0;
            if let Some(proc) = pm.processes[idx].as_mut() {
                proc.state = ProcessState::Terminated;
                pid = proc.pid;
                crate::println!(
                    "  process PID={} ({}) -> Terminated",
                    proc.pid, proc.name
                );
            }
            // Драйвер в user space завершился — возвращаем IRQ ядру,
            // иначе прерывание больше никто не обслужит.
            if pid != 0 {
                for irq in 0..16u8 {
                    if IRQ_OWNER[irq as usize].load(core::sync::atomic::Ordering::SeqCst) == pid {
                        IRQ_OWNER[irq as usize]
                            .store(0, core::sync::atomic::Ordering::SeqCst);
                        crate::println!("  IRQ{} returned to kernel", irq);
                    }
                }
            }
            pm.current = None;
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

        // Если получатель был заблокирован (ждёт recv) — разблокируем
        if proc.state == ProcessState::Blocked {
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
                if p.waiting_send_to == Some(recv_pid) && p.state == ProcessState::Blocked {
                    p.waiting_send_to = None;
                    p.state = ProcessState::Ready;
                }
            }
        }

        Some((msg.from_pid, copy_len))
    })
}

/// Помечает текущий процесс как ждущий освобождения ящика получателя.
/// Вызывается перед block_current_and_switch при IPC_FULL.
pub fn set_waiting_send(to_pid: usize) {
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(idx) = pm.current {
        if let Some(proc) = pm.processes[idx].as_mut() {
            proc.waiting_send_to = Some(to_pid);
        }
    }
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
/// Возвращает (next_idx, NextProcessInfo) для переключения.
/// Если готовых процессов нет — возвращает None (нельзя блокировать).
pub fn block_current_and_switch(
    gpr: &[u64; 15],
    rip: u64,
    rsp: u64,
    cs: u64,
    ss: u64,
    rflags: u64,
) -> Option<(usize, NextProcessInfo)> {
    let mut pm = PROCESS_MANAGER.lock();

    let cur_idx = pm.current?;
    {
        let proc = pm.processes[cur_idx].as_mut()?;
        proc.state = ProcessState::Blocked;
        proc.started = true;

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
        })?;

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

/// Сколько всего раз срабатывал каждый IRQ (диагностика: живой ли IRQ4).
static IRQ_TOTAL: [core::sync::atomic::AtomicU64; 16] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 16];

/// PID процесса-владельца IRQ (0 — IRQ обслуживается ядром).
static IRQ_OWNER: [core::sync::atomic::AtomicUsize; 16] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; 16];

/// Общее количество прерываний, доставленных в user space.
pub static IRQ_DELIVERED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Возвращает, сколько раз срабатывал IRQ (всего).
pub fn irq_total(irq: u8) -> u64 {
    if irq >= 16 {
        return 0;
    }
    IRQ_TOTAL[irq as usize].load(core::sync::atomic::Ordering::SeqCst)
}

/// Возвращает количество необработанных прерываний в счётчике.
pub fn irq_pending_count(irq: u8) -> u32 {
    if irq >= 16 {
        return 0;
    }
    IRQ_PENDING[irq as usize].load(core::sync::atomic::Ordering::SeqCst)
}

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

/// Сохраняет контекст ядра перед переключением на user-процесс.
/// Вызывается из таймера, когда shell вытесняется впервые.
///
/// # Safety
/// Вызывается только из обработчика прерывания (монопольный доступ
/// к статике через addr_of_mut, без создания ссылок на static mut).
unsafe fn save_idle_context(regs: *const GprRegs, frame: *const IretFrame) {
    let ctx = core::ptr::addr_of_mut!(IDLE_CTX);
    (*ctx).gpr = *regs;
    (*ctx).iret = *frame;
    (*ctx).cr3 = crate::paging::kernel_p4_phys();
    IDLE_CTX_VALID.store(true, core::sync::atomic::Ordering::SeqCst);
}

/// Возвращает сохранённый контекст ядра, если он есть.
/// Используется обработчиком syscall, когда процесс заблокировался,
/// а других готовых процессов нет: iretq вернёт управление в shell
/// ровно туда, где его прервал таймер.
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

/// Сбрасывает сохранённый контекст ядра (например, при запуске
/// процесса напрямую из shell через run_process).
pub fn invalidate_idle_context() {
    IDLE_CTX_VALID.store(false, core::sync::atomic::Ordering::SeqCst);
}

/// Есть ли сохранённый контекст ядра.
pub fn idle_context_valid() -> bool {
    IDLE_CTX_VALID.load(core::sync::atomic::Ordering::SeqCst)
}

/// Регистрирует процесс как владельца IRQ. После этого ядро перестаёт
/// само обслуживать устройство и только уведомляет драйвер.
///
/// Возвращает true при успехе.
pub fn claim_irq(irq: u8, pid: usize) -> bool {
    if irq >= 16 || pid == 0 {
        return false;
    }
    IRQ_OWNER[irq as usize].store(pid, core::sync::atomic::Ordering::SeqCst);
    true
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
    IRQ_TOTAL[irq as usize].fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    IRQ_PENDING[irq as usize].fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    // ВАЖНО: обработчик прерывания не должен блокироваться на мьютексе —
    // если ядро уже держит PROCESS_MANAGER, try_lock вернёт None, и
    // ожидающего разбудит таймер (wake_irq_waiters вызывается из
    // планировщика). Счётчик прерывания уже увеличен, поэтому
    // прерывание не потеряется.
    let mut delivered = false;
    if let Some(mut pm) = PROCESS_MANAGER.try_lock() {
        for i in 0..MAX_PROCESSES {
            if let Some(proc) = pm.processes[i].as_mut() {
                if proc.waiting_irq == Some(irq) && proc.state == ProcessState::Blocked {
                    proc.waiting_irq = None;
                    proc.state = ProcessState::Ready;
                    delivered = true;
                }
            }
        }
    }
    if delivered {
        IRQ_DELIVERED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    delivered
}

/// Будит процессы, ожидающие прерываний, по которым уже есть
/// необработанные счётчики. Вызывается из планировщика (таймер) —
/// это страховка на случай, когда notify_irq не смог взять лок.
pub fn wake_irq_waiters() {
    let mut pm = PROCESS_MANAGER.lock();
    for i in 0..MAX_PROCESSES {
        let pending_irq = match pm.processes[i].as_ref() {
            Some(p) if p.state == ProcessState::Blocked => p.waiting_irq,
            _ => None,
        };
        if let Some(irq) = pending_irq {
            if irq < 16
                && IRQ_PENDING[irq as usize].load(core::sync::atomic::Ordering::SeqCst) > 0
            {
                if let Some(p) = pm.processes[i].as_mut() {
                    p.waiting_irq = None;
                    p.state = ProcessState::Ready;
                }
            }
        }
    }
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

/// Помечает текущий процесс как ожидающий указанный IRQ.
/// Вызывается перед block_current_and_switch из sys_irq_wait.
pub fn set_waiting_irq(irq: u8) {
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(idx) = pm.current {
        if let Some(proc) = pm.processes[idx].as_mut() {
            proc.waiting_irq = Some(irq);
        }
    }
}

/// Снимает ожидание IRQ с текущего процесса.
pub fn clear_waiting_irq() {
    let mut pm = PROCESS_MANAGER.lock();
    if let Some(idx) = pm.current {
        if let Some(proc) = pm.processes[idx].as_mut() {
            proc.waiting_irq = None;
        }
    }
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
    let start = cur_idx + 1;
    let next_idx = (0..MAX_PROCESSES)
        .map(|i| (start + i) % MAX_PROCESSES)
        .find(|&idx| {
            pm.processes[idx]
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

/// Находит процесс по физическому адресу P4 и помечает его как Running.
/// Устанавливает current = Some(index).
/// Вызывается из run_process ПЕРЕД переходом в ring 3 — чтобы
/// terminate_current() при exit() знал, какой процесс завершать.
///
/// without_interrupts: защита от deadlock при timer IRQ.
pub fn set_current_by_p4(p4_phys: u64) {
    interrupts::without_interrupts(|| {
        let mut pm = PROCESS_MANAGER.lock();
        // Сначала сбрасываем предыдущий текущий процесс (если был)
        if let Some(idx) = pm.current {
            if let Some(ref mut proc) = pm.processes[idx] {
                if proc.state == ProcessState::Running {
                    proc.state = ProcessState::Ready;
                }
            }
        }
        // Ищем процесс с matching p4_phys
        for i in 0..MAX_PROCESSES {
            if let Some(ref mut proc) = pm.processes[i] {
                if proc.p4_phys == p4_phys && proc.state != ProcessState::Terminated {
                    proc.state = ProcessState::Running;
                    proc.ticks_left = proc.quantum;
                    // Инициализируем контекст для первого запуска в ring 3.
                    // timer_schedule_rust при первом вытеснении сохранит
                    // реальные значения, но при первом запуске (если timer
                    // сработает до того, как процесс начнёт выполняться)
                    // нужны корректные USER selectors и entry point.
                    if !proc.started {
                        proc.context.rip = proc.entry_point;
                        proc.context.rsp = proc.stack_top;
                        proc.context.cs = crate::gdt::USER_CODE_SELECTOR as u64;
                        proc.context.ss = crate::gdt::USER_DATA_SELECTOR as u64;
                        proc.context.rflags = 0x202; // IF=1, резервный бит 1
                        proc.context.cr3 = proc.p4_phys;
                    }
                    pm.current = Some(i);
                    return;
                }
            }
        }
    })
}

/// Выбирает следующий процесс для выполнения.
/// Возвращает индекс в таблице или None.
///
/// without_interrupts: защита от deadlock при timer IRQ.
pub fn next_process() -> Option<usize> {
    interrupts::without_interrupts(|| {
        PROCESS_MANAGER.lock().select_next()
    })
}

/// Планировщик, вызываемый из naked timer handler.
///
/// # Аргументы (от asm):
/// - `regs` — указатель на сохранённые GPR на стеке (массив 15 u64)
/// - `frame` — указатель на IRET-фрейм на стеке (5 u64)
///
/// # Возврат
/// - `false` — переключения нет, asm просто восстанавливает GPR и iretq
/// - `true` — контекст и IRET-фрейм модифицированы, asm восстановит
///   новые GPR и iretq к следующему процессу
///
/// # Логика
/// 1. Увеличить счётчик тиков.
/// 2. Если нет текущего процесса (ядро работает) — вернуть false.
/// 3. Уменьшить квант. Если не истёк — вернуть false.
/// 4. Квант истёк: сохранить контекст текущего процесса
///    (GPR + IRET-фрейм + CR3).
/// 5. Выбрать следующий процесс. Если нет — сбросить квант, вернуть false.
/// 6. Записать контекст следующего процесса в GPR и IRET-фрейм на стеке.
/// 7. Переключить CR3 на P4 следующего процесса.
/// 8. Вернуть true.
#[no_mangle]
pub extern "C" fn timer_schedule_rust(
    regs: *mut GprRegs,
    frame: *mut IretFrame,
) -> bool {
    // Счётчик тиков для диагностики
    crate::interrupts::TIMER_TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let mut pm = PROCESS_MANAGER.lock();

    // ДИАГНОСТИКА (временно): раз в 200 тиков печатаем состояние
    // планировщика и IRQ4 напрямую в COM1 (без lock терминала).
    let ticks = crate::interrupts::TIMER_TICKS.load(core::sync::atomic::Ordering::Relaxed);
    if ticks % 200 == 0 {
        crate::debug::serial_write_str_direct("\n[T] tick=");
        crate::debug::write_hex(ticks as u64);
        crate::debug::serial_write_str_direct(" cur=");
        match pm.current {
            Some(idx) => crate::debug::write_hex(idx as u64),
            None => crate::debug::serial_write_str_direct("none"),
        }
        crate::debug::serial_write_str_direct(" idle=");
        crate::debug::write_hex(idle_context_valid() as u64);
        crate::debug::serial_write_str_direct(" i4=");
        crate::debug::write_hex(irq_total(4));
        crate::debug::serial_write_str_direct(" p4=");
        crate::debug::write_hex(irq_pending_count(4) as u64);
        crate::debug::serial_write_str_direct(" o4=");
        match irq_owner(4) {
            Some(pid) => crate::debug::write_hex(pid as u64),
            None => crate::debug::serial_write_str_direct("none"),
        }
        crate::debug::serial_write_str_direct(" ");
    }

    // Страховка для доставки прерываний в user space: если IRQ-handler
    // не смог взять лок (был занят), ожидающий процесс будится здесь.
    for i in 0..MAX_PROCESSES {
        let pending_irq = match pm.processes[i].as_ref() {
            Some(p) if p.state == ProcessState::Blocked => p.waiting_irq,
            _ => None,
        };
        if let Some(irq) = pending_irq {
            if irq < 16
                && IRQ_PENDING[irq as usize].load(core::sync::atomic::Ordering::SeqCst) > 0
            {
                if let Some(p) = pm.processes[i].as_mut() {
                    p.waiting_irq = None;
                    p.state = ProcessState::Ready;
                }
            }
        }
    }

    let current_idx = match pm.current {
        Some(idx) => idx,
        None => {
            // Ядро (shell) работает. Если появился готовый процесс —
            // например, IRQ разбудил user-space драйвер, — запускаем его.
            // Это idle-цикл микроядерной модели: shell остаётся
            // «фоновым» потоком ядра, а сервисы получают CPU по событиям.
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
                save_idle_context(regs, frame);
            }

            // ДИАГНОСТИКА (временно): кто и куда запускается из idle.
            {
                static IDLE_SWITCHES: core::sync::atomic::AtomicU32 =
                    core::sync::atomic::AtomicU32::new(0);
                if IDLE_SWITCHES
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                    < 6
                {
                    crate::debug::serial_write_str_direct("\n[TS] idle->pidx=");
                    crate::debug::write_hex(next_idx as u64);
                    crate::debug::serial_write_str_direct(" rip=0x");
                    crate::debug::write_hex(ctx.rip);
                    crate::debug::serial_write_str_direct(" rflags=0x");
                    crate::debug::write_hex(ctx.rflags);
                    crate::debug::serial_write_str_direct(" ");
                }
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
        proc.context.rsp = f.rsp;
        proc.context.ss = f.ss;
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

/// Переключает контекст CPU между двумя процессами.
///
/// # Аргументы
/// - `current` — указатель на контекст текущего процесса (в него будут
///   сохранены регистры)
/// - `next` — указатель на контекст следующего процесса (его регистры
///   будут загружены)
///
/// # Что делает
/// 1. Сохраняет регистры текущего процесса в `current`
/// 2. Загружает регистры из `next`
/// 3. Переключает CR3 на P4 следующего процесса
/// 4. Возвращает управление в точку, где следующий процесс был прерван
///    (или в его entry point при первом запуске)
///
/// # Безопасность
/// Функция naked: не имеет пролога/эпилога компилятора. Все регистры
/// сохраняются/восстанавливаются вручную. Вызывается ТОЛЬКО из
/// контекста планировщика (timer IRQ), где нет активного компиляторного
/// фрейма, который нужно сохранить.
#[unsafe(naked)]
pub unsafe extern "C" fn context_switch(
    current: *mut ProcessContext,
    next: *const ProcessContext,
) -> ! {
    // asm: сохраняем текущий контекст, загружаем следующий.
    // Порядок полей ProcessContext (см. struct):
    //   rax, rbx, rcx, rdx, rsi, rdi, rbp, r8-r15, rip, rsp, rflags, cr3
    core::arch::naked_asm!(
        // Сохраняем контекст текущего процесса в *current (rdi)
        "mov [rdi + 0x00], rax",
        "mov [rdi + 0x08], rbx",
        "mov [rdi + 0x10], rcx",
        "mov [rdi + 0x18], rdx",
        "mov [rdi + 0x20], rsi",
        "mov [rdi + 0x28], rdi",
        "mov [rdi + 0x30], rbp",
        "mov [rdi + 0x38], r8",
        "mov [rdi + 0x40], r9",
        "mov [rdi + 0x48], r10",
        "mov [rdi + 0x50], r11",
        "mov [rdi + 0x58], r12",
        "mov [rdi + 0x60], r13",
        "mov [rdi + 0x68], r14",
        "mov [rdi + 0x70], r15",
        // RIP: адрес возврата лежит на стеке (мы попали сюда через call)
        "pop rax",
        "mov [rdi + 0x78], rax",
        // RSP: текущий указатель стека
        "mov [rdi + 0x80], rsp",
        // RFLAGS
        "pushfq",
        "pop rax",
        "mov [rdi + 0x88], rax",
        // CR3
        "mov rax, cr3",
        "mov [rdi + 0x90], rax",

        // Загружаем контекст следующего процесса из *next (rsi)
        "mov rax, [rsi + 0x00]",
        "mov rbx, [rsi + 0x08]",
        "mov rcx, [rsi + 0x10]",
        "mov rdx, [rsi + 0x18]",
        "mov rbp, [rsi + 0x30]",
        "mov r8,  [rsi + 0x38]",
        "mov r9,  [rsi + 0x40]",
        "mov r10, [rsi + 0x48]",
        "mov r11, [rsi + 0x50]",
        "mov r12, [rsi + 0x58]",
        "mov r13, [rsi + 0x60]",
        "mov r14, [rsi + 0x68]",
        "mov r15, [rsi + 0x70]",

        // Переключаем CR3 на P4 следующего процесса
        "mov rax, [rsi + 0x90]",
        "mov cr3, rax",

        // Восстанавливаем RSP
        "mov rsp, [rsi + 0x80]",

        // Восстанавливаем RFLAGS
        "push qword ptr [rsi + 0x88]",
        "popfq",

        // Сохраняем RIP следующего процесса в rcx (пока rsi ещё указывает на next)
        "mov rcx, [rsi + 0x78]",

        // Восстанавливаем RDI и RSI (последними, т.к. они нужны для загрузки)
        "mov rdi, [rsi + 0x28]",
        "mov rsi, [rsi + 0x20]",

        // Возвращаемся в точку прерывания следующего процесса
        "push rcx",
        "ret",
    );
}
