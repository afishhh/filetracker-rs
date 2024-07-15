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

    let mut result = [0u8; N];
    for (i, j) in (0..N * 2).step_by(2).enumerate() {
        result[i] = u8::from_str_radix(&data[j..j + 2], 16).ok()?
    }

    Some(result)
}
