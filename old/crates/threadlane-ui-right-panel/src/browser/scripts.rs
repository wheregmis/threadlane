//! Canned JavaScript for the agent browser tools.
//!
//! Pure string builders: the scripts run inside the panel `WKWebView` via
//! `evaluate_script_with_callback`, which JSON-serializes the return value.
//! Every script returns a JSON string; the pump parses one layer before
//! replying so tool results stay plain text.

/// Maximum interactive elements per snapshot. Bounds both JS work and result size.
pub const SNAPSHOT_MAX_ELEMENTS: usize = 200;

/// Outline drawn around the acted-on element inside the page. This annotates
/// third-party page content (audited exception to the token rule): it must
/// stay legible on arbitrary websites independent of the app theme, so it is
/// a fixed high-visibility blue defined once here, not a theme token.
const ACT_HIGHLIGHT_OUTLINE: &str = "3px solid #3b82f6";

/// Compact interactive-element tree: links, buttons, inputs, plus headings
/// for orientation. Elements are stamped with `data-tlane-ref` so a later
/// `browser_act` can address them by ref.
pub fn snapshot_js() -> String {
    SNAPSHOT_TEMPLATE.replace("__MAX__", &SNAPSHOT_MAX_ELEMENTS.to_string())
}

const SNAPSHOT_TEMPLATE: &str = r#"(() => {
  const MAX = __MAX__;
  const out = [];
  const seen = new Set();
  const sel = 'a[href],button,input,select,textarea,summary,label,[role=button],[role=link],[role=textbox],[role=searchbox],[role=checkbox],[role=radio],[role=switch],[role=tab],[role=menuitem],[role=combobox],[role=listbox],[role=option],[role=slider],[role=treeitem],[onclick],[contenteditable]:not([contenteditable="false"]),h1,h2,h3';
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
pub fn act_script(
    action: &str,
    target_json: &str,
    text_json: &str,
    key_json: &str,
) -> String {
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
pub fn evaluate_script_wrap(expression: &str) -> String {
    format!(
        r#"(() => {{ let r; try {{ r = eval({expr}); }} catch (e) {{ return JSON.stringify({{ ok: false, message: 'eval threw: ' + (e && e.message || e) }}); }} try {{ const j = JSON.stringify(r); return JSON.stringify({{ ok: true, result: j === undefined ? String(r) : j }}); }} catch (e) {{ return JSON.stringify({{ ok: true, result: String(r) }}); }} }})()"#,
        expr = serde_json::to_string(expression).unwrap_or_else(|_| "\"\"".into()),
    )
}

/// Unwrap one JSON layer from wry's callback payload: wry serializes the
/// script's string return value, so a script returning JSON arrives
/// double-encoded. Falls back to the raw payload.
pub fn unwrap_callback_payload(payload: &str) -> String {
    if let Ok(inner) = serde_json::from_str::<String>(payload) {
        inner
    } else {
        payload.to_string()
    }
}

/// Script injected into every page at document start to trap console errors,
/// warnings, and uncaught exceptions into an in-memory ring buffer.
pub fn console_interceptor_js() -> String {
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
pub fn drain_console_logs_js(clear: bool, level: &str) -> String {
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
pub fn wait_check_js(selector: Option<&str>, text: Option<&str>) -> String {
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
        assert!(script.contains(&format!("const MAX = {};", SNAPSHOT_MAX_ELEMENTS)));
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

/// Installs the annotate overlay: hovering paints a detached highlight box
/// over the element under the cursor (the page's own DOM is never touched),
/// clicking selects it, and an in-page comment card collects an optional note.
/// Shift-click adds more elements. Enter attaches, Escape cancels. The pick is
/// recorded to `window.__tlane_pick` and the host polls [`annotate_poll_js`].
pub fn annotate_install_js() -> String {
    ANNOTATE_INSTALL.to_string()
}

const ANNOTATE_INSTALL: &str = r##"(() => {
  if (window.__tlane_annotating) return "already";
  if (!document.body || !document.documentElement) return "retry";
  window.__tlane_annotating = true;
  window.__tlane_pick = null;

  const HOST_ATTR = "data-tlane-annotator";
  const Z = 2147483646;
  const HOVER_BORDER = "#f59e0b";
  const HOVER_FILL = "rgba(245, 158, 11, 0.10)";
  const PICK_BORDER = "#3b82f6";
  const PICK_FILL = "rgba(59, 130, 246, 0.16)";
  const COMMENT_MAX = 1000;
  const TEXT_MAX = 240;
  const NAME_MAX = 120;
  const HTML_MAX = 600;
  const SELECTOR_MAX = 512;

  const normalize = (value, max) =>
    (value || "").replace(/[\p{Cc}\p{Cf}]+/gu, " ").replace(/\s+/g, " ").trim().slice(0, max);

  const cssEscape = (value) =>
    (globalThis.CSS && CSS.escape)
      ? CSS.escape(value)
      : String(value).replace(/[^a-zA-Z0-9_-]/g, (c) => "\\" + c);

  // A generated-looking or secret-bearing id makes the selector useless or
  // leaks page state into the composer; fall back to the structural path.
  const looksSensitiveId = (value) =>
    /(?:^|[-_:])[a-z0-9_-]{24,}(?:$|[-_:])/i.test(value) ||
    /@/.test(value) ||
    /token|secret|session|password|passwd/i.test(value);

  const uniqueIdSelector = (el) => {
    if (!el || !el.id || looksSensitiveId(el.id)) return null;
    const byId = "#" + cssEscape(el.id);
    if (byId.length > SELECTOR_MAX) return null;
    try {
      return document.querySelectorAll(byId).length === 1 ? byId : null;
    } catch (_) {
      return null;
    }
  };

  // nth-of-type path anchored at the nearest uniquely-identified ancestor so
  // deep framework trees stay short enough to reuse.
  const uniqueSelector = (el) => {
    const byId = uniqueIdSelector(el);
    if (byId) return byId;
    const segments = [];
    let current = el;
    while (current && current !== document.documentElement) {
      const parent = current.parentElement;
      const tag = current.tagName.toLowerCase();
      if (!parent) {
        segments.unshift(tag);
        break;
      }
      const siblings = Array.from(parent.children).filter(
        (sibling) => sibling.tagName === current.tagName,
      );
      segments.unshift(tag + ":nth-of-type(" + (siblings.indexOf(current) + 1) + ")");
      const anchor = uniqueIdSelector(parent);
      if (anchor) {
        const anchored = [anchor, ...segments].join(" > ");
        if (anchored.length <= SELECTOR_MAX) return anchored;
      }
      current = parent;
    }
    segments.unshift("html");
    const selector = segments.join(" > ");
    return selector.length <= SELECTOR_MAX ? selector : null;
  };

  const implicitRole = (el) => {
    const tag = el.tagName;
    if (tag === "BUTTON") return "button";
    if (tag === "A" && el.hasAttribute("href")) return "link";
    if (tag === "TEXTAREA") return "textbox";
    if (tag === "SELECT") return el.multiple || el.size > 1 ? "listbox" : "combobox";
    if (tag === "INPUT") {
      const type = (el.type || "").toLowerCase();
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (["button", "submit", "reset", "image"].includes(type)) return "button";
      if (type === "range") return "slider";
      if (type === "number") return "spinbutton";
      if (type === "search") return "searchbox";
      if (type !== "hidden") return "textbox";
    }
    if (tag === "IMG") return "img";
    if (tag === "MAIN") return "main";
    if (tag === "NAV") return "navigation";
    if (tag === "FORM") return "form";
    if (tag === "TABLE") return "table";
    if (tag === "LI") return "listitem";
    if (tag === "UL" || tag === "OL") return "list";
    if (tag === "SUMMARY") return "button";
    return "";
  };

  const labelledByText = (el) => {
    const ids = (el.getAttribute("aria-labelledby") || "").split(/\s+/).filter(Boolean);
    if (!ids.length) return "";
    return ids
      .map((id) => {
        const label = document.getElementById(id);
        return label ? label.textContent : "";
      })
      .join(" ");
  };

  const associatedLabelText = (el) => {
    if (!("labels" in el) || !el.labels) return "";
    return Array.from(el.labels)
      .map((label) => label.innerText || "")
      .join(" ");
  };

  const accessibleName = (el) => {
    const direct =
      el.getAttribute("aria-label") ||
      labelledByText(el) ||
      associatedLabelText(el) ||
      el.getAttribute("alt") ||
      el.getAttribute("title") ||
      "";
    if (direct) return normalize(direct, NAME_MAX);
    if (el instanceof HTMLInputElement && ["button", "submit", "reset"].includes((el.type || "").toLowerCase())) {
      return normalize(el.value, NAME_MAX);
    }
    if (el instanceof HTMLElement && ["BUTTON", "A", "SUMMARY", "OPTION", "LABEL"].includes(el.tagName)) {
      return normalize(el.innerText, NAME_MAX);
    }
    return "";
  };

  // Freeform inputs hold user-typed content (search text, credentials): record
  // their label and shape but never their value.
  const isSensitiveElement = (el) =>
    (el instanceof HTMLInputElement && !["button", "submit", "reset", "image", "checkbox", "radio"].includes((el.type || "").toLowerCase())) ||
    el.tagName === "TEXTAREA" ||
    el.tagName === "SELECT" ||
    el.tagName === "OPTION" ||
    (el instanceof HTMLElement && el.isContentEditable) ||
    (el.matches && el.matches("[autocomplete*='password' i], [autocomplete*='cc-' i], [type='password' i]")) ||
    !!(el.querySelector && Array.from(el.querySelectorAll("[contenteditable]")).some((descendant) => descendant instanceof HTMLElement && descendant.isContentEditable)) ||
    !!(el.querySelector && el.querySelector("input[type='password'], [autocomplete*='cc-' i]"));

  const describe = (el) => {
    const r = el.getBoundingClientRect();
    const sensitive = isSensitiveElement(el);
    const link = el instanceof HTMLAnchorElement ? el : el.closest ? el.closest("a[href]") : null;
    return {
      tag: el.tagName.toLowerCase(),
      selector: uniqueSelector(el),
      role: normalize(el.getAttribute("role") || implicitRole(el), 64) || null,
      name: sensitive ? null : accessibleName(el) || null,
      text: sensitive ? null : normalize(el instanceof HTMLElement ? el.innerText : el.textContent, TEXT_MAX) || null,
      html: sensitive ? null : (el.outerHTML || "").slice(0, HTML_MAX) || null,
      href: link ? link.getAttribute("href") : null,
      rect: {
        x: Math.round(r.x),
        y: Math.round(r.y),
        w: Math.round(r.width),
        h: Math.round(r.height),
      },
    };
  };

  const shortLabel = (el) => {
    const classes = el instanceof HTMLElement && typeof el.className === "string"
      ? el.className.trim().split(/\s+/).filter(Boolean).slice(0, 2).map((c) => "." + c).join("")
      : "";
    return el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + classes;
  };

  // --- Overlay -----------------------------------------------------------

  const host = document.createElement("div");
  host.setAttribute(HOST_ATTR, "");
  host.style.cssText = "position:fixed;inset:0;z-index:" + Z + ";pointer-events:none";
  const shadow = host.attachShadow({ mode: "closed" });
  const style = document.createElement("style");
  style.textContent = [
    ".box{position:fixed;pointer-events:none;border:2px solid;border-radius:3px;box-sizing:border-box;display:none}",
    ".tag{position:fixed;pointer-events:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:280px;background:#111827;color:#f9fafb;font:11px/1.4 ui-monospace,monospace;padding:1px 6px;border-radius:4px;display:none}",
    ".card{position:fixed;pointer-events:auto;display:none;box-sizing:border-box;background:#15181f;border:1px solid rgba(255,255,255,0.15);border-radius:17px;box-shadow:0 8px 24px rgba(0,0,0,0.45)}",
  ].join("\n");
  shadow.appendChild(style);
  const root = document.createElement("div");
  shadow.appendChild(root);

  const hoverBox = document.createElement("div");
  hoverBox.className = "box";
  hoverBox.style.borderColor = HOVER_BORDER;
  hoverBox.style.background = HOVER_FILL;
  root.appendChild(hoverBox);

  // The comment pill lives in a same-origin srcdoc iframe, not the page DOM:
  // events inside an iframe document never propagate to the parent document,
  // so page listeners — capture or bubble — cannot observe comment keystrokes.
  // Bind its handlers from this host-injected script: srcdoc inherits the
  // page's CSP, which can block inline scripts and silently disable Attach.
  const card = document.createElement("iframe");
  card.className = "card";
  card.setAttribute(
    "srcdoc",
    "<style>" +
      "html,body{margin:0;height:100%;background:#15181f;overflow:hidden}" +
      ".row{display:flex;align-items:center;gap:6px;height:100%;box-sizing:border-box;padding:4px 6px 4px 12px;color:#f9fafb;font:12px/1.4 -apple-system,system-ui,sans-serif;white-space:nowrap}" +
      ".count{font-size:11px;opacity:0.65;flex:0 1 auto;min-width:0;max-width:120px;overflow:hidden;text-overflow:ellipsis}" +
      "#c{flex:1 1 auto;min-width:40px;background:transparent;border:0;outline:none;color:#f9fafb;font:12px/1.4 -apple-system,system-ui,sans-serif;padding:0}" +
      "#c::placeholder{color:rgba(249,250,251,0.45)}" +
      "#a{flex:none;width:22px;height:22px;border-radius:50%;border:0;background:" + PICK_BORDER + ";color:#fff;font:600 12px/1 -apple-system,system-ui,sans-serif;cursor:pointer;padding:0}" +
      "</style>" +
      "<div class=row><span id=count class=count></span>" +
      "<input id=c type=text placeholder='Add a comment\u2026' aria-label='Annotation comment'>" +
      "<button id=a type=button aria-label='Attach annotation' title='Attach'>\u21b5</button></div>",
  );
  root.appendChild(card);
  const cardEl = (id) => card.contentDocument && card.contentDocument.getElementById(id);

  const selected = new Map();
  let hovered = null;
  let pointer = { x: 0, y: 0, inside: false, overOverlay: false, needsHitTest: false };
  let frame = null;
  let done = false;

  const isOverlayNode = (node) =>
    node instanceof Node && (node === host || (node instanceof Element && !!node.closest("[" + HOST_ATTR + "]")));

  // elementsFromPoint returns the full hit stack: skip our overlay nodes and
  // page-level shells so transparent covers can't swallow the real target.
  const pickAt = (x, y) => {
    for (const el of document.elementsFromPoint(x, y)) {
      if (!(el instanceof Element)) continue;
      if (isOverlayNode(el)) continue;
      if (el === document.documentElement || el === document.body) continue;
      return el;
    }
    return null;
  };

  const placeBox = (box, rect) => {
    box.style.display = "block";
    box.style.transform = "translate(" + rect.left + "px," + rect.top + "px)";
    box.style.width = rect.width + "px";
    box.style.height = rect.height + "px";
  };

  const unionRect = () => {
    let union = null;
    for (const el of selected.keys()) {
      if (!el.isConnected) continue;
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      union = union
        ? {
            left: Math.min(union.left, r.left),
            top: Math.min(union.top, r.top),
            right: Math.max(union.right, r.right),
            bottom: Math.max(union.bottom, r.bottom),
          }
        : { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
    }
    return union;
  };

  const positionCard = (bounds) => {
    const w = Math.min(360, window.innerWidth - 12);
    const h = 34;
    card.style.width = w + "px";
    card.style.height = h + "px";
    card.style.display = "block";
    const gap = 8;
    let left = bounds.left + (bounds.right - bounds.left) / 2 - w / 2;
    let top = bounds.bottom + gap;
    if (top + h > window.innerHeight - gap) top = bounds.top - h - gap;
    left = Math.max(gap, Math.min(left, window.innerWidth - w - gap));
    top = Math.max(gap, Math.min(top, window.innerHeight - h - gap));
    card.style.transform = "translate(" + left + "px," + top + "px)";
  };

  const refreshCard = () => {
    const bounds = unionRect();
    if (!bounds) {
      card.style.display = "none";
      return;
    }
    const count = cardEl("count");
    if (count) {
      count.textContent =
        selected.size === 1 ? shortLabel(selected.keys().next().value) : selected.size + " selected";
    }
    positionCard(bounds);
  };

  const repaint = () => {
    frame = null;
    if (done) return;
    if (hovered && !hovered.isConnected) hovered = null;
    if (pointer.needsHitTest && selected.size === 0) {
      pointer.needsHitTest = false;
      hovered = pointer.inside && !pointer.overOverlay ? pickAt(pointer.x, pointer.y) : null;
    }
    if (hovered) placeBox(hoverBox, hovered.getBoundingClientRect());
    else hoverBox.style.display = "none";
    for (const [el, visuals] of selected) {
      if (!el.isConnected) {
        visuals.box.remove();
        visuals.label.remove();
        selected.delete(el);
        continue;
      }
      const rect = el.getBoundingClientRect();
      placeBox(visuals.box, rect);
      visuals.label.style.display = "block";
      visuals.label.style.transform =
        "translate(" + Math.max(4, rect.left) + "px," + Math.max(4, rect.top - 20) + "px)";
    }
    if (selected.size === 0) card.style.display = "none";
    else refreshCard();
    scheduleFrame();
  };

  const scheduleFrame = () => {
    if (frame === null && !done) frame = requestAnimationFrame(repaint);
  };

  const clearSelection = () => {
    for (const [, visuals] of selected) {
      visuals.box.remove();
      visuals.label.remove();
    }
    selected.clear();
  };

  const toggleSelect = (el, additive) => {
    if (selected.has(el)) {
      const visuals = selected.get(el);
      visuals.box.remove();
      visuals.label.remove();
      selected.delete(el);
      return;
    }
    if (!additive) clearSelection();
    const box = document.createElement("div");
    box.className = "box";
    box.style.borderColor = PICK_BORDER;
    box.style.background = PICK_FILL;
    const label = document.createElement("div");
    label.className = "tag";
    label.textContent = shortLabel(el);
    root.appendChild(box);
    root.appendChild(label);
    selected.set(el, { box, label });
  };

  const commit = () => {
    if (selected.size === 0) return;
    const elements = [];
    for (const el of selected.keys()) {
      if (el.isConnected) elements.push(describe(el));
    }
    if (!elements.length) return;
    const bounds = unionRect();
    const pad = 16;
    let crop = null;
    if (bounds) {
      const x = Math.max(0, Math.round(bounds.left - pad));
      const y = Math.max(0, Math.round(bounds.top - pad));
      const w = Math.min(Math.round(window.innerWidth) - x, Math.round(bounds.right - bounds.left + pad * 2));
      const h = Math.min(Math.round(window.innerHeight) - y, Math.round(bounds.bottom - bounds.top + pad * 2));
      if (w > 0 && h > 0) crop = { x, y, w, h };
    }
    // Hide the overlay before the host screenshots so boxes never leak into
    // the attached image.
    root.style.display = "none";
    window.__tlane_pick = {
      comment: normalize((cardEl("c") && cardEl("c").value) || "", COMMENT_MAX),
      elements,
      crop,
      viewport: { w: Math.round(window.innerWidth), h: Math.round(window.innerHeight) },
      url: location.href,
      title: document.title,
    };
    uninstall();
  };

  const isolate = (event, prevent) => {
    // Events aimed at overlay nodes retarget to `host` here; leave them be.
    if (isOverlayNode(event.target)) return true;
    if (prevent && event.cancelable) event.preventDefault();
    event.stopImmediatePropagation();
    return false;
  };

  const onPointerMove = (event) => {
    pointer.x = event.clientX;
    pointer.y = event.clientY;
    pointer.inside = true;
    pointer.overOverlay = isOverlayNode(event.target);
    if (!pointer.overOverlay && selected.size === 0) pointer.needsHitTest = true;
    scheduleFrame();
  };

  const onPointerDown = (event) => {
    if (event.button !== 0) return;
    if (isolate(event, true)) return;
    const el = pickAt(event.clientX, event.clientY);
    if (el) {
      hovered = null;
      toggleSelect(el, event.shiftKey);
      pointer.needsHitTest = true;
      repaint();
      if (selected.size > 0) {
        const input = cardEl("c");
        if (input) input.focus({ preventScroll: true });
      }
    }
  };

  const onKeyDown = (event) => {
    if (event.isComposing) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      uninstall();
      return;
    }
    if (isOverlayNode(event.target)) return;
    // Keep the page from observing picker keystrokes (space scrolls,
    // single-letter shortcuts, etc.) while a session is live.
    isolate(event, true);
  };

  function uninstall() {
    if (done) return;
    done = true;
    window.removeEventListener("pointermove", onPointerMove, true);
    window.removeEventListener("pointerdown", onPointerDown, true);
    window.removeEventListener("pointerup", onPointerUp, true);
    window.removeEventListener("click", onClick, true);
    window.removeEventListener("dblclick", onClick, true);
    window.removeEventListener("auxclick", onClick, true);
    window.removeEventListener("contextmenu", onClick, true);
    window.removeEventListener("keydown", onKeyDown, true);
    window.removeEventListener("scroll", scheduleFrame, true);
    window.removeEventListener("resize", scheduleFrame);
    card.removeEventListener("load", onCardLoad);
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    host.remove();
    window.__tlane_annotating = false;
  }
  window.__tlane_uninstall = uninstall;

  const onPointerUp = (event) => {
    if (event.button === 0) isolate(event, false);
  };
  // Click-family events must never reach the page mid-pick (a link click that
  // navigates would destroy the selection before the poll can read it).
  const onClick = (event) => {
    isolate(event, true);
  };

  const onCardLoad = () => {
    if (done) return;
    const input = cardEl("c");
    const attach = cardEl("a");
    if (!input || !attach) return;
    input.addEventListener("keydown", (event) => {
      if (event.isComposing) return;
      if (event.key === "Enter") {
        event.preventDefault();
        commit();
      } else if (event.key === "Escape") {
        event.preventDefault();
        uninstall();
      }
    });
    attach.addEventListener("click", commit);
    refreshCard();
    // Selection can precede the asynchronous srcdoc load. Do not lose the
    // initial focus request, or subsequent typing is swallowed by the picker.
    if (selected.size > 0) input.focus({ preventScroll: true });
  };

  card.addEventListener("load", onCardLoad);
  window.addEventListener("pointermove", onPointerMove, true);
  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("pointerup", onPointerUp, true);
  window.addEventListener("click", onClick, true);
  window.addEventListener("dblclick", onClick, true);
  window.addEventListener("auxclick", onClick, true);
  window.addEventListener("contextmenu", onClick, true);
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("scroll", scheduleFrame, true);
  window.addEventListener("resize", scheduleFrame);
  document.documentElement.appendChild(host);
  return "ok";
})()"##;

/// Returns the picker state as JSON: `{pick, active}`. `pick` is the
/// recorded `{comment, elements, crop, viewport, url, title}` object (or
/// null); `active` is false once the session ends, telling the host to stop
/// polling. The overlay element must also be present: SPA navigation can
/// remove the injected DOM while leaving `__tlane_annotating` set, which
/// would otherwise wedge the panel in the annotating state.
pub fn annotate_poll_js() -> String {
    r#"(() => JSON.stringify({ pick: window.__tlane_pick || null, active: !!window.__tlane_annotating && !!document.querySelector("[data-tlane-annotator]") }))()"#.to_string()
}

/// Tears down the picker overlay and listeners without recording.
pub fn annotate_uninstall_js() -> String {
    r##"(() => { if (window.__tlane_uninstall) window.__tlane_uninstall(); return "ok"; })()"##
        .to_string()
}

#[cfg(test)]
mod annotate_tests {
    use super::*;

    #[test]
    fn annotate_scripts_form_a_complete_protocol() {
        let install = annotate_install_js();
        let poll = annotate_poll_js();
        let uninstall = annotate_uninstall_js();
        // The picker records to a well-known slot the poll reads back.
        for script in [&install, &poll, &uninstall] {
            assert!(script.contains("__tlane_"), "picker slot missing: {script}");
        }
        assert!(install.contains("Escape"));
        assert!(poll.contains("active"));
        // The overlay keeps picking robust: the hit stack skips it, and the
        // page's own DOM is never mutated for highlights.
        assert!(install.contains("elementsFromPoint"));
        assert!(install.contains("attachShadow"));
        // The pick payload carries everything finish_annotation consumes.
        for key in ["comment", "elements", "crop", "url", "title"] {
            assert!(install.contains(key), "pick payload missing {key}");
        }
        // No template placeholders left unsubstituted.
        for script in [&install, &poll, &uninstall] {
            assert!(!script.contains("__MAX__"), "unsubstituted placeholder");
        }
    }
}
