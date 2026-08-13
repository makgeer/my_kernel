use core::sync::atomic::{AtomicUsize, Ordering};

use spin::Mutex;

#[allow(dead_code)]
pub struct Task {
    pub id: usize,
    pub name: &'static str,
    pub func: fn(),
}

/* TASKS под Mutex — защита от data race при доступе из прерываний */
pub static TASKS: Mutex<[Option<Task>; 4]> = Mutex::new([None, None, None, None]);
static NEXT_TASK: AtomicUsize = AtomicUsize::new(0);

pub fn add_task(name: &'static str, func: fn()) {
    let mut tasks = TASKS.lock();
    for i in 0..tasks.len() {
        if tasks[i].is_none() {
            tasks[i] = Some(Task {
                id: i,
                name,
                func,
            });
            return;
        }
    }
    crate::print!("ERROR: No free task slots\n");
}

pub fn run_scheduler() {
    loop {
        run_scheduler_once();
        // Кооперативная пауза (без вытеснения)
        for _ in 0..100000 {
            unsafe { core::arch::asm!("nop"); }
        }
    }
}

/// Один проход планировщика: выполнить каждую задачу по одному разу.
/// Используется из shell (команда "tasks") как демонстрация задач,
/// не блокируя основной цикл оболочки.
pub fn run_scheduler_once() {
    let task_count = TASKS.lock().len();
    for _ in 0..task_count {
        let current = NEXT_TASK.load(Ordering::Relaxed);
        let func = {
            let tasks = TASKS.lock();
            tasks[current].as_ref().map(|t| t.func)
        };
        if let Some(func) = func {
            func();
        }
        let count = TASKS.lock().len();
        NEXT_TASK.store((current + 1) % count, Ordering::Relaxed);
    }
}
