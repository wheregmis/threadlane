//! Canned JavaScript for the agent browser tools.
//!
//! Pure string builders: the scripts run inside the panel `WKWebView` via
//! `evaluate_script_with_callback`, which JSON-serializes the return value.
//! Every script returns a JSON string; the pump parses one layer before
//! replying so tool results stay plain text.

/// Maximum interactive elements per snapshot. Bounds both JS work and result size.
pub(crate) const SNAPSHOT_MAX_ELEMENTS: usize = 200;

/// Outline drawn around the acted-on element inside the page. This annotates
/// third-party page content (audited exception to the token rule): it must
/// stay legible on arbitrary websites independent of the app theme, so it is
/// a fixed high-visibility blue defined once here, not a theme token.
const ACT_HIGHLIGHT_OUTLINE: &str = "3px solid #3b82f6";

/// Compact interactive-element tree: links, buttons, inputs, plus headings
/// for orientation. Elements are stamped with `data-tlane-ref` so a later
/// `browser_act` can address them by ref.
pub(crate) fn snapshot_js() -> String {
    SNAPSHOT_TEMPLATE.replace("__MAX__", &SNAPSHOT_MAX_ELEMENTS.to_string())
}

const SNAPSHOT_TEMPLATE: &str = r#"(() => {
  const MAX = __MAX__;
  const out = [];
  const seen = new Set();
  const sel = 'a[href],button,input,select,textarea,[role=button],[role=link],[role=textbox],[role=checkbox],[role=radio],[role=switch],[role=tab],[role=menuitem],[contenteditable="true"],h1,h2,h3';
  let i = 0;
  for (const el of document.querySelectorAll(sel)) {
    if (i >= MAX || seen.has(el)) continue;
    seen.add(el);
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none') continue;
    el.setAttribute('data-tlane-ref', String(i));
    const name = (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || el.getAttribute('title') || '').trim().replace(/\s+/g, ' ').slice(0, 120);
    out.push({ ref: i, tag: el.tagName.toLowerCase(), type: el.type || '', name, x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) });
    i++;
  }
  return JSON.stringify({ url: location.href, title: document.title, count: i, elements: out });
})()"#;

/// Build the act script. Target is located by stamped ref first, then CSS
/// selector. Returns a JSON string shaped `{ok, message}`.
pub(crate) fn act_script(action: &str, target_json: &str, text_json: &str, key_json: &str) -> String {
    format!(
        r#"(() => {{
  const fail = (message) => JSON.stringify({{ ok: false, message }});
  const done = (message) => JSON.stringify({{ ok: true, message }});
  const target = {target_json};
  let el = null;
  if (target.ref !== null && target.ref !== undefined) {{
    el = document.querySelector('[data-tlane-ref="' + target.ref + '"]');
    if (!el) return fail('no element with ref ' + target.ref + '; take a fresh browser_snapshot, refs expire on re-render');
  }} else if (target.selector) {{
    try {{
      el = document.querySelector(target.selector);
    }} catch (e) {{
      return fail('bad selector: ' + e.message);
    }}
    if (!el) return fail('selector matched nothing: ' + target.selector);
  }} else {{
    return fail('act needs ref or selector');
  }}
  const action = {action_json};
  const fire = (type, opts) => el.dispatchEvent(new Event(type, Object.assign({{ bubbles: true, cancelable: true }}, opts || {{}})));
  if (typeof el.scrollIntoView === 'function') el.scrollIntoView({{ block: 'center' }});
  try {{
    const prevOutline = el.style.outline;
    const prevTransition = el.style.transition;
    el.style.transition = 'outline 0.15s ease-in-out';
    el.style.outline = '{highlight}';
    setTimeout(() => {{
      try {{
        el.style.outline = prevOutline;
        el.style.transition = prevTransition;
      }} catch (_) {{}}
    }}, 800);
  }} catch (_) {{}}
  if (action === 'click') {{
    el.click();
    return done('clicked <' + el.tagName.toLowerCase() + '>');
  }}
  if (action === 'focus') {{
    el.focus();
    return done('focused <' + el.tagName.toLowerCase() + '>');
  }}
  if (action === 'type') {{
    const text = {text_json};
    if ('value' in el) {{
      el.focus();
      el.value = text;
      fire('input');
      fire('change');
    }} else if (el.isContentEditable) {{
      el.focus();
      document.execCommand('insertText', false, text);
      fire('input');
    }} else {{
      return fail('element is not typeable: <' + el.tagName.toLowerCase() + '>');
    }}
    return done('typed ' + text.length + ' chars');
  }}
  if (action === 'press') {{
    const key = {key_json};
    const init = {{ key, code: key, bubbles: true, cancelable: true }};
    el.dispatchEvent(new KeyboardEvent('keydown', init));
    el.dispatchEvent(new KeyboardEvent('keypress', init));
    if (key === 'Enter' && el.tagName === 'INPUT') {{
      const form = el.form || (el.closest && el.closest('form'));
      if (form && form.requestSubmit) form.requestSubmit();
      else fire('change');
    }}
    el.dispatchEvent(new KeyboardEvent('keyup', init));
    return done('pressed ' + key);
  }}
  if (action === 'select') {{
    const text = {text_json};
    if (el.tagName !== 'SELECT') return fail('select needs a <select> element');
    const opt = Array.from(el.options).find((o) => o.text.trim() === text || o.value === text);
    if (!opt) return fail('no option matching ' + text);
    el.value = opt.value;
    fire('input');
    fire('change');
    return done('selected ' + opt.text.trim());
  }}
  return fail('unknown action: ' + action);
}})()"#,
        action_json = action_json(action),
        highlight = ACT_HIGHLIGHT_OUTLINE,
    )
}

fn action_json(action: &str) -> String {
    serde_json::to_string(action).unwrap_or_else(|_| "\"\"".into())
}

/// Wrap an agent expression so the result arrives JSON-serialized when
/// possible, plain-stringified otherwise. Result length is capped in Rust.
pub(crate) fn evaluate_script_wrap(expression: &str) -> String {
    format!(
        r#"(() => {{ let r; try {{ r = eval({expr}); }} catch (e) {{ return JSON.stringify({{ ok: false, message: 'eval threw: ' + (e && e.message || e) }}); }} try {{ const j = JSON.stringify(r); return JSON.stringify({{ ok: true, result: j === undefined ? String(r) : j }}); }} catch (e) {{ return JSON.stringify({{ ok: true, result: String(r) }}); }} }})()"#,
        expr = serde_json::to_string(expression).unwrap_or_else(|_| "\"\"".into()),
    )
}

/// Unwrap one JSON layer from wry's callback payload: wry serializes the
/// script's string return value, so a script returning JSON arrives
/// double-encoded. Falls back to the raw payload.
pub(crate) fn unwrap_callback_payload(payload: &str) -> String {
    if let Ok(inner) = serde_json::from_str::<String>(payload) {
        inner
    } else {
        payload.to_string()
    }
}

/// Script injected into every page at document start to trap console errors,
/// warnings, and uncaught exceptions into an in-memory ring buffer.
pub(crate) fn console_interceptor_js() -> String {
    r#"(() => {
  if (window.__threadlane_logs_installed) return;
  window.__threadlane_logs_installed = true;
  window.__threadlane_logs = [];
  const MAX = 100;
  function push(level, message, source, line, col, stack) {
    if (window.__threadlane_logs.length >= MAX) window.__threadlane_logs.shift();
    window.__threadlane_logs.push({
      level,
      message: String(message || '').slice(0, 500),
      source: source ? String(source).slice(0, 200) : null,
      line: line || null,
      col: col || null,
      stack: stack ? String(stack).slice(0, 500) : null,
      ts: Date.now()
    });
  }
  const origErr = console.error;
  console.error = function(...args) {
    try {
      push('error', args.map(a => typeof a === 'object' ? JSON.stringify(a) : String(a)).join(' '));
    } catch (_) {}
    return origErr.apply(this, args);
  };
  const origWarn = console.warn;
  console.warn = function(...args) {
    try {
      push('warn', args.map(a => typeof a === 'object' ? JSON.stringify(a) : String(a)).join(' '));
    } catch (_) {}
    return origWarn.apply(this, args);
  };
  window.addEventListener('error', (e) => {
    try {
      push('error', e.message || 'Uncaught error', e.filename, e.lineno, e.colno, e.error && e.error.stack);
    } catch (_) {}
  });
  window.addEventListener('unhandledrejection', (e) => {
    try {
      const r = e.reason;
      const msg = r && (r.message || r.stack) ? (r.message || String(r)) : String(r);
      push('error', 'Unhandled rejection: ' + msg, null, null, null, r && r.stack);
    } catch (_) {}
  });
})()"#.to_string()
}

/// Script to extract captured console logs and optionally clear them.
pub(crate) fn drain_console_logs_js(clear: bool, level: &str) -> String {
    let level_json = serde_json::to_string(level).unwrap_or_else(|_| "\"all\"".into());
    format!(
        r#"(() => {{
  const logs = window.__threadlane_logs || [];
  const level = {level_json};
  const filtered = logs.filter(l => level === 'all' || l.level === level);
  if ({clear}) {{
    if (level === 'all') {{
      window.__threadlane_logs = [];
    }} else {{
      window.__threadlane_logs = logs.filter(l => l.level !== level);
    }}
  }}
  return JSON.stringify({{ count: filtered.length, logs: filtered }});
}})()"#
    )
}

/// Script to check if document condition (selector, text, readyState) is met.
pub(crate) fn wait_check_js(selector: Option<&str>, text: Option<&str>) -> String {
    let sel_json = serde_json::to_string(&selector).unwrap_or_else(|_| "null".into());
    let txt_json = serde_json::to_string(&text).unwrap_or_else(|_| "null".into());
    format!(
        r#"(() => {{
  const readyState = document.readyState;
  let selectorFound = null;
  const sel = {sel_json};
  if (sel) {{
    try {{
      selectorFound = !!document.querySelector(sel);
    }} catch (e) {{
      return JSON.stringify({{ ok: false, error: 'Invalid selector: ' + e.message }});
    }}
  }}
  let textFound = null;
  const txt = {txt_json};
  if (txt) {{
    textFound = (document.body ? document.body.innerText : '').includes(txt);
  }}
  return JSON.stringify({{
    ok: true,
    readyState,
    selectorFound,
    textFound
  }});
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_script_embeds_args_as_json() {
        let script = act_script(
            "type",
            r#"{"ref":3,"selector":null}"#,
            r#""hello""#,
            r#"null"#,
        );
        assert!(script.contains(r#""type""#));
        assert!(script.contains(r#"{"ref":3,"selector":null}"#));
    }

    #[test]
    fn unwrap_double_encoded_payload() {
        let inner = r#"{"ok":true}"#;
        let outer = serde_json::to_string(inner).unwrap();
        assert_eq!(unwrap_callback_payload(&outer), inner);
    }

    #[test]
    fn unwrap_plain_passthrough() {
        assert_eq!(unwrap_callback_payload("null"), "null");
    }

    #[test]
    fn snapshot_bakes_in_element_cap() {
        let script = snapshot_js();
        assert!(!script.contains("__MAX__"));
        assert!(script.contains(&format!(
            "const MAX = {};",
            SNAPSHOT_MAX_ELEMENTS
        )));
    }

    #[test]
    fn evaluate_wrap_is_an_iife() {
        let wrapped = evaluate_script_wrap("document.title");
        assert!(wrapped.starts_with("(() =>"));
        assert!(wrapped.contains("\"document.title\""));
    }

    #[test]
    fn console_interceptor_defines_window_logs() {
        let script = console_interceptor_js();
        assert!(script.contains("__threadlane_logs"));
        assert!(script.contains("console.error"));
        assert!(script.contains("unhandledrejection"));
    }

    #[test]
    fn drain_console_logs_embeds_options() {
        let script = drain_console_logs_js(true, "error");
        assert!(script.contains(r#"const level = "error";"#));
        assert!(script.contains("if (true)"));
    }

    #[test]
    fn wait_check_embeds_selectors() {
        let script = wait_check_js(Some(".submit-btn"), Some("Submit"));
        assert!(script.contains(r#"".submit-btn""#));
        assert!(script.contains(r#""Submit""#));
        assert!(script.contains("document.readyState"));
    }
}
