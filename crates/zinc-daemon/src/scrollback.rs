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
}

impl Default for ScrollbackBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}
