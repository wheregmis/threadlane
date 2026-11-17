//! Runtime probe for the same Tree-sitter versions used by the desktop kit.

#[no_mangle]
pub extern "C" fn check_grammars() -> usize {
    let mut count = 0;
    for (language, source) in [
        (tree_sitter_rust::LANGUAGE.into(), "pub fn render() {}"),
        (tree_sitter_json::LANGUAGE.into(), "{\"ok\":true}"),
        (
            tree_sitter_bash::LANGUAGE.into(),
            "cargo check -p threadlane-gpui",
        ),
        (
            tree_sitter_bash::LANGUAGE.into(),
            "cat <<EOF\nsource\nEOF\n",
        ),
    ] {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        count += 1;
    }
    count
}
