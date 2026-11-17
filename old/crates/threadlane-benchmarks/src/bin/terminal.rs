//! Headless benchmarks for the terminal parser hot paths.

const SCROLLBACK_ROWS: usize = 10_000;
const SAMPLES: usize = 10;

fn fixture() -> Vec<u8> {
    let mut output = Vec::with_capacity(2_000 * 64);
    for line in 0..2_000 {
        output.extend_from_slice(
            format!(
                "\x1b[38;5;{}mterminal line {line:04}: fixed ANSI-colored output\x1b[0m\r\n",
                line % 256
            )
            .as_bytes(),
        );
    }
    output
}

fn parse_terminal_output_inner(bytes: &[u8]) {
    let mut parser = vt100::Parser::new(24, 80, SCROLLBACK_ROWS);
    parser.process(bytes);
    std::hint::black_box(parser.screen().state_formatted());
}

#[hotpath::measure]
fn parse_terminal_output(bytes: &[u8]) {
    parse_terminal_output_inner(bytes);
}

#[hotpath::measure]
fn resize_and_scrollback(parser: &mut vt100::Parser) {
    for (rows, cols) in [(40, 120), (24, 80), (60, 160), (24, 80)] {
        parser.screen_mut().set_size(rows, cols);
        let offset = parser.screen().scrollback().min(rows as usize);
        parser.screen_mut().set_scrollback(offset);
    }
    std::hint::black_box(parser.screen().state_formatted());
}

#[hotpath::main]
fn main() {
    let bytes = fixture();
    parse_terminal_output_inner(&bytes);
    for _ in 0..SAMPLES {
        parse_terminal_output(&bytes);
    }

    let mut parser = vt100::Parser::new(24, 80, SCROLLBACK_ROWS);
    parser.process(&bytes);
    for _ in 0..SAMPLES {
        parser.screen_mut().set_scrollback(SCROLLBACK_ROWS);
        assert_ne!(parser.screen().scrollback(), 0);
        resize_and_scrollback(&mut parser);
    }
}
