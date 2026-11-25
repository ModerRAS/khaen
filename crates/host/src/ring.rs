//! 单生产者/单消费者无锁环形缓冲（SPSC）。
//!
//! 约束：构造时一次性分配，之后 push/pop 不再分配；容量向上取整为 2 的幂，
//! 因此用索引掩码代替取模。生产者只写 head，消费者只写 tail，两侧用 acquire/release 配对。

use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// SPSC 队列的共享状态。
struct Shared<T> {
    buffer: Box<[Option<T>]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

/// 生产者端。
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
}

/// 消费者端。
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
}

// SAFETY：生产者与消费者各只被一个线程持有；队列没有共享引用访问 T 的通道，
// 元素只在线程间转移，因此只要 T: Send 就能安全跨界。
unsafe impl<T: Send> Send for Producer<T> {}
unsafe impl<T: Send> Send for Consumer<T> {}

/// 创建一对生产者/消费者。容量向上取整到 2 的幂，最小为 1。
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let capacity = capacity.max(1).next_power_of_two();
    let mut buffer = Vec::with_capacity(capacity);
    buffer.resize_with(capacity, || None);
    let shared = Arc::new(Shared {
        buffer: buffer.into_boxed_slice(),
        mask: capacity - 1,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
        },
        Consumer { shared },
    )
}

impl<T> Shared<T> {
    /// 当前占用槽位数。
    fn len(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }
}

impl<T> Producer<T> {
    /// 队列容量。
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// 尝试入队；队列满返回 false。实时安全：不分配、不加锁、不阻塞。
    pub fn push(&self, value: T) -> bool {
        let head = self.shared.head.load(Ordering::Relaxed);
        let tail = self.shared.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.capacity() {
            return false;
        }
        let index = head & self.shared.mask;
        // SAFETY（逻辑安全）：该槽位由生产者独占——消费者只读取 head 之前的槽位，
        // 而本次写入尚未通过 head 发布，故无别名。
        let slot = unsafe { self.shared.buffer.get_unchecked(index) } as *const Option<T>
            as *mut Option<T>;
        unsafe { slot.write(Some(value)) };
        self.shared
            .head
            .store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// 当前占用槽位数。
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Consumer<T> {
    /// 队列容量。
    pub fn capacity(&self) -> usize {
        self.shared.mask + 1
    }

    /// 尝试出队；队列空返回 None。实时安全：不分配、不加锁、不阻塞。
    pub fn pop(&self) -> Option<T> {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        let head = self.shared.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let index = tail & self.shared.mask;
        // SAFETY（逻辑安全）：该槽位已由生产者以 Release 发布 head，消费者独占读取。
        let slot = unsafe { self.shared.buffer.get_unchecked(index) } as *const Option<T>
            as *mut Option<T>;
        let value = unsafe { slot.read() };
        unsafe { slot.write(None) };
        self.shared
            .tail
            .store(tail.wrapping_add(1), Ordering::Release);
        value
    }

    /// 当前占用槽位数。
    pub fn len(&self) -> usize {
        self.shared.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::channel;

    #[test]
    fn fifo_order_is_preserved() {
        let (producer, consumer) = channel(4);
        for value in 0..4 {
            assert!(producer.push(value));
        }
        for value in 0..4 {
            assert_eq!(consumer.pop(), Some(value));
        }
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn full_queue_rejects_and_recovers() {
        let (producer, consumer) = channel(3);
        assert_eq!(producer.capacity(), 4, "容量向上取整为 2 的幂");
        for value in 0..4 {
            assert!(producer.push(value));
        }
        assert!(!producer.push(99));
        assert_eq!(consumer.pop(), Some(0));
        assert!(producer.push(99));
        assert_eq!(consumer.pop(), Some(1));
        assert_eq!(consumer.pop(), Some(2));
        assert_eq!(consumer.pop(), Some(3));
        assert_eq!(consumer.pop(), Some(99));
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn wraps_around_many_times() {
        let (producer, consumer) = channel(8);
        let mut next_out = 0u64;
        for value in 0..10_000u64 {
            assert!(producer.push(value));
            if value % 3 == 2 {
                while let Some(item) = consumer.pop() {
                    assert_eq!(item, next_out);
                    next_out += 1;
                }
            }
        }
        while let Some(item) = consumer.pop() {
            assert_eq!(item, next_out);
            next_out += 1;
        }
        assert_eq!(next_out, 10_000);
    }

    #[test]
    fn crosses_threads() {
        let (producer, consumer) = channel(64);
        let handle = std::thread::spawn(move || {
            let mut observed = Vec::with_capacity(20_000);
            while observed.len() < 20_000 {
                if let Some(value) = consumer.pop() {
                    observed.push(value);
                } else {
                    std::hint::spin_loop();
                }
            }
            observed
        });
        for value in 0..20_000u32 {
            while !producer.push(value) {
                std::hint::spin_loop();
            }
        }
        let observed = handle.join().expect("producer thread");
        assert_eq!(observed, (0..20_000).collect::<Vec<u32>>());
    }
}
