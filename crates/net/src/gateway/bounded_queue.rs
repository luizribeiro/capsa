use std::collections::VecDeque;

/// A bounded queue for overflow buffering.
///
/// This is a simple wrapper around `VecDeque` that enforces a maximum capacity.
/// Used to replace ad-hoc `VecDeque + len < MAX` patterns throughout the codebase.
pub struct BoundedQueue<T> {
    items: VecDeque<T>,
    capacity: usize,
}

impl<T> BoundedQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push an item to the queue.
    /// Returns `Err(item)` if the queue is full.
    pub fn push(&mut self, item: T) -> Result<(), T> {
        if self.items.len() >= self.capacity {
            return Err(item);
        }
        self.items.push_back(item);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        self.items.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_room(&self) -> bool {
        self.items.len() < self.capacity
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_succeeds_when_room_available() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(3);
        assert!(queue.push(1).is_ok());
        assert!(queue.push(2).is_ok());
        assert!(queue.push(3).is_ok());
        assert_eq!(queue.len(), 3);
    }

    #[test]
    fn push_returns_item_when_full() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(2);
        assert!(queue.push(1).is_ok());
        assert!(queue.push(2).is_ok());

        let result = queue.push(3);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 3);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn pop_returns_none_when_empty() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(5);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn pop_returns_items_in_fifo_order() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(3);
        queue.push(1).unwrap();
        queue.push(2).unwrap();
        queue.push(3).unwrap();

        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn capacity_enforcement() {
        let mut queue: BoundedQueue<String> = BoundedQueue::new(2);
        assert_eq!(queue.capacity(), 2);

        queue.push("a".to_string()).unwrap();
        queue.push("b".to_string()).unwrap();
        assert!(queue.push("c".to_string()).is_err());

        queue.pop();
        assert!(queue.push("c".to_string()).is_ok());
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn has_room_reflects_state() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(2);
        assert!(queue.has_room());

        queue.push(1).unwrap();
        assert!(queue.has_room());

        queue.push(2).unwrap();
        assert!(!queue.has_room());

        queue.pop();
        assert!(queue.has_room());
    }

    #[test]
    fn is_empty_reflects_state() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(2);
        assert!(queue.is_empty());

        queue.push(1).unwrap();
        assert!(!queue.is_empty());

        queue.pop();
        assert!(queue.is_empty());
    }

    #[test]
    fn len_tracks_items() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(5);
        assert_eq!(queue.len(), 0);

        queue.push(1).unwrap();
        assert_eq!(queue.len(), 1);

        queue.push(2).unwrap();
        queue.push(3).unwrap();
        assert_eq!(queue.len(), 3);

        queue.pop();
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn zero_capacity_queue_rejects_all() {
        let mut queue: BoundedQueue<i32> = BoundedQueue::new(0);
        assert!(!queue.has_room());
        assert!(queue.push(1).is_err());
        assert!(queue.is_empty());
    }
}
