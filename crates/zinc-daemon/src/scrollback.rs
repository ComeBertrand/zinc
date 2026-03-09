use std::collections::VecDeque;

/// Ring buffer that stores the last N bytes of agent output.
pub struct ScrollbackBuffer {
    buffer: VecDeque<u8>,
    capacity: usize,
}

/// Default scrollback: 1MB
const DEFAULT_CAPACITY: usize = 1_048_576;

impl ScrollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity.min(DEFAULT_CAPACITY)),
            capacity,
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        // If incoming data exceeds capacity, only keep the tail
        if data.len() >= self.capacity {
            self.buffer.clear();
            let start = data.len() - self.capacity;
            self.buffer.extend(&data[start..]);
            return;
        }

        // Drain enough space for new data
        let overflow = (self.buffer.len() + data.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.buffer.drain(..overflow);
        }

        self.buffer.extend(data);
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.buffer.iter().copied().collect()
    }
}

impl Default for ScrollbackBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_within_capacity() {
        let mut buf = ScrollbackBuffer::new(16);
        buf.write(b"hello");
        assert_eq!(buf.to_vec(), b"hello");
        assert_eq!(buf.len(), 5);
    }

    #[test]
    fn write_exactly_at_capacity() {
        let mut buf = ScrollbackBuffer::new(5);
        buf.write(b"12345");
        assert_eq!(buf.to_vec(), b"12345");
    }

    #[test]
    fn overflow_keeps_tail() {
        let mut buf = ScrollbackBuffer::new(8);
        buf.write(b"12345");
        buf.write(b"67890");
        // Total 10 bytes, capacity 8: keep last 8
        assert_eq!(buf.to_vec(), b"34567890");
    }

    #[test]
    fn single_write_larger_than_capacity() {
        let mut buf = ScrollbackBuffer::new(8);
        buf.write(b"abcdefghijklmnopqrst"); // 20 bytes
        assert_eq!(buf.to_vec(), b"mnopqrst");
    }

    #[test]
    fn single_write_equal_to_capacity() {
        let mut buf = ScrollbackBuffer::new(8);
        buf.write(b"abcdefgh");
        assert_eq!(buf.to_vec(), b"abcdefgh");
    }

    #[test]
    fn multiple_small_writes_overflow() {
        let mut buf = ScrollbackBuffer::new(10);
        buf.write(b"aaa");
        buf.write(b"bbb");
        buf.write(b"ccc");
        buf.write(b"ddd");
        // 12 bytes total, keep last 10
        assert_eq!(buf.to_vec(), b"abbbcccddd");
    }

    #[test]
    fn write_empty() {
        let mut buf = ScrollbackBuffer::new(8);
        buf.write(b"hello");
        buf.write(b"");
        assert_eq!(buf.to_vec(), b"hello");
    }

    #[test]
    fn capacity_zero() {
        let mut buf = ScrollbackBuffer::new(0);
        buf.write(b"anything");
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn capacity_one() {
        let mut buf = ScrollbackBuffer::new(1);
        buf.write(b"abc");
        assert_eq!(buf.to_vec(), b"c");
        buf.write(b"x");
        assert_eq!(buf.to_vec(), b"x");
    }

    #[test]
    fn default_capacity() {
        let buf = ScrollbackBuffer::default();
        assert_eq!(buf.capacity, 1_048_576);
    }

    #[test]
    fn large_capacity_beyond_prealloc() {
        // Capacity > DEFAULT_CAPACITY: VecDeque pre-allocates less, but logical capacity works
        let mut buf = ScrollbackBuffer::new(2_000_000);
        let data = vec![0x42u8; 1_500_000];
        buf.write(&data);
        assert_eq!(buf.len(), 1_500_000);
    }
}
