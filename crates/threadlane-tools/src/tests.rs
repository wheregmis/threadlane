    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn canonical_workspace_root_reuses_successful_resolution() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let expected = root.canonicalize().unwrap();
        assert_eq!(canonical_workspace_root(&root).unwrap(), expected);
        std::fs::remove_dir(&root).unwrap();
        assert_eq!(canonical_workspace_root(&root).unwrap(), expected);
    }

    #[test]
    fn validate_path_allows_a_new_absolute_destination_under_a_symlinked_root() {
        // `tempdir()` lives under a symlinked prefix on macOS (`/var` ->
        // `/private/var`), which is exactly the shape a caller sends back after
        // being handed a non-canonical workspace path.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let target = root.join("nested").join("new.txt");

        let resolved = validate_path_in_workspace(&target.to_string_lossy(), root)
            .expect("a new file inside the workspace must be allowed");

        let canonical_root = root.canonicalize().unwrap();
        assert!(resolved.starts_with(&canonical_root));
        assert!(resolved.ends_with("nested/new.txt"));
    }

    #[test]
    fn validate_path_resolves_existing_and_new_paths_to_the_same_root() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("exists.txt"), "x").unwrap();

        let existing =
            validate_path_in_workspace(&root.join("exists.txt").to_string_lossy(), root).unwrap();
        let new =
            validate_path_in_workspace(&root.join("new.txt").to_string_lossy(), root).unwrap();

        assert_eq!(existing.parent(), new.parent());
    }

    #[test]
    fn validate_path_still_denies_escapes_for_paths_that_do_not_exist() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let relative = validate_path_in_workspace("../escaped.txt", dir.path());
        assert!(relative.is_err(), "got: {relative:?}");

        let absolute = validate_path_in_workspace(
            &outside.path().join("new.txt").to_string_lossy(),
            dir.path(),
        );
        assert!(absolute.is_err(), "got: {absolute:?}");
    }

    #[cfg(unix)]
    #[test]
    fn fuzzy_path_ignores_symlinks_outside_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "secret").unwrap();
        symlink(
            outside.path().join("secret.rs"),
            root.path().join("secret.rs"),
        )
        .unwrap();

        assert_eq!(
            find_fuzzy_workspace_path("secret.rs", root.path()).unwrap(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn fuzzy_path_rechecks_cached_symlink_matches() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let linked = root.path().join("secret.rs");
        fs::write(outside.path().join("secret.rs"), "secret").unwrap();
        symlink(outside.path().join("secret.rs"), &linked).unwrap();
        let canonical_root = canonical_workspace_root(root.path()).unwrap();
        FUZZY_PATH_CACHE.write().unwrap().insert(
            (canonical_root, "secret.rs".into()),
            (linked, "secret.rs".into()),
        );

        assert_eq!(
            find_fuzzy_workspace_path("secret.rs", root.path()).unwrap(),
            None
        );
    }

    #[test]
    fn test_run_post_edit_diagnostics_non_rust_file() {
        let dir = tempdir().unwrap();
        let res = run_post_edit_diagnostics(dir.path(), "readme.txt");
        assert_eq!(res, "");
    }

    #[test]
    fn rust_write_surfaces_compile_diagnostics_in_the_same_tool_result() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"diagnostic-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let result = execute_tool_in_workspace(
            "write_file",
            &serde_json::json!({
                "path": "src/lib.rs",
                "content": "pub fn broken() { let _: = 1; }"
            })
            .to_string(),
            dir.path(),
        );
        assert!(result.starts_with("Successfully wrote"));
        assert!(result.contains("[LSP Diagnostics Post-Check]"));
        assert!(result.contains("Found 1 error(s)"), "{result}");
        assert!(result.contains("Line 1"), "{result}");
    }

    #[test]
    fn test_path_matches_normalization() {
        assert!(path_matches("src/main.rs", "./src/main.rs"));
        assert!(path_matches("./src/main.rs", "src/main.rs"));
        assert!(path_matches("crates/threadlane/src/main.rs", "src/main.rs"));
        assert!(path_matches(
            "src/main.rs",
            "/Users/foo/project/src/main.rs"
        ));
        assert!(!path_matches("src/other_main.rs", "main.rs"));
    }

    #[test]
    fn test_list_dir_tool() {
        let res = execute_tool("list_dir", r#"{"path": "."}"#);
        assert!(res.contains("Cargo.toml"));
    }

    #[test]
    fn list_dir_returns_typed_error_for_regular_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let result =
            try_execute_tool_in_workspace("list_dir", r#"{"path":"file.txt"}"#, dir.path());

        assert!(result.is_err());
    }

    #[test]
    fn list_dir_preserves_successful_output() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("alpha")).unwrap();
        fs::write(dir.path().join("beta.txt"), "content").unwrap();

        let typed =
            try_execute_tool_in_workspace("list_dir", r#"{"path":"."}"#, dir.path()).unwrap();
        let wrapped = execute_tool_in_workspace("list_dir", r#"{"path":"."}"#, dir.path());

        assert_eq!(typed, "[DIR]  alpha\n[FILE] beta.txt");
        assert_eq!(wrapped, typed);
    }
    #[test]
    fn edit_files_hashline_is_atomic_on_stale_anchor() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "three\nfour\n").unwrap();
        let a_anchor = hashline::format_line_hashline(1, "one")
            .split('|')
            .next()
            .unwrap()
            .to_string();
        let args = serde_json::json!({"files": [
            {"path":"a.txt","edits":[{"start_anchor":a_anchor,"action":"replace","new_content":"changed"}]},
            {"path":"b.txt","edits":[{"start_anchor":"1:bad","action":"replace","new_content":"broken"}]}
        ]});
        let result =
            try_execute_tool_in_workspace("edit_files_hashline", &args.to_string(), dir.path());
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "three\nfour\n"
        );
    }

    #[test]
    fn edit_files_hashline_commits_all_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let anchor = |line: &str| {
            hashline::format_line_hashline(1, line)
                .split('|')
                .next()
                .unwrap()
                .to_string()
        };
        let args = serde_json::json!({"files": [
            {"path":"a.txt","edits":[{"start_anchor":anchor("one"),"action":"replace","new_content":"first"}]},
            {"path":"b.txt","edits":[{"start_anchor":anchor("two"),"action":"replace","new_content":"second"}]}
        ]});
        try_execute_tool_in_workspace("edit_files_hashline", &args.to_string(), dir.path())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn apply_workspace_edit_plan_handles_utf16_and_is_atomic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let rocket = \"🚀\";\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "rocket();\n").unwrap();
        let plan = serde_json::json!({"kind":"lsp_workspace_edit_plan","version":1,"files":[
            {"path":"a.rs","text_edits":[{"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":10}},"newText":"ship"}]},
            {"path":"b.rs","text_edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":6}},"newText":"ship"}]}
        ]});
        try_execute_tool_in_workspace(
            "apply_workspace_edit_plan",
            &serde_json::json!({"plan":plan}).to_string(),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let ship = \"🚀\";\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
            "ship();\n"
        );
    }

    #[test]
    fn apply_workspace_edit_plan_preflights_every_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "old\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "old\n").unwrap();
        let plan = serde_json::json!({"kind":"lsp_workspace_edit_plan","files":[
            {"path":"a.rs","text_edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"new"}]},
            {"path":"b.rs","text_edits":[{"range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}},"newText":"bad"}]}
        ]});
        assert!(try_execute_tool_in_workspace(
            "apply_workspace_edit_plan",
            &serde_json::json!({"plan":plan}).to_string(),
            dir.path()
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn test_edit_file_hashline_schema_description() {
        let tools = get_available_tools();
        let hashline_tool = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_file_hashline")
            .expect("edit_file_hashline tool should exist");

        let desc = hashline_tool["function"]["description"].as_str().unwrap();
        assert!(
            desc.contains("Supports line and range replace, insert_after, and delete operations")
        );
        assert!(desc.contains("Always batch multiple edits for the same file in one tool call"));

        let params = &hashline_tool["function"]["parameters"]["properties"];
        let start_anchor_desc = params["edits"]["items"]["properties"]["start_anchor"]
            ["description"]
            .as_str()
            .unwrap();
        assert!(start_anchor_desc.contains("formatted as 'line_number:hash'"));

        let end_anchor_desc = params["edits"]["items"]["properties"]["end_anchor"]["description"]
            .as_str()
            .unwrap();
        assert!(end_anchor_desc.contains("multi-line range edits"));

        let action_desc = params["edits"]["items"]["properties"]["action"]["description"]
            .as_str()
            .unwrap();
        assert!(action_desc.contains("'replace'"));
        assert!(action_desc.contains("'insert_after'"));
        assert!(action_desc.contains("'delete'"));

        let new_content_desc = params["edits"]["items"]["properties"]["new_content"]["description"]
            .as_str()
            .unwrap();
        assert!(new_content_desc.contains("Omit or leave empty for 'delete' actions"));
    }

    #[test]
    fn test_workspace_containment_read_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let res = execute_tool_in_workspace("read_file", r#"{"path": "../secret.txt"}"#, root);
        assert!(res.contains("Access denied"));
    }

    #[test]
    fn test_workspace_containment_command_cwd_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let res =
            execute_tool_in_workspace("run_command", r#"{"command": "ls", "cwd": "/tmp"}"#, root);
        assert!(res.contains("Access denied"));
    }

    #[test]
    fn test_read_file_rejects_reversed_line_range_without_panicking() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let res = execute_tool_in_workspace(
            "read_file",
            r#"{"path": "sample.txt", "start_line": 3, "end_line": 2}"#,
            dir.path(),
        );

        assert_eq!(
            res,
            "Invalid line range: end_line (2) must not be before start_line (3)."
        );
    }

    #[test]
    fn test_save_and_read_memory_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let initial_read = execute_tool_in_workspace("read_memory", "{}", root);
        assert!(initial_read.contains("No persistent memory found"));

        let payload =
            json!({"content": "## Architectural Decision\nUse GPUI with threadlane state."})
                .to_string();
        let save_res = execute_tool_in_workspace("save_memory", &payload, root);
        assert!(save_res.contains("Successfully saved memory"));

        let read_res = execute_tool_in_workspace("read_memory", "{}", root);
        assert!(read_res.contains("Use GPUI with threadlane state."));
    }

    #[test]
    fn test_save_memory_fails_on_existing_non_utf8_file_and_preserves_bytes() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let memory_dir = root.join(".threadlane");
        fs::create_dir_all(&memory_dir).unwrap();
        let memory_file = memory_dir.join("memory.md");
        let original_bytes: Vec<u8> = vec![0xff, 0x00, 0xfe, b'a', b'b'];
        fs::write(&memory_file, &original_bytes).unwrap();

        let payload = json!({"content": "## Append Attempt"}).to_string();
        let typed = try_execute_tool_in_workspace("save_memory", &payload, root)
            .expect_err("Expected invalid UTF-8 memory file to produce read error");
        assert!(typed.starts_with("Error reading .threadlane/memory.md:"));

        let wrapped = execute_tool_in_workspace("save_memory", &payload, root);
        assert_eq!(wrapped, typed);

        let on_disk = fs::read(&memory_file).unwrap();
        assert_eq!(on_disk, original_bytes);
    }

    #[test]
    fn test_get_repo_map_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let rs_file = src_dir.join("main.rs");
        fs::write(
            &rs_file,
            "pub struct AppState {}\npub fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let map_res = execute_tool_in_workspace("get_repo_map", "{}", root);
        assert!(map_res.contains("src/main.rs"));
        assert!(map_res.contains("pub struct AppState"));
        assert!(map_res.contains("pub fn main()"));
    }

    #[test]
    fn get_repo_map_returns_typed_error_for_regular_file() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("not-a-directory.rs"),
            "pub fn visible() {}\n",
        )
        .unwrap();

        let args = r#"{"path":"not-a-directory.rs"}"#;
        let error = try_execute_tool_in_workspace("get_repo_map", args, dir.path())
            .expect_err("regular files must not produce a successful map");

        assert_eq!(
            execute_tool_in_workspace("get_repo_map", args, dir.path()),
            error
        );
    }

    #[test]
    fn get_repo_map_returns_typed_error_when_source_file_cannot_be_read_as_text() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("invalid.rs"), [0xff, 0xfe]).unwrap();

        let result = try_execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert!(result.is_err(), "source read failures must not be skipped");
    }

    #[test]
    fn get_repo_map_preserves_successful_output() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct App {}\nfn helper() {}\n",
        )
        .unwrap();

        let typed = try_execute_tool_in_workspace("get_repo_map", "{}", dir.path()).unwrap();
        let wrapped = execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert_eq!(
            typed,
            "src/lib.rs\n  L1: pub struct App {}\n  L2: fn helper() {}"
        );
        assert_eq!(wrapped, typed);
    }

    #[test]
    fn test_truncate_tool_output() {
        let long_string = "a".repeat(5000);
        let truncated = truncate_tool_output(&long_string);
        assert!(truncated.contains("[... Output truncated:"));
        assert!(truncated.len() < 3000);
    }

    #[test]
    fn test_truncate_tool_output_uses_characters_for_unicode() {
        let output = "😀".repeat(2_000);
        let truncated = truncate_tool_output(&output);

        assert_eq!(truncated, output);
    }

    #[cfg(unix)]
    #[test]
    fn test_code_tools_skip_symlinked_directories_outside_workspace() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(
            outside.path().join("secret.rs"),
            "pub fn external_secret_symbol() {}\n",
        )
        .unwrap();
        symlink(outside.path(), dir.path().join("linked")).unwrap();

        let map = execute_tool_in_workspace("get_repo_map", "{}", dir.path());

        assert!(!map.contains("external_secret_symbol"));
    }

    #[test]
    fn test_consolidate_memory_preserves_unmanaged_content() {
        let existing = "# Project Memory\n\nPersonal notes with\nmultiple lines.\n\n## Other Notes\n- Keep this.\n\n## Architecture\n- Existing architecture\n";
        assert_eq!(
            consolidate_memory_entries(existing, &[], &[], &[]),
            existing.trim()
        );
        let merged =
            consolidate_memory_entries(existing, &["New architecture".to_string()], &[], &[]);

        assert!(merged.contains("Personal notes with\nmultiple lines."));
        assert!(merged.contains("## Other Notes\n- Keep this."));
        assert!(merged.contains("- Existing architecture"));
        assert!(merged.contains("- New architecture"));
    }

    #[test]
    fn test_manage_memory_tool() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let save_payload = json!({
            "action": "save",
            "content": "Rule: Always check cargo diff"
        })
        .to_string();
        let save_res = execute_tool_in_workspace("manage_memory", &save_payload, root);
        assert!(save_res.contains("Successfully saved memory to .threadlane/memory.md"));

        let read_payload = json!({ "action": "read" }).to_string();
        let read_res = execute_tool_in_workspace("manage_memory", &read_payload, root);
        assert!(read_res.contains("Rule: Always check cargo diff"));

        let consolidate_payload = json!({
            "action": "consolidate",
            "architecture": ["Use GPUI UI components"],
            "gotchas": ["cargo check requires unsandboxed bypass on macOS"],
            "verification": ["cargo test --workspace"]
        })
        .to_string();

        let res = execute_tool_in_workspace("manage_memory", &consolidate_payload, root);
        assert!(res.contains("Successfully consolidated memory entries"));

        let mem_content = execute_tool_in_workspace("manage_memory", &read_payload, root);
        assert!(mem_content.contains("## Architecture"));
        assert!(mem_content.contains("Use GPUI UI components"));

        assert!(mem_content.contains("## Gotchas"));
        assert!(mem_content.contains("cargo check requires unsandboxed bypass on macOS"));
        assert!(mem_content.contains("## Verification Commands"));
        assert!(mem_content.contains("cargo test --workspace"));
    }

    #[test]
    fn test_read_file_virtual_schemes_and_urls() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Test skill:// discovery in .agents/skills/<name>/SKILL.md
        let skill_dir = root.join(".agents/skills/ponytail");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "ponytail instructions").unwrap();

        let read_skill_payload = json!({ "path": "skill://ponytail" }).to_string();
        let skill_res = execute_tool_in_workspace("read_file", &read_skill_payload, root);
        assert_eq!(skill_res, "ponytail instructions");

        // Test PR URL parsing error format when gh CLI or git remote is missing
        let pr_url_payload =
            json!({ "path": "https://github.com/wheregmis/threadlane/pull/70" }).to_string();
        let pr_res = execute_tool_in_workspace("read_file", &pr_url_payload, root);
        assert!(
            pr_res.contains("pr://70")
                || pr_res.contains("\"number\": 70")
                || pr_res.contains("https://github.com/"),
            "unexpected PR response: {pr_res}"
        );

        // Test GitLab MR URL parsing error format when glab CLI or git remote is missing
        let mr_url_payload =
            json!({ "path": "https://gitlab.com/gitlab-org/gitlab/-/merge_requests/99" })
                .to_string();
        let mr_res = execute_tool_in_workspace("read_file", &mr_url_payload, root);
        assert!(
            mr_res.contains("mr://99")
                || mr_res.contains("GitLab mr #99")
                || mr_res.contains("gitlab.com")
        );
    }
    #[test]
    fn typed_execution_marks_hashline_mismatch_as_error() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("sample.txt"), "original\n").unwrap();
        let result = try_execute_tool_in_workspace(
            "edit_file_hashline",
            &serde_json::json!({
                "path": "sample.txt",
                "edits": [{
                    "start_anchor": "1:bad",
                    "action": "replace",
                    "new_content": "changed"
                }]
            })
            .to_string(),
            dir.path(),
        );

        let error = result.expect_err("a stale hashline anchor must fail");
        assert!(error.contains("Error applying hashline edits"), "{error}");
        assert!(error.contains("Hashline mismatch"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.path().join("sample.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn typed_execution_marks_invalid_arguments_as_error() {
        let result = try_execute_tool("read_file", "{}");
        assert_eq!(result, Err("Error: 'path' parameter is required".into()));
    }

    #[test]
    fn typed_execution_marks_nonzero_command_exit_as_error() {
        let dir = tempdir().unwrap();
        let result = try_execute_tool_in_workspace(
            "run_command",
            r#"{"command":"printf 'out'; printf 'err' >&2; exit 7"}"#,
            dir.path(),
        );

        let error = result.expect_err("a non-zero command exit must fail");
        assert!(error.contains("Exit Status: exit status: 7"), "{error}");
        assert!(error.contains(&format!("--- STDOUT ---\nout")), "{error}");
        assert!(error.contains(&format!("--- STDERR ---\nerr")), "{error}");
    }

    #[test]
    fn typed_execution_keeps_successful_calls_successful() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("sample.txt"), "contents").unwrap();

        let read =
            try_execute_tool_in_workspace("read_file", r#"{"path":"sample.txt"}"#, dir.path())
                .unwrap();
        fs::write(dir.path().join("sample.txt"), "changed after read").unwrap();
        assert_eq!(
            read_file_snapshot_digest(&read),
            Some("d1b2a59fbea7e20077af9f91b27e95e865061b270be03ff539ab3b73587882e8")
        );

        let command =
            try_execute_tool_in_workspace("run_command", r#"{"command":"printf ok"}"#, dir.path());
        assert!(command.is_ok(), "{command:?}");
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_skill_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"skill://definitely-not-installed-review-fixture"}"#;
        let expected = "Unknown skill reference 'definitely-not-installed-review-fixture': No skill file found in workspace or user skills directories";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_skill_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"skill://"}"#;
        let expected = "Error: 'skill://' reference requires a skill name";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_agent_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"agent://definitely-not-installed-review-fixture"}"#;
        let expected = "Unknown agent reference 'definitely-not-installed-review-fixture': No agent file found in workspace or user agent directories";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_agent_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"path":"agent://"}"#;
        let expected = "Error: 'agent://' reference requires an agent name";

        assert_eq!(
            try_execute_tool_in_workspace("read_file", args, dir.path()),
            Err(expected.to_string())
        );
        assert_eq!(
            execute_tool_in_workspace("read_file", args, dir.path()),
            expected
        );
    }

    #[test]
    fn typed_execution_marks_unavailable_virtual_remote_refs_as_errors_without_changing_output() {
        let dir = tempdir().unwrap();

        for path in ["pr://7", "mr://7", "issue://7"] {
            let args = json!({ "path": path }).to_string();
            let kind = path.split_once("://").unwrap().0;
            let expected = format!(
                "{kind}://7 requires a git origin remote or an explicit repository URL (e.g. pr://owner/repo/7)"
            );

            assert_eq!(
                try_execute_tool_in_workspace("read_file", &args, dir.path()),
                Err(expected.clone()),
                "typed result for {path}"
            );
            assert_eq!(
                execute_tool_in_workspace("read_file", &args, dir.path()),
                expected,
                "compatibility output for {path}"
            );
        }
    }

    #[test]
    fn typed_execution_marks_malformed_virtual_remote_refs_as_errors_without_changing_output() {
        let dir = tempdir().unwrap();

        for path in [
            "pr://not-a-number",
            "mr://not-a-number",
            "issue://not-a-number",
        ] {
            let args = json!({ "path": path }).to_string();
            let expected = format!(
                "Invalid repository reference '{path}': expected pr://<num>, issue://<num>, mr://<num>, or GitHub/GitLab URL"
            );

            assert_eq!(
                try_execute_tool_in_workspace("read_file", &args, dir.path()),
                Err(expected.clone()),
                "typed result for {path}"
            );
            assert_eq!(
                execute_tool_in_workspace("read_file", &args, dir.path()),
                expected,
                "compatibility output for {path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn typed_execution_marks_signal_terminated_command_as_error_without_changing_output() {
        let dir = tempdir().unwrap();
        let args = r#"{"command":"kill -TERM $$"}"#;

        let typed = try_execute_tool_in_workspace("run_command", args, dir.path());
        let error = typed.expect_err("signal termination must fail");
        assert!(error.starts_with("Exit Status: signal:"), "{error}");
        assert_eq!(
            execute_tool_in_workspace("run_command", args, dir.path()),
            error
        );
    }

    #[test]
    fn test_read_file_auto_resolves_unique_workspace_suffix() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("crates").join("my-crate").join("src");
        fs::create_dir_all(&sub).unwrap();
        let file_path = sub.join("view.rs");
        fs::write(&file_path, "pub fn render() {}\n").unwrap();

        // Reading with partial path "src/view.rs"
        let args = json!({ "path": "src/view.rs" }).to_string();
        let result = try_execute_tool_in_workspace("read_file", &args, dir.path()).unwrap();
        assert!(result
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(result.contains("pub fn render()"));

        // Reading with bare filename "view.rs"
        let args_bare = json!({ "path": "view.rs" }).to_string();
        let result_bare =
            try_execute_tool_in_workspace("read_file", &args_bare, dir.path()).unwrap();
        assert!(result_bare
            .contains("[Notice: Auto-resolved 'view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(result_bare.contains("pub fn render()"));
    }

    #[test]
    fn test_read_file_errors_with_suggestions_when_path_is_ambiguous() {
        let dir = tempdir().unwrap();
        let sub1 = dir.path().join("crate_a").join("src");
        let sub2 = dir.path().join("crate_b").join("src");
        fs::create_dir_all(&sub1).unwrap();
        fs::create_dir_all(&sub2).unwrap();
        fs::write(sub1.join("lib.rs"), "pub mod a;\n").unwrap();
        fs::write(sub2.join("lib.rs"), "pub mod b;\n").unwrap();

        let args = json!({ "path": "lib.rs" }).to_string();
        let err = try_execute_tool_in_workspace("read_file", &args, dir.path()).unwrap_err();
        assert!(err.contains("File 'lib.rs' not found. Did you mean one of:"));
        assert!(err.contains("crate_a/src/lib.rs"));
        assert!(err.contains("crate_b/src/lib.rs"));
    }

    #[test]
    fn test_run_command_dyn_cli() {
        let dir = tempdir().unwrap();
        // 1. "dyn" list
        let args_list = json!({ "command": "dyn" }).to_string();
        let out_list =
            try_execute_tool_in_workspace("run_command", &args_list, dir.path()).unwrap();
        assert!(out_list.contains("Threadlane In-Process Tool Runner (dyn)"));
        assert!(out_list.contains("manage_memory"));

        // 2. "dyn --help manage_memory"
        let args_help = json!({ "command": "dyn manage_memory --help" }).to_string();
        let out_help =
            try_execute_tool_in_workspace("run_command", &args_help, dir.path()).unwrap();
        assert!(out_help.contains("Tool Schema for 'manage_memory'"));

        // 3. JSON arguments preserve the tool schema's types and quoting.
        let args_run = json!({ "command": "dyn manage_memory {\"action\":\"read\"}" }).to_string();
        let out_run = try_execute_tool_in_workspace("run_command", &args_run, dir.path()).unwrap();
        assert!(out_run.contains("No persistent memory found"));

        let flags = json!({ "command": "dyn manage_memory --action read" }).to_string();
        assert!(
            try_execute_tool_in_workspace("run_command", &flags, dir.path())
                .unwrap_err()
                .contains("JSON arguments")
        );

        let escaped_cwd = json!({ "command": "dyn", "cwd": "../outside" }).to_string();
        assert!(try_execute_tool_in_workspace("run_command", &escaped_cwd, dir.path()).is_err());
    }

    #[test]
    fn test_edit_file_hashline_auto_resolves_unique_workspace_suffix() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("crates").join("my-crate").join("src");
        fs::create_dir_all(&sub).unwrap();
        let file_path = sub.join("view.rs");
        fs::write(&file_path, "hello world\n").unwrap();

        // 1. read_file with shorthand path
        let read_args = json!({ "path": "src/view.rs" }).to_string();
        let read_res = try_execute_tool_in_workspace("read_file", &read_args, dir.path()).unwrap();
        assert!(read_res
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));

        // Extract hash from line 1
        let hash = read_res
            .lines()
            .find(|l| l.starts_with("1:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|s| s.split('|').next())
            .unwrap();

        // 2. edit_file_hashline with the same shorthand path
        let edit_args = json!({
            "path": "src/view.rs",
            "edits": [{
                "start_anchor": format!("1:{hash}"),
                "action": "replace",
                "new_content": "hello threadlane"
            }]
        })
        .to_string();

        let edit_res =
            try_execute_tool_in_workspace("edit_file_hashline", &edit_args, dir.path()).unwrap();
        assert!(edit_res
            .contains("[Notice: Auto-resolved 'src/view.rs' to 'crates/my-crate/src/view.rs']"));
        assert!(edit_res.contains("Successfully applied 1 hashline edit(s)"));

        let updated = fs::read_to_string(&file_path).unwrap();
        assert_eq!(updated, "hello threadlane\n");
    }
