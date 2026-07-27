/// Remove CSI, OSC and a few single-character ANSI control sequences.
///
/// This intentionally operates on bytes so invalid UTF-8 from a PTY is retained until the
/// final lossy display conversion.
pub fn strip_terminal_controls(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != 0x1b {
            output.push(input[index]);
            index += 1;
            continue;
        }
        index += 1;
        if index >= input.len() {
            break;
        }
        match input[index] {
            b'[' => {
                index += 1;
                while index < input.len() {
                    let byte = input[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                index += 1;
                while index < input.len() {
                    if input[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if input[index] == 0x1b && input.get(index + 1).copied() == Some(b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    output
}

pub fn display_bytes(input: &[u8]) -> String {
    let clean = strip_terminal_controls(input);
    let mut output = String::new();
    for character in String::from_utf8_lossy(&clean).chars() {
        match character {
            '\r' => {}
            '\n' | '\t' => output.push(character),
            '\u{7f}' => output.push('␡'),
            control if control <= '\u{1f}' => {
                output.push(char::from_u32(control as u32 + 0x2400).unwrap_or('�'));
            }
            printable => output.push(printable),
        }
    }
    output
}

pub fn display_input(input: &[u8]) -> String {
    let mut normalized = Vec::with_capacity(input.len());
    for byte in input {
        match byte {
            0x7f | 0x08 => {
                normalized.pop();
            }
            value => normalized.push(*value),
        }
    }
    display_bytes(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc() {
        let input = b"\x1b[31mred\x1b[0m\x1b]0;title\x07!";
        assert_eq!(strip_terminal_controls(input), b"red!");
    }

    #[test]
    fn displays_controls_without_executing_them() {
        assert_eq!(display_bytes(b"a\0b\x7f"), "a␀b␡");
    }
}
