#![cfg_attr(not(test), no_std)]
//! KnightOS Input / HID Event Layer (`knightos-input`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key { scancode: u16, ascii: u8, pressed: bool },
    PointerMove { dx: i16, dy: i16 },
    PointerButton { button: u8, pressed: bool },
}

pub struct InputQueue<const CAP: usize = 64> {
    ring: [Option<InputEvent>; CAP],
    head: usize,
    tail: usize,
}

impl<const CAP: usize> Default for InputQueue<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> InputQueue<CAP> {
    pub const fn new() -> Self {
        Self {
            ring: [None; CAP],
            head: 0,
            tail: 0,
        }
    }

    pub fn push(&mut self, ev: InputEvent) -> bool {
        let next = (self.tail + 1) % CAP;
        if next == self.head {
            return false;
        }
        self.ring[self.tail] = Some(ev);
        self.tail = next;
        true
    }

    pub fn pop(&mut self) -> Option<InputEvent> {
        if self.head == self.tail {
            return None;
        }
        let ev = self.ring[self.head].take();
        self.head = (self.head + 1) % CAP;
        ev
    }
}

/// Decodes a Set-1 PS/2 / AT make/break scancode into an [`InputEvent`].
pub fn decode_scancode_set1(code: u8) -> InputEvent {
    let pressed = (code & 0x80) == 0;
    let base = code & 0x7F;
    let ascii = match base {
        0x1E => b'a',
        0x30 => b'b',
        0x2E => b'c',
        0x20 => b'd',
        0x12 => b'e',
        0x21 => b'f',
        0x22 => b'g',
        0x23 => b'h',
        0x17 => b'i',
        0x24 => b'j',
        0x25 => b'k',
        0x26 => b'l',
        0x32 => b'm',
        0x31 => b'n',
        0x18 => b'o',
        0x19 => b'p',
        0x10 => b'q',
        0x13 => b'r',
        0x1F => b's',
        0x14 => b't',
        0x16 => b'u',
        0x2F => b'v',
        0x11 => b'w',
        0x2D => b'x',
        0x15 => b'y',
        0x2C => b'z',
        0x39 => b' ',
        0x1C => b'\n',
        _ => 0,
    };
    InputEvent::Key {
        scancode: base as u16,
        ascii,
        pressed,
    }
}

/// Decodes a VirtIO-Input event (`type: u16, code: u16, value: i32`).
pub fn decode_virtio_input_event(ev_type: u16, code: u16, value: i32) -> Option<InputEvent> {
    match ev_type {
        1 => Some(InputEvent::Key {
            scancode: code,
            ascii: 0,
            pressed: value != 0,
        }),
        2 => match code {
            0 => Some(InputEvent::PointerMove {
                dx: value.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                dy: 0,
            }),
            1 => Some(InputEvent::PointerMove {
                dx: 0,
                dy: value.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            }),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_queue_and_scancodes() {
        let mut q: InputQueue<8> = InputQueue::new();
        assert!(q.push(decode_scancode_set1(0x1E)));
        assert!(q.push(decode_scancode_set1(0x9E)));
        assert_eq!(
            q.pop(),
            Some(InputEvent::Key { scancode: 0x1E, ascii: b'a', pressed: true })
        );
        assert_eq!(
            q.pop(),
            Some(InputEvent::Key { scancode: 0x1E, ascii: b'a', pressed: false })
        );
        assert_eq!(q.pop(), None);
    }
}
