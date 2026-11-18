# Switch session

Choose **Go to Task…** in the command palette, **View → Switch Session…** on
macOS, or press **Cmd+Shift+K** (macOS) / **Ctrl+Shift+K** (other platforms).
Press Enter to return to the previous available session. Arrow keys or a click
choose another session; Escape dismisses without switching, even with a query.

**Recently visited** tracks actual session visits, not agent activity. It keeps
at most five identities, including the current session, for this window and
daemon client only. The current session is omitted from the picker. Remaining
sessions appear under **Other sessions**, independent of the sidebar filter.
Search matches title, project, ID and branch. Clearing it restores the frozen
invocation order. Nothing is persisted, scanned or preloaded for this list.

Confirmation rechecks the project, ID and session-file identity against current
metadata, then uses normal session selection and opens Chat without submitting.
Removed targets report an error rather than selecting another session. Offline
or reconnecting metadata cannot be confirmed; reopen after the connection and
project metadata recover. Restarting the window starts a fresh visit history.

## Validation

Automated coverage includes bounded/deduplicated visit order, previous-session
selection, stable metadata updates, draft/Settings exclusion, daemon replacement,
remote metadata readiness, identity ambiguity/removal/detach, query filtering,
and mounted Enter/Escape/stale-target interactions under the kit Root.

Before release, manually exercise the source-built desktop with terminal focus,
a streaming third agent, drafts and images, remote disconnect/reconnect,
VoiceOver, light/dark themes, large fonts and narrow windows. Automated tests
are not evidence that these real-window checks have been completed.
