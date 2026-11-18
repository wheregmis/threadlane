# Chat image attachments

## Legacy Threadlane

The desktop chat in `old/crates/threadlane-ui-chat/src/view.rs` intercepted Paste, inserted any clipboard text, and staged clipboard images as `ImageAttachment { display_name, data_url }`. The data URL held a MIME type and base64 bytes. The draft followed its session, accepted image-only sends, and exposed removable attachment chips. Clicking a chip opened a preview with fit and actual-size modes. Preview decoding ran asynchronously and left the staged bytes intact on failure. The separate saved-draft feature accepted text only.

`old/crates/threadlane-protocol/src/daemon.rs` carried a defaulted image vector on `SessionCommand::SubmitPrompt` and `SteerMessage`. The client forwarded the typed commands to the daemon. Worktree setup, queued input, and retry records also carried images. The daemon's `core.rs` and `chat.rs` passed the vector with the prompt to the session runtime. The harness stored `AgentMessage::UserWithImages` or a text-only user message in its append-only session journal. The daemon projected the transcript back to the client, and the chat used the stored image payloads to display sent messages and reconstruct retries.

The provider converter in `old/crates/threadlane-provider/src/convert.rs` produced provider-native image blocks, including OpenAI Chat Completions `image_url`, Responses `input_image`, and Anthropic base64 sources. The ACP engine's `prompt_blocks` used the agent's image capability to either produce image blocks or name attachments in the text when it could not send them.

## Current Threadlane

Home and the space's Main Chat share the General lead draft. Use **Attach images…** to choose files or paste an image into the composer. A draft can carry up to eight PNG, JPEG, WebP, or GIF files totaling 2 MiB. It can send images without a caption. Select a staged thumbnail to preview its first frame in **Fit** or **Actual size** mode. Use **Remove** to take it out of the draft. Project History displays attachments from that project's lead conversation. The space Lead remains the desktop entry point for new chat, and the client API can still address a named project lead directly. Task messages keep their existing text and decision behavior.

Local sockets and iroh carry base64 image bytes on the image-send request. The hub validates them and stores them in the same event as the caption. Image metadata and bytes are optional in stored events and conversation replies, so existing text records continue to load. Home's merged timeline and project history read the same events. The conversation page is capped at 20 entries to accommodate full image messages within the remote response limit.

When the lead runs, the hub names the attachments in the prompt and supplies their bytes to the runner. Claude Code receives stream-JSON image blocks. Codex receives `--image` paths in a temporary directory held for the run. Image messages arriving during a run remain pending for the next turn so the text-only follow-up path cannot consume them. An image supplied to one lead goes to that lead's session. Lead-to-lead handoffs carry the instructions or observations the lead includes in its text.

## Baseline

T-0016 started at `a3027522` on `origin/v2`. The task branch was fast-forwarded locally to current `origin/main` at `92feeb97`, which already includes T-0012 through PR #509 (`1cf7a2eb`, integration `0d5481ae`, task tip `4871344c`) and the later Main Chat and project History changes. The attachment changes are based on that merged implementation.
