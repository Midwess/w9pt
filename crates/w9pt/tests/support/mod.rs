mod model_driver;

pub use model_driver::{ModelDriver, ModelEvent};

pub fn frame(message_type: u8, tag: u16, payload: &[u8]) -> Vec<u8> {
    let size = 7 + payload.len();
    let mut frame = Vec::with_capacity(size);
    frame.extend_from_slice(&(size as u32).to_le_bytes());
    frame.push(message_type);
    frame.extend_from_slice(&tag.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub fn wire_string(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + value.len());
    bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes
}
