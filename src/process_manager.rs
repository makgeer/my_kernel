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

/// Максимальное количество процессов в системе.
pub const MAX_PROCESSES: usize = 16;

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
    // Указатель стека ядра (RSP в момент переключения)
    pub rsp: u64,
    // Флаги (RFLAGS)
    pub rflags: u64,
    // CR3 — физический адрес P4 таблицы процесса
    pub cr3: u64,
}

impl ProcessContext {
    /// Создаёт пустой контекст (все регистры = 0).
    pub const fn new() -> Self {
        Self {
            rax: 0, rbx: 0, rcx: 0, rdx: 0,
            rsi: 0, rdi: 0, rbp: 0,
            r8: 0, r9: 0, r10: 0, r11: 0,
            r12: 0, r13: 0, r14: 0, r15: 0,
            rip: 0, rsp: 0, rflags: 0, cr3: 0,
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
    pub fn list(&self) {
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
    }
}

/// Глобальный экземпляр менеджера процессов.
pub static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());

/// Добавляет процесс в менеджер (обёртка над PROCESS_MANAGER.lock()).
/// Возвращает PID при успехе.
pub fn spawn(name: &'static str, p4_phys: u64, entry_point: u64, stack_top: u64) -> Result<usize, &'static str> {
    PROCESS_MANAGER.lock().add(name, p4_phys, entry_point, stack_top)
}

/// Печатает список процессов (для команды shell `ps`).
pub fn list_processes() {
    PROCESS_MANAGER.lock().list();
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

/// Выбирает следующий процесс для выполнения.
/// Возвращает индекс в таблице или None.
pub fn next_process() -> Option<usize> {
    PROCESS_MANAGER.lock().select_next()
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
