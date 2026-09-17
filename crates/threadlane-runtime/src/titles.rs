pub fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    loop {
        let before = title.clone();
        if title
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("title:"))
        {
            title = title[6..].trim().to_string();
        }
        let quoted = ((title.starts_with('"') && title.ends_with('"'))
            || (title.starts_with('\'') && title.ends_with('\'')))
            && title.len() >= 2;
        if quoted {
            title = title[1..title.len() - 1].trim().to_string();
        }
        if title == before {
            break;
        }
    }
    let mut collapsed = String::with_capacity(title.len());
    let mut previous_was_space = true;
    for character in title.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
                previous_was_space = true;
            }
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }
    collapsed.chars().take(42).collect()
}

pub fn extract_session_title(
    store: &impl crate::harness::SessionStore,
    fallback_id: &str,
) -> String {
    if let Some(name) = store.name() {
        if !name.trim().is_empty() {
            return name;
        }
    }
    let messages = {
        let active = store.active_branch_messages("main");
        if active.is_empty() {
            store.get_persisted_messages()
        } else {
            active
        }
    };
    for message in messages {
        let content = match message {
            threadlane_protocol::AgentMessage::User { content }
            | threadlane_protocol::AgentMessage::UserWithImages { content, .. } => content,
            _ => continue,
        };
        let first_line = content.trim().lines().next().unwrap_or("");
        if !first_line.is_empty() {
            return first_line.chars().take(40).collect::<String>()
                + if first_line.chars().count() > 40 {
                    "…"
                } else {
                    ""
                };
        }
    }
    fallback_id.to_string()
}
