const SNAPSHOT_ROWS: u16 = 40;
const SNAPSHOT_COLUMNS: u16 = 120;
const SNAPSHOT_SCROLLBACK: usize = 256;

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

/// Render PTY output into a bounded virtual terminal and return its plain-text state.
///
/// Unlike stripping escape sequences from the byte stream, this applies cursor movement,
/// erasure, carriage returns and screen redraws. The result therefore resembles what was
/// visible in the terminal instead of containing every intermediate frame.
pub fn terminal_snapshot(input: &[u8]) -> String {
    snapshot_with_size(input, SNAPSHOT_ROWS, SNAPSHOT_COLUMNS, SNAPSHOT_SCROLLBACK)
}

fn snapshot_with_size(input: &[u8], rows: u16, columns: u16, scrollback: usize) -> String {
    let mut parser = vt100::Parser::new(rows, columns, scrollback);
    let mut last_alternate_snapshot = None;
    let mut consumed = 0;
    while let Some(exit_byte) = next_alternate_screen_exit(input, consumed) {
        parser.process(&input[consumed..exit_byte]);
        let was_alternate = parser.screen().alternate_screen();
        let candidate = was_alternate.then(|| trim_screen_contents(parser.screen().contents()));
        parser.process(&input[exit_byte..=exit_byte]);
        if was_alternate && !parser.screen().alternate_screen() {
            last_alternate_snapshot = candidate.filter(|snapshot| !snapshot.is_empty());
        }
        consumed = exit_byte + 1;
    }
    parser.process(&input[consumed..]);

    // Full-screen programs normally discard their alternate buffer on exit. Preserve the
    // final rendered frame so inspecting that command shows the application, not merely
    // the shell screen restored underneath it.
    if let Some(snapshot) = last_alternate_snapshot {
        return snapshot;
    }

    // Asking for the largest possible offset lets vt100 clamp to the actual number of
    // scrollback rows. Read the first row at each offset, then append the current screen.
    parser.set_scrollback(usize::MAX);
    let available = parser.screen().scrollback();
    let mut lines = Vec::with_capacity(available + usize::from(rows));
    for offset in (1..=available).rev() {
        parser.set_scrollback(offset);
        if let Some(line) = parser.screen().rows(0, columns).next() {
            lines.push(line);
        }
    }
    parser.set_scrollback(0);
    lines.extend(parser.screen().rows(0, columns));

    let first_content = lines.iter().position(|line| !line.trim().is_empty());
    let last_content = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first_content, last_content) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

fn next_alternate_screen_exit(input: &[u8], start: usize) -> Option<usize> {
    [b"\x1b[?47l".as_slice(), b"\x1b[?1047l", b"\x1b[?1049l"]
        .iter()
        .filter_map(|sequence| {
            input[start..]
                .windows(sequence.len())
                .position(|window| window == *sequence)
                .map(|position| start + position + sequence.len() - 1)
        })
        .min()
}

fn trim_screen_contents(contents: String) -> String {
    contents
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_owned()
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

    #[test]
    fn snapshot_collapses_carriage_return_redraws() {
        let snapshot = snapshot_with_size(b"progress 0%\rprogress 50%\rprogress 100%\n", 4, 40, 8);
        assert_eq!(snapshot, "progress 100%");
    }

    #[test]
    fn snapshot_applies_cursor_positioning_and_erasure() {
        let snapshot = snapshot_with_size(
            b"obsolete\x1b[2J\x1b[Hmenu\nitem\x1b[2;1Hselected\x1b[K",
            4,
            40,
            8,
        );
        assert_eq!(snapshot, "menu\nselected");
    }

    #[test]
    fn snapshot_preserves_the_last_alternate_screen_frame() {
        let snapshot = snapshot_with_size(
            b"shell\x1b[?1049hmenu\nitem\x1b[2;1Hselected\x1b[K\x1b[?1049lshell",
            4,
            40,
            8,
        );
        assert_eq!(snapshot, "menu\nselected");
    }

    #[test]
    fn snapshot_keeps_bounded_scrollback() {
        let snapshot = snapshot_with_size(b"one\ntwo\nthree\nfour\nfive", 2, 20, 2);
        assert!(!snapshot.contains("one"));
        assert!(snapshot.contains("three"));
        assert!(snapshot.contains("five"));
        assert!(snapshot.lines().count() <= 4);
    }
}
