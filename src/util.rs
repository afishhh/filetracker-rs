use std::path::{Path, PathBuf};

pub fn bytes_to_hex(data: &[u8]) -> String {
    data.iter()
        .flat_map(|x| {
            [
                char::from_digit(((x & 0xF0) >> 4).into(), 16).unwrap(),
                char::from_digit((x & 0xF).into(), 16).unwrap(),
            ]
        })
        .collect::<String>()
}

pub fn hex_to_byte_array<const N: usize>(data: &str) -> Option<[u8; N]> {
    if data.len() != N * 2 {
        return None;
    }

    let mut it = data.chars();
    let mut result = [0u8; N];
    for byte in result.iter_mut() {
        *byte = ((it.next()?.to_digit(16)? << 4) | it.next()?.to_digit(16)?) as u8;
    }

    Some(result)
}

pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut new = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => _ = new.pop(),
            other => new.push(other),
        }
    }

    new
}
