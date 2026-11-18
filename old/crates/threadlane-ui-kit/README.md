# Threadlane UI Kit

Native agent components built on GPUI Kit. The desktop chat, sidebar, and iOS
client consume this crate; it replaces the former `threadlane-ui-session` crate.
Theme selection, tokens, and assets remain in `threadlane-ui-theme`.

The [Agents Kit component collection](https://agents-ui.github.io/agents-kit/components)
is the design reference: compact disclosures, explicit tool states, calm message
surfaces, and decisions that keep their scope visible. This toolkit contains
the components Threadlane uses, rather than importing the entire collection.

| Component family | API | Consumer |
| --- | --- | --- |
| Conversation | `message_row`, `user_message_bubble`, `transcript_list` | Desktop and iOS chat |
| Conversation chrome | `conversation_transcript_viewport`, `last_run_duration`, `message_actions`, `message_copy_button`, `message_edit_button`, `message_context_menu` | Desktop and native/WASM saved-session preview; hosts own scrolling, clipboard and draft guards |
| Conversation code | `code_block_surface`, `code_block_header`, `code_block_body`, `code_block_actions`, Run/Open/Copy buttons | Desktop and native/WASM captured-session preview; hosts retain terminal confirmation and file guards |
| Prompt navigation | `prompt_navigation_rail`, `prompt_rail_tick`, `active_prompt_landmark`, `conversation_outline_popover`, `conversation_outline_content`, `conversation_outline_row` | Desktop and native/WASM saved-session preview; canonical prompt identity, virtual lists and host keyboard navigation |
| Composer | `composer_surface`, `composer_input` | Desktop and iOS chat |
| Reasoning | `reasoning_card`, `reasoning_detail` | Desktop and iOS chat |
| Tool result | `tool_activity`, `tool_detail` | Desktop and iOS chat |
| Completed activity | `completed_activity_group`, `disclosure_button` | Desktop chat and gallery |
| Command, file, search, and diff output | `result_surface`, `result_header`, `result_viewport` | Desktop tool previews |
| Permission | `permission_card` | Desktop and iOS decisions |
| Question | `question_surface`, `question_item` | Desktop and iOS decisions |
| Conversation find | `conversation_find_button`, `ConversationFindStrip`, `conversation_find_status`, `conversation_transcript_row`, scoped find actions | Desktop and native/WASM captured-session preview; hosts retain scanning, selection and focus |
| Workspace palette | `workspace_commands`, `workspace_palette_command`, `workspace_palette_frame`, `palette_item`, `palette_conversation_item`, `palette_scope`, `palette_empty`, `palette_footer` | Desktop commands/conversation search and native/WASM saved-session preview |
| Settings destinations | `SETTINGS_SEARCH_ITEMS`, `settings_search_page` | Desktop and native/WASM palette; service refresh stays in settings host |
| Workspace | `WorkspaceLayout`, `workspace_sidebar_split`, `workspace_right_panel_split`, `workspace_right_panel_focus`, `workspace_sidebar_toggle`, `restore_workspace_panel_sizes`, `conversation_surface`, `chat_header_surface` | Desktop and WASM workspace |
| Right-panel chrome | `RightPanelSurface`, `right_panel_header`, `right_panel_refresh_button`, `right_panel_chooser` | Desktop right panel and native/WASM Agents preview |
| Browser panel | `browser_chrome`, `browser_viewport`, `browser_tab_title`, `BrowserAction` | Desktop webview host and native/WASM local browser preview |
| Files and documents | `project_file_tree`, `project_file_row`, `project_file_menu`, `panel_document_header`, `review_whitespace_control` | Desktop Files/Review and native/WASM file tree and Components gallery |
| Working-tree review | `review_workspace_context`, `review_branch_header`, `review_tabs`, `review_filters`, `review_selection_bar`, `review_file_row`, `review_commit_footer`, `review_diff_body` | Desktop Review and native/WASM saved-checkout preview |
| Review actions | `review_pr_card`, `review_file_menu`, `review_discard_menu`, `ReviewDiscardTarget` | Desktop Review and native/WASM saved-checkout preview; labeled PR states in Components |
| Branch management and Git forms | `review_branch_manager`, `review_branch_section`, `review_new_branch_form`, `review_merge_form`, `review_switch_form`, `review_stash_form`, `review_git_dialog`, `review_delete_branch_alert` | Desktop Review and native/WASM saved-checkout preview |
| Draft pull request | `review_draft_pr_form`, `review_draft_pr_dialog`, `ReviewDraftPrFields`, `review_draft_pr_prefill`, `form_field` | Desktop and native/WASM preview |
| Checkout history and stash | `review_history_surface`, `review_commit_card`, `review_commit_file`, `review_stash_card`, `review_stash_file` | Desktop Review and native/WASM saved-checkout preview |
| Selectable diffs | `diff_text_view` | Review, PR file review and editor diffs; one portable syntax-theme highlighter on desktop and WASM |
| Sidebar | `sidebar_surface`, `sidebar_header`, `sidebar_navigation`, `session_group_header` | Desktop and WASM sidebar |
| Sidebar project filter | `sidebar_project_filter`, `sidebar_project_menu`, `sidebar_project_filter_item` | Desktop and native/WASM sidebar |
| Sidebar session commands | `sidebar_session_menu`, `SidebarSessionAction`, `SidebarSessionMenuState`, `SidebarSnoozeMenu`, `session_pin_button`, `session_archive_button`, `session_actions_button` | Desktop and native/WASM saved-session sidebar |
| Complete sidebar rows | `sidebar_session_card`, `SidebarSessionCardState`, `SidebarSnoozeStatus`, `session_history_card_row`, `session_history_group`, `sidebar_snoozed_header`, `sidebar_history_empty` | Desktop and native/WASM saved-session sidebar; labeled state samples in Components |
| Session confirmations | `sidebar_session_removal_dialog`, `SidebarSessionRemoval`, `SidebarSessionRemovalTarget` | Desktop archive/remove flow and local native/WASM preview confirmations |
| Composer controls | `composer_model_button`, `composer_mode_button`, `composer_effort_button`, `composer_send_button` | Desktop and WASM composer |
| Composer context | `composer_context_bar`, `composer_project_button`, `composer_work_mode_button`, `composer_skills_button` | Desktop and WASM composer |
| Composer queue | `queued_message_panel`, `queued_message_row`, `queued_steer_button`, `queued_edit_button`, `queued_remove_button` | Desktop and native/WASM gallery |
| Pending follow-up | `pending_message_row`, `pending_queue_button`, `pending_steer_button`, `pending_edit_button` | Desktop and native/WASM gallery |
| Staged images | `staged_image_chip`, `composer_attachment_group`, `image_preview_dialog`, `image_preview_content` | Desktop and native/WASM gallery |
| Saved drafts | `saved_draft_banner`, `save_draft_button`, `discard_saved_draft_dialog` | Desktop and native/WASM gallery |
| Prompt recall | `prompt_recall_strip`, `recall_prompt_button`, `recall_older_button`, `recall_newer_button`, scoped `RecallOlderPrompt` / `RecallNewerPrompt` actions | Desktop and native/WASM saved-session workspace and gallery |
| Model picker | `composer_model_picker`, `PickerItem`, `PickerSection`, `PickerDelegate` | Desktop and native/WASM workspace |
| Project picker | `project_picker_menu`, `project_picker_item`, `new_task_project_button` | Desktop new task/composer and native/WASM workspace |
| Editor | `editor_surface`, `editor_tab_bar`, `editor_tabs`, `editor_tab`, `editor_tab_title`, `editor_save_button`, `editor_actions`, `editor_buffer`, `editor_diff`, `editor_empty_state` | Desktop and native/WASM editor |
| Trajectory panel | `TrajectoryView`, `TrajectorySource`, `TrajectoryMode`, `TrajectoryInspectorTab` | Desktop live projections and native/WASM imported sessions; same virtual list, filters and inspector |
| Trajectory atoms | `trajectory_event_row`, `trajectory_section_header`, `trajectory_overview`, `trajectory_stats`, `trajectory_entry_preview`, `TrajectorySummary` | Desktop Trajectory and native/WASM saved-event gallery |
| Agent worktrees | `AgentWorktreeAction`, `agent_worktree_action_enabled`, `agent_worktree_controls`, `agent_worktree_discard_dialog` | Desktop and native/WASM preview; canonical isolation metadata, shared states, wrapping buttons and guarded host callbacks |
| Agents | `agent_panel_surface`, `agent_profile_tabs`, `agent_profile_button`, `agent_detail_header`, `agent_main_summary`, `agent_activity_message`, `agent_tool_activity`, `agent_empty_state` | Desktop Agents panel and native/WASM preview; shared chat tool disclosures and output cards |
| Terminal output | `TerminalOutputMenu`, `TerminalOutputAction`, `terminal_output_surface`, `terminal_status`, `terminal_live_output` | Desktop terminal; native/WASM output menu, surface and status fixtures |
| Terminal grid | `TerminalGrid`, `TerminalTextMetrics`, `TerminalGridGeometry`, `terminal_selected_excerpt`, `terminal_selection_bounds` | Desktop terminal and native/WASM preview |
| Terminal chrome | `TerminalTab`, `terminal_tab_menu`, `TerminalToolbar`, `terminal_surface`, `terminal_unavailable`, `workspace_terminal_split`, `TerminalFindStrip`, `TerminalFindStatus`, `TerminalLinkPicker`, `terminal_link_commands`, `terminal_link_overlay` | Desktop workspace and native/WASM preview |
| Automations | `automation_screen`, `automation_definition_row`, `automation_detail`, `automation_run_row`, `automation_picker` | Desktop and native/WASM preview |
| Automation editor | `automation_form`, `form_field`, `automation_editor_sheet` | Desktop and native/WASM preview |
| GitHub collections | `github_toolbar`, `github_list_row`, `github_master_detail` | Desktop and native/WASM preview |
| GitHub details | `GitHubDetailHeader`, `github_issue_body`, `github_linked_task`, `github_comment`, `github_pr_summary`, `github_pr_tabs` | Desktop and native/WASM preview |
| GitHub discussion and commits | `github_conversation_row`, `github_pr_conversation`, `GitHubCommentEditor`, `github_draft_action`, `github_pr_commit_row`, `github_pr_commits` | Desktop and native/WASM preview |
| PR file review | `github_pr_file_row`, `github_pr_file_list`, `GitHubPrFilesControls`, `github_pr_diff`, `github_pr_files` | Desktop and native/WASM preview |
| Issue creation | `github_issue_create_form`, `github_issue_create_dialog`, `validate_issue_title` | Desktop and native/WASM preview |
| Issue task and deletion | `GitHubIssueStartForm`, `github_issue_start_form`, `github_issue_start_dialog`, `github_issue_delete_dialog` | Desktop and native/WASM preview |
| Settings | `settings_screen`, `settings_navigation`, `settings_navigation_button`, `settings_group`, `settings_general`, `settings_appearance`, `settings_theme_card`, `settings_shortcuts`, `settings_shortcut_row`, `settings_agent`, `settings_catalog`, `settings_catalog_row`, `settings_scope_picker` | Desktop and native/WASM preview |
| Plan | `plan_tracker` | Desktop and WASM conversation |
| Environment | `environment_panel`, `environment_git_menu`, `environment_fits` | Desktop and WASM workspace |
| Token accounting | `token_efficiency`, `context_meter` | Desktop and WASM workspace |
| Session navigation | `session_card`, `session_list`, `session_identity`, `session_attention` | Desktop sidebar and iOS |
| Markdown | `markdown` | Desktop and iOS transcript bodies |
| Transcript behavior | `transcript` | Desktop and iOS scrolling |

## Composition contract

- Workspace hosts retain visibility, resize preferences and selected entities. The kit computes one responsive layout for sidebar availability, compact inspector focus, header clearance and Environment space. Both hosts restore visible splits after measurement; resizing and replacing a split never recreates the selected panel, draft or transcript.
- Agent worktree hosts own daemon/Git commands and revalidate session, lane, isolation, checkout and daemon identity before acting or confirming Discard. The kit owns controls, availability/status presentation, full accessible descriptions and confirmation copy. Imports capture branch diffs and worktree availability read-only; preview actions remain local.
- Palette hosts retain `CommandState`, prior focus, recent commands, session routing and search workers. The kit owns the catalogue, frame and row slots; long content truncates within a bounded width, and disabled commands carry a visible reason. The preview searches only its immutable captured transcript and never scans or writes the original journal.
- Conversation hosts supply canonical transcript lists and prompt landmarks. The kit shares rail, outline, transcript inset, scrollbar, latest-message control, duration and message actions. Hosts validate message identity before jumping, retain draft/selection state, guard modal interactions and own clipboard feedback. Recall bindings use the same scoped action types; hosts arm them only for eligible drafts and validate caret boundaries. Preview editing only loads its local composer and never changes the captured messages.
- Hosts own selections, input entities, expansion, request IDs, and domain actions.
- Code blocks share parsing, cache invalidation, syntax rendering, responsive headers and copy controls. Desktop hosts validate file paths and shell availability, confirm multiline commands and dispatch services. Captured previews retain shell availability as metadata; Run/Open show local notices. Streaming blocks defer their actions.
- Hosts pass current text-control focus to `composer_surface` and notify their
  view on focus/blur events. The shared frame shows focus without shifting layout.
- Components receive controlled state and callbacks; they never execute tools,
  resolve permissions, write preferences, or discover sessions.
- Tool results start as one row. Hosts create rich output when expanded or briefly exiting.
  The state remains visible and appears in the accessible control name.
- `DisclosureMotion` shares reversible reveal/fade and chevron motion. It uses
  theme motion tokens and respects reduced motion. Virtual transcript hosts call
  `remeasure_list_row` so animated height changes preserve the reading position.
- Result content scrolls inside its viewport without moving the conversation.
- Queue rows retain their text while removal awaits acknowledgement and hide their actions. Hosts own steering, draft restoration, and removal confirmation.
- Attachment hosts own bytes, safe decoding, preview lifetime, mode, and focus restoration. The kit owns chip appearance, the dialog shell with its Close footer, and the loading/error/Fit/Actual size preview layout.
- Draft hosts own persistence, restore eligibility, confirmation guards, prompt identity and keyboard navigation. The kit owns their banner, discard confirmation and recall controls; the recall status remains separate from the actionable Older/Newer buttons.
- Picker hosts capture catalog rows, refresh snapshots while closed, revalidate selections and apply domain actions. The kit owns grouped row rendering, token search, current checkmarks, empty-state recovery and popup appearance. Project hosts supply guarded selection and attach callbacks. Sidebar filter choices change only visible history, preserving the active chat and composer project.
- Sidebar menus emit typed commands and share compact/full scopes, disabled states, snooze return times, copy/export submenus and focus transfer. Hosts retain journal writes, title generation, fork/export services, and archive/remove confirmations. Snooze duration choices and formatted return times come from the host; deadlines are resolved at activation by the existing controller.
- Complete sidebar cards share title/recency geometry, context, Git/PR/pin/result/snooze/attention signals, tooltips, accessible labels and command targets. Hosts supply facts and handlers. Recency overlays the actions' intrinsic slot so a hidden timestamp does not reduce title width; long snooze badges stay within the row with their full text retained in the tooltip and accessible label. History hosts retain filtering, durable snooze reconciliation, sort/cache invalidation and virtualization.
- Session confirmations name the session and distinguish archived transcripts from permanent removal. The kit controls presentation of the worktree checkbox; hosts own its value and guarded confirmation callback. No kit dialog performs filesystem operations.
- Editor hosts own buffers, language setup, file reads/writes, save status lifetime, and unsaved-close guards. The kit owns the editor surface, tab controls and menu presentation, save/dirty/diff states, and empty view. Tab IDs use file identity; selection and close controls are separate keyboard targets.
- Automation hosts own scope, selection, prompt disclosure, attention filters, pagination and service commands. The kit renders canonical definitions and runs, including disabled and empty states. The preview supplies labeled in-memory samples; it never schedules prompts. The form and sheet chrome are shared; hosts retain input entities, catalog discovery, validation and saving. Pure schedule-field helpers preserve custom days and produce the same next-run preview.
- Settings hosts own page selection, catalogs, authentication, preference writes and update actions. The kit owns navigation, page descriptors, layout, General, Appearance, Shortcuts, Agent & Fusion, skills and extension presentation. Theme cards and shortcut rows are reusable atomic components. Agent & Fusion uses a host-captured model/effort snapshot and typed change requests; the desktop keeps catalog discovery, persisted preferences, and the canonical session-mode controller. Preview controls change only local sample state; `preview_theme` applies colors to the preview process without saving the desktop preference.
- Completed activity groups preserve tool order; running, thinking, and failed
  rows remain visible while completed rows reveal or fold away.
- Touch targets can grow for iOS without changing the decision or state model.
- Permission buttons expose their scope and request title in their accessible
  names; request-derived IDs keep replacement cards from sharing retained state.
- New components enter the kit when a production consumer needs them. Keep
  provider parsing, file guards, and navigation in their existing feature crates.

## Component preview

The [example folder](examples/preview/README.md) runs a saved-session workspace
on native GPUI and GPUI Web. Its Components button opens the interactive gallery.
The workspace reuses the production sidebar, shell, conversation, composer, environment, plan and context disclosures, token accounting, and command/file/search/directory/diff renderers. The preview host owns only fixture
state and local callbacks. Shared typography comes from `threadlane-ui-theme`.

Native, from the repository root:

```sh
cargo run -p threadlane-ui-kit-preview
```

Web, from the example folder:

```sh
cd crates/threadlane-ui-kit/examples/preview
trunk serve
```

The web build uses the project's pinned GPUI Web backend and a preview-only
nightly toolchain for WASM atomics. Trunk watches the kit and shared theme source.

Skills and extension catalogs share controlled inventory rows and scope controls. Hosts supply scope-qualified identities, availability and status; typed requests delegate discovery, installs, removal and runtime invalidation to the existing desktop services. Web samples exercise the same controls in memory.
