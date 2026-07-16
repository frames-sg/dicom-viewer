pub(super) fn format_integer(value: u64) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (len - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(*byte));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_integer;

    #[test]
    fn groups_integer_digits_in_threes() {
        assert_eq!(format_integer(0), "0");
        assert_eq!(format_integer(999), "999");
        assert_eq!(format_integer(1_000), "1,000");
        assert_eq!(format_integer(1_234_567), "1,234,567");
    }
}
