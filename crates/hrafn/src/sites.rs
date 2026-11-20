//! Site adapters: ChatGPT, LinkedIn and Google Docs/Sheets/Slides.
//!
//! They are built from the generic commands (navigate, click, type, keys, eval),
//! so they work in your own logged-in browser session and need no extension
//! changes. Page structure lives in the small JavaScript snippets below, with
//! several fallbacks each, because these sites change their markup often.

use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub type Rpc<'a> = &'a dyn Fn(&str, Value, Option<i64>) -> Result<Value, String>;

fn eval(rpc: Rpc, tab: Option<i64>, code: &str) -> Result<Value, String> {
    rpc("eval", json!({ "code": code }), tab)
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The tab to use for a site: the current tab if it shows the site (by address or
/// by its page markup), else another open tab of the site, else open `home` (or
/// fail when `home` is empty).
fn site_tab(rpc: Rpc, tab: Option<i64>, host: &str, home: &str, looks_right: &str) -> Result<i64, String> {
    if let Some(t) = tab {
        return Ok(t);
    }
    // The current tab, if it already shows the site (or a page that looks like it).
    if eval(rpc, None, &format!("location.host.includes({}) || !!({looks_right})", js_str(host))).is_ok_and(|v| v == Value::Bool(true))
        && let Some(id) = rpc("current_tab", json!({}), None)?["id"].as_i64()
    {
        return Ok(id);
    }
    let tabs = rpc("tabs", json!({}), None)?;
    if let Some(id) = tabs
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["url"].as_str().is_some_and(|u| u.contains(host)))
        .and_then(|t| t["id"].as_i64())
    {
        return Ok(id);
    }
    if home.is_empty() {
        return Err(format!("no {host} page is open: open one in the browser (or pass its URL)"));
    }
    let opened = rpc("open", json!({ "url": home }), None)?;
    opened["id"].as_i64().ok_or_else(|| "could not open the site".into())
}

fn wait_for(rpc: Rpc, tab: i64, selector: &str, ms: u64) -> Result<(), String> {
    rpc("wait_for", json!({ "selector": selector, "timeout_ms": ms }), Some(tab)).map(|_| ())
}

// ======================================================================== ChatGPT

const CHATGPT_HOME: &str = "https://chatgpt.com/";
const COMPOSER: &str = "#prompt-textarea, form [contenteditable='true'], form textarea";
const LOOKS_LIKE_CHATGPT: &str = "document.querySelector('#prompt-textarea') && document.querySelector('button[aria-label*=\"ChatGPT model\" i], [data-testid=\"model-switcher-dropdown-button\"], [data-content-search-unit-key], [data-message-author-role]')";

/// Messages in either markup ChatGPT has used: `data-message-author-role` (older)
/// and `data-content-search-unit-key="<conversation>:<n>:<role>"` (late 2026).
/// Screen-reader labels ("ChatGPT said:", "You said:") are removed.
const CHATGPT_MESSAGES: &str = r#"(() => {
  const strip = (t) => t.replace(/^\s*(ChatGPT|You)\s+said:\s*/i, '').trim();
  const old = [...document.querySelectorAll('[data-message-author-role]')]
    .map((m) => ({ role: m.getAttribute('data-message-author-role'), text: strip(m.innerText) }));
  if (old.length) return old;
  return [...document.querySelectorAll('[data-content-search-unit-key]')]
    .map((m) => ({ role: (m.getAttribute('data-content-search-unit-key').split(':').pop() || ''), text: strip(m.innerText) }))
    .filter((m) => m.role === 'user' || m.role === 'assistant');
})()"#;

/// Selectors for ChatGPT's controls. The page keeps hidden copies of some
/// buttons, so only *visible* matches count.
const STOP_BUTTONS: &str = "[data-testid=\"stop-button\"], button[aria-label=\"Stop\" i], button[aria-label*=\"stop streaming\" i], button[aria-label*=\"stop generating\" i]";
const MODEL_BUTTONS: &str = "[data-testid=\"model-switcher-dropdown-button\"], button[aria-label*=\"ChatGPT model\" i], button[aria-label*=\"Model selector\" i]";

const CHATGPT_STATE: &str = r#"(() => {
  const visible = (sel) => [...document.querySelectorAll(sel)].find((e) => e.getClientRects().length > 0);
  const msgs = (MESSAGES);
  const answers = msgs.filter((m) => m.role === 'assistant');
  const stop = visible(STOP);
  const model = visible(MODEL);
  const login = visible('[data-testid="login-button"], a[href*="/auth/login"]');
  return {
    assistant_count: answers.length,
    last: answers.length ? answers[answers.length - 1].text : '',
    generating: !!stop,
    model: model ? model.innerText.trim().split('\n')[0].trim() : '',
    logged_in: !login && !!document.querySelector('#prompt-textarea, form [contenteditable="true"], form textarea'),
    url: location.href,
    title: document.title,
  };
})()"#;

fn chatgpt_state_js() -> String {
    CHATGPT_STATE
        .replace("(MESSAGES)", CHATGPT_MESSAGES)
        .replace("visible(STOP)", &format!("visible({})", js_str(STOP_BUTTONS)))
        .replace("visible(MODEL)", &format!("visible({})", js_str(MODEL_BUTTONS)))
}

/// Click the first *visible* element matching `selectors` (by its on-screen centre).
fn click_visible(rpc: Rpc, tab: i64, selectors: &str) -> Result<(), String> {
    let xy = eval(
        rpc,
        Some(tab),
        &format!(
            "(() => {{ const e = [...document.querySelectorAll({})].find((x) => x.getClientRects().length > 0); if (!e) return null; e.scrollIntoView({{ block: 'center' }}); const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
            js_str(selectors)
        ),
    )?;
    if xy.is_null() {
        return Err("not visible".into());
    }
    rpc("click", json!({ "at": xy }), Some(tab)).map(|_| ())
}

fn chatgpt_state(rpc: Rpc, tab: i64) -> Result<Value, String> {
    eval(rpc, Some(tab), &chatgpt_state_js())
}

pub fn chatgpt(rpc: Rpc, tab: Option<i64>, args: &[String], raw_json: bool) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let rest = &args[args.len().min(1)..];
    let tab = site_tab(rpc, tab, "chatgpt.com", CHATGPT_HOME, LOOKS_LIKE_CHATGPT)?;
    match sub {
        "status" => {
            let s = chatgpt_state(rpc, tab)?;
            if raw_json {
                println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
            } else {
                println!("tab {tab}  {}", s["url"].as_str().unwrap_or(""));
                println!("signed in: {}", if s["logged_in"] == true { "yes" } else { "no (sign in in the browser first)" });
                println!("model: {}", s["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("(not shown)"));
                println!("answering now: {}", if s["generating"] == true { "yes" } else { "no" });
            }
            Ok(())
        }
        "send" | "ask" => {
            let stream = rest.iter().any(|a| a == "--stream");
            let timeout = rest
                .iter()
                .position(|a| a == "--timeout")
                .and_then(|i| rest.get(i + 1))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(300);
            let words: Vec<&str> = {
                let mut skip = false;
                rest.iter()
                    .filter(|a| {
                        if skip {
                            skip = false;
                            return false;
                        }
                        if *a == "--timeout" {
                            skip = true;
                            return false;
                        }
                        *a != "--stream"
                    })
                    .map(String::as_str)
                    .collect()
            };
            let prompt = if words == ["-"] {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
                s
            } else {
                words.join(" ")
            };
            if prompt.trim().is_empty() {
                return Err("usage: hrafn chatgpt send <prompt> [--stream] [--timeout SECONDS]  (prompt `-` reads stdin)".into());
            }
            let answer = chatgpt_send(rpc, tab, &prompt, stream, Duration::from_secs(timeout))?;
            if !stream {
                println!("{answer}");
            }
            Ok(())
        }
        "read" => {
            if rest.iter().any(|a| a == "--all") {
                let all = eval(rpc, Some(tab), CHATGPT_MESSAGES)?;
                if raw_json {
                    println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
                } else {
                    for m in all.as_array().into_iter().flatten() {
                        println!("--- {}\n{}\n", m["role"].as_str().unwrap_or(""), m["text"].as_str().unwrap_or(""));
                    }
                }
            } else {
                println!("{}", chatgpt_state(rpc, tab)?["last"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "conversations" | "list" => {
            let list = eval(
                rpc,
                Some(tab),
                "[...document.querySelectorAll('nav a[href*=\"/c/\"], aside a[href*=\"/c/\"]')].map((a) => ({ id: a.getAttribute('href').split('/c/')[1].split(/[/?#]/)[0], title: a.innerText.trim() })).filter((c, i, all) => c.id && all.findIndex((d) => d.id === c.id) === i).slice(0, 100)",
            )?;
            if raw_json {
                println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
            } else {
                for c in list.as_array().into_iter().flatten() {
                    println!("{}  {}", c["id"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        "switch" | "open" => {
            let id = rest.first().ok_or("usage: hrafn chatgpt switch <conversation-id>")?;
            let origin = eval(rpc, Some(tab), "location.origin")?;
            rpc("navigate", json!({ "url": format!("{}/c/{id}", origin.as_str().unwrap_or("https://chatgpt.com")) }), Some(tab))?;
            wait_for(rpc, tab, COMPOSER, 30_000)?;
            println!("opened conversation {id}");
            Ok(())
        }
        "new" => {
            let origin = eval(rpc, Some(tab), "location.origin")?;
            rpc("navigate", json!({ "url": format!("{}/", origin.as_str().unwrap_or("https://chatgpt.com")) }), Some(tab))?;
            wait_for(rpc, tab, COMPOSER, 30_000)?;
            println!("new chat ready");
            Ok(())
        }
        "stop" => {
            click_visible(rpc, tab, STOP_BUTTONS).map_err(|_| "ChatGPT is not answering right now".to_string())?;
            println!("stopped");
            Ok(())
        }
        "model" => {
            if rest.is_empty() {
                println!("{}", chatgpt_state(rpc, tab)?["model"].as_str().unwrap_or("(not shown)"));
                return Ok(());
            }
            let name = rest.join(" ");
            click_visible(rpc, tab, MODEL_BUTTONS).map_err(|_| "could not find the model menu on this page".to_string())?;
            std::thread::sleep(Duration::from_millis(500));
            let picked = eval(
                rpc,
                Some(tab),
                &format!(
                    "(() => {{ const want = {}.toLowerCase(); const items = [...document.querySelectorAll('[role=\"menuitem\"], [role=\"menuitemradio\"], [role=\"option\"]')]; const hit = items.find((i) => i.innerText.toLowerCase().includes(want)); if (!hit) return items.map((i) => i.innerText.split('\\n')[0]).join(' | '); hit.click(); return true; }})()",
                    js_str(&name)
                ),
            )?;
            if picked == Value::Bool(true) {
                std::thread::sleep(Duration::from_millis(500));
                println!("model: {}", chatgpt_state(rpc, tab)?["model"].as_str().unwrap_or(""));
                Ok(())
            } else {
                let _ = rpc("keys", json!({ "combo": "Escape" }), Some(tab));
                Err(format!("no model matching \"{name}\"; the menu offers: {}", picked.as_str().unwrap_or("?")))
            }
        }
        _ => Err("usage: hrafn chatgpt status | send <prompt> [--stream] | read [--all] | conversations | switch <id> | new | stop | model [name]".into()),
    }
}

fn chatgpt_send(rpc: Rpc, tab: i64, prompt: &str, stream: bool, timeout: Duration) -> Result<String, String> {
    wait_for(rpc, tab, COMPOSER, 30_000).map_err(|_| "no ChatGPT message box on the page (are you signed in?)".to_string())?;
    let before = chatgpt_state(rpc, tab)?;
    if before["logged_in"] != true {
        return Err("ChatGPT is not signed in in this browser".into());
    }
    if before["generating"] == true {
        return Err("ChatGPT is still answering; wait or run `hrafn chatgpt stop`".into());
    }
    let count = before["assistant_count"].as_i64().unwrap_or(0);
    // `type` empties the composer first, then types (insertText works with the rich editor).
    rpc("type", json!({ "target": "#prompt-textarea", "text": prompt }), Some(tab))
        .or_else(|_| rpc("type", json!({ "target": COMPOSER, "text": prompt }), Some(tab)))?;
    std::thread::sleep(Duration::from_millis(300));
    if click_visible(rpc, tab, "[data-testid=\"send-button\"], button[aria-label=\"Send prompt\" i], button[aria-label*=\"Send message\" i]").is_err() {
        rpc("keys", json!({ "combo": "Enter" }), Some(tab))?;
    }
    // Wait for a new answer, then for it to finish and stop changing.
    let start = Instant::now();
    let mut shown = String::new();
    let mut last_text = String::new();
    let mut stable_since = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(format!("no complete answer after {}s (the partial answer is in `hrafn chatgpt read`)", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(400));
        let s = chatgpt_state(rpc, tab)?;
        if s["assistant_count"].as_i64().unwrap_or(0) <= count {
            continue;
        }
        let text = s["last"].as_str().unwrap_or("").to_string();
        // Stream only what was appended; if the page rewrote earlier text, the
        // final answer is still complete in `hrafn chatgpt read`.
        if stream && text.len() > shown.len() && text.starts_with(shown.as_str()) {
            use std::io::Write;
            print!("{}", &text[shown.len()..]);
            let _ = std::io::stdout().flush();
            shown = text.clone();
        }
        if text != last_text {
            last_text = text;
            stable_since = Instant::now();
        } else if s["generating"] != true && !last_text.is_empty() && stable_since.elapsed() > Duration::from_millis(1200) {
            if stream {
                println!();
            }
            return Ok(last_text);
        }
    }
}

// ======================================================================== LinkedIn

/// LinkedIn's own class-name families (for a page saved or served elsewhere).
const LOOKS_LIKE_LINKEDIN: &str = "document.querySelector('[class*=\"job-details-jobs\"], [class*=\"jobs-unified\"], [class*=\"update-components\"], [class*=\"feed-shared\"], [class*=\"top-card-layout\"], [class*=\"events-top-card\"]')";

const LINKEDIN_JOB: &str = r#"(() => {
  const t = (s) => { for (const q of s.split('||')) { const e = document.querySelector(q.trim()); if (e && e.innerText.trim()) return e.innerText.trim(); } return ''; };
  let ld = null;
  for (const s of document.querySelectorAll('script[type="application/ld+json"]')) {
    try { const j = JSON.parse(s.textContent); const all = [].concat(j['@graph'] || j); ld = all.find((x) => x['@type'] === 'JobPosting') || ld; } catch (_) {}
  }
  const tmp = document.createElement('div');
  const strip = (h) => { tmp.innerHTML = h || ''; return tmp.innerText.trim(); };
  const insights = [...document.querySelectorAll('.job-details-jobs-unified-top-card__job-insight, .job-details-preferences-and-skills__pill, .job-details-fit-level-preferences button')].map((e) => e.innerText.trim().replace(/\s+/g, ' ')).filter(Boolean);
  // Markup-independent reading (LinkedIn's late-2026 job pages use scrambled class
  // names): the tab title is "Job | Company | LinkedIn", the text around the job
  // title on screen holds location/age/applicants, and "About the job" heads the
  // description.
  const parts = document.title.split(' | ');
  const gTitle = parts.length >= 3 ? parts[0].trim() : '';
  const gCompany = parts.length >= 3 ? parts[1].trim() : '';
  let lines = [];
  if (gTitle) {
    const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT); let n; const hits = [];
    while ((n = w.nextNode())) if (n.nodeValue.trim() === gTitle && n.parentElement.getClientRects().length) hits.push(n.parentElement);
    const hit = hits[hits.length - 1];
    if (hit) { let card = hit; for (let i = 0; i < 8 && card.parentElement && card.innerText.length < 300; i++) card = card.parentElement;
      lines = card.innerText.split('\n').map((s) => s.trim()).filter((s) => s && s !== '·' && !/^selected,/i.test(s)); }
  }
  const about = [...document.querySelectorAll('h1, h2, h3')].find((h) => /^about the job$/i.test(h.innerText.trim()));
  // The largest block around "About the job" that holds no other visible heading.
  const headings = (e) => [...e.querySelectorAll('h1, h2, h3')].filter((h) => h.getClientRects().length > 0).length;
  let sec = about; while (sec && sec.parentElement && headings(sec.parentElement) <= 1) sec = sec.parentElement;
  const gDescription = sec ? sec.innerText.replace(/^\s*About the job\s*/i, '').trim() : '';
  // Short facts, split at LinkedIn's " · " separators ("Seattle, WA · 2 weeks ago · 19 people clicked apply").
  const facts = [...new Set(lines.filter((l) => l.length < 90).flatMap((l) => l.split(/\s+·\s+/)).map((s) => s.trim()).filter(Boolean))];
  const gLocation = facts.find((l) => l !== gTitle && l !== gCompany && /,\s*[A-Z]{2}\b|\bremote\b|\bhybrid\b|on-site|united states/i.test(l)) || '';
  const gSummary = [gLocation, facts.find((l) => /\bago\b|posted|reposted/i.test(l)), facts.find((l) => /applicant|clicked apply/i.test(l))].filter(Boolean).join(' · ');
  const gDetails = facts.filter((l) => l !== gLocation && l !== gTitle && !/premium/i.test(l) && /full-time|part-time|contract|temporary|internship|\bremote\b|hybrid|on-site|\$\d/i.test(l));
  return {
    title: t('.job-details-jobs-unified-top-card__job-title || .top-card-layout__title || .jobs-unified-top-card__job-title') || (ld && ld.title) || gTitle,
    company: t('.job-details-jobs-unified-top-card__company-name || .topcard__org-name-link || .jobs-unified-top-card__company-name') || (ld && ld.hiringOrganization && ld.hiringOrganization.name) || gCompany,
    location: t('.job-details-jobs-unified-top-card__primary-description-container .tvm__text || .topcard__flavor--bullet || .jobs-unified-top-card__bullet') || (ld && ld.jobLocation && [].concat(ld.jobLocation)[0]?.address?.addressLocality) || gLocation,
    summary: t('.job-details-jobs-unified-top-card__primary-description-container || .jobs-unified-top-card__primary-description') || gSummary,
    details: insights.length ? insights : gDetails,
    description: t('.jobs-description__content || #job-details || .description__text || .show-more-less-html__markup') || (ld ? strip(ld.description) : '') || gDescription,
    url: location.href,
  };
})()"#;

const LINKEDIN_EVENT: &str = r#"(() => {
  const t = (s) => { for (const q of s.split('||')) { const e = document.querySelector(q.trim()); if (e && e.innerText.trim()) return e.innerText.trim(); } return ''; };
  let ld = null;
  for (const s of document.querySelectorAll('script[type="application/ld+json"]')) {
    try { const j = JSON.parse(s.textContent); const all = [].concat(j['@graph'] || j); ld = all.find((x) => /Event$/.test(x['@type'] || '')) || ld; } catch (_) {}
  }
  const attendees = [...document.querySelectorAll('a, button, span')].map((e) => e.innerText.trim()).find((x) => /^[\d,.]+[KkMm]?\s+(attendees?|going)/i.test(x)) || '';
  return {
    title: t('.events-top-card__title || h1') || (ld && ld.name) || '',
    organizer: t('.events-top-card__organizer-name || .event-organizer a || [data-test-id="event-organizer"]') || (ld && ld.organizer && [].concat(ld.organizer)[0]?.name) || '',
    when: t('.events-top-card__time || .event-time || time') || (ld && ld.startDate) || '',
    where: t('.events-top-card__location || .event-location') || (ld && ld.location && (ld.location.name || ld.location.url)) || '',
    attendees,
    description: t('.events-about__description || .event-description || [data-test-id="event-description"]') || (ld && ld.description) || '',
    url: location.href,
  };
})()"#;

const LINKEDIN_POST: &str = r#"(() => {
  const root = document.querySelector('.feed-shared-update-v2, [data-urn*="activity"], article') || document;
  const t = (s) => { for (const q of s.split('||')) { const e = root.querySelector(q.trim()); if (e && e.innerText.trim()) return e.innerText.trim(); } return ''; };
  return {
    author: t('.update-components-actor__title span[aria-hidden="true"] || .update-components-actor__name || .feed-shared-actor__name'),
    author_headline: t('.update-components-actor__description span[aria-hidden="true"] || .update-components-actor__description'),
    text: t('.update-components-text || .feed-shared-update-v2__description || .feed-shared-text'),
    reactions: t('.social-details-social-counts__reactions-count || .social-details-social-counts__social-proof-text'),
    comments: t('.social-details-social-counts__comments || [aria-label*="comment" i]'),
    url: location.href,
  };
})()"#;

const LINKEDIN_PROFILE: &str = r#"(() => {
  const t = (s) => { for (const q of s.split('||')) { const e = document.querySelector(q.trim()); if (e && e.innerText.trim()) return e.innerText.trim(); } return ''; };
  const section = (id) => { const a = document.getElementById(id); const s = a && a.closest('section'); return s ? s.innerText.trim() : ''; };
  return {
    name: t('h1'),
    headline: t('.text-body-medium.break-words || .top-card-layout__headline'),
    location: t('.text-body-small.inline.t-black--light.break-words || .top-card__subline-item'),
    about: section('about'),
    experience: section('experience'),
    url: location.href,
  };
})()"#;

/// Collect the attendee list from the open dialog (or the page), scrolling to load more.
const LINKEDIN_ATTENDEES: &str = r#"(async (max) => {
  const visibleDialog = () => [...document.querySelectorAll('[role="dialog"]')].find((d) => d.getClientRects().length > 0);
  const opener = [...document.querySelectorAll('a, button')].find((e) => /attendee|going/i.test(e.innerText) && /\d/.test(e.innerText));
  if (opener && !visibleDialog()) { opener.click(); await new Promise((r) => setTimeout(r, 1500)); }
  const box = visibleDialog() || document;
  const scroller = box.querySelector('.artdeco-modal__content, [class*="scroll"]') || box;
  const seen = new Map();
  for (let round = 0; round < 40 && seen.size < max; round++) {
    for (const li of box.querySelectorAll('li')) {
      const a = li.querySelector('a[href*="/in/"]');
      if (!a) continue;
      const url = a.href.split('?')[0];
      if (seen.has(url)) continue;
      const lines = li.innerText.split('\n').map((x) => x.trim()).filter((x) => x && !/^(•|·|\d+(st|nd|rd|th)|Connect|Follow|Message)$/i.test(x));
      seen.set(url, { name: lines[0] || '', headline: lines.find((x, i) => i > 0 && x !== lines[0]) || '', url });
      if (seen.size >= max) break;
    }
    const before = seen.size;
    scroller.scrollTop = scroller.scrollHeight;
    const more = [...box.querySelectorAll('button')].find((b) => /show more|load more/i.test(b.innerText));
    if (more) more.click();
    await new Promise((r) => setTimeout(r, 900));
    if (seen.size === before && !more && round > 2) break;
  }
  return [...seen.values()];
})"#;

pub fn linkedin(rpc: Rpc, tab: Option<i64>, args: &[String], raw_json: bool) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let mut rest: Vec<String> = args[args.len().min(1)..].to_vec();
    let max = match rest.iter().position(|a| a == "--max") {
        Some(i) if i + 1 < rest.len() => {
            let v = rest.remove(i + 1);
            rest.remove(i);
            v.parse::<u64>().map_err(|_| "--max needs a number")?.min(1000)
        }
        _ => 100,
    };
    let script = match sub {
        "job" => LINKEDIN_JOB,
        "event" => LINKEDIN_EVENT,
        "post" => LINKEDIN_POST,
        "profile" => LINKEDIN_PROFILE,
        "attendees" => "",
        _ => return Err("usage: hrafn linkedin job|event|post|profile [url] | attendees [url] [--max N]".into()),
    };
    // A URL opens in a new tab; otherwise read the current LinkedIn tab.
    let tab = match rest.first().filter(|u| u.starts_with("http")) {
        Some(url) => rpc("open", json!({ "url": url }), None)?["id"].as_i64().ok_or("could not open the page")?,
        None => site_tab(rpc, tab, "linkedin.com", "", LOOKS_LIKE_LINKEDIN)?,
    };
    let _ = rpc("wait_stable", json!({ "quiet_ms": 600, "timeout_ms": 10_000 }), Some(tab));
    let data = if sub == "attendees" {
        eval(rpc, Some(tab), &format!("{LINKEDIN_ATTENDEES}({max})"))?
    } else {
        eval(rpc, Some(tab), script)?
    };
    if raw_json {
        println!("{}", serde_json::to_string_pretty(&data).unwrap_or_default());
        return Ok(());
    }
    match &data {
        Value::Array(people) => {
            for p in people {
                println!("{} — {}  {}", p["name"].as_str().unwrap_or(""), p["headline"].as_str().unwrap_or(""), p["url"].as_str().unwrap_or(""));
            }
            println!("({} people)", people.len());
        }
        Value::Object(map) => {
            let empty = map.iter().filter(|(k, _)| *k != "url").all(|(_, v)| v.as_str().is_some_and(str::is_empty) || v.as_array().is_some_and(Vec::is_empty));
            if empty {
                return Err("nothing found: is the right LinkedIn page open (and fully loaded)?".into());
            }
            for (k, v) in map {
                match v {
                    Value::String(s) if s.contains('\n') => println!("{k}:\n{s}\n"),
                    Value::String(s) if !s.is_empty() => println!("{k}: {s}"),
                    Value::Array(a) if !a.is_empty() => {
                        println!("{k}: {}", a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" · "))
                    }
                    _ => {}
                }
            }
        }
        _ => println!("{data}"),
    }
    Ok(())
}

// ======================================================================== Google Docs

/// (kind, id, gid) from a Docs/Sheets/Slides URL path.
pub fn parse_google_url(url: &str) -> Option<(String, String, Option<String>)> {
    let after = url.split("://").nth(1).unwrap_or(url);
    let path = after.split_once('/').map(|x| x.1).unwrap_or("");
    let mut parts = path.split(['/', '?', '#']);
    let kind = parts.next()?;
    if !["document", "spreadsheets", "presentation"].contains(&kind) {
        return None;
    }
    // Paths can be /document/d/ID/... or /document/u/0/d/ID/...
    let segs: Vec<&str> = path.split(['?', '#']).next()?.split('/').collect();
    let d = segs.iter().position(|s| *s == "d")?;
    let id = segs.get(d + 1).filter(|s| !s.is_empty())?.to_string();
    let gid = url.split(['#', '&', '?']).find_map(|p| p.strip_prefix("gid=")).map(String::from);
    Some((kind.to_string(), id, gid))
}

/// Export URL for a file in a given format.
pub fn export_url(origin: &str, kind: &str, id: &str, gid: Option<&str>, format: &str) -> Result<String, String> {
    let ok: &[&str] = match kind {
        "document" => &["txt", "md", "pdf", "docx", "odt", "rtf", "html", "epub"],
        "spreadsheets" => &["csv", "tsv", "xlsx", "ods", "pdf", "html"],
        "presentation" => &["txt", "pdf", "pptx", "odp"],
        _ => &[],
    };
    if !ok.contains(&format) {
        return Err(format!("a {kind} can be exported as: {}", ok.join(", ")));
    }
    Ok(match kind {
        "presentation" => format!("{origin}/presentation/d/{id}/export/{format}"),
        "spreadsheets" => match gid {
            Some(g) if ["csv", "tsv", "pdf"].contains(&format) => format!("{origin}/spreadsheets/d/{id}/export?format={format}&gid={g}"),
            _ => format!("{origin}/spreadsheets/d/{id}/export?format={format}"),
        },
        _ => format!("{origin}/document/d/{id}/export?format={format}"),
    })
}

/// Fetch a URL inside the page (with the user's session) and return its bytes.
fn page_fetch(rpc: Rpc, tab: i64, url: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let code = format!(
        "fetch({}, {{ credentials: 'include' }}).then(async (r) => {{ if (!r.ok) throw new Error('export failed: HTTP ' + r.status); const b = new Uint8Array(await r.arrayBuffer()); let s = ''; for (let i = 0; i < b.length; i += 32768) s += String.fromCharCode.apply(null, b.subarray(i, i + 32768)); return btoa(s); }})",
        js_str(url)
    );
    let b64 = eval(rpc, Some(tab), &code)?;
    base64::engine::general_purpose::STANDARD.decode(b64.as_str().unwrap_or("")).map_err(|e| format!("bad export data: {e}"))
}

const SLIDES_LIST: &str = r#"(() => [...document.querySelectorAll('.punch-filmstrip-thumbnail, [id^="filmstrip-slide-"][id$="-bg"]')]
  .map((s, i) => { const g = s.closest('g') || s; const text = [...g.querySelectorAll('text')].map((t) => t.textContent.trim()).filter(Boolean).join(' / ');
    return { n: i + 1, text, current: !!(g.closest('.punch-filmstrip-selected-thumbnail-pagenumber') || g.querySelector('.punch-filmstrip-selected-thumbnail-pagenumber') || g.getAttribute('aria-selected') === 'true') }; }))()"#;

const SLIDE_NOTES: &str = r#"(() => { const n = document.querySelector('#speakernotes, .punch-viewer-speakernotes-text-body-scrollable, [aria-label*="speaker notes" i]'); return n ? n.innerText.trim() : null; })()"#;

pub fn gdocs(rpc: Rpc, tab: Option<i64>, args: &[String], raw_json: bool) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("info");
    let rest = &args[args.len().min(1)..];
    // Never opens Google Docs by itself: it works on a file you already have open.
    let tab = site_tab(rpc, tab, "docs.google.com", "", "/\\/(document|spreadsheets|presentation)\\/(u\\/\\d+\\/)?d\\//.test(location.pathname)")?;
    let page = eval(rpc, Some(tab), "({ url: location.href, origin: location.origin, title: document.title })")?;
    let url = page["url"].as_str().unwrap_or("");
    let origin = page["origin"].as_str().unwrap_or("https://docs.google.com");
    let (kind, id, gid) = parse_google_url(url).ok_or("open a Google Doc, Sheet or Slides file first (or use --tab)")?;
    let title = page["title"].as_str().unwrap_or("").rsplit_once(" - ").map(|x| x.0).unwrap_or(page["title"].as_str().unwrap_or("")).to_string();
    match sub {
        "info" => {
            let v = json!({ "kind": kind, "id": id, "title": title, "url": url, "sheet_gid": gid });
            if raw_json {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                println!("{kind}: {title}\nid: {id}\nurl: {url}");
            }
            Ok(())
        }
        "text" => {
            let format = match kind.as_str() {
                "spreadsheets" => "csv",
                _ => "txt",
            };
            let bytes = page_fetch(rpc, tab, &export_url(origin, &kind, &id, gid.as_deref(), format)?)?;
            let text = String::from_utf8_lossy(&bytes);
            print!("{}", text.trim_start_matches('\u{feff}'));
            if !text.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        "export" => {
            let (Some(format), Some(file)) = (rest.first(), rest.get(1)) else {
                return Err("usage: hrafn gdocs export <format> <file>".into());
            };
            let bytes = page_fetch(rpc, tab, &export_url(origin, &kind, &id, gid.as_deref(), format)?)?;
            std::fs::write(file, &bytes).map_err(|e| format!("could not write {file}: {e}"))?;
            println!("{file} ({} bytes)", bytes.len());
            Ok(())
        }
        "insert" | "type" => {
            if kind == "spreadsheets" {
                return Err("insert works in Docs and Slides; in Sheets use the cell editor with `hrafn keys`/`type`".into());
            }
            let at_end = rest.iter().any(|a| a == "--end");
            let text: Vec<&str> = rest.iter().filter(|a| *a != "--end").map(String::as_str).collect();
            if text.is_empty() {
                return Err("usage: hrafn gdocs insert <text> [--end]".into());
            }
            if at_end {
                // Put the cursor in the document, then jump to its end.
                let _ = rpc("click", json!({ "target": ".kix-appview-editor, .kix-page-paginated, .kix-canvas-tile-content" }), Some(tab));
                rpc("keys", json!({ "combo": "Control+End" }), Some(tab))?;
            }
            // Typing goes to Docs' hidden input, wherever its cursor is.
            let r = rpc("insert_text", json!({ "text": text.join(" ") }), Some(tab));
            if r.is_err() {
                return Err("could not type into the document (click into it once, then retry)".into());
            }
            println!("inserted {} characters", text.join(" ").chars().count());
            Ok(())
        }
        "slides" => {
            if kind != "presentation" {
                return Err("this is not a Slides file".into());
            }
            let list = eval(rpc, Some(tab), SLIDES_LIST)?;
            if raw_json {
                println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
            } else {
                for s in list.as_array().into_iter().flatten() {
                    println!("{}{:>3}  {}", if s["current"] == true { "*" } else { " " }, s["n"], s["text"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        "slide" | "goto" => {
            let n: usize = rest.first().and_then(|s| s.parse().ok()).ok_or("usage: hrafn gdocs slide <number>")?;
            let clicked = eval(
                rpc,
                Some(tab),
                &format!(
                    "(() => {{ const all = [...document.querySelectorAll('.punch-filmstrip-thumbnail, [id^=\"filmstrip-slide-\"][id$=\"-bg\"]')]; const s = all[{}]; if (!s) return all.length; const r = s.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
                    n.saturating_sub(1)
                ),
            )?;
            match clicked.as_array() {
                Some(xy) => {
                    rpc("click", json!({ "at": xy }), Some(tab))?;
                    println!("on slide {n}");
                    Ok(())
                }
                None => Err(format!("there are {} slides", clicked)),
            }
        }
        "notes" => {
            let notes = eval(rpc, Some(tab), SLIDE_NOTES)?;
            match notes.as_str() {
                Some(t) => {
                    println!("{t}");
                    Ok(())
                }
                None => Err("no speaker notes panel on this page".into()),
            }
        }
        _ => Err("usage: hrafn gdocs info | text | export <format> <file> | insert <text> [--end] | slides | slide <n> | notes".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_urls() {
        assert_eq!(
            parse_google_url("https://docs.google.com/document/d/abc123/edit"),
            Some(("document".into(), "abc123".into(), None))
        );
        assert_eq!(
            parse_google_url("https://docs.google.com/spreadsheets/d/XyZ/edit#gid=42"),
            Some(("spreadsheets".into(), "XyZ".into(), Some("42".into())))
        );
        assert_eq!(
            parse_google_url("https://docs.google.com/presentation/u/1/d/p-9_x/edit?slide=id.p"),
            Some(("presentation".into(), "p-9_x".into(), None))
        );
        assert_eq!(parse_google_url("https://docs.google.com/forms/d/x/edit"), None);
        assert_eq!(parse_google_url("https://example.com/"), None);
    }

    #[test]
    fn export_urls() {
        let o = "https://docs.google.com";
        assert_eq!(export_url(o, "document", "A", None, "md").unwrap(), "https://docs.google.com/document/d/A/export?format=md");
        assert_eq!(export_url(o, "spreadsheets", "S", Some("7"), "csv").unwrap(), "https://docs.google.com/spreadsheets/d/S/export?format=csv&gid=7");
        assert_eq!(export_url(o, "spreadsheets", "S", Some("7"), "xlsx").unwrap(), "https://docs.google.com/spreadsheets/d/S/export?format=xlsx");
        assert_eq!(export_url(o, "presentation", "P", None, "pptx").unwrap(), "https://docs.google.com/presentation/d/P/export/pptx");
        assert!(export_url(o, "document", "A", None, "exe").unwrap_err().contains("txt"));
    }
}
