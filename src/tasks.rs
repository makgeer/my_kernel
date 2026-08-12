use core::sync::atomic::{AtomicUsize, Ordering};

use crate::println;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn task_hello() {
    println!("Task: Hello from task");
}

pub fn task_counter() {
    COUNTER.fetch_add(1, Ordering::Relaxed);
    let count = COUNTER.load(Ordering::Relaxed);
    println!("Task: Counter = {}", count);
}
