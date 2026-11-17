#[test]
fn height_resize_preserves_soft_wraps_and_saved_cursor() {
    let mut parser = vt100::Parser::new(6, 4, 20);
    parser.process(b"abcdefghijkl\r\n>\x1b7");
    let before = parser.screen().contents();
    parser.screen_mut().set_size(2, 4);
    parser.screen_mut().set_size(6, 4);
    assert_eq!(parser.screen().contents(), before);
    assert!(parser.screen().row_wrapped(0));
    assert!(parser.screen().row_wrapped(1));
    parser.process(b"\x1b[H\x1b8X");
    assert_eq!(parser.screen().cursor_position(), (3, 2));
    assert!(parser.screen().contents().ends_with(">X"));
}

#[test]
fn shrinking_in_alternate_screen_preserves_normal_shell_output() {
    let mut parser = vt100::Parser::new(6, 20, 20);
    parser.process(b"first\r\nsecond\r\nthird\r\nprompt> ");
    parser.process(b"\x1b[?1049hTUI");
    parser.screen_mut().set_size(2, 20);
    assert!(parser.screen().alternate_screen());
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 0);
    parser.screen_mut().set_size(6, 20);
    parser.process(b"\x1b[?1049l");
    assert!(!parser.screen().alternate_screen());
    assert_eq!(parser.screen().contents(), "first\nsecond\nthird\nprompt> ");
    assert_eq!(parser.screen().cursor_position(), (3, 8));
}

#[test]
fn shrinking_keeps_the_existing_scrollback_limit() {
    let mut parser = vt100::Parser::new(6, 20, 2);
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nprompt");
    parser.screen_mut().set_size(1, 20);
    assert_eq!(parser.screen().contents(), "prompt");
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 2);
    assert_eq!(parser.screen().contents(), "four");
    parser.screen_mut().set_size(6, 20);
    assert_eq!(parser.screen().scrollback(), 0);
    assert_eq!(parser.screen().contents(), "four\nfive\nprompt");
    assert_eq!(parser.screen().cursor_position(), (2, 6));
}
